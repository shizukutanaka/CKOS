//! Session manager (§927) — a durable record of a working session.
//!
//! A [`Session`] wraps a [`Storage`] backend and persists the things worth
//! resuming: the workflow execution history and the reflections produced from it
//! (§921). Pair it with a [`FileStore`](ckos_memory::FileStore) and the history
//! survives process restarts, giving the "fast resume" the spec calls for —
//! reopening a session directory immediately exposes everything that ran before.
//!
//! Live, code-backed state (runtimes, tools) is reconstructed by the host on
//! resume; what the session persists is the *data* trail those subsystems
//! produced.

use crate::engine::ExecutionResult;
use crate::reflection::Reflection;
use ckos_kernel::error::Result;
use ckos_memory::{
    cosine, rank_memories, recency_decay, Document, Embedder, MemorySignals, MemoryWeights, Query,
    Storage,
};

/// Document type tag for persisted execution records.
const EXECUTION: &str = "execution";
/// Document type tag for persisted reflections.
const REFLECTION: &str = "reflection";
/// Metadata key identifying the owning session.
const SESSION_KEY: &str = "session";
/// Metadata key holding a monotonic write sequence, the recency signal (§896).
const SEQ_KEY: &str = "seq";

/// A durable working session backed by a storage layer (§927).
pub struct Session {
    id: String,
    store: Box<dyn Storage>,
    embedder: Option<Box<dyn Embedder>>,
    /// Monotonic write counter for recency; seeded lazily from the store.
    seq: u64,
    seq_seeded: bool,
}

impl Session {
    /// Open (or create) a session with the given id over a storage backend.
    /// Pass a [`FileStore`](ckos_memory::FileStore) for persistence across runs.
    pub fn new(id: impl Into<String>, store: Box<dyn Storage>) -> Self {
        Session {
            id: id.into(),
            store,
            embedder: None,
            seq: 0,
            seq_seeded: false,
        }
    }

    /// Next write sequence, seeded once from the highest existing `seq` in the
    /// store so it keeps increasing across process restarts.
    fn next_seq(&mut self) -> u64 {
        if !self.seq_seeded {
            self.seq = self
                .store
                .search(&Query::default())
                .unwrap_or_default()
                .iter()
                .filter_map(|d| d.metadata.get(SEQ_KEY).and_then(|s| s.parse::<u64>().ok()))
                .max()
                .unwrap_or(0);
            self.seq_seeded = true;
        }
        self.seq += 1;
        self.seq
    }

    /// Attach an embedder so persisted execution outputs carry embeddings,
    /// enabling later vector search (§944, §950).
    pub fn with_embedder(mut self, embedder: Box<dyn Embedder>) -> Self {
        self.embedder = Some(embedder);
        self
    }

    /// Session identifier.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Borrow the underlying store (e.g. to query arbitrary documents).
    pub fn store(&self) -> &dyn Storage {
        self.store.as_ref()
    }

    /// Persist a batch of execution results to the session (§927).
    pub fn record_run(&mut self, results: &[ExecutionResult]) -> Result<()> {
        for r in results {
            let mut doc = Document::new(
                EXECUTION,
                format!("{} on {}", r.capability, r.runtime),
                r.output.clone(),
            );
            doc.confidence = if r.verified { 100 } else { 0 };
            doc.metadata.insert(SESSION_KEY.into(), self.id.clone());
            doc.metadata.insert("task".into(), r.task.to_string());
            doc.metadata
                .insert("capability".into(), r.capability.to_string());
            doc.metadata.insert("runtime".into(), r.runtime.clone());
            doc.metadata
                .insert("verified".into(), r.verified.to_string());
            if let Some(agent) = &r.agent {
                doc.metadata.insert("agent".into(), agent.clone());
            }
            if let Some(embedder) = &self.embedder {
                doc.embedding = Some(embedder.embed(&doc.body));
            }
            let seq = self.next_seq();
            doc.metadata.insert(SEQ_KEY.into(), seq.to_string());
            self.store.write(doc)?;
        }
        Ok(())
    }

    /// Persist a batch of reflections to the session (§921).
    pub fn record_reflections(&mut self, reflections: &[Reflection]) -> Result<()> {
        for r in reflections {
            let mut doc = Document::new(
                REFLECTION,
                format!("reflection for {}", r.task),
                r.hint.clone(),
            );
            doc.confidence = r.score;
            doc.metadata.insert(SESSION_KEY.into(), self.id.clone());
            doc.metadata.insert("task".into(), r.task.to_string());
            let seq = self.next_seq();
            doc.metadata.insert(SEQ_KEY.into(), seq.to_string());
            self.store.write(doc)?;
        }
        Ok(())
    }

    /// All documents of `doc_type` belonging to this session.
    fn owned(&self, doc_type: &str) -> Result<Vec<Document>> {
        let mut docs = self.store.search(&Query {
            doc_type: Some(doc_type.into()),
            ..Default::default()
        })?;
        docs.retain(|d| d.metadata.get(SESSION_KEY).map(String::as_str) == Some(self.id.as_str()));
        Ok(docs)
    }

    /// Execution history for this session, restored from storage (§927).
    pub fn history(&self) -> Result<Vec<Document>> {
        self.owned(EXECUTION)
    }

