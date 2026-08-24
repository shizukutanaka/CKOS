//! Knowledge bus (§923).
//!
//! Wraps a [`KnowledgeGraph`] so every mutation publishes a
//! [`Event::GraphChanged`] on an [`EventBus`]. Subscribers react automatically —
//! the canonical use is regenerating embeddings / re-indexing the changed node
//! (§938) without the writer knowing who consumes the change. [`ReindexQueue`]
//! is a ready-made subscriber that collects changed node ids for an async
//! re-index worker to drain.

use ckos_graph::{EdgeKind, ExtractReport, KnowledgeGraph, NodeKind};
use ckos_kernel::event::{Event, EventBus, InMemoryEventBus};
use ckos_kernel::NodeId;
use ckos_memory::{Document, Embedder, Query, Storage};
use std::sync::{Arc, Mutex};

/// A graph paired with an event bus: mutations emit `GraphChanged` (§923).
#[derive(Default)]
pub struct KnowledgeBus {
    graph: KnowledgeGraph,
    bus: InMemoryEventBus,
}

impl KnowledgeBus {
    /// Create an empty knowledge bus.
    pub fn new() -> Self {
        Self::default()
    }

    /// Wrap an existing graph so its future mutations announce themselves.
    ///
    /// Ingest starts from a graph loaded off disk rather than an empty one, so
    /// re-indexing accumulates into a session instead of replacing it; only
    /// nodes created *after* this call are announced, which is what a
    /// re-index subscriber wants — the pre-existing nodes are already indexed.
    pub fn from_graph(graph: KnowledgeGraph) -> Self {
        KnowledgeBus {
            graph,
            bus: InMemoryEventBus::default(),
        }
    }

    /// The underlying event bus, for subscribing.
    pub fn bus(&self) -> &InMemoryEventBus {
        &self.bus
    }

    /// The underlying graph, for reading/querying.
    pub fn graph(&self) -> &KnowledgeGraph {
        &self.graph
    }

    /// Add a node and announce the change (§923).
    pub fn add_node(&mut self, kind: NodeKind, label: impl Into<String>, confidence: u8) -> NodeId {
        let id = self.graph.add_node(kind, label, confidence);
        self.bus.publish(Event::GraphChanged(id.clone()));
        id
    }

    /// Connect two nodes and announce the change to the source node (§923).
    pub fn connect(&mut self, from: &NodeId, to: &NodeId, kind: EdgeKind) {
        self.graph.connect(from, to, kind);
        self.bus.publish(Event::GraphChanged(from.clone()));
    }

    /// Ingest free document text with no recorded source.
    ///
    /// Prefer [`ingest_text_from`](Self::ingest_text_from) whenever the text
    /// came from somewhere nameable. This form leaves every new concept
    /// unsourced, so a later `RETURN Sources` query answers `<unknown>` — see
    /// that method for why that mattered in practice.
    pub fn ingest_text(&mut self, text: &str) -> ExtractReport {
        self.ingest_text_from(text, None)
    }

    /// Ingest free document text: heuristically extract concepts (§941) into the
    /// graph and announce every newly-created node so re-index subscribers pick
    /// them up (§938 index pipeline). Entities already present are reinforced in
    /// place and emit no event. Returns the [`ExtractReport`] from the pass.
    ///
    /// `source` stamps provenance (§947) on newly created nodes — e.g. the file
    /// the text was read from. Reinforced nodes keep their original source, so
    /// the answer to "where did this first come from" is stable across
    /// re-ingestion.
    ///
    /// This exists because ingestion used to call the *non*-provenance
    /// extraction path unconditionally. `ckos index` — the command whose entire
    /// job is loading a corpus, and where provenance is most valuable because
    /// the user wants to know which file a fact came from — was therefore the
    /// one command that recorded no source at all. The README's own quickstart
    /// runs `ckos index` and then queries `RETURN Sources`; every row came back
    /// `src=<unknown>`.
    pub fn ingest_text_from(&mut self, text: &str, source: Option<&str>) -> ExtractReport {
        use std::collections::HashSet;
        let before: HashSet<NodeId> = self.graph.nodes().map(|n| n.id.clone()).collect();
        let report = self.graph.extract_concepts_with_provenance(text, source);
        let new_ids: Vec<NodeId> = self
            .graph
            .nodes()
            .filter(|n| !before.contains(&n.id))
            .map(|n| n.id.clone())
            .collect();
        for id in new_ids {
            self.bus.publish(Event::GraphChanged(id));
        }
        report
    }

    /// Attach a subscriber that queues every changed node id for re-indexing
    /// (§923 → §938). Returns the queue an async worker can drain.
    pub fn subscribe_reindex(&self) -> ReindexQueue {
        let queue = ReindexQueue::default();
        let sink = queue.clone();
        self.bus.subscribe(Arc::new(move |event: &Event| {
            if let Event::GraphChanged(id) = event {
                sink.push(id.clone());
            }
        }));
        queue
    }
}

