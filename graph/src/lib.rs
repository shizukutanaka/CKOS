//! # CKOS Knowledge Graph
//!
//! Typed nodes and edges (§897) forming the substrate for graph reasoning
//! (§951) and multi-hop retrieval (§952). The store is in-memory and adjacency
//! based; a persistent backend (Neo4j/SurrealDB) plugs in behind the same API
//! via the storage abstraction (§936).

use ckos_kernel::NodeId;
use std::collections::HashMap;

pub mod extract;
pub mod store;
pub mod versioning;
pub use extract::ExtractReport;
pub use store::GraphStore;
pub use versioning::{GraphRepo, MergeConflict, MergeReport, MergeStrategy, VersionId};

/// Validate an ISO `YYYY-MM-DD` date, returning it unchanged or naming what
/// was wrong (§946).
///
/// Every date in CKOS is compared **lexicographically** — [`Node::date`]
/// against KQL's `BEFORE`/`AFTER` bounds, and a document's `expires` metadata
/// against `ckos gc --now`. That equals chronological order *only* for
/// well-formed `YYYY-MM-DD`, so an unvalidated date does not fail: it produces
/// a confident, arbitrary answer decided by where the garbage happens to sort.
///
/// It lives here, next to the temporal model, because it was previously
/// enforced in neither place and then in only one. `ckos gc --now today`
/// **deleted a document whose `expires` was `2999-12-31`**, reported as
/// "Expired", because `"2999-12-31" < "today"`. Other typos (`2026-8-25`,
/// `08/25/2026`) happened to be harmless — sorting *below* the expiry rather
/// than above it — which is precisely why guessing is unacceptable here: the
/// outcome depended on luck, not on a rule, and the operation is destructive.
///
/// Deliberately not a full calendar check: ordering is the only use, and a
/// well-formed but non-existent date like `2017-02-30` still orders correctly
/// against every real one, so leap-year arithmetic would add complexity with
/// no correctness gain. The month/day ranges catch transpositions such as
/// `2017-31-06`, not bad history.
pub fn validate_iso_date(raw: &str) -> Result<&str, String> {
    let bad = || format!("expected an ISO date (YYYY-MM-DD), found {raw:?}");
    let bytes = raw.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return Err(bad());
    }
    for range in [0..4, 5..7, 8..10] {
        if !raw[range].chars().all(|c| c.is_ascii_digit()) {
            return Err(bad());
        }
    }
    let month: u8 = raw[5..7].parse().map_err(|_| bad())?;
    let day: u8 = raw[8..10].parse().map_err(|_| bad())?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return Err(bad());
    }
    Ok(raw)
}

/// Node categories from §897.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    /// An abstract idea or topic.
    Concept,
    /// A written artifact such as a paper or page.
    Document,
    /// An individual human.
    Person,
    /// A company, team or institution.
    Organization,
    /// A software tool or utility.
    Tool,
    /// A programmatic interface or service endpoint.
    Api,
    /// A body of work with a goal, such as a codebase.
    Project,
    /// Open category for domain-specific nodes.
    Other(String),
}

impl NodeKind {
    /// Canonical lowercase token for this kind, used by persistence (§936) and
    /// versioning identity (§942). `Other(s)` is emitted verbatim (lowercased).
    pub fn as_token(&self) -> String {
        match self {
            NodeKind::Concept => "concept".into(),
            NodeKind::Document => "document".into(),
            NodeKind::Person => "person".into(),
            NodeKind::Organization => "organization".into(),
            NodeKind::Tool => "tool".into(),
            NodeKind::Api => "api".into(),
            NodeKind::Project => "project".into(),
            NodeKind::Other(s) => s.to_lowercase(),
        }
    }

    /// Parse a token produced by [`NodeKind::as_token`]; unknown tokens become
    /// [`NodeKind::Other`] so custom kinds round-trip.
    pub fn from_token(token: &str) -> NodeKind {
        match token.to_lowercase().as_str() {
            "concept" => NodeKind::Concept,
            "document" => NodeKind::Document,
            "person" => NodeKind::Person,
            "organization" => NodeKind::Organization,
            "tool" => NodeKind::Tool,
            "api" => NodeKind::Api,
            "project" => NodeKind::Project,
            other => NodeKind::Other(other.to_string()),
        }
    }
}