    /// Reflections recorded in this session.
    pub fn reflections(&self) -> Result<Vec<Document>> {
        self.owned(REFLECTION)
    }

    /// All documents belonging to this session, regardless of type.
    fn all_owned(&self) -> Result<Vec<Document>> {
        let mut docs = self.store.search(&Query::default())?;
        docs.retain(|d| d.metadata.get(SESSION_KEY).map(String::as_str) == Some(self.id.as_str()));
        // Stable order so equal-scored recalls are deterministic.
        docs.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        Ok(docs)
    }

    /// Recall this session's records most relevant to `query`, ranked by the
    /// Generative-Agents memory score — recency (write order) × importance
    /// (confidence) × relevance (embedding similarity, if an embedder is
    /// attached) (§896). Returns up to `k` documents, best first.
    pub fn recall(&self, query: &str, k: usize) -> Result<Vec<Document>> {
        let docs = self.all_owned()?;
        if docs.is_empty() {
            return Ok(Vec::new());
        }
        let max_seq = docs
            .iter()
            .filter_map(|d| d.metadata.get(SEQ_KEY).and_then(|s| s.parse::<u64>().ok()))
            .max()
            .unwrap_or(0);
        let query_emb = self.embedder.as_ref().map(|e| e.embed(query));

        let signals: Vec<MemorySignals> = docs
            .iter()
            .map(|d| {
                let seq = d
                    .metadata
                    .get(SEQ_KEY)
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                let age = max_seq.saturating_sub(seq) as f32;
                let relevance = match (&query_emb, &d.embedding) {
                    (Some(q), Some(e)) => cosine(q, e).clamp(0.0, 1.0),
                    _ => 0.0,
                };
                MemorySignals {
                    recency: recency_decay(age, 0.9),
                    importance: d.confidence as f32 / 100.0,
                    relevance,
                }
            })
            .collect();

        let ranked = rank_memories(&signals, &MemoryWeights::default());
        Ok(ranked
            .into_iter()
            .take(k)
            .map(|(i, _)| docs[i].clone())
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reflection::{HeuristicReflector, Reflector};
    use ckos_kernel::{Capability, TaskId};
    use ckos_memory::InMemoryStore;

    fn result(verified: bool) -> ExecutionResult {
        ExecutionResult {
            task: TaskId::new(),
            capability: Capability::Reasoning,
            agent: Some("reasoning-agent".into()),
            runtime: "echo".into(),
            output: "summary".into(),
            verified,
        }
    }

    #[test]
    fn records_and_restores_history() {
        let mut session = Session::new("s1", Box::new(InMemoryStore::new()));
        let results = vec![result(true), result(false)];
        session.record_run(&results).unwrap();

        let history = session.history().unwrap();
        assert_eq!(history.len(), 2);
        assert!(history.iter().all(|d| d.doc_type == "execution"));
        assert!(history
            .iter()
            .any(|d| d.metadata.get("verified").map(String::as_str) == Some("false")));
    }

    #[test]
    fn records_reflections() {
        let mut session = Session::new("s1", Box::new(InMemoryStore::new()));
        let reflections: Vec<Reflection> = [result(true), result(false)]
            .iter()
            .map(|r| HeuristicReflector::new().reflect(r))
            .collect();
        session.record_reflections(&reflections).unwrap();
        assert_eq!(session.reflections().unwrap().len(), 2);
    }

    #[test]
    fn recall_ranks_by_recency_importance_relevance() {
        use ckos_memory::HashingEmbedder;
        let mut session = Session::new("s1", Box::new(InMemoryStore::new()))
            .with_embedder(Box::new(HashingEmbedder::new(64)));
        // Three runs written in order; the last is most recent.
        let mk = |verified, output: &str| ExecutionResult {
            task: TaskId::new(),
            capability: Capability::Reasoning,
            agent: Some("a".into()),
            runtime: "echo".into(),
            output: output.into(),
            verified,
        };
        session
            .record_run(&[
                mk(true, "kernel scheduling internals"),
                mk(false, "unrelated chatter"),
                mk(true, "kernel priority queue dispatch"),
            ])
            .unwrap();

        // A recall returns at most k, best first; nothing crashes and the
        // verified, relevant, recent records outrank the unrelated failed one.
        let top = session.recall("kernel scheduling", 2).unwrap();
        assert_eq!(top.len(), 2);
        assert!(top.iter().all(|d| d.body.contains("kernel")));

        // Empty query set still works on an empty session.
        let empty = Session::new("none", Box::new(InMemoryStore::new()));
        assert!(empty.recall("x", 5).unwrap().is_empty());
    }

    #[test]
    fn isolates_sessions_sharing_a_store() {
        // Two sessions over the same in-memory store must not see each other's data.
        let mut a = Session::new("a", Box::new(InMemoryStore::new()));
        a.record_run(&[result(true)]).unwrap();
        // A fresh session id over a new store sees nothing.
        let b = Session::new("b", Box::new(InMemoryStore::new()));
        assert!(b.history().unwrap().is_empty());
        assert_eq!(a.history().unwrap().len(), 1);
    }
}
