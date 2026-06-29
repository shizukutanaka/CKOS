//! Integration tests for the `ckos` binary — runs the built executable and
//! asserts on its output and exit codes, guarding the CLI surface against
//! argument-parsing regressions. Dependency-free: Cargo exposes the binary path
//! via `CARGO_BIN_EXE_ckos`.

use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

fn ckos(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ckos"))
        .args(args)
        .output()
        .expect("failed to run ckos")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn version_and_help() {
    let v = ckos(&["version"]);
    assert!(v.status.success());
    assert!(stdout(&v).contains("ckos"));

    // No args prints help and succeeds.
    let h = ckos(&[]);
    assert!(h.status.success());
    assert!(stdout(&h).contains("USAGE"));
}

#[test]
fn capabilities_lists_vocabulary() {
    let out = ckos(&["capabilities"]);
    assert!(out.status.success());
    let s = stdout(&out);
    assert!(s.contains("planning"));
    assert!(s.contains("verification"));
}

#[test]
fn plan_and_run_research() {
    let plan = ckos(&["plan", "research the Transformer paper"]);
    assert!(plan.status.success());
    assert!(stdout(&plan).contains("execution order"));

    let run = ckos(&["run", "research the Transformer paper"]);
    assert!(run.status.success());
    let s = stdout(&run);
    assert!(s.contains("5/5 step(s) verified"));
    assert!(s.contains("audit:"));
    assert!(s.contains("telemetry:"));
}

#[test]
fn plan_dot_emits_graphviz() {
    let out = ckos(&["plan", "--dot", "research X"]);
    assert!(out.status.success());
    let s = stdout(&out);
    assert!(s.starts_with("digraph workflow {"));
    assert!(s.contains("->"));
}

#[test]
fn kql_runs_against_demo_graph() {
    let out = ckos(&["kql", "FIND Concept \"Transformer\" RELATED Algorithm"]);
    assert!(out.status.success());
    let s = stdout(&out);
    assert!(s.contains("Transformer"));
    assert!(s.contains("Attention"));
}

#[test]
fn graph_extracts_concepts_from_text() {
    let out = ckos(&["graph", "CKOS uses a Knowledge Graph. CKOS is fast."]);
    assert!(out.status.success());
    let s = stdout(&out);
    assert!(s.contains("CKOS"));
    assert!(s.contains("Knowledge Graph"));
    assert!(s.contains("concept(s)"));

    // --dot emits a Graphviz digraph with at least one edge.
    let dot = ckos(&["graph", "--dot", "CKOS depends on the Scheduler."]);
    assert!(dot.status.success());
    let d = stdout(&dot);
    assert!(d.starts_with("digraph knowledge {"));
    assert!(d.contains("->"));
}

#[test]
fn run_session_builds_graph_used_by_search() {
    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("ckos-run-sess-{}-{n}", std::process::id()));
    let _guard = TempDir(dir.clone());

    // Running with --session extracts proper nouns from the intent into the
    // session's knowledge graph and persists it.
    let run = ckos(&[
        "run",
        "--session",
        dir.to_str().unwrap(),
        "study the Transformer paper",
    ]);
    assert!(run.status.success());
    assert!(stdout(&run).contains("graph updated"));
    assert!(dir.join("graph.kg").exists());

    // A later search process loads that graph and returns a graph-sourced hit.
    let s = ckos(&["search", dir.to_str().unwrap(), "Transformer"]);
    assert!(s.status.success());
    assert!(stdout(&s).contains("Graph"));
}

#[test]
fn graph_persists_to_session_and_search_uses_it() {
    // A temp session directory removed on drop.
    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("ckos-graph-sess-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = TempDir(dir.clone());

    // Seed a document directly in the FileStore format (header + blank + body).
    std::fs::write(
        dir.join("note1.doc"),
        "doc_type: note\ntitle: Intro\nconfidence: 100\n\nCKOS uses a Knowledge Graph.\n",
    )
    .unwrap();

    // Build and persist the graph from the session's documents.
    let g = ckos(&["graph", "--session", dir.to_str().unwrap()]);
    assert!(g.status.success());
    assert!(stdout(&g).contains("CKOS"));
    assert!(dir.join("graph.kg").exists(), "graph.kg should be written");

    // Search now loads the persisted graph and yields a Graph-sourced hit.
    let s = ckos(&["search", dir.to_str().unwrap(), "CKOS"]);
    assert!(s.status.success());
    assert!(stdout(&s).contains("Graph"), "expected a graph-sourced hit");
}

#[test]
fn verify_fails_on_bad_content() {
    let ok = ckos(&["verify", "clean text"]);
    assert!(ok.status.success());

    let bad = ckos(&["verify", "dangling [9] and password=secret"]);
    assert!(!bad.status.success()); // non-zero exit on verification failure
}

#[test]
fn unknown_command_fails() {
    let out = ckos(&["wat"]);
    assert!(!out.status.success());
}

#[test]
fn workflow_file_executes() {
    // Unique temp file, removed on drop.
    struct TempFile(PathBuf);
    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!("ckos-cli-{}-{n}.wf", std::process::id()));
    std::fs::write(
        &path,
        "workflow: demo\nstep fetch: retrieval\nstep think: reasoning <- fetch\n",
    )
    .unwrap();
    let _guard = TempFile(path.clone());

    let out = ckos(&["workflow", path.to_str().unwrap()]);
    assert!(out.status.success());
    assert!(stdout(&out).contains("2/2 step(s) verified"));
}
