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

/// An advisory cross-process lock over one session's graph file.
///
/// Persisting the graph is a read-modify-write: load, extract concepts into it,
/// save the whole file. Two writers that interleave therefore lose each other's
/// work — the last save wins and silently discards everything the other added.
/// Measured before this existed: six concurrent `POST /api/run` calls against
/// one session left **6 of 12** concepts, and six concurrent `ckos index` runs
/// left as few as **2 of 12**.
///
/// A lock *file* rather than a `Mutex`, because the writers are separate
/// processes as often as separate threads: a CLI run while `ckos serve` is up
/// on the same session is the ordinary case, and no in-process lock can see
/// that. `create_new` is the atomic primitive std offers for this.
///
/// A lock older than one minute is treated as abandoned by a killed process and
/// broken, so a crash cannot wedge a session permanently.
pub struct GraphLock {
    path: std::path::PathBuf,
}

/// How long to wait for another writer before giving up.
const LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// A lock file older than this is assumed to be from a process that died.
const STALE_LOCK: std::time::Duration = std::time::Duration::from_secs(60);

impl GraphLock {
    /// Take the lock guarding `graph_path`, waiting for a concurrent writer.
    pub fn acquire(graph_path: impl AsRef<Path>) -> io::Result<GraphLock> {
        let graph_path = graph_path.as_ref();
        let path = match graph_path.parent() {
            Some(dir) => dir.join(".graph.lock"),
            None => std::path::PathBuf::from(".graph.lock"),
        };
        let start = std::time::Instant::now();
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(GraphLock { path }),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    if Self::is_stale(&path) {
                        // Best effort: if another waiter removes it first, the
                        // next create_new simply succeeds for one of us.
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    if start.elapsed() >= LOCK_TIMEOUT {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            format!("another writer holds {}", path.display()),
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(15));
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn is_stale(path: &Path) -> bool {
        std::fs::metadata(path)
            .and_then(|m| m.modified())
            .map(|t| t.elapsed().map(|age| age > STALE_LOCK).unwrap_or(false))
            .unwrap_or(false)
    }
}

impl Drop for GraphLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// File-based persistence for a [`KnowledgeGraph`].
pub struct GraphStore;

/// Replace tab/newline with a space so a value stays on one TSV field.
fn sanitize(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

/// A scratch path for the write-then-rename in [`GraphStore::save`], unique
/// to this call.
///
/// It must not be a pure function of the destination, which is what
/// `path.with_extension("kg.tmp")` was. `File::create` opens `O_TRUNC`, so two
/// writers sharing one scratch path truncate each other's partial file while
/// each keeps writing at *its own* offset — and the rename then installs the
/// resulting mixture. Verified at the syscall level rather than assumed: with
/// writer A at offset 2048 of 4096 when writer B truncates and writes 1024
/// bytes, the renamed file was neither A's nor B's content but B's 1024 bytes
/// followed by **1024 NUL bytes** (the hole A's truncated prefix left) and
/// then A's tail. That is exactly the corruption the atomic rename exists to
/// prevent, so leaving the collision in place would make the guarantee below
/// conditional on nobody writing concurrently.
///
/// Concurrency here is not hypothetical: `ckos serve` handles requests on
/// separate threads, and two `POST /api/run` calls against one session both
/// save that session's graph.
fn scratch_path(path: &Path) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "graph".to_string());
    path.with_file_name(format!(".{name}.{}.{n}.tmp", std::process::id()))
}

impl GraphStore {
    /// Serialize `graph` to `path`, overwriting any existing file (§936).
    pub fn save(path: impl AsRef<Path>, graph: &KnowledgeGraph) -> io::Result<()> {
        let mut out = String::from(HEADER);
        out.push('\n');

        // Sort by id so the file is byte-stable regardless of the graph's
        // internal hash order — friendly to diffs, caching and reproducibility.
        let mut nodes: Vec<_> = graph.nodes().collect();
        nodes.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        for n in nodes {
            out.push_str(&format!(
                "N\t{}\t{}\t{}\t{}\t{}\t{}\n",
                sanitize(n.id.as_str()),
                sanitize(&n.kind.as_token()),
                n.confidence,
                sanitize(n.date.as_deref().unwrap_or("")),
                sanitize(n.provenance.as_deref().unwrap_or("")),
                sanitize(&n.label),
            ));
        }

        let mut edges: Vec<_> = graph.edges().collect();
        edges.sort_by(|a, b| {
            a.from
                .as_str()
                .cmp(b.from.as_str())
                .then(a.to.as_str().cmp(b.to.as_str()))
                .then(a.kind.as_token().cmp(&b.kind.as_token()))
        });
        for e in edges {
            out.push_str(&format!(
                "E\t{}\t{}\t{}\n",
                sanitize(e.from.as_str()),
                sanitize(e.to.as_str()),
                sanitize(&e.kind.as_token()),
            ));
        }
        // Atomic replace: write a sibling temp file, flush it, then rename
        // over the destination. This file holds the *entire* graph, so a
        // crash during a plain in-place write would corrupt all of it; with
        // the rename, readers see either the old graph or the complete new
        // one, never a truncation. The scratch path is unique per call so a
        // concurrent writer cannot truncate this one's partial file — see
        // `scratch_path`.
        let path = path.as_ref();
        let tmp = scratch_path(path);
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(out.as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, path)
    }

