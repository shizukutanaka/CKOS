//! # CKOS Knowledge Graph
//!
//! Typed nodes and edges (§897) forming the substrate for graph reasoning
//! (§951) and multi-hop retrieval (§952). The store is in-memory and adjacency
//! based; a persistent backend (Neo4j/SurrealDB) plugs in behind the same API
//! via the storage abstraction (§936).

use ckos_kernel::NodeId;
use std::collections::HashMap;

pub mod versioning;
pub use versioning::{GraphRepo, MergeConflict, MergeReport, MergeStrategy, VersionId};

/// Node categories from §897.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    Concept,
    Document,
    Person,
    Organization,
    Tool,
    Api,
    Project,
    /// Open category for domain-specific nodes.
    Other(String),
}

/// Edge relationship types from §897.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EdgeKind {
    DependsOn,
    Implements,
    References,
    CreatedBy,
    RelatedTo,
    /// Open category for domain-specific relations.
    Other(String),
}

/// A graph node with an optional confidence score (§948).
#[derive(Debug, Clone)]
pub struct Node {
    pub id: NodeId,
    pub kind: NodeKind,
    pub label: String,
    /// Confidence 0..=100 (§948).
    pub confidence: u8,
    /// Optional ISO date for temporal knowledge (§946); lexicographically
    /// comparable, so range queries work without a date library.
    pub date: Option<String>,
    /// Optional origin of this knowledge — GitHub, paper, wiki, … (§947).
    pub provenance: Option<String>,
}

/// A directed, typed edge.
#[derive(Debug, Clone)]
pub struct Edge {
    pub from: NodeId,
    pub to: NodeId,
    pub kind: EdgeKind,
}

/// In-memory knowledge graph.
#[derive(Default, Clone)]
pub struct KnowledgeGraph {
    nodes: HashMap<NodeId, Node>,
    /// Adjacency: source node -> outgoing edges.
    adjacency: HashMap<NodeId, Vec<Edge>>,
}

impl KnowledgeGraph {
    /// Create an empty graph.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert (or replace) a node, returning its id.
    pub fn add_node(&mut self, kind: NodeKind, label: impl Into<String>, confidence: u8) -> NodeId {
        let id = NodeId::new();
        self.nodes.insert(
            id.clone(),
            Node {
                id: id.clone(),
                kind,
                label: label.into(),
                confidence: confidence.min(100),
                date: None,
                provenance: None,
            },
        );
        self.adjacency.entry(id.clone()).or_default();
        id
    }

    /// Attach a temporal date (ISO string) to a node (§946).
    pub fn set_date(&mut self, id: &NodeId, date: impl Into<String>) {
        if let Some(n) = self.nodes.get_mut(id) {
            n.date = Some(date.into());
        }
    }

    /// Attach a provenance/source to a node (§947).
    pub fn set_provenance(&mut self, id: &NodeId, source: impl Into<String>) {
        if let Some(n) = self.nodes.get_mut(id) {
            n.provenance = Some(source.into());
        }
    }

    /// Connect two nodes with a typed edge.
    pub fn connect(&mut self, from: &NodeId, to: &NodeId, kind: EdgeKind) {
        self.adjacency.entry(from.clone()).or_default().push(Edge {
            from: from.clone(),
            to: to.clone(),
            kind,
        });
    }

    /// Look up a node.
    pub fn node(&self, id: &NodeId) -> Option<&Node> {
        self.nodes.get(id)
    }

    /// Iterate over all nodes (order unspecified). Used by retrieval to scan
    /// the graph for label matches (§951).
    pub fn nodes(&self) -> impl Iterator<Item = &Node> {
        self.nodes.values()
    }

    /// Iterate over all edges (order unspecified). Used by versioning/merge.
    pub fn edges(&self) -> impl Iterator<Item = &Edge> {
        self.adjacency.values().flatten()
    }

    /// Render the graph as a Graphviz DOT digraph for visualization (a building
    /// block for the v2.8 Graph Explorer). Node labels show the kind, label and
    /// confidence; edges are typed.
    pub fn to_dot(&self) -> String {
        // Stable per-node index so node names are clean identifiers.
        let index: HashMap<&NodeId, usize> = self
            .nodes
            .keys()
            .enumerate()
            .map(|(i, id)| (id, i))
            .collect();
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");

        let mut out = String::from("digraph knowledge {\n");
        for (id, node) in &self.nodes {
            let i = index[id];
            out.push_str(&format!(
                "  n{i} [label=\"{} [{:?}] {}\"];\n",
                esc(&node.label),
                node.kind,
                node.confidence
            ));
        }
        for edge in self.edges() {
            if let (Some(&f), Some(&t)) = (index.get(&edge.from), index.get(&edge.to)) {
                out.push_str(&format!("  n{f} -> n{t} [label=\"{:?}\"];\n", edge.kind));
            }
        }
        out.push_str("}\n");
        out
    }

