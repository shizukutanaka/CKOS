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
    plan <intent...>                 Decompose an intent into a workflow DAG
    run [--session <dir>] <intent…>  Plan and execute a workflow end-to-end,
                                     persisting the run when --session is given
    history <dir>                    Show the execution history of a session
    search <dir> <query…>            Hybrid-search a session's stored documents
    kql <query>                      Run a KQL query against a demo knowledge graph
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

fn cmd_plan(rest: &[String]) -> ExitCode {
    if rest.is_empty() {
        eprintln!("error: `plan` needs an intent, e.g. `ckos plan research transformers`");
        return ExitCode::FAILURE;
    }
    let intent = rest.join(" ");
    let dag = HeuristicPlanner::new().plan(&intent);

    // Register one demo agent per capability so we can show discovery.
    let mut registry = CapabilityRegistry::new();
    for cap in [
        Capability::Retrieval,
        Capability::Embedding,
        Capability::Reasoning,
        Capability::Verification,
    ] {
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
    for cap in [
        Capability::Retrieval,
        Capability::Embedding,
        Capability::Reasoning,
        Capability::Verification,
    ] {
        runtimes.register(Box::new(EchoRuntime::new(vec![cap.clone()])));
        agents.register(AgentManifest::new(format!("{cap}-agent"), cap));
    }
    let verifier = Verifier::new().with_check(Box::new(NonEmptyCheck));
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
        println!("  [{:?} {:.2}] {} — {}", h.source, h.score, h.title, h.snippet);
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

    // A small demo knowledge graph to run the query against (§897).
    let mut graph = KnowledgeGraph::new();
    let transformer = graph.add_node(NodeKind::Concept, "Transformer", 96);
    let attention = graph.add_node(NodeKind::Other("algorithm".into()), "Attention", 93);
    let rnn = graph.add_node(NodeKind::Other("algorithm".into()), "RNN", 55);
    let vaswani = graph.add_node(NodeKind::Person, "Vaswani", 90);
    graph.connect(&transformer, &attention, EdgeKind::References);
    graph.connect(&transformer, &rnn, EdgeKind::References);
    graph.connect(&transformer, &vaswani, EdgeKind::CreatedBy);

    let result = kql_execute(&query, &graph);
    println!("primary ({}):", result.primary.len());
    for m in &result.primary {
        println!("  - {} [{}] conf={}", m.label, m.kind, m.confidence);
    }
    if query.related.is_some() {
        println!("related ({}):", result.related.len());
        for m in &result.related {
            println!("  - {} [{}] conf={}", m.label, m.kind, m.confidence);
        }
    }
    ExitCode::SUCCESS
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
