//! # Knowledge-graph persistence (§897 / §936)
//!
//! A dependency-free, file-based store so an extracted [`KnowledgeGraph`]
//! survives across processes, mirroring the way [`memory::FileStore`] persists
//! documents. The whole graph lives in one text file:
//!
//! ```text
//! # CKOS knowledge graph v1
//! N\t<id>\t<kind>\t<confidence>\t<date>\t<provenance>\t<label>
//! E\t<from-id>\t<to-id>\t<edge-kind>
//! ```
//!
//! Fields are tab-separated and the free-form `label` is placed last, so labels
//! containing spaces, colons or pipes round-trip safely. `date`/`provenance` are
//! empty strings when absent. Node ids are preserved (via [`NodeId::from_raw`])
//! so edges still resolve after a reload.
//!
//! [`memory::FileStore`]: ../../ckos_memory/struct.FileStore.html

use crate::{Edge, EdgeKind, KnowledgeGraph, Node, NodeKind};
use ckos_kernel::NodeId;
use std::io;
use std::path::Path;

const HEADER: &str = "# CKOS knowledge graph v1";

/// File-based persistence for a [`KnowledgeGraph`].
pub struct GraphStore;

/// Replace tab/newline with a space so a value stays on one TSV field.
fn sanitize(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

impl GraphStore {
    /// Serialize `graph` to `path`, overwriting any existing file (§936).
    pub fn save(path: impl AsRef<Path>, graph: &KnowledgeGraph) -> io::Result<()> {
        let mut out = String::from(HEADER);
        out.push('\n');
        for n in graph.nodes() {
            out.push_str(&format!(
                "N\t{}\t{}\t{}\t{}\t{}\t{}\n",
                sanitize(n.id.as_str()),
                n.kind.as_token(),
                n.confidence,
                sanitize(n.date.as_deref().unwrap_or("")),
                sanitize(n.provenance.as_deref().unwrap_or("")),
                sanitize(&n.label),
            ));
        }
        for e in graph.edges() {
            out.push_str(&format!(
                "E\t{}\t{}\t{}\n",
                sanitize(e.from.as_str()),
                sanitize(e.to.as_str()),
                e.kind.as_token(),
            ));
        }
        std::fs::write(path, out)
    }

    /// Load a graph from `path`. A missing file yields an empty graph, so callers
    /// can load unconditionally on startup.
    pub fn load(path: impl AsRef<Path>) -> io::Result<KnowledgeGraph> {
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(KnowledgeGraph::new()),
            Err(e) => return Err(e),
        };
        let mut graph = KnowledgeGraph::new();
        let mut edges: Vec<Edge> = Vec::new();

        for line in content.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let fields: Vec<&str> = line.split('\t').collect();
            match fields.first() {
                Some(&"N") if fields.len() >= 7 => {
                    let id = NodeId::from_raw(fields[1]);
                    let kind = NodeKind::from_token(fields[2]);
                    let confidence = fields[3].parse::<u8>().unwrap_or(0).min(100);
                    let date = non_empty(fields[4]);
                    let provenance = non_empty(fields[5]);
                    // label is the remainder; it cannot contain a tab (sanitized).
                    let label = fields[6..].join("\t");
                    graph.insert(Node {
                        id,
                        kind,
                        label,
                        confidence,
                        date,
                        provenance,
                    });
                }
                Some(&"E") if fields.len() >= 4 => {
                    edges.push(Edge {
                        from: NodeId::from_raw(fields[1]),
                        to: NodeId::from_raw(fields[2]),
                        kind: EdgeKind::from_token(fields[3]),
                    });
                }
                _ => {} // skip blank/unknown lines for forward compatibility
            }
        }

        // Connect edges after all nodes exist so adjacency is complete.
        for e in edges {
            graph.connect(&e.from, &e.to, e.kind);
        }
        Ok(graph)
    }
}

/// `Some(s)` for a non-empty string, else `None`.
fn non_empty(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A temp path removed on drop.
    struct TempPath(std::path::PathBuf);
    impl Drop for TempPath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    fn temp(name: &str) -> TempPath {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        TempPath(
            std::env::temp_dir().join(format!("ckos-graph-{}-{n}-{name}.kg", std::process::id())),
        )
    }

    #[test]
    fn round_trips_nodes_edges_and_metadata() {
        let mut g = KnowledgeGraph::new();
        let t = g.add_node(NodeKind::Concept, "Transformer model", 96);
        g.set_date(&t, "2017-06-12");
        g.set_provenance(&t, "paper:Vaswani");
        let v = g.add_node(NodeKind::Person, "Vaswani", 90);
        let algo = g.add_node(NodeKind::Other("algorithm".into()), "Attention", 88);
        g.connect(&t, &v, EdgeKind::CreatedBy);
        g.connect(&t, &algo, EdgeKind::References);

        let path = temp("roundtrip");
        GraphStore::save(&path.0, &g).unwrap();
        let loaded = GraphStore::load(&path.0).unwrap();

        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded.edges().count(), 2);

        // The multi-word label and metadata survive.
        let tn = loaded.node(&t).unwrap();
        assert_eq!(tn.label, "Transformer model");
        assert_eq!(tn.confidence, 96);
        assert_eq!(tn.date.as_deref(), Some("2017-06-12"));
        assert_eq!(tn.provenance.as_deref(), Some("paper:Vaswani"));

        // The custom kind round-trips as Other.
        assert_eq!(
            loaded.node(&algo).unwrap().kind,
            NodeKind::Other("algorithm".into())
        );

        // Ids round-trip, so edges resolve to the right neighbours/kinds.
        let created_by: Vec<_> = loaded
            .edges()
            .filter(|e| e.kind == EdgeKind::CreatedBy)
            .collect();
        assert_eq!(created_by.len(), 1);
        assert_eq!(created_by[0].from, t);
        assert_eq!(created_by[0].to, v);
    }

    #[test]
    fn missing_file_loads_empty() {
        let loaded = GraphStore::load("/nonexistent/path/does-not-exist.kg").unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn extracted_graph_round_trips() {
        let mut g = KnowledgeGraph::new();
        g.extract_concepts("CKOS depends on the Scheduler. The Engine uses the Runtime.");
        let path = temp("extracted");
        GraphStore::save(&path.0, &g).unwrap();
        let loaded = GraphStore::load(&path.0).unwrap();
        assert_eq!(loaded.len(), g.len());
        assert_eq!(loaded.edges().count(), g.edges().count());
        assert!(loaded
            .edges()
            .any(|e| e.kind == EdgeKind::DependsOn || e.kind == EdgeKind::References));
    }
}