/// Edge relationship types from §897.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EdgeKind {
    /// The source requires the target to function.
    DependsOn,
    /// The source realises or fulfils the target.
    Implements,
    /// The source cites or mentions the target.
    References,
    /// The source was authored by the target.
    CreatedBy,
    /// A generic association between the two nodes.
    RelatedTo,
    /// Open category for domain-specific relations.
    Other(String),
}

impl EdgeKind {
    /// Canonical snake_case token for this relation, used by persistence (§936)
    /// and versioning (§942). `Other(s)` is emitted verbatim (lowercased).
    pub fn as_token(&self) -> String {
        match self {
            EdgeKind::DependsOn => "depends_on".into(),
            EdgeKind::Implements => "implements".into(),
            EdgeKind::References => "references".into(),
            EdgeKind::CreatedBy => "created_by".into(),
            EdgeKind::RelatedTo => "related_to".into(),
            EdgeKind::Other(s) => s.to_lowercase(),
        }
    }

    /// Parse a token produced by [`EdgeKind::as_token`]; unknown tokens become
    /// [`EdgeKind::Other`] so custom relations round-trip.
    pub fn from_token(token: &str) -> EdgeKind {
        match token.to_lowercase().as_str() {
            "depends_on" => EdgeKind::DependsOn,
            "implements" => EdgeKind::Implements,
            "references" => EdgeKind::References,
            "created_by" => EdgeKind::CreatedBy,
            "related_to" => EdgeKind::RelatedTo,
            other => EdgeKind::Other(other.to_string()),
        }
    }
}

/// A graph node with an optional confidence score (§948).
#[derive(Debug, Clone)]
pub struct Node {
    /// Stable identifier within the graph.
    pub id: NodeId,
    /// Node category (§897).
    pub kind: NodeKind,
    /// Human-readable name of the node.
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
    /// Source node of the relation.
    pub from: NodeId,
    /// Target node of the relation.
    pub to: NodeId,
    /// Relation type (§897).
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

    /// Insert a fully-formed node, preserving its [`NodeId`]. Unlike
    /// [`KnowledgeGraph::add_node`], which mints a fresh id, this keeps the
    /// node's existing id so persisted edges still resolve after a reload (§936).
    pub fn insert(&mut self, node: Node) {
        self.adjacency.entry(node.id.clone()).or_default();
        self.nodes.insert(node.id.clone(), node);
    }