/// A thread-safe queue of node ids awaiting re-indexing (§923). Cheap to clone;
/// clones share the same backing store.
#[derive(Clone, Default)]
pub struct ReindexQueue {
    inner: Arc<Mutex<Vec<NodeId>>>,
}

impl ReindexQueue {
    /// Take the queue lock, recovering from poisoning: the queue is a plain
    /// Vec with no partial-update invariants, and a poisoned queue must not
    /// turn every later push/drain into a panic cascade.
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<NodeId>> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Push a node id onto the queue.
    pub fn push(&self, id: NodeId) {
        self.lock().push(id);
    }

    /// Number of pending ids.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Whether the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Remove and return all pending ids (the worker processes these).
    pub fn drain(&self) -> Vec<NodeId> {
        std::mem::take(&mut *self.lock())
    }
}

/// Drains a [`ReindexQueue`] and (re-)embeds the changed graph nodes into a
/// document store, making them vector-searchable (§923 → §938). Document type
/// `graph_node`; the node id is kept in metadata so re-indexing the same node
/// replaces its document rather than duplicating it.
pub struct Reindexer<'a> {
    graph: &'a KnowledgeGraph,
    embedder: &'a dyn Embedder,
}

impl<'a> Reindexer<'a> {
    /// Create a reindexer over a graph and an embedder.
    pub fn new(graph: &'a KnowledgeGraph, embedder: &'a dyn Embedder) -> Self {
        Reindexer { graph, embedder }
    }

