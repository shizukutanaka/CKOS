//! Knowledge-graph versioning (§942) with merge strategies (§943).
//!
//! A [`GraphRepo`] manages immutable snapshots of a [`KnowledgeGraph`] like a
//! tiny Git: commits form a parent chain, branches are named pointers, and
//! [`GraphRepo::merge`] combines two branches by *semantic node identity*
//! (kind + label) rather than the internal [`NodeId`], which differs across
//! branches. Conflicts (same identity, differing attributes) are resolved by a
//! [`MergeStrategy`] and reported in a [`MergeReport`].

use crate::{EdgeKind, KnowledgeGraph, Node, NodeKind};
use std::collections::{HashMap, HashSet};

/// A monotonically assigned version number.
pub type VersionId = u64;

/// How to resolve a node conflict during merge (§943).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeStrategy {
    /// The current branch wins (a "policy merge" favouring ours).
    PreferOurs,
    /// The merged-in branch wins.
    PreferTheirs,
    /// Keep the higher-confidence version (an "AI merge" heuristic, §943).
    HigherConfidence,
}

/// A node that existed in both branches with differing attributes.
#[derive(Debug, Clone, PartialEq)]
pub struct MergeConflict {
    /// Semantic identity (`kind:label`).
    pub identity: String,
    pub ours_confidence: u8,
    pub theirs_confidence: u8,
    pub resolved_confidence: u8,
}

/// Summary of a merge.
#[derive(Debug, Clone, Default)]
pub struct MergeReport {
    /// Conflicts encountered and how they were resolved.
    pub conflicts: Vec<MergeConflict>,
    /// Nodes that existed only in the merged-in branch.
    pub added_from_theirs: usize,
}

struct Version {
    #[allow(dead_code)]
    id: VersionId,
    parent: Option<VersionId>,
    graph: KnowledgeGraph,
}

/// A versioned repository of knowledge-graph snapshots.
pub struct GraphRepo {
    versions: Vec<Version>,
    branches: HashMap<String, VersionId>,
    current: String,
}

/// Semantic identity of a node: kind token + lowercased label.
fn identity(node: &Node) -> String {
    let kind = match &node.kind {
        NodeKind::Concept => "concept",
        NodeKind::Document => "document",
        NodeKind::Person => "person",
        NodeKind::Organization => "organization",
        NodeKind::Tool => "tool",
        NodeKind::Api => "api",
        NodeKind::Project => "project",
        NodeKind::Other(s) => s,
    };
    format!("{}:{}", kind.to_lowercase(), node.label.to_lowercase())
}

fn edge_token(kind: &EdgeKind) -> String {
    match kind {
        EdgeKind::DependsOn => "depends_on".into(),
        EdgeKind::Implements => "implements".into(),
        EdgeKind::References => "references".into(),
        EdgeKind::CreatedBy => "created_by".into(),
        EdgeKind::RelatedTo => "related_to".into(),
        EdgeKind::Other(s) => s.to_lowercase(),
    }
}

impl GraphRepo {
    /// Create a repo with an empty `main` branch at version 0.
    pub fn new() -> Self {
        let initial = Version {
            id: 0,
            parent: None,
            graph: KnowledgeGraph::new(),
        };
        let mut branches = HashMap::new();
        branches.insert("main".to_string(), 0);
        GraphRepo {
            versions: vec![initial],
            branches,
            current: "main".to_string(),
        }
    }

    /// Name of the current branch.
    pub fn current_branch(&self) -> &str {
        &self.current
    }

    /// Version id at the tip of the current branch.
    pub fn head_id(&self) -> VersionId {
        self.branches[&self.current]
    }

    /// The graph at the current branch tip.
    pub fn head(&self) -> &KnowledgeGraph {
        &self.versions[self.head_id() as usize].graph
    }

    /// Commit a new graph snapshot onto the current branch; returns its id.
    pub fn commit(&mut self, graph: KnowledgeGraph) -> VersionId {
        let id = self.versions.len() as VersionId;
        let parent = Some(self.head_id());
        self.versions.push(Version { id, parent, graph });
        self.branches.insert(self.current.clone(), id);
        id
    }

    /// Create a branch pointing at the current head (does not switch to it).
    pub fn branch(&mut self, name: impl Into<String>) {
        self.branches.insert(name.into(), self.head_id());
    }

    /// Switch the current branch. Returns false if the branch is unknown.
    pub fn checkout(&mut self, name: &str) -> bool {
        if self.branches.contains_key(name) {
            self.current = name.to_string();
            true
        } else {
            false
        }
    }

    /// Ancestry of the current head, newest first.
    pub fn log(&self) -> Vec<VersionId> {
        let mut out = Vec::new();
        let mut cur = Some(self.head_id());
        while let Some(id) = cur {
            out.push(id);
            cur = self.versions[id as usize].parent;
        }
        out
    }