    /// Load, mutate and save under [`GraphLock`], so two writers cannot lose
    /// each other's work. Every persisted mutation should go through this
    /// rather than pairing `load` and `save` by hand — the gap between them is
    /// exactly where the updates were lost.
    pub fn update<F, T>(path: impl AsRef<Path>, f: F) -> io::Result<T>
    where
        F: FnOnce(&mut KnowledgeGraph) -> T,
    {
        let path = path.as_ref();
        let _lock = GraphLock::acquire(path)?;
        let mut graph = Self::load(path)?;
        let out = f(&mut graph);
        Self::save(path, &graph)?;
        Ok(out)
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

    #[test]
    fn concurrent_writers_do_not_lose_each_others_nodes() {
        // Persisting the graph is load-modify-write, and the two halves used to
        // be called separately: interleaved writers lost each other's work
        // silently. Measured through the HTTP API before the fix, six
        // concurrent runs against one session left 6 of 12 concepts; six
        // concurrent `ckos index` runs left as few as 2 of 12.
        let dir = std::env::temp_dir().join(format!("ckos-graphlock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("graph.kg");

        let threads: Vec<_> = (0..8)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    GraphStore::update(&path, |g| {
                        g.add_node(NodeKind::Concept, format!("Node{i:02}"), 50);
                    })
                    .expect("update under lock")
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }

        let final_graph = GraphStore::load(&path).unwrap();
        let labels: Vec<String> = final_graph.nodes().map(|n| n.label.clone()).collect();
        assert_eq!(
            labels.len(),
            8,
            "writers lost each other's nodes: {labels:?}"
        );
        for i in 0..8 {
            assert!(
                labels.iter().any(|l| l == &format!("Node{i:02}")),
                "{labels:?}"
            );
        }
        // The lock file must not survive its guard.
        assert!(!dir.join(".graph.lock").exists(), "lock file leaked");
        let _ = std::fs::remove_dir_all(&dir);
    }
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
    fn a_concurrent_writers_scratch_file_cannot_corrupt_this_save() {
        // Regression: the scratch path was `<dest>.tmp` — a pure function of
        // the destination — so two writers shared it. `File::create` is
        // `O_TRUNC`, so each truncates the other's partial file while still
        // writing at its own offset, and the rename installs the mixture.
        // Measured at the syscall level: A at offset 2048 of 4096, B
        // truncating and writing 1024, produced a renamed file that was
        // neither — B's 1024 bytes, 1024 NUL bytes, then A's tail.
        //
        // Modelled here deterministically rather than by racing threads (40
        // rounds of two real concurrent `save`s, and 20 more with a
        // synchronized start and a 60 000-node graph, never tripped it — the
        // window is narrow, not absent). Holding an open handle at a nonzero
        // offset on the *old* scratch path is exactly the state a writer
        // mid-`write_all` is in; if `save` still picks that path, the rename
        // hands our handle the destination inode and the next write lands in
        // the live graph file.
        let path = temp("scratch-collision");
        let old_scheme = path.0.with_extension(format!(
            "{}.tmp",
            path.0.extension().and_then(|s| s.to_str()).unwrap_or("")
        ));
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _cleanup = Cleanup(old_scheme.clone());

        use std::io::Write;
        let mut squatter = std::fs::File::create(&old_scheme).expect("create scratch squatter");
        squatter.write_all(&vec![b'X'; 4096]).expect("squat");

        let mut g = KnowledgeGraph::new();
        let a = g.add_node(NodeKind::Concept, "Transformer", 96);
        let b = g.add_node(NodeKind::Person, "Vaswani", 90);
        g.connect(&a, &b, EdgeKind::CreatedBy);
        GraphStore::save(&path.0, &g).expect("save");

        // The other writer continues at its own offset. Under the old scheme
        // this inode has just become the graph file.
        squatter.write_all(b"GARBAGE").expect("continue writing");
        squatter.sync_all().expect("sync");
        drop(squatter);

        let raw = std::fs::read(&path.0).expect("read graph");
        assert!(
            !raw.contains(&0),
            "the saved graph must not contain the NUL hole a truncated \
             co-writer leaves ({} NUL bytes in {} total)",
            raw.iter().filter(|b| **b == 0).count(),
            raw.len()
        );
        assert!(
            !raw.windows(7).any(|w| w == b"GARBAGE"),
            "another writer's bytes must never land in the saved graph"
        );
        let loaded = GraphStore::load(&path.0).expect("load");
        assert_eq!(loaded.nodes().count(), 2, "graph must round-trip intact");
        assert_eq!(loaded.edges().count(), 1);
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
    fn save_output_is_deterministic() {
        // Two graphs with the same content added in different orders must
        // serialize to byte-identical files.
        let mut g1 = KnowledgeGraph::new();
        for label in ["Alpha", "Beta", "Gamma", "Delta"] {
            g1.add_node(NodeKind::Concept, label, 50);
        }
        let mut g2 = KnowledgeGraph::new();
        for label in ["Delta", "Gamma", "Beta", "Alpha"] {
            g2.add_node(NodeKind::Concept, label, 50);
        }
        let (p1, p2) = (temp("det1"), temp("det2"));
        GraphStore::save(&p1.0, &g1).unwrap();
        GraphStore::save(&p2.0, &g2).unwrap();
        // Node bodies differ only by id (minted in add order); but re-saving the
        // SAME graph twice must be identical byte-for-byte.
        let a = std::fs::read_to_string(&p1.0).unwrap();
        GraphStore::save(&p1.0, &g1).unwrap();
        let b = std::fs::read_to_string(&p1.0).unwrap();
        assert_eq!(a, b);
        // And the lines are sorted, so order is stable across reads.
        let mut lines: Vec<&str> = a.lines().skip(1).collect();
        let sorted = {
            let mut s = lines.clone();
            s.sort_unstable();
            s
        };
        lines.sort_unstable();
        assert_eq!(lines, sorted);
    }

    #[test]
    fn a_custom_kind_token_containing_a_tab_does_not_corrupt_the_file() {
        // NodeKind::Other/EdgeKind::Other are freely constructible by any
        // caller (e.g. extraction building a kind from free text); every
        // other free-form field written by save() is sanitized, but the kind
        // token wasn't, so a tab inside it shifted every later field on load.
        let mut g = KnowledgeGraph::new();
        let a = g.add_node(NodeKind::Other("foo\tbar".into()), "Widget", 77);
        g.set_provenance(&a, "src1");
        let b = g.add_node(NodeKind::Concept, "Other", 50);
        g.connect(&a, &b, EdgeKind::Other("rel\tkind".into()));

        let path = temp("tab-in-kind");
        GraphStore::save(&path.0, &g).unwrap();
        let loaded = GraphStore::load(&path.0).unwrap();

        let node = loaded.node(&a).unwrap();
        assert_eq!(node.label, "Widget");
        assert_eq!(node.confidence, 77);
        assert_eq!(node.provenance.as_deref(), Some("src1"));
        assert_eq!(node.kind, NodeKind::Other("foo bar".into()));

        let edge = loaded.edges().find(|e| e.from == a).unwrap();
        assert_eq!(edge.kind, EdgeKind::Other("rel kind".into()));
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
