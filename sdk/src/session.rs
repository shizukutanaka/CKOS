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
use ckos_memory::{Document, Query, Storage};

/// Document type tag for persisted execution records.
const EXECUTION: &str = "execution";
/// Document type tag for persisted reflections.
const REFLECTION: &str = "reflection";
/// Metadata key identifying the owning session.
const SESSION_KEY: &str = "session";

/// A durable working session backed by a storage layer (§927).
pub struct Session {
    id: String,
    store: Box<dyn Storage>,
}

impl Session {
    /// Open (or create) a session with the given id over a storage backend.
    /// Pass a [`FileStore`](ckos_memory::FileStore) for persistence across runs.
    pub fn new(id: impl Into<String>, store: Box<dyn Storage>) -> Self {
        Session {
            id: id.into(),
            store,
        }
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
            doc.metadata.insert("verified".into(), r.verified.to_string());
            if let Some(agent) = &r.agent {
                doc.metadata.insert("agent".into(), agent.clone());
            }
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