    /// Merge `other` branch into the current branch, committing the result.
    /// Returns the merge report, or `None` if `other` does not exist.
    pub fn merge(&mut self, other: &str, strategy: MergeStrategy) -> Option<MergeReport> {
        let other_id = *self.branches.get(other)?;
        let ours = self.head().clone();
        let theirs = self.versions[other_id as usize].graph.clone();

        let ours_nodes: HashMap<String, &Node> = ours.nodes().map(|n| (identity(n), n)).collect();
        let theirs_nodes: HashMap<String, &Node> =
            theirs.nodes().map(|n| (identity(n), n)).collect();

        let mut all: Vec<String> = ours_nodes
            .keys()
            .chain(theirs_nodes.keys())
            .cloned()
            .collect();
        all.sort();
        all.dedup();

        let mut merged = KnowledgeGraph::new();
        let mut idmap: HashMap<String, crate::NodeId> = HashMap::new();
        let mut report = MergeReport::default();

        for ident in &all {
            let o = ours_nodes.get(ident);
            let t = theirs_nodes.get(ident);
            let base = o.or(t).expect("identity came from one of the maps");

            let (confidence, date, provenance) = match (o, t) {
                (Some(o), Some(t)) => {
                    let pick_theirs = match strategy {
                        MergeStrategy::PreferOurs => false,
                        MergeStrategy::PreferTheirs => true,
                        MergeStrategy::HigherConfidence => t.confidence > o.confidence,
                    };
                    let chosen = if pick_theirs { t } else { o };
                    if o.confidence != t.confidence
                        || o.date != t.date
                        || o.provenance != t.provenance
                    {
                        report.conflicts.push(MergeConflict {
                            identity: ident.clone(),
                            ours_confidence: o.confidence,
                            theirs_confidence: t.confidence,
                            resolved_confidence: chosen.confidence,
                        });
                    }
                    (
                        chosen.confidence,
                        chosen.date.clone(),
                        chosen.provenance.clone(),
                    )
                }
                (Some(o), None) => (o.confidence, o.date.clone(), o.provenance.clone()),
                (None, Some(t)) => {
                    report.added_from_theirs += 1;
                    (t.confidence, t.date.clone(), t.provenance.clone())
                }
                (None, None) => unreachable!(),
            };

            let nid = merged.add_node(base.kind.clone(), base.label.clone(), confidence);
            if let Some(d) = date {
                merged.set_date(&nid, d);
            }
            if let Some(p) = provenance {
                merged.set_provenance(&nid, p);
            }
            idmap.insert(ident.clone(), nid);
        }

        // Union of edges, mapped through semantic identity and deduplicated.
        let mut seen: HashSet<(String, String, String)> = HashSet::new();
        for src in [&ours, &theirs] {
            for e in src.edges() {
                let (Some(from), Some(to)) = (src.node(&e.from), src.node(&e.to)) else {
                    continue;
                };
                let (fid, tid) = (identity(from), identity(to));
                let token = edge_token(&e.kind);
                if let (Some(nf), Some(nt)) = (idmap.get(&fid), idmap.get(&tid)) {
                    if seen.insert((fid.clone(), tid.clone(), token)) {
                        merged.connect(nf, nt, e.kind.clone());
                    }
                }
            }
        }

        self.commit(merged);
        Some(report)
    }
}

impl Default for GraphRepo {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EdgeKind, NodeKind};

    #[test]
    fn commits_form_a_history() {
        let mut repo = GraphRepo::new();
        let mut g = KnowledgeGraph::new();
        g.add_node(NodeKind::Concept, "A", 50);
        repo.commit(g);
        assert_eq!(repo.log().len(), 2); // initial + one commit
        assert_eq!(repo.head().len(), 1);
    }

    #[test]
    fn branches_are_independent() {
        let mut repo = GraphRepo::new();
        repo.branch("feature");
        repo.checkout("feature");
        let mut g = KnowledgeGraph::new();
        g.add_node(NodeKind::Tool, "scheduler", 90);
        repo.commit(g);
        assert_eq!(repo.head().len(), 1);
        // main is untouched.
        repo.checkout("main");
        assert_eq!(repo.head().len(), 0);
    }

    #[test]
    fn merge_unions_nodes_and_resolves_conflicts() {
        let mut repo = GraphRepo::new();
        // main: A(conf 60) -> B
        let mut main = KnowledgeGraph::new();
        let a = main.add_node(NodeKind::Concept, "A", 60);
        let b = main.add_node(NodeKind::Concept, "B", 70);
        main.connect(&a, &b, EdgeKind::DependsOn);
        repo.commit(main);

        // feature branched from main, then diverges: A(conf 95), new node C.
        repo.branch("feature");
        repo.checkout("feature");
        let mut feat = KnowledgeGraph::new();
        feat.add_node(NodeKind::Concept, "A", 95); // conflict with main's A
        feat.add_node(NodeKind::Concept, "C", 80); // new
        repo.commit(feat);

        // Merge feature into main, preferring higher confidence.
        repo.checkout("main");
        let report = repo
            .merge("feature", MergeStrategy::HigherConfidence)
            .unwrap();

        // A, B, C present.
        assert_eq!(repo.head().len(), 3);
        // One conflict on A, resolved to the higher confidence (95).
        assert_eq!(report.conflicts.len(), 1);
        assert_eq!(report.conflicts[0].resolved_confidence, 95);
        // C came from theirs.
        assert_eq!(report.added_from_theirs, 1);
        // The A->B edge survived the merge.
        let a_node = repo.head().nodes().find(|n| n.label == "A").unwrap();
        assert!(repo
            .head()
            .neighbors(&a_node.id)
            .iter()
            .any(|n| n.label == "B"));
    }

    #[test]
    fn merge_of_unknown_branch_is_none() {
        let mut repo = GraphRepo::new();
        assert!(repo.merge("nope", MergeStrategy::PreferOurs).is_none());
    }
}