    /// Remove orphaned nodes — those with no incoming or outgoing edges — and
    /// return how many were removed (§954 GC, "orphaned graph nodes"). Useful
    /// after deletions or merges that leave isolated nodes behind.
    pub fn remove_orphans(&mut self) -> usize {
        use std::collections::HashSet;
        let mut referenced: HashSet<NodeId> = HashSet::new();
        for edges in self.adjacency.values() {
            for e in edges {
                referenced.insert(e.from.clone());
                referenced.insert(e.to.clone());
            }
        }
        let orphans: Vec<NodeId> = self
            .nodes
            .keys()
            .filter(|id| !referenced.contains(*id))
            .cloned()
            .collect();
        for id in &orphans {
            self.nodes.remove(id);
            self.adjacency.remove(id);
        }
        orphans.len()
    }

    /// Number of nodes.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the graph is empty.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Outgoing neighbours of a node.
    pub fn neighbors(&self, id: &NodeId) -> Vec<&Node> {
        self.adjacency
            .get(id)
            .into_iter()
            .flatten()
            .filter_map(|e| self.nodes.get(&e.to))
            .collect()
    }

    /// Breadth-first multi-hop traversal up to `max_hops` (§952).
    ///
    /// Returns nodes reachable from `start`, nearest first, excluding `start`.
    pub fn traverse(&self, start: &NodeId, max_hops: usize) -> Vec<&Node> {
        use std::collections::{HashSet, VecDeque};
        let mut visited: HashSet<NodeId> = HashSet::new();
        let mut out = Vec::new();
        let mut queue: VecDeque<(NodeId, usize)> = VecDeque::new();
        visited.insert(start.clone());
        queue.push_back((start.clone(), 0));
        while let Some((id, hops)) = queue.pop_front() {
            if hops >= max_hops {
                continue;
            }
            if let Some(edges) = self.adjacency.get(&id) {
                for e in edges {
                    if visited.insert(e.to.clone()) {
                        if let Some(n) = self.nodes.get(&e.to) {
                            out.push(n);
                        }
                        queue.push_back((e.to.clone(), hops + 1));
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_hop_traversal_reaches_distant_nodes() {
        let mut g = KnowledgeGraph::new();
        let a = g.add_node(NodeKind::Project, "CKOS", 100);
        let b = g.add_node(NodeKind::Tool, "scheduler", 90);
        let c = g.add_node(NodeKind::Organization, "ACME", 80);
        g.connect(&a, &b, EdgeKind::DependsOn);
        g.connect(&b, &c, EdgeKind::CreatedBy);

        let one_hop = g.traverse(&a, 1);
        assert_eq!(one_hop.len(), 1);
        assert_eq!(one_hop[0].label, "scheduler");

        let two_hop = g.traverse(&a, 2);
        assert_eq!(two_hop.len(), 2);
        assert!(two_hop.iter().any(|n| n.label == "ACME"));
    }

    #[test]
    fn removes_orphaned_nodes() {
        let mut g = KnowledgeGraph::new();
        let a = g.add_node(NodeKind::Project, "CKOS", 100);
        let b = g.add_node(NodeKind::Tool, "scheduler", 90);
        g.add_node(NodeKind::Concept, "orphan", 50); // isolated
        g.connect(&a, &b, EdgeKind::DependsOn);

        assert_eq!(g.remove_orphans(), 1);
        assert_eq!(g.len(), 2);
        assert!(g.nodes().all(|n| n.label != "orphan"));
        // a and b are connected, so a second pass removes nothing.
        assert_eq!(g.remove_orphans(), 0);
    }

    #[test]
    fn exports_graphviz_dot() {
        let mut g = KnowledgeGraph::new();
        let a = g.add_node(NodeKind::Project, "CKOS", 100);
        let b = g.add_node(NodeKind::Tool, "scheduler", 90);
        g.connect(&a, &b, EdgeKind::DependsOn);
        let dot = g.to_dot();
        assert!(dot.starts_with("digraph knowledge {"));
        assert!(dot.contains("CKOS"));
        assert!(dot.contains("DependsOn"));
        assert_eq!(dot.matches("->").count(), 1);
    }

    #[test]
    fn nodes_carry_temporal_and_provenance_metadata() {
        let mut g = KnowledgeGraph::new();
        let n = g.add_node(NodeKind::Concept, "Transformer", 96);
        assert!(g.node(&n).unwrap().date.is_none());
        g.set_date(&n, "2017-06-12");
        g.set_provenance(&n, "paper:Vaswani");
        let node = g.node(&n).unwrap();
        assert_eq!(node.date.as_deref(), Some("2017-06-12"));
        assert_eq!(node.provenance.as_deref(), Some("paper:Vaswani"));
    }
}
