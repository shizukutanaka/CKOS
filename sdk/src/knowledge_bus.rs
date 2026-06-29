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
use ckos_memory::{Document, Embedder, Storage};
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

    /// Ingest free document text: heuristically extract concepts (§941) into the
    /// graph and announce every newly-created node so re-index subscribers pick
    /// them up (§938 index pipeline). Entities already present are reinforced in
    /// place and emit no event. Returns the [`ExtractReport`] from the pass.
    pub fn ingest_text(&mut self, text: &str) -> ExtractReport {
        use std::collections::HashSet;
        let before: HashSet<NodeId> = self.graph.nodes().map(|n| n.id.clone()).collect();
        let report = self.graph.extract_concepts(text);
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
    /// Push a node id onto the queue.
    pub fn push(&self, id: NodeId) {
        self.inner.lock().expect("reindex queue poisoned").push(id);
    }

    /// Number of pending ids.
    pub fn len(&self) -> usize {
        self.inner.lock().expect("reindex queue poisoned").len()
    }

    /// Whether the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Remove and return all pending ids (the worker processes these).
    pub fn drain(&self) -> Vec<NodeId> {
        std::mem::take(&mut *self.inner.lock().expect("reindex queue poisoned"))
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
