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

    // The --expand and --diverse search refinements are reachable and succeed.
    for flag in ["--expand", "--diverse"] {
        let out = ckos(&["search", flag, dir.to_str().unwrap(), "Transformer"]);
        assert!(out.status.success(), "search {flag} should succeed");
    }

    // KQL can query the same persisted graph.
    let k = ckos(&[
        "kql",
        "--session",
        dir.to_str().unwrap(),
        "FIND Concept \"Transformer\"",
    ]);
    assert!(k.status.success());
    assert!(stdout(&k).contains("Transformer"));

    // eval scores the search against a known-relevant title.
    let e = ckos(&[
        "eval",
        "--relevant",
        "Transformer",
        dir.to_str().unwrap(),
        "Transformer",
    ]);
    assert!(e.status.success());
    let es = stdout(&e);
    assert!(es.contains("precision@") && es.contains("MRR"));
    assert!(es.contains("1.000")); // the relevant hit ranks first → MRR 1.0
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
fn eval_reports_correct_precision_and_recall() {
    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("ckos-eval-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = TempDir(dir.clone());

    // Two documents match "widget"; only "Widget A" is considered relevant, so
    // with k=2 precision must be exactly 0.5 and recall exactly 1.0 (the single
    // relevant document is captured within the top 2).
    std::fs::write(
        dir.join("a.doc"),
        "doc_type: note\ntitle: Widget A\nconfidence: 100\n\nAll about the widget.\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("b.doc"),
        "doc_type: note\ntitle: Widget B\nconfidence: 100\n\nAlso a widget mention.\n",
    )
    .unwrap();

    let out = ckos(&[
        "eval",
        "--relevant",
        "Widget A",
        "--k",
        "2",
        dir.to_str().unwrap(),
        "widget",
    ]);
    assert!(out.status.success());
    let s = stdout(&out);
    assert!(s.contains("precision@2  0.500"), "got: {s}");
    assert!(s.contains("recall@2     1.000"), "got: {s}");

    // --k 0 is rejected rather than silently producing meaningless 0.0 metrics.
    let zero = ckos(&[
        "eval",
        "--relevant",
        "Widget A",
        "--k",
        "0",
        dir.to_str().unwrap(),
        "widget",
    ]);
    assert!(!zero.status.success());
}

#[test]
fn search_synonyms_flag_bridges_vocabulary_gap() {
    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("ckos-synonyms-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = TempDir(dir.clone());

    // A true paraphrase sharing zero words with the query.
    std::fs::write(
        dir.join("a.doc"),
        "doc_type: note\ntitle: Task Prioritization\nconfidence: 100\n\n\
         Ready work is ordered and run according to importance.\n",
    )
    .unwrap();

    let baseline = ckos(&["search", dir.to_str().unwrap(), "scheduler priority"]);
    assert!(baseline.status.success());
    assert!(stdout(&baseline).contains("no results"));

    let with_synonyms = ckos(&[
        "search",
        "--synonyms",
        dir.to_str().unwrap(),
        "scheduler priority",
    ]);
    assert!(with_synonyms.status.success());
    assert!(stdout(&with_synonyms).contains("Task Prioritization"));
}

#[test]
fn search_lambda_flag_controls_diversity() {
    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("ckos-lambda-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = TempDir(dir.clone());
    std::fs::write(
        dir.join("a.doc"),
        "doc_type: note\ntitle: Alpha\nconfidence: 100\n\nkernel scheduling\n",
    )
    .unwrap();

    let ok = ckos(&[
        "search",
        "--diverse",
        "--lambda",
        "0.3",
        dir.to_str().unwrap(),
        "kernel",
    ]);
    assert!(ok.status.success());

    let bad = ckos(&[
        "search",
        "--diverse",
        "--lambda",
        "not-a-number",
        dir.to_str().unwrap(),
        "kernel",
    ]);
    assert!(!bad.status.success());
}

#[test]
fn tool_permission_gate_denies_then_grants() {
    // No permission granted: the gate denies.
    let denied = ckos(&["tool", "reverse", "hello"]);
    assert!(!denied.status.success());

    // Exact grant permits.
    let exact = ckos(&["tool", "--grant", "text.transform", "reverse", "hello"]);
    assert!(exact.status.success());
    assert_eq!(stdout(&exact).trim(), "olleh");

    // A wildcard grant covers the required permission too (policy::PolicyEngine
    // parity), and a permissionless tool needs no grant at all.
    let wildcard = ckos(&["tool", "--grant", "text.*", "reverse", "hello"]);
    assert!(wildcard.status.success());
    let uppercase = ckos(&["tool", "uppercase", "hi"]);
    assert!(uppercase.status.success());
    assert_eq!(stdout(&uppercase).trim(), "HI");
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
fn flags_work_in_any_position() {
    // --dot after the intent is accepted (position-independent flags).
    let trailing = ckos(&["plan", "research X", "--dot"]);
    assert!(trailing.status.success());
    assert!(stdout(&trailing).starts_with("digraph workflow {"));

    // Same result as the leading-flag form.
    let leading = ckos(&["plan", "--dot", "research X"]);
    assert_eq!(stdout(&leading), stdout(&trailing));
}

#[test]
fn per_command_help_is_shown() {
    for cmd in ["plan", "run", "graph", "kql", "search", "eval", "tool"] {
        let out = ckos(&[cmd, "--help"]);
        assert!(out.status.success(), "{cmd} --help should succeed");
        assert!(
            stdout(&out).contains("usage:"),
            "{cmd} --help should print usage"
        );
    }
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
