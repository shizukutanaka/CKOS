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
fn graph_session_accumulates_instead_of_overwriting_the_persisted_graph() {
    // `ckos graph --session` used to always start from a fresh, empty graph
    // and unconditionally overwrite graph.kg — silently destroying any
    // concepts `ckos run --session` had already accumulated from intents/
    // outputs that are never themselves persisted as session documents.
    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let dir =
        std::env::temp_dir().join(format!("ckos-graph-accumulate-{}-{n}", std::process::id()));
    let _guard = TempDir(dir.clone());

    // Two runs, each contributing a distinct concept to the session's graph
    // purely from intent text that is never itself stored as a document.
    for intent in [
        "research Transformer Attention",
        "learn about Vaswani Attention",
    ] {
        let run = ckos(&["run", "--session", dir.to_str().unwrap(), intent]);
        assert!(run.status.success());
    }
    let before = std::fs::read_to_string(dir.join("graph.kg")).unwrap();
    assert!(before.contains("Transformer Attention"));
    assert!(before.contains("Vaswani Attention"));

    // A plain `ckos graph --session <dir>` (the CLI's documented way to
    // (re)build a session's graph) must accumulate, not replace: both
    // concepts from the two runs above must still be present afterward.
    let g = ckos(&["graph", "--session", dir.to_str().unwrap()]);
    assert!(g.status.success());
    let after = std::fs::read_to_string(dir.join("graph.kg")).unwrap();
    assert!(
        after.contains("Transformer Attention"),
        "graph --session must not drop concepts from earlier runs: {after}"
    );
    assert!(after.contains("Vaswani Attention"));
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

    // Out of range, and the values `str::parse::<f32>` quietly accepts. The
    // flag promised "a number in 0.0..=1.0" but only rejected non-numbers, so
    // these were taken and clamped downstream — and NaN silently disabled the
    // very diversification being requested.
    for bad in ["not-a-number", "5", "-3", "NaN", "inf"] {
        let out = ckos(&[
            "search",
            "--diverse",
            "--lambda",
            bad,
            dir.to_str().unwrap(),
            "kernel",
        ]);
        assert!(
            !out.status.success(),
            "--lambda {bad} was accepted: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
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

    // Code is not a citation: a subscript such as `argv[0]` must not be
    // rejected as an undefined reference (it was, once).
    let code = ckos(&["verify", "print sys.argv[0] to show the program name"]);
    assert!(
        code.status.success(),
        "subscript rejected: {}",
        String::from_utf8_lossy(&code.stdout)
    );
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
fn repeated_boolean_flag_is_idempotent_not_leaked_into_positionals() {
    // take_flag used to consume only the first occurrence of a flag; a second
    // "--dot" would fall through into the positional args and get joined into
    // the intent text itself, corrupting it. A repeated flag must behave
    // exactly like a single one.
    let once = ckos(&["plan", "--dot", "research X"]);
    let twice = ckos(&["plan", "--dot", "--dot", "research X"]);
    assert!(twice.status.success());
    assert_eq!(stdout(&twice), stdout(&once));
    assert!(!stdout(&twice).contains("--dot"));
}

#[test]
fn repeated_value_flag_last_occurrence_wins() {
    // take_value_flag used to consume only the first occurrence of a flag; a
    // second "--k 1" would fall through into the positional args, becoming
    // part of the search query instead of overriding k. The last occurrence
    // should win, the usual CLI convention.
    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("ckos-repeated-flag-{}-{n}", std::process::id()));
    let _guard = TempDir(dir.clone());

    for intent in ["the quokka eats bamboo", "the platypus swims upriver"] {
        let run = ckos(&["run", "--session", dir.to_str().unwrap(), intent]);
        assert!(run.status.success());
    }

    let out = ckos(&[
        "history",
        dir.to_str().unwrap(),
        "--k",
        "3",
        "--k",
        "1",
        "quokka",
    ]);
    assert!(out.status.success());
    let s = stdout(&out);
    assert!(s.contains("recalled"), "expected a recall, got: {s}");
    assert_eq!(
        s.matches("] ").count(),
        1,
        "expected the last --k (1) to win, got: {s}"
    );
}

#[test]
fn per_command_help_is_shown() {
    for cmd in [
        "plan", "run", "graph", "kql", "search", "eval", "tool", "serve", "verify",
    ] {
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

    // Without --role you are `guest`, so a medical step is denied.
    //
    // This assertion is REVERSED from what it said before. It used to read
    // "the engine has no policy attached: unrestricted, as it always was
    // before this authorization gate existed" — a deliberate
    // backward-compatibility choice, not an oversight, locked in right here.
    // It is reversed anyway, because compatibility with "before the gate
    // existed" is compatibility with the ungated state the gate exists to
    // end: omitting a flag lowered nothing, it switched authorization off.
    // `ckos tool` had already made the opposite choice for the same §929
    // mechanism, so one binary shipped both defaults. Nothing external
    // depends on the old behaviour — no tags, no releases, not on crates.io.
    let unrestricted = ckos(&["workflow", path.to_str().unwrap()]);
    assert!(
        !unrestricted.status.success(),
        "a medical step must not run unauthorized: {}",
        stdout(&unrestricted)
    );
    assert!(String::from_utf8_lossy(&unrestricted.stderr).contains("denied"));

    // A token grant authorizes it too, not just a bare role (§928 path).
    let by_token = ckos(&[
        "workflow",
        "--token",
        "tok-admin-hq",
        path.to_str().unwrap(),
    ]);
    assert!(
        by_token.status.success(),
        "--token tok-admin-hq should authorize: {}",
        String::from_utf8_lossy(&by_token.stderr)
    );

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

    // …and with no flag at all, which is the case the new `guest` default
    // actually changed. Without this, tightening the default could have
    // quietly broken every unauthenticated run.
    let ordinary_bare = ckos(&["workflow", ordinary_path.to_str().unwrap()]);
    assert!(
        ordinary_bare.status.success(),
        "an ordinary workflow must still run without a role: {}",
        String::from_utf8_lossy(&ordinary_bare.stderr)
    );
    assert!(ckos(&["run", "research the Transformer paper"])
        .status
        .success());
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

#[test]
fn closed_output_pipe_never_panics() {
    // Regression: `ckos … | head -1` used to dump a "Broken pipe" panic and
    // backtrace, because Rust ignores SIGPIPE and println! panics when the
    // pipe's read end closes. main() now exits quietly with the shell
    // convention 141 (128 + SIGPIPE). Dropping the child's stdout handle
    // closes the read end; whether the child hits EPIPE is a race we don't
    // control, so the invariant asserted is: EITHER a clean success (child
    // finished writing first) OR exit 141 — and never a panic on stderr.
    use std::process::{Command, Stdio};
    for _ in 0..5 {
        let mut child = Command::new(env!("CARGO_BIN_EXE_ckos"))
            .arg("capabilities")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn ckos");
        drop(child.stdout.take()); // close the pipe's read end immediately
        let status = child.wait().expect("wait ckos");
        let mut stderr = String::new();
        use std::io::Read;
        child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        assert!(
            !stderr.contains("panicked"),
            "broken pipe must not panic: {stderr}"
        );
        let code = status.code();
        assert!(
            matches!(code, Some(0) | Some(141)),
            "expected exit 0 or 141, got {code:?}"
        );
    }
}

#[test]
fn serve_binds_and_answers_a_real_http_request() {
    // End-to-end: spawn `ckos serve --port 0` (OS-assigned free port), parse
    // the bound address it prints, then make a real HTTP request over
    // TcpStream and assert on the response — proving the §902 gateway
    // actually accepts connections and serves the dashboard.
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpStream;
    use std::process::{Command, Stdio};

    let mut child = Command::new(env!("CARGO_BIN_EXE_ckos"))
        .args(["serve", "--port", "0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ckos serve");

    let stdout = child.stdout.take().expect("child stdout");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    reader.read_line(&mut line).expect("read listening line");
    assert!(
        line.contains("listening on http://"),
        "expected a listening banner, got: {line}"
    );
    let addr = line
        .split("http://")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .expect("parse bound address")
        .to_string();

    let mut stream = TcpStream::connect(&addr).expect("connect to ckos serve");
    stream
        .write_all(b"GET /api/capabilities HTTP/1.1\r\nHost: x\r\n\r\n")
        .expect("write request");
    let mut resp = String::new();
    stream.read_to_string(&mut resp).expect("read response");

    child.kill().ok();
    child.wait().ok();

    assert!(resp.starts_with("HTTP/1.1 200 OK"), "got: {resp}");
    assert!(resp.contains("\"capabilities\":["));
    assert!(resp.contains("planning"));
}

#[test]
fn a_malformed_gc_now_is_rejected_before_anything_is_deleted() {
    // Regression, and the most serious kind: silent data loss from a typo.
    // A document's `expires` metadata is compared *lexicographically* against
    // `--now`, so a malformed value did not fail — it silently changed which
    // documents counted as expired. Measured before the fix, with a document
    // whose `expires` was `2999-12-31`:
    //   --now today      -> collected 1 document, reported "Expired"
    //   --now notadate   -> collected 1 document, reported "Expired"
    //   --now 2026-8-25  -> collected 0   (harmless only by luck of sorting)
    // `"2999-12-31" < "today"`, so a document expiring in the year 2999 was
    // deleted. The typos that happened to be harmless were harmless because of
    // where they sorted, not because of any rule — and `gc` deletes files.
    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("ckos-gc-now-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = TempDir(dir.clone());
    let d = dir.to_str().unwrap();

    let doc = dir.join("keep.doc");
    let write_doc = || {
        std::fs::write(
            &doc,
            "doc_type: note\ntitle: Important\nconfidence: 100\nmeta.expires: 2999-12-31\n\nMust survive.\n",
        )
        .unwrap();
    };

    for bad in ["today", "notadate", "2026-8-25", "08/25/2026", "yesterday"] {
        write_doc();
        let out = ckos(&["gc", d, "--now", bad]);
        assert!(
            !out.status.success(),
            "gc --now {bad} must fail, not guess: {}",
            stdout(&out)
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("ISO date"),
            "the error should name the expected format for {bad}"
        );
        assert!(
            doc.exists(),
            "gc --now {bad} deleted a document that expires in 2999"
        );
    }

    // A well-formed date still works, and still spares an unexpired document —
    // otherwise this test would pass by disabling expiry altogether.
    write_doc();
    let ok = ckos(&["gc", d, "--now", "2026-08-25"]);
    assert!(ok.status.success(), "a valid date must work: {ok:?}");
    assert!(
        doc.exists(),
        "a document expiring in 2999 is not expired as of 2026"
    );

    // And expiry genuinely fires when the date really has passed, so the
    // validation did not quietly break the feature it guards.
    std::fs::write(
        &doc,
        "doc_type: note\ntitle: Stale\nconfidence: 100\nmeta.expires: 2020-01-01\n\nGone.\n",
    )
    .unwrap();
    let expired = ckos(&["gc", d, "--now", "2026-08-25"]);
    assert!(expired.status.success());
    assert!(
        !doc.exists(),
        "a document past its expiry must still be collected: {}",
        stdout(&expired)
    );
}

#[test]
fn index_ingests_files_and_is_idempotent() {
    // The §938 ingest path end to end: chunk a file into passages (§939), store
    // them embedded, extract concepts into the session graph (§941), and
    // re-index the new nodes (§938) so search reaches passages *and* concepts.
    // Before `ckos index` existed, all of that was library code no user could
    // reach from the product's own entry points.
    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("ckos-index-sess-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = TempDir(dir.clone());

    let src = dir.join("paper.md");
    std::fs::write(
        &src,
        "The Photon Accelerator is a runtime built by Vector Labs.\n\n\
         It schedules inference across NPU devices. The Photon Accelerator depends on \
         the CKOS Scheduler for priority ordering.\n\n\
         Vector Labs maintains the Photon Accelerator alongside an edge runtime.\n",
    )
    .unwrap();

    let d = dir.to_str().unwrap();
    let s = src.to_str().unwrap();
    let first = ckos(&["index", d, s, "--chunk", "200", "--overlap", "40"]);
    assert!(first.status.success(), "index failed: {first:?}");
    let f = stdout(&first);
    assert!(f.contains("passage(s)"), "{f}");
    assert!(f.contains("new concept(s)"), "{f}");

    // A passage is retrievable: text that appears only in the body, not in any
    // concept label.
    let passages = stdout(&ckos(&["search", d, "schedules inference"]));
    assert!(
        passages.contains("paper.md#"),
        "a chunked passage should be searchable: {passages}"
    );

    // A concept node is retrievable as its own re-indexed document.
    let concepts = stdout(&ckos(&["search", d, "Vector Labs"]));
    assert!(
        concepts.contains("Vector Labs"),
        "an extracted concept should be searchable: {concepts}"
    );

    // Re-indexing the same file replaces its passages rather than storing a
    // second copy, and accumulates into the graph instead of duplicating it.
    let before = stdout(&ckos(&["search", d, "photon"]));
    let second = ckos(&["index", d, s, "--chunk", "200", "--overlap", "40"]);
    assert!(second.status.success());
    assert!(
        stdout(&second).contains("0 new concept(s)"),
        "second pass must reinforce, not re-add: {}",
        stdout(&second)
    );
    let after = stdout(&ckos(&["search", d, "photon"]));
    assert_eq!(
        before.lines().next(),
        after.lines().next(),
        "re-indexing must not duplicate passages"
    );

    // A concept result must say something. Its document body used to be its
    // own title repeated, so `search` returned "Vector Labs — Vector Labs":
    // top-ranked (an empty doc matches every retrieval leg) and informative
    // to nobody.
    let concept_hit = stdout(&ckos(&["search", d, "Vector Labs"]));
    assert!(
        !concept_hit.contains("Vector Labs — Vector Labs\n"),
        "a concept snippet must carry more than its own label: {concept_hit}"
    );
    assert!(
        concept_hit.contains("organization") || concept_hit.contains("file:"),
        "a concept snippet should carry what the graph knows: {concept_hit}"
    );

    // Regression: indexed concepts carried no provenance, because the ingest
    // path called the non-provenance extraction. This is the README's own
    // quickstart sequence — `ckos index` then a `RETURN Sources` query — and
    // every row came back `src=<unknown>` while §947 claimed extraction stamps
    // the source. Asserted through the CLI, not just the SDK, because that is
    // where a user meets it.
    let sources = stdout(&ckos(&[
        "kql",
        "--session",
        d,
        // `Concept`, and "Photon Accelerator" rather than "Vector Labs": the
        // extractor classifies an `… Labs` entity as an Organization, so a
        // `FIND Concept "Vector Labs"` matches nothing. That was a fixture
        // error in the first draft of this assertion, not a code defect —
        // verified by dumping graph.kg, where all four nodes carry `file:`.
        "FIND Concept \"Photon Accelerator\" RETURN Graph + Sources",
    ]));
    assert!(
        sources.contains("file:") && sources.contains("paper.md"),
        "an indexed concept must report the file it came from, got: {sources}"
    );
    assert!(
        !sources.contains("<unknown>"),
        "no indexed concept should be unsourced: {sources}"
    );
}
