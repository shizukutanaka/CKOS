//! `ckos` — the CKOS command-line interface (§902, §906).
//!
//! A deliberately small surface that demonstrates the kernel end-to-end without
//! any external runtime: plan an intent into a DAG, show the resulting tasks and
//! the agents that would be selected by capability.

use ckos_sdk::prelude::*;
use std::process::ExitCode;

const HELP: &str = "\
ckos — Cognitive Kernel OS

USAGE:
    ckos <COMMAND> [ARGS]

COMMANDS:
    plan [--dot] <intent...>         Decompose an intent into a workflow DAG
                                     (--dot emits Graphviz)
    run [--session <dir>] <intent…>  Plan and execute a workflow end-to-end,
                                     persisting the run when --session is given
    history <dir>                    Show the execution history of a session
    search <dir> <query…>            Hybrid-search a session's stored documents
    kql <query>                      Run a KQL query against a demo knowledge graph
    gc <dir> [--min-confidence N]    Garbage-collect a session's stored documents
    verify <text…>                   Run the built-in verifier checks on text
    capabilities                     List the built-in capability vocabulary
    version                          Print the CKOS version
    help                             Show this help
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("plan") => cmd_plan(&args[1..]),
        Some("run") => cmd_run(&args[1..]),
        Some("history") => cmd_history(&args[1..]),
        Some("search") => cmd_search(&args[1..]),
        Some("kql") => cmd_kql(&args[1..]),
        Some("gc") => cmd_gc(&args[1..]),
        Some("verify") => cmd_verify(&args[1..]),
        Some("capabilities") => cmd_capabilities(),
        Some("version") => {
            println!("ckos {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("help") | None => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("unknown command: {other}\n");
            print!("{HELP}");
            ExitCode::FAILURE
        }
    }
}

/// Capabilities the demo CLI provides agents/runtimes for — enough to run any
/// intent the heuristic planner produces.
fn demo_capabilities() -> [Capability; 7] {
    [
        Capability::Planning,
        Capability::Retrieval,
        Capability::Embedding,
        Capability::Reasoning,
        Capability::Verification,
        Capability::Coding,
        Capability::Translation,
    ]
}

fn cmd_plan(rest: &[String]) -> ExitCode {
    // Optional `--dot` flag emits Graphviz instead of the step listing.
    let (dot, intent_args): (bool, &[String]) = match rest {
        [flag, tail @ ..] if flag == "--dot" => (true, tail),
        _ => (false, rest),
    };
    if intent_args.is_empty() {
        eprintln!("error: `plan` needs an intent, e.g. `ckos plan research transformers`");
        return ExitCode::FAILURE;
    }
    let intent = intent_args.join(" ");
    let dag = HeuristicPlanner::new().plan(&intent);

    if dot {
        print!("{}", dag.to_dot());
        return ExitCode::SUCCESS;
    }

    // Register one demo agent per capability so we can show discovery.
    let mut registry = CapabilityRegistry::new();
    for cap in demo_capabilities() {
        registry.register(AgentManifest::new(format!("{cap}-agent"), cap));
    }

    println!("intent : {intent}");
    println!("workflow: {} ({} step(s))", dag.name(), dag.len());

    match dag.topological_order() {
        Some(order) => {
            println!("\nexecution order:");
            for (i, step) in order.iter().enumerate() {
                if let Some(task) = dag.task(*step) {
                    let agents = registry.discover(&task.capability).len();
                    println!(
                        "  {}. [{}] {}  (agents available: {})",
                        i + 1,
                        task.capability,
                        task.description,
                        agents
                    );
                }
            }
            ExitCode::SUCCESS
        }
        None => {
            eprintln!("error: workflow contains a cycle and cannot be scheduled");
            ExitCode::FAILURE
        }
    }
}