    /// Process all queued node ids, writing an embedded document per node that
    /// still exists in the graph. Returns the number of documents written.
    pub fn process(&self, queue: &ReindexQueue, store: &mut dyn Storage) -> usize {
        let mut written = 0;
        for id in queue.drain() {
            let Some(node) = self.graph.node(&id) else {
                continue; // node was deleted before re-indexing
            };
            let mut doc = Document::new("graph_node", node.label.clone(), node.label.clone());
            // `Document::new` always mints a fresh id; reuse the existing
            // `graph_node` document's id for this node, if one is already
            // stored, so writing actually replaces it in place. Without this,
            // every reinforcement-then-reindex of the same node (a normal,
            // repeated occurrence — see `KnowledgeBus::ingest_text`) piles up
            // another duplicate document instead of updating the one that's
            // there, silently breaking this doc comment's promise.
            if let Ok(existing) = store.search(&Query {
                doc_type: Some("graph_node".to_string()),
                ..Default::default()
            }) {
                if let Some(prev) = existing
                    .into_iter()
                    .find(|d| d.metadata.get("node_id").map(String::as_str) == Some(id.as_str()))
                {
                    doc.id = prev.id;
                }
            }
            doc.confidence = node.confidence;
            doc.embedding = Some(self.embedder.embed(&node.label));
            doc.metadata.insert("node_id".to_string(), id.to_string());
            if store.write(doc).is_ok() {
                written += 1;
            }
        }
        written
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ckos_memory::{HashingEmbedder, InMemoryStore, Query};

    #[test]
    fn mutations_queue_reindex_work() {
        let mut kb = KnowledgeBus::new();
        let queue = kb.subscribe_reindex();

        let a = kb.add_node(NodeKind::Concept, "Transformer", 96);
        let b = kb.add_node(NodeKind::Tool, "attention", 90);
        kb.connect(&a, &b, EdgeKind::References);

        // Two adds + one connect → three change events queued.
        assert_eq!(queue.len(), 3);
        let drained = queue.drain();
        assert_eq!(drained.len(), 3);
        assert!(queue.is_empty());
        // The first queued id is the first node added.
        assert_eq!(drained[0], a);
        // The graph reflects the mutations.
        assert_eq!(kb.graph().len(), 2);
    }

    #[test]
    fn ingestion_stamps_the_source_and_reinforcement_keeps_the_original() {
        // Regression: `ingest_text` called the non-provenance extraction path,
        // so every concept loaded by `ckos index` was unsourced and the
        // README's own quickstart query (`RETURN Sources`) answered
        // `src=<unknown>` for all of them — while §947 claimed extraction
        // stamps the source.
        let mut kb = KnowledgeBus::new();
        kb.ingest_text_from("The Transformer uses Attention.", Some("file:paper.md"));
        let sourced: Vec<_> = kb
            .graph()
            .nodes()
            .map(|n| (n.label.clone(), n.provenance.clone()))
            .collect();
        assert!(!sourced.is_empty(), "expected concepts to be extracted");
        for (label, provenance) in &sourced {
            assert_eq!(
                provenance.as_deref(),
                Some("file:paper.md"),
                "{label} was left unsourced"
            );
        }

        // Re-ingesting the same entity from a *different* file reinforces the
        // existing node rather than creating a second one, and must not
        // rewrite where it first came from — otherwise "the source" silently
        // becomes "the most recent source".
        kb.ingest_text_from("The Transformer is fast.", Some("file:notes.md"));
        let transformer = kb
            .graph()
            .nodes()
            .find(|n| n.label.eq_ignore_ascii_case("transformer"))
            .expect("Transformer node");
        assert_eq!(
            transformer.provenance.as_deref(),
            Some("file:paper.md"),
            "reinforcement must keep the original source"
        );

        // The no-source form stays available and honest about recording none.
        let mut plain = KnowledgeBus::new();
        plain.ingest_text("The Transformer uses Attention.");
        assert!(plain.graph().nodes().all(|n| n.provenance.is_none()));
    }

    #[test]
    fn ingest_text_extracts_and_queues_new_nodes() {
        let mut kb = KnowledgeBus::new();
        let queue = kb.subscribe_reindex();

        let report = kb.ingest_text("CKOS uses a Knowledge Graph. CKOS is fast.");
        // Three concepts: CKOS, Knowledge Graph, (no others) — "is"/"a" filtered.
        assert_eq!(report.nodes_added, 2);
        assert_eq!(kb.graph().len(), 2);
        // Each new node emitted one GraphChanged event onto the reindex queue.
        assert_eq!(queue.len(), 2);

        // A second ingest mentioning an existing entity reinforces it: no new
        // node, so no new reindex work is queued.
        queue.drain();
        let again = kb.ingest_text("CKOS ships today.");
        assert_eq!(again.nodes_added, 0);
        assert_eq!(again.nodes_reinforced, 1);
        assert_eq!(queue.len(), 0);
    }

    #[test]
    fn ingested_concepts_become_searchable_via_reindex() {
        let mut kb = KnowledgeBus::new();
        let queue = kb.subscribe_reindex();
        kb.ingest_text("The Scheduler dispatches tasks to the Runtime.");

        let embedder = HashingEmbedder::new(64);
        let mut store = InMemoryStore::new();
        let written = Reindexer::new(kb.graph(), &embedder).process(&queue, &mut store);
        assert_eq!(written, 2); // Scheduler, Runtime
        let docs = store
            .search(&Query {
                doc_type: Some("graph_node".into()),
                ..Default::default()
            })
            .unwrap();
        assert!(docs.iter().any(|d| d.title == "Scheduler"));
        assert!(docs.iter().any(|d| d.title == "Runtime"));
    }

    #[test]
    fn multiple_subscribers_each_observe_changes() {
        let mut kb = KnowledgeBus::new();
        let q1 = kb.subscribe_reindex();
        let q2 = kb.subscribe_reindex();
        kb.add_node(NodeKind::Person, "Vaswani", 90);
        assert_eq!(q1.len(), 1);
        assert_eq!(q2.len(), 1);
    }

    #[test]
    fn reindexing_the_same_node_again_replaces_its_document_not_duplicates_it() {
        let mut kb = KnowledgeBus::new();
        let queue = kb.subscribe_reindex();
        let id = kb.add_node(NodeKind::Concept, "Transformer", 90);

        let embedder = HashingEmbedder::new(64);
        let mut store = InMemoryStore::new();
        Reindexer::new(kb.graph(), &embedder).process(&queue, &mut store);

        // Queue the same node for re-indexing again (e.g. its confidence
        // changed after being reinforced) instead of a brand-new one.
        queue.push(id.clone());
        Reindexer::new(kb.graph(), &embedder).process(&queue, &mut store);

        let docs = store
            .search(&Query {
                doc_type: Some("graph_node".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            docs.len(),
            1,
            "re-indexing the same node must replace its document, not duplicate it: {docs:?}"
        );
    }

    #[test]
    fn reindexer_embeds_changed_nodes_into_store() {
        let mut kb = KnowledgeBus::new();
        let queue = kb.subscribe_reindex();
        kb.add_node(NodeKind::Concept, "Transformer", 96);
        kb.add_node(NodeKind::Tool, "Attention", 90);

        let embedder = HashingEmbedder::new(64);
        let mut store = InMemoryStore::new();
        // Borrow the graph after mutations are done.
        let written = Reindexer::new(kb.graph(), &embedder).process(&queue, &mut store);

        assert_eq!(written, 2);
        assert!(queue.is_empty()); // drained
        let docs = store
            .search(&Query {
                doc_type: Some("graph_node".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(docs.len(), 2);
        // Every reindexed document carries an embedding for vector search.
        assert!(docs.iter().all(|d| d.embedding.is_some()));
    }
}