    /// Raise a node's confidence to at least `floor` (used when re-observing an
    /// entity during extraction, §941/§948). Never lowers an existing score.
    pub fn bump_confidence(&mut self, id: &NodeId, floor: u8) {
        if let Some(n) = self.nodes.get_mut(id) {
            n.confidence = n.confidence.max(floor.min(100));
        }
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

    /// Remove a node and every edge touching it (incoming or outgoing),
    /// returning whether the node existed (§897 mutation; supports retraction in
    /// the learning loop §959). Use [`remove_orphans`](Self::remove_orphans) to
    /// sweep only isolated nodes.
    pub fn remove_node(&mut self, id: &NodeId) -> bool {
        let existed = self.nodes.remove(id).is_some();
        self.adjacency.remove(id); // outgoing edges
                                   // Incoming edges: drop any edge pointing at `id` from other nodes.
        for edges in self.adjacency.values_mut() {
            edges.retain(|e| &e.to != id);
        }
        existed
    }

    /// Remove all edges from `from` to `to` of the given `kind`, returning how
    /// many were removed.
    pub fn remove_edge(&mut self, from: &NodeId, to: &NodeId, kind: &EdgeKind) -> usize {
        let Some(edges) = self.adjacency.get_mut(from) else {
            return 0;
        };
        let before = edges.len();
        edges.retain(|e| !(&e.to == to && &e.kind == kind));
        before - edges.len()
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

    /// Outgoing neighbours reached by an edge of a specific [`EdgeKind`] — the
    /// typed-relation traversal §951/§952 reasoning needs (e.g. "what does X
    /// *depend on*", following only `DependsOn` edges). Complements
    /// [`neighbors`](Self::neighbors), which ignores edge type.
    pub fn neighbors_via(&self, id: &NodeId, kind: &EdgeKind) -> Vec<&Node> {
        self.adjacency
            .get(id)
            .into_iter()
            .flatten()
            .filter(|e| &e.kind == kind)
            .filter_map(|e| self.nodes.get(&e.to))
            .collect()
    }

    /// PageRank node importance (Page & Brin, 1998) — the query-independent
    /// centrality used by Graph-RAG systems (FastGraphRAG, HippoRAG) to rank
    /// influential nodes (§948/§951). `damping` is the teleport factor (0.85 is
    /// standard); `iterations` power-iteration steps (~20 converges for small
    /// graphs). Returns a score per node summing to ~1.0; empty graph → empty.
    pub fn pagerank(&self, damping: f32, iterations: usize) -> HashMap<NodeId, f32> {
        let n = self.nodes.len();
        if n == 0 {
            return HashMap::new();
        }
        // Uniform teleport = classic PageRank.
        let uniform = 1.0 / n as f32;
        let teleport: HashMap<NodeId, f32> =
            self.nodes.keys().map(|id| (id.clone(), uniform)).collect();
        self.pagerank_core(&teleport, damping, iterations)
    }

    /// Personalized PageRank (Haveliwala 2002), the retrieval-time ranking at
    /// the heart of HippoRAG (arXiv:2405.14831 / 2502.14802): instead of
    /// teleporting uniformly, restart mass flows back only to the query
    /// `seeds`, so probability concentrates in their multi-hop neighbourhood.
    /// A node reachable from the seeds by several short paths accumulates more
    /// mass than one reachable by a single long path — capturing multi-hop
    /// relevance and path multiplicity that a fixed per-hop decay cannot.
    ///
    /// Seeds not present in the graph are ignored; if none remain, returns an
    /// empty map (a clear "nothing to personalize on" signal for callers).
    pub fn personalized_pagerank(
        &self,
        seeds: &[NodeId],
        damping: f32,
        iterations: usize,
    ) -> HashMap<NodeId, f32> {
        let present: Vec<&NodeId> = seeds
            .iter()
            .filter(|id| self.nodes.contains_key(id))
            .collect();
        if present.is_empty() {
            return HashMap::new();
        }
        let share = 1.0 / present.len() as f32;
        let mut teleport: HashMap<NodeId, f32> =
            self.nodes.keys().map(|id| (id.clone(), 0.0)).collect();
        for id in present {
            teleport.insert(id.clone(), share);
        }
        self.pagerank_core(&teleport, damping, iterations)
    }

    /// Power-iteration core shared by [`pagerank`](Self::pagerank) and
    /// [`personalized_pagerank`](Self::personalized_pagerank). `teleport` is
    /// the restart distribution (must sum to ~1 over the graph's nodes):
    /// uniform yields classic PageRank, seed-concentrated yields the
    /// personalized variant. Restart mass *and* dangling-node mass both flow
    /// back along `teleport`, so total rank is conserved either way.
    fn pagerank_core(
        &self,
        teleport: &HashMap<NodeId, f32>,
        damping: f32,
        iterations: usize,
    ) -> HashMap<NodeId, f32> {
        let n = self.nodes.len();
        if n == 0 {
            return HashMap::new();
        }
        let d = damping.clamp(0.0, 1.0);
        let tp = |id: &NodeId| teleport.get(id).copied().unwrap_or(0.0);
        // Start from the teleport distribution (converges regardless, but this
        // needs no warm-up iterations for the personalized case).
        let mut rank: HashMap<NodeId, f32> =
            self.nodes.keys().map(|id| (id.clone(), tp(id))).collect();

        for _ in 0..iterations {
            let mut next: HashMap<NodeId, f32> = self
                .nodes
                .keys()
                .map(|id| (id.clone(), (1.0 - d) * tp(id)))
                .collect();
            let mut dangling = 0.0f32;
            for id in self.nodes.keys() {
                let r = rank[id];
                let edges = self.adjacency.get(id).map(Vec::as_slice).unwrap_or(&[]);
                // Count only edges whose target still exists. Neither `connect`
                // nor `GraphStore::load` validates endpoints, so an edge can
                // point at a missing node; including it in the divisor while
                // being unable to deliver its share would destroy that share
                // outright. Restricting to live targets keeps the transition
                // matrix column-stochastic over the nodes that do exist
                // (Langville & Meyer, "Google's PageRank and Beyond").
                let live = || edges.iter().filter(|e| self.nodes.contains_key(&e.to));
                let out = live().count();
                if out == 0 {
                    // No live out-links: fully dangling, redistributed below.
                    dangling += r;
                    continue;
                }
                let share = d * r / out as f32;
                for edge in live() {
                    if let Some(slot) = next.get_mut(&edge.to) {
                        *slot += share;
                    }
                }
            }
            // Dangling mass teleports along the same distribution, so total
            // rank is conserved for both uniform and personalized teleport.
            for (id, slot) in next.iter_mut() {
                *slot += d * dangling * tp(id);
            }
            rank = next;
        }
        rank
    }

    /// The `top_n` most central nodes by [`pagerank`](Self::pagerank) (damping
    /// 0.85, 20 iterations), highest first; ties broken by label.
    pub fn central_nodes(&self, top_n: usize) -> Vec<(&Node, f32)> {
        let pr = self.pagerank(0.85, 20);
        let mut scored: Vec<(&Node, f32)> = self
            .nodes
            .values()
            .map(|node| (node, pr.get(&node.id).copied().unwrap_or(0.0)))
            .collect();
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.label.cmp(&b.0.label))
        });
        scored.truncate(top_n);
        scored
    }

    /// Breadth-first multi-hop traversal up to `max_hops` (§952), annotated with
    /// each node's hop distance from `start` — so callers can decay relevance by
    /// distance instead of treating every reached node as equally close.
    /// Nearest first, excluding `start`.
    pub fn traverse_with_hops(&self, start: &NodeId, max_hops: usize) -> Vec<(&Node, usize)> {
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
                            out.push((n, hops + 1));
                        }
                        queue.push_back((e.to.clone(), hops + 1));
                    }
                }
            }
        }
        out
    }

    /// Breadth-first multi-hop traversal up to `max_hops` (§952).
    ///
    /// Returns nodes reachable from `start`, nearest first, excluding `start`.
    pub fn traverse(&self, start: &NodeId, max_hops: usize) -> Vec<&Node> {
        self.traverse_with_hops(start, max_hops)
            .into_iter()
            .map(|(n, _)| n)
            .collect()
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
    fn pagerank_ranks_the_hub_highest() {
        // A star: three leaves all point at a central hub. The hub should have
        // the highest PageRank.
        let mut g = KnowledgeGraph::new();
        let hub = g.add_node(NodeKind::Concept, "Hub", 50);
        for leaf in ["L1", "L2", "L3"] {
            let l = g.add_node(NodeKind::Concept, leaf, 50);
            g.connect(&l, &hub, EdgeKind::References);
        }
        let pr = g.pagerank(0.85, 30);
        let hub_score = pr[&hub];
        assert!(
            pr.values().all(|&s| s <= hub_score + 1e-6),
            "hub must be most central"
        );
        // Scores form a distribution summing to ~1.
        let total: f32 = pr.values().sum();
        assert!((total - 1.0).abs() < 1e-3, "ranks sum to ~1, got {total}");

        // central_nodes surfaces the hub first.
        let top = g.central_nodes(1);
        assert_eq!(top[0].0.label, "Hub");
    }

    #[test]
    fn pagerank_conserves_mass_when_an_edge_points_at_a_missing_node() {
        // Regression: out-degree counted every adjacency entry, but the
        // distribution loop skipped edges whose target is not in the graph
        // (`next.get_mut` yields None). The share allocated to that edge was
        // therefore neither delivered nor redistributed — it vanished, every
        // iteration, so the documented "scores summing to ~1.0" silently broke
        // and surviving neighbours were under-credited.
        //
        // The full-dangling case (a node with *no* out-edges) was already
        // handled; only a *partially* dangling node slipped past the guard.
        let mut g = KnowledgeGraph::new();
        let a = g.add_node(NodeKind::Concept, "A", 50);
        let b = g.add_node(NodeKind::Concept, "B", 50);
        // `connect` does not validate endpoints, and `GraphStore::load`
        // replays edge rows without checking that both nodes were loaded, so a
        // graph file missing one `N` line reaches exactly this state.
        let ghost = NodeId::new();
        g.connect(&a, &b, EdgeKind::References);
        g.connect(&a, &ghost, EdgeKind::References);

        let pr = g.pagerank(0.85, 30);
        let total: f32 = pr.values().sum();
        assert!(
            (total - 1.0).abs() < 1e-3,
            "rank mass must be conserved, got {total}"
        );

        // The dead edge must not dilute B either: dropping a target that does
        // not exist should leave the same ranking as if it had never been
        // written, i.e. the plain A -> B chain.
        let mut clean = KnowledgeGraph::new();
        let ca = clean.add_node(NodeKind::Concept, "A", 50);
        let cb = clean.add_node(NodeKind::Concept, "B", 50);
        clean.connect(&ca, &cb, EdgeKind::References);
        let clean_pr = clean.pagerank(0.85, 30);
        assert!(
            (pr[&b] - clean_pr[&cb]).abs() < 1e-4,
            "B should score as in the clean graph: {} vs {}",
            pr[&b],
            clean_pr[&cb]
        );

        // Personalized PageRank shares the same core, so it leaks identically.
        let ppr = g.personalized_pagerank(std::slice::from_ref(&a), 0.85, 30);
        let ppr_total: f32 = ppr.values().sum();
        assert!(
            (ppr_total - 1.0).abs() < 1e-3,
            "PPR mass must be conserved, got {ppr_total}"
        );
    }

    #[test]
    fn personalized_pagerank_matches_uniform_when_seeded_on_all_nodes() {
        // Seeding PPR uniformly on every node is exactly classic PageRank —
        // proves pagerank_core is behaviour-preserving.
        let mut g = KnowledgeGraph::new();
        let hub = g.add_node(NodeKind::Concept, "Hub", 50);
        let mut all = vec![hub.clone()];
        for leaf in ["L1", "L2", "L3"] {
            let l = g.add_node(NodeKind::Concept, leaf, 50);
            g.connect(&l, &hub, EdgeKind::References);
            all.push(l);
        }
        let uniform = g.pagerank(0.85, 30);
        let ppr = g.personalized_pagerank(&all, 0.85, 30);
        for (id, u) in &uniform {
            assert!(
                (ppr[id] - u).abs() < 1e-5,
                "uniform-seeded PPR must equal plain PageRank"
            );
        }
    }

    #[test]
    fn personalized_pagerank_concentrates_mass_near_seeds() {
        // Two disjoint chains: A→B→C and X→Y→Z. Seeding only A must give the
        // A-chain far more mass than the untouched X-chain — the property a
        // fixed per-hop decay can't express (it would score C and Z equally
        // by distance alone).
        let mut g = KnowledgeGraph::new();
        let a = g.add_node(NodeKind::Concept, "A", 50);
        let b = g.add_node(NodeKind::Concept, "B", 50);
        let c = g.add_node(NodeKind::Concept, "C", 50);
        let x = g.add_node(NodeKind::Concept, "X", 50);
        let y = g.add_node(NodeKind::Concept, "Y", 50);
        let z = g.add_node(NodeKind::Concept, "Z", 50);
        g.connect(&a, &b, EdgeKind::References);
        g.connect(&b, &c, EdgeKind::References);
        g.connect(&x, &y, EdgeKind::References);
        g.connect(&y, &z, EdgeKind::References);

        let ppr = g.personalized_pagerank(std::slice::from_ref(&a), 0.85, 40);
        // A's own chain dominates the unrelated chain.
        assert!(ppr[&b] > ppr[&y], "seed neighbour B must beat unrelated Y");
        assert!(ppr[&c] > ppr[&z], "seed's 2-hop C must beat unrelated Z");
        assert!(ppr[&a] > ppr[&x], "seed A must beat unrelated X");

        // Unknown/empty seeds → empty map (clear contract).
        assert!(g.personalized_pagerank(&[], 0.85, 10).is_empty());
        assert!(g
            .personalized_pagerank(&[NodeId::from_raw("ghost")], 0.85, 10)
            .is_empty());
    }

    #[test]
    fn personalized_pagerank_aggregates_path_multiplicity() {
        // Diamond: S reaches D by two 2-hop paths (S→A→D, S→B→D); S reaches
        // E by a single 2-hop path (S→C→E). D and E are the same hop distance,
        // but D is corroborated by two paths and must score higher — the
        // multi-path aggregation HippoRAG's PPR gives us and a per-hop
        // geometric decay cannot.
        let mut g = KnowledgeGraph::new();
        let s = g.add_node(NodeKind::Concept, "S", 50);
        let a = g.add_node(NodeKind::Concept, "A", 50);
        let b = g.add_node(NodeKind::Concept, "B", 50);
        let cc = g.add_node(NodeKind::Concept, "C", 50);
        let d = g.add_node(NodeKind::Concept, "D", 50);
        let e = g.add_node(NodeKind::Concept, "E", 50);
        g.connect(&s, &a, EdgeKind::References);
        g.connect(&s, &b, EdgeKind::References);
        g.connect(&s, &cc, EdgeKind::References);
        g.connect(&a, &d, EdgeKind::References);
        g.connect(&b, &d, EdgeKind::References);
        g.connect(&cc, &e, EdgeKind::References);

        let ppr = g.personalized_pagerank(&[s], 0.85, 50);
        assert!(
            ppr[&d] > ppr[&e],
            "two-path node D ({}) must beat one-path node E ({})",
            ppr[&d],
            ppr[&e]
        );
    }

    #[test]
    fn traverse_with_hops_annotates_distance() {
        let mut g = KnowledgeGraph::new();
        let a = g.add_node(NodeKind::Project, "CKOS", 100);
        let b = g.add_node(NodeKind::Tool, "scheduler", 90);
        let c = g.add_node(NodeKind::Organization, "ACME", 80);
        g.connect(&a, &b, EdgeKind::DependsOn);
        g.connect(&b, &c, EdgeKind::CreatedBy);

        let hops = g.traverse_with_hops(&a, 2);
        let scheduler_hop = hops.iter().find(|(n, _)| n.label == "scheduler").unwrap().1;
        let acme_hop = hops.iter().find(|(n, _)| n.label == "ACME").unwrap().1;
        assert_eq!(scheduler_hop, 1);
        assert_eq!(acme_hop, 2);
    }

    #[test]
    fn removes_nodes_and_edges() {
        let mut g = KnowledgeGraph::new();
        let a = g.add_node(NodeKind::Project, "CKOS", 100);
        let b = g.add_node(NodeKind::Tool, "scheduler", 90);
        let c = g.add_node(NodeKind::Tool, "runtime", 90);
        g.connect(&a, &b, EdgeKind::DependsOn);
        g.connect(&a, &c, EdgeKind::DependsOn);
        g.connect(&b, &c, EdgeKind::RelatedTo);

        // Remove one specific edge.
        assert_eq!(g.remove_edge(&a, &b, &EdgeKind::DependsOn), 1);
        assert_eq!(g.remove_edge(&a, &b, &EdgeKind::DependsOn), 0); // already gone
        assert_eq!(g.edges().count(), 2);

        // Removing node `c` drops both its incoming edges (a->c, b->c).
        assert!(g.remove_node(&c));
        assert!(!g.remove_node(&c)); // already gone
        assert_eq!(g.len(), 2);
        assert_eq!(g.edges().count(), 0);
        assert!(g.node(&c).is_none());
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
