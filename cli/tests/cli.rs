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
fn runtimes_lists_the_registry_table() {
    // §900 registry table (RuntimeRegistry::list / RuntimeInfo) — previously
    // had no caller anywhere; this surfaces it.
    let out = ckos(&["runtimes"]);
    assert!(out.status.success());
    let s = stdout(&out);
    assert!(s.contains("registered runtimes"));
    // The demo pool serves each capability with a Cpu-local echo runtime.
    assert!(s.contains("echo"));
    assert!(s.contains("Cpu"));
    assert!(s.contains("reasoning"));
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
fn tool_permission_gate_is_authorized_by_policy_not_self_granted() {
    // Default role (guest) has no RBAC grants: the policy denies.
    let denied = ckos(&["tool", "reverse", "hello"]);
    assert!(!denied.status.success());
    let denied_explicit = ckos(&["tool", "--role", "guest", "reverse", "hello"]);
    assert!(!denied_explicit.status.success());

    // The admin role's PolicyEngine grant (text.*) authorizes the tool's
    // required permission (text.transform) via wildcard match — there is no
    // client-supplied --grant flag; the role alone determines the outcome.
    let admin = ckos(&["tool", "--role", "admin", "reverse", "hello"]);
    assert!(admin.status.success());
    assert_eq!(stdout(&admin).trim(), "olleh");

    // An unrecognized role has no policy grants either, same as guest.
    let unknown_role = ckos(&["tool", "--role", "nosuchrole", "reverse", "hello"]);
    assert!(!unknown_role.status.success());

    // A permissionless tool needs no authorization at all, regardless of role.
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

#[test]
fn sensitive_capability_requires_an_authorized_role() {
    struct TempFile(PathBuf);
    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!("ckos-medical-{}-{n}.wf", std::process::id()));
    std::fs::write(&path, "workflow: clinic\nstep diagnose: medical\n").unwrap();
    let _guard = TempFile(path.clone());

    // Without --role, the engine has no policy attached: unrestricted, as
    // it always was before this authorization gate existed.
    let unrestricted = ckos(&["workflow", path.to_str().unwrap()]);
    assert!(unrestricted.status.success());

    // guest has no RBAC grant for the medical capability: denied.
    let denied = ckos(&["workflow", "--role", "guest", path.to_str().unwrap()]);
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("policy denied"));

    // admin's capability.* RBAC grant authorizes it.
    let allowed = ckos(&["workflow", "--role", "admin", path.to_str().unwrap()]);
    assert!(allowed.status.success());
    assert!(stdout(&allowed).contains("1/1 step(s) verified"));

    // An ordinary (non-sensitive) capability is never gated, even under a
    // role with zero grants.
    let ordinary_path =
        std::env::temp_dir().join(format!("ckos-ordinary-{}-{n}.wf", std::process::id()));
    std::fs::write(&ordinary_path, "workflow: chat\nstep reply: reasoning\n").unwrap();
    let _guard2 = TempFile(ordinary_path.clone());
    let ordinary = ckos(&[
        "workflow",
        "--role",
        "guest",
        ordinary_path.to_str().unwrap(),
    ]);
    assert!(ordinary.status.success());
}

#[test]
fn gc_consolidate_compresses_oversized_documents_before_collecting() {
    // §953 previously had no entry point anywhere (memory::consolidate had
    // zero callers outside its own tests, and wasn't even in the SDK
    // prelude) — this proves `ckos gc --consolidate` actually reaches it.
    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("ckos-gc-consolidate-{}-{n}", std::process::id()));
    let _guard = TempDir(dir.clone());

    // A generic (non-classified) intent echoes back verbatim as the session
    // document's body, so a long intent yields a long stored document.
    let long_intent = "lorem ipsum dolor sit amet consectetur adipiscing elit ".repeat(4);
    let run = ckos(&[
        "run",
        "--session",
        dir.to_str().unwrap(),
        long_intent.trim(),
    ]);
    assert!(run.status.success());

    // Without --consolidate, gc's document pass is unaffected (no flag ->
    // no compression, matching every other opt-in flag on this command).
    let plain = ckos(&["gc", dir.to_str().unwrap()]);
    assert!(plain.status.success());
    assert!(!stdout(&plain).contains("consolidated"));

    // --consolidate 50 compresses the oversized document before GC runs.
    let consolidated = ckos(&["gc", dir.to_str().unwrap(), "--consolidate", "50"]);
    assert!(consolidated.status.success());
    let s = stdout(&consolidated);
    assert!(s.contains("consolidated"));
    assert!(!s.contains("consolidated 0 document"));
}

#[test]
fn history_with_a_query_recalls_instead_of_dumping_raw_history() {
    // Session::recall (§896/§927, Generative-Agents memory scoring) had zero
    // callers anywhere outside its own unit tests — `ckos history` only ever
    // dumped raw history. This proves a query on the CLI actually reaches it,
    // taking a visibly different, --k-bounded code path. The ranking math
    // itself is already covered by
    // sdk::session::tests::recall_ranks_by_recency_importance_relevance.
    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("ckos-history-recall-{}-{n}", std::process::id()));
    let _guard = TempDir(dir.clone());

    for intent in ["the quokka eats bamboo", "the platypus swims upriver"] {
        let run = ckos(&["run", "--session", dir.to_str().unwrap(), intent]);
        assert!(run.status.success());
    }

    // No query: unchanged raw-dump behavior.
    let plain = ckos(&["history", dir.to_str().unwrap()]);
    assert!(plain.status.success());
    let plain_s = stdout(&plain);
    assert!(plain_s.contains("recorded step(s)"));
    assert!(!plain_s.contains("recalled"));

    // With a query and --k 1: a different, bounded code path.
    let recalled = ckos(&["history", dir.to_str().unwrap(), "quokka", "--k", "1"]);
    assert!(recalled.status.success());
    let recalled_s = stdout(&recalled);
    assert!(recalled_s.contains("recalled"));
    assert!(!recalled_s.contains("recorded step(s)"));
    // Exactly one record line ("[ok] " or "[FAIL] " prefix per record).
    assert_eq!(
        recalled_s.matches("] ").count(),
        1,
        "expected --k 1 to bound output to one record, got: {recalled_s}"
    );
}