fn cmd_run(rest: &[String]) -> ExitCode {
    // Optional `--session <dir>` prefix; the remainder is the intent.
    let (session_dir, intent_args): (Option<&str>, &[String]) = match rest {
        [flag, dir, tail @ ..] if flag == "--session" => (Some(dir.as_str()), tail),
        _ => (None, rest),
    };
    if intent_args.is_empty() {
        eprintln!("error: `run` needs an intent, e.g. `ckos run research transformers`");
        return ExitCode::FAILURE;
    }
    let intent = intent_args.join(" ");
    let dag = HeuristicPlanner::new().plan(&intent);

    // Assemble a fully offline engine: an echo runtime and a demo agent per
    // capability, plus a non-empty output check on the verifier.
    let mut runtimes = RuntimeRegistry::new();
    let mut agents = CapabilityRegistry::new();
    for cap in demo_capabilities() {
        runtimes.register(Box::new(EchoRuntime::new(vec![cap.clone()])));
        agents.register(AgentManifest::new(format!("{cap}-agent"), cap));
    }
    let verifier = Verifier::new()
        .with_check(Box::new(NonEmptyCheck))
        .with_check(Box::new(CitationCheck));
    let engine = Engine::new(runtimes, agents, verifier);

    println!("intent : {intent}");
    println!("workflow: {} ({} step(s))\n", dag.name(), dag.len());

    match engine.run_workflow(&dag) {
        Ok(results) => {
            for (i, r) in results.iter().enumerate() {
                let mark = if r.verified { "ok" } else { "FAIL" };
                let agent = r.agent.as_deref().unwrap_or("<none>");
                println!(
                    "  {}. [{}] {} via {}/{}  -> {}",
                    i + 1,
                    r.capability,
                    mark,
                    agent,
                    r.runtime,
                    r.output
                );
            }
            let ok = results.iter().filter(|r| r.verified).count();
            println!("\n{ok}/{} step(s) verified", results.len());

            // Audit trail (§903): verifiable I/O hashes, no raw payloads.
            println!(
                "\naudit: {} record(s), {} error(s)",
                engine.audit().len(),
                engine.audit().error_count()
            );

            // Telemetry (§904): latency / token throughput.
            let tel = engine.telemetry();
            println!(
                "telemetry: {} tokens, mean latency {:.1}ms, {:.0} tok/s",
                tel.total_tokens(),
                tel.mean_latency_ms().unwrap_or(0.0),
                tel.mean_tokens_per_sec()
            );

            // Collective reflection over the run (§921–§922).
            let reflections = engine.reflect(&HeuristicReflector::new(), &results);
            let verdict = consensus(&reflections);
            println!("\nreflection: consensus score {}/100", verdict.score);
            for hint in &verdict.hints {
                println!("  - {hint}");
            }

            // Persist the run to a durable session if requested (§927).
            if let Some(dir) = session_dir {
                match FileStore::open(dir) {
                    Ok(store) => {
                        let mut session = Session::new("cli", Box::new(store))
                            .with_embedder(Box::new(HashingEmbedder::default()));
                        if let Err(e) = session
                            .record_run(&results)
                            .and_then(|_| session.record_reflections(&reflections))
                        {
                            eprintln!("warning: failed to persist session: {e}");
                        } else {
                            println!("\nsession saved to {dir}");
                        }
                    }
                    Err(e) => eprintln!("warning: could not open session {dir}: {e}"),
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_history(rest: &[String]) -> ExitCode {
    let Some(dir) = rest.first() else {
        eprintln!("error: `history` needs a session directory, e.g. `ckos history ./my-session`");
        return ExitCode::FAILURE;
    };
    let store = match FileStore::open(dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: could not open session {dir}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let session = Session::new("cli", Box::new(store));
    let history = match session.history() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    if history.is_empty() {
        println!("session {dir}: no execution history");
        return ExitCode::SUCCESS;
    }
    println!("session {dir}: {} recorded step(s)", history.len());
    for doc in &history {
        let verified = doc.metadata.get("verified").map(String::as_str) == Some("true");
        let mark = if verified { "ok" } else { "FAIL" };
        println!("  [{mark}] {} -> {}", doc.title, doc.body);
    }
    ExitCode::SUCCESS
}

fn cmd_search(rest: &[String]) -> ExitCode {
    let (dir, query) = match rest {
        [dir, q @ ..] if !q.is_empty() => (dir.as_str(), q.join(" ")),
        _ => {
            eprintln!("error: usage `ckos search <dir> <query…>`");
            return ExitCode::FAILURE;
        }
    };
    let store = match FileStore::open(dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: could not open session {dir}: {e}");
            return ExitCode::FAILURE;
        }
    };
    // Documents persist; the graph is rebuilt per process, so it is empty here.
    let graph = KnowledgeGraph::new();
    let embedder = HashingEmbedder::default();
    let retriever = Retriever::with_embedder(&store, &graph, &embedder);
    let hits = retriever.search(&query, 10);
    if hits.is_empty() {
        println!("no results for {query:?} in {dir}");
        return ExitCode::SUCCESS;
    }
    println!("{} hit(s) for {query:?}:", hits.len());
    for h in &hits {
        println!(
            "  [{:?} {:.2}] {} — {}",
            h.source, h.score, h.title, h.snippet
        );
    }
    ExitCode::SUCCESS
}

fn cmd_kql(rest: &[String]) -> ExitCode {
    if rest.is_empty() {
        eprintln!("error: usage `ckos kql \"FIND Concept \\\"Transformer\\\" RELATED Algorithm\"`");
        return ExitCode::FAILURE;
    }
    let source = rest.join(" ");
    let query = match kql_parse(&source) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    // A small demo knowledge graph to run the query against (§897), with
    // temporal dates (§946) and provenance (§947).
    let mut graph = KnowledgeGraph::new();
    let transformer = graph.add_node(NodeKind::Concept, "Transformer", 96);
    graph.set_date(&transformer, "2017-06-12");
    graph.set_provenance(&transformer, "paper:Vaswani-2017");
    let attention = graph.add_node(NodeKind::Other("algorithm".into()), "Attention", 93);
    graph.set_date(&attention, "2015-09-01");
    graph.set_provenance(&attention, "paper:Bahdanau-2015");
    let rnn = graph.add_node(NodeKind::Other("algorithm".into()), "RNN", 55);
    let vaswani = graph.add_node(NodeKind::Person, "Vaswani", 90);
    graph.connect(&transformer, &attention, EdgeKind::References);
    graph.connect(&transformer, &rnn, EdgeKind::References);
    graph.connect(&transformer, &vaswani, EdgeKind::CreatedBy);

    let show_sources = query.returns.contains(&ReturnTarget::Sources);
    let print_match = |m: &NodeMatch| {
        let mut line = format!("  - {} [{}] conf={}", m.label, m.kind, m.confidence);
        if let Some(date) = &m.date {
            line.push_str(&format!(" @{date}"));
        }
        if show_sources {
            line.push_str(&format!(
                " src={}",
                m.provenance.as_deref().unwrap_or("<unknown>")
            ));
        }
        println!("{line}");
    };

    let result = kql_execute(&query, &graph);
    println!("primary ({}):", result.primary.len());
    result.primary.iter().for_each(&print_match);
    if query.related.is_some() {
        println!("related ({}):", result.related.len());
        result.related.iter().for_each(&print_match);
    }
    ExitCode::SUCCESS
}

fn cmd_gc(rest: &[String]) -> ExitCode {
    let Some(dir) = rest.first() else {
        eprintln!("error: `gc` needs a session directory, e.g. `ckos gc ./my-session`");
        return ExitCode::FAILURE;
    };
    // Optional `--min-confidence N`.
    let min_confidence: u8 = match rest.iter().position(|a| a == "--min-confidence") {
        Some(i) => match rest.get(i + 1).and_then(|v| v.parse().ok()) {
            Some(n) => n,
            None => {
                eprintln!("error: --min-confidence needs a number 0..=255");
                return ExitCode::FAILURE;
            }
        },
        None => 0,
    };

    let mut store = match FileStore::open(dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: could not open session {dir}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let policy = GcPolicy {
        min_confidence,
        ..GcPolicy::default()
    };
    match gc_collect(&mut store, &policy, None) {
        Ok(report) => {
            println!(
                "garbage-collected {} document(s) from {dir}",
                report.count()
            );
            for (id, reason) in &report.removed {
                println!("  - {id} ({reason:?})");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_verify(rest: &[String]) -> ExitCode {
    if rest.is_empty() {
        eprintln!("error: `verify` needs text, e.g. `ckos verify 'see [1]'`");
        return ExitCode::FAILURE;
    }
    let text = rest.join(" ");
    // The full built-in §899 check set.
    let verifier = Verifier::new()
        .with_check(Box::new(NonEmptyCheck))
        .with_check(Box::new(JsonBalanceCheck))
        .with_check(Box::new(CitationCheck))
        .with_check(Box::new(ForbiddenContentCheck::new([
            "begin private key",
            "password=",
            "api_key=",
        ])));
    let report = verifier.verify(&text);
    for (name, verdict) in &report.results {
        let status = match verdict {
            Verdict::Pass => "pass".to_string(),
            Verdict::Skip => "skip".to_string(),
            Verdict::Fail(why) => format!("FAIL — {why}"),
        };
        println!("  {name:<16} {status}");
    }
    if report.passed() {
        println!("\nverified: all checks passed");
        ExitCode::SUCCESS
    } else {
        println!("\nverification FAILED");
        ExitCode::FAILURE
    }
}

fn cmd_capabilities() -> ExitCode {
    let caps = [
        "planning",
        "reasoning",
        "coding",
        "translation",
        "embedding",
        "retrieval",
        "verification",
        "simulation",
        "vision",
        "speech",
        "robotics",
        "finance",
        "medical",
        "legal",
    ];
    println!("built-in capabilities (§911):");
    for c in caps {
        println!("  - {c}");
    }
    ExitCode::SUCCESS
}
