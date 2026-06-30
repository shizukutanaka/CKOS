//! `ckos` — the CKOS command-line interface (§902, §906).
//!
//! A deliberately small surface that demonstrates the kernel end-to-end without
//! any external runtime: plan an intent into a DAG, show the resulting tasks and
//! the agents that would be selected by capability.

use ckos_sdk::prelude::*;
use std::path::Path;
use std::process::ExitCode;

/// Filename under a session directory holding the persisted knowledge graph.
const GRAPH_FILE: &str = "graph.kg";

/// Remove the first occurrence of a boolean flag `name` from `args`, returning
/// whether it was present and the remaining positional args. Lets flags appear
/// in any position, consistently across commands.
fn take_flag(args: &[String], name: &str) -> (bool, Vec<String>) {
    let mut present = false;
    let mut rest = Vec::with_capacity(args.len());
    for a in args {
        if !present && a == name {
            present = true;
        } else {
            rest.push(a.clone());
        }
    }
    (present, rest)
}

/// Remove the first `--flag <value>` pair from `args`, returning the value (if
/// the flag was present) and the remaining positional args. Errors if the flag
/// appears with no following value.
fn take_value_flag(
    args: &[String],
    name: &str,
) -> std::result::Result<(Option<String>, Vec<String>), String> {
    let mut value = None;
    let mut rest = Vec::with_capacity(args.len());
    let mut i = 0;
    while i < args.len() {
        if value.is_none() && args[i] == name {
            match args.get(i + 1) {
                Some(v) => {
                    value = Some(v.clone());
                    i += 2;
                    continue;
                }
                None => return Err(format!("{name} needs a value")),
            }
        }
        rest.push(args[i].clone());
        i += 1;
    }
    Ok((value, rest))
}

/// Whether the user asked for help on a (sub)command: `-h`/`--help` as the
/// first argument.
fn wants_help(args: &[String]) -> bool {
    matches!(args.first().map(String::as_str), Some("-h" | "--help"))
}

const HELP: &str = "\
ckos — Cognitive Kernel OS

USAGE:
    ckos <COMMAND> [ARGS]

COMMANDS:
    plan [--dot] <intent...>         Decompose an intent into a workflow DAG
                                     (--dot emits Graphviz)
    run [--session <dir>] <intent…>  Plan and execute a workflow end-to-end;
                                     with --session, persist the run and grow
                                     the session's knowledge graph
    history <dir>                    Show the execution history of a session
    search <dir> <query…>            Hybrid-search a session's stored documents
    workflow <file>                  Load and execute a workflow definition file
    kql [--session <dir>] <query>    Run a KQL query (demo graph, or a session's
                                     persisted graph with --session)
    graph [--dot] <text…>            Extract a knowledge graph from text (§941)
    graph [--dot] --session <dir>    Extract a graph from a session's documents
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
        Some("workflow") => cmd_workflow(&args[1..]),
        Some("kql") => cmd_kql(&args[1..]),
        Some("graph") => cmd_graph(&args[1..]),
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

/// Capabilities the demo CLI provides agents/runtimes for — the full built-in
/// vocabulary, so it can run any intent the heuristic planner produces.
fn demo_capabilities() -> Vec<Capability> {
    Capability::builtin()
}

fn cmd_plan(rest: &[String]) -> ExitCode {
    if wants_help(rest) {
        println!("usage: ckos plan [--dot] <intent…>\n  Decompose an intent into a workflow DAG; --dot emits Graphviz.");
        return ExitCode::SUCCESS;
    }
    // Optional `--dot` flag (any position) emits Graphviz instead of the listing.
    let (dot, intent_args) = take_flag(rest, "--dot");
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
    if wants_help(rest) {
        println!("usage: ckos run [--session <dir>] <intent…>\n  Plan and execute end-to-end; --session persists the run and grows its graph.");
        return ExitCode::SUCCESS;
    }
    // Optional `--session <dir>` in any position; the remainder is the intent.
    let (session_dir, intent_args) = match take_value_flag(rest, "--session") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let session_dir = session_dir.as_deref();
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
            if let Some(top) = &verdict.majority_hint {
                println!(
                    "  top improvement ({:.0}% agreement): {top}",
                    verdict.agreement * 100.0
                );
            }
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

                // Grow the session's knowledge graph from this run's outputs
                // (§941 extraction → §936 persistence), accumulating into any
                // graph already saved so `ckos search` gains graph context.
                let graph_path = Path::new(dir).join(GRAPH_FILE);
                // If an existing graph can't be read, skip the update rather than
                // clobber a possibly-recoverable file with a fresh one.
                match GraphStore::load(&graph_path) {
                    Ok(mut graph) => {
                        // Extract from the intent (which carries the proper nouns
                        // the user named) as well as the step outputs.
                        let mut text = intent.clone();
                        for r in &results {
                            text.push('\n');
                            text.push_str(&r.output);
                        }
                        // Record where this knowledge came from (§947) so KQL
                        // `RETURN Sources` is meaningful on the session graph.
                        let report = graph.extract_concepts_with_provenance(
                            &text,
                            Some(&format!("run:{intent}")),
                        );
                        match GraphStore::save(&graph_path, &graph) {
                            Ok(()) => println!(
                                "graph updated: +{} concept(s), +{} relation(s) ({} total node(s))",
                                report.nodes_added,
                                report.edges_added,
                                graph.len()
                            ),
                            Err(e) => eprintln!(
                                "warning: could not save graph to {}: {e}",
                                graph_path.display()
                            ),
                        }
                    }
                    Err(e) => eprintln!(
                        "warning: could not load existing graph ({e}); skipping graph update"
                    ),
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
    // Load the persisted knowledge graph (empty if none has been built yet),
    // so graph-based hits work across processes (§936). Build it with
    // `ckos graph --session <dir>`. A genuine read/parse error is surfaced
    // rather than silently treated as an empty graph.
    let graph = match GraphStore::load(Path::new(dir).join(GRAPH_FILE)) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("warning: could not load graph ({e}); searching documents only");
            KnowledgeGraph::default()
        }
    };
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

fn cmd_workflow(rest: &[String]) -> ExitCode {
    let Some(path) = rest.first() else {
        eprintln!("error: `workflow` needs a definition file, e.g. `ckos workflow pipeline.wf`");
        return ExitCode::FAILURE;
    };
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: could not read {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let dag = match Dag::from_definition(&text) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Demo engine that can serve any capability the definition uses.
    let mut runtimes = RuntimeRegistry::new();
    let mut agents = CapabilityRegistry::new();
    for cap in demo_capabilities() {
        runtimes.register(Box::new(EchoRuntime::new(vec![cap.clone()])));
        agents.register(AgentManifest::new(format!("{cap}-agent"), cap));
    }
    let engine = Engine::new(
        runtimes,
        agents,
        Verifier::new().with_check(Box::new(NonEmptyCheck)),
    );

    println!("workflow: {} ({} step(s))\n", dag.name(), dag.len());
    match engine.run_workflow(&dag) {
        Ok(results) => {
            for (i, r) in results.iter().enumerate() {
                let mark = if r.verified { "ok" } else { "FAIL" };
                println!("  {}. [{}] {} -> {}", i + 1, r.capability, mark, r.output);
            }
            let ok = results.iter().filter(|r| r.verified).count();
            println!("\n{ok}/{} step(s) verified", results.len());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// A small demo knowledge graph for `ckos kql` when no session is given.
fn demo_graph() -> KnowledgeGraph {
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
    graph
}

fn cmd_kql(rest: &[String]) -> ExitCode {
    if wants_help(rest) {
        println!("usage: ckos kql [--session <dir>] <query>\n  Run a KQL query against the demo graph, or a session's persisted graph with --session.");
        return ExitCode::SUCCESS;
    }
    // Optional `--session <dir>` (any position) queries that session's persisted
    // graph instead of the built-in demo graph.
    let (session_dir, rest) = match take_value_flag(rest, "--session") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let session_dir = session_dir.as_deref();
    if rest.is_empty() {
        eprintln!("error: usage `ckos kql [--session <dir>] \"FIND Concept \\\"Transformer\\\" RELATED Algorithm\"`");
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

    // Either the session's persisted graph (§936) or a small demo graph (§897)
    // with temporal dates (§946) and provenance (§947).
    let graph = match session_dir {
        Some(dir) => match GraphStore::load(Path::new(dir).join(GRAPH_FILE)) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("error: could not load graph from {dir}: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => demo_graph(),
    };

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

fn cmd_graph(rest: &[String]) -> ExitCode {
    if wants_help(rest) {
        println!("usage: ckos graph [--dot] <text…> | ckos graph [--dot] --session <dir>\n  Extract a knowledge graph from text or a session's documents; --dot emits Graphviz.");
        return ExitCode::SUCCESS;
    }
    // Flags may appear in any position.
    let (dot, rest) = take_flag(rest, "--dot");
    let (session_dir, rest) = match take_value_flag(&rest, "--session") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let session_dir = session_dir.as_deref();

    // Source the text: either a session's documents or inline arguments.
    let text: String = match session_dir {
        Some(dir) => {
            let store = match FileStore::open(dir) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("error: could not open session {dir}: {e}");
                    return ExitCode::FAILURE;
                }
            };
            // Empty query (limit 0) returns every stored document.
            match store.search(&Query::default()) {
                Ok(docs) => docs
                    .iter()
                    .map(|d| format!("{}. {}", d.title, d.body))
                    .collect::<Vec<_>>()
                    .join("\n"),
                Err(e) => {
                    eprintln!("error: {e}");
                    return ExitCode::FAILURE;
                }
            }
        }
        None if !rest.is_empty() => rest.join(" "),
        None => {
            eprintln!(
                "error: usage `ckos graph [--dot] <text…>` or `ckos graph [--dot] --session <dir>`"
            );
            return ExitCode::FAILURE;
        }
    };

    let mut graph = KnowledgeGraph::new();
    // Tag concepts from a session build with a documents provenance (§947);
    // inline text has no durable source.
    let report = match session_dir {
        Some(_) => graph.extract_concepts_with_provenance(&text, Some("session documents")),
        None => graph.extract_concepts(&text),
    };

    // Persist the extracted graph so `ckos search` can use it across runs (§936).
    if let Some(dir) = session_dir {
        let path = Path::new(dir).join(GRAPH_FILE);
        if let Err(e) = GraphStore::save(&path, &graph) {
            eprintln!("warning: could not save graph to {}: {e}", path.display());
        } else {
            eprintln!("graph saved to {}", path.display());
        }
    }

    if dot {
        print!("{}", graph.to_dot());
        return ExitCode::SUCCESS;
    }

    println!(
        "extracted {} concept(s), {} relation(s) ({} reinforced)",
        report.nodes_added, report.edges_added, report.nodes_reinforced
    );
    let mut nodes: Vec<_> = graph.nodes().collect();
    nodes.sort_by(|a, b| b.confidence.cmp(&a.confidence).then(a.label.cmp(&b.label)));
    for n in nodes {
        println!("  - {} [{:?}] conf={}", n.label, n.kind, n.confidence);
    }

    // Most central concepts by PageRank (§951 graph reasoning).
    let central = graph.central_nodes(3);
    if central.iter().any(|(_, s)| *s > 0.0) {
        println!("\nmost central:");
        for (node, score) in central {
            println!("  - {} ({:.3})", node.label, score);
        }
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
    let report = Verifier::builtin().verify(&text);
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
    println!("built-in capabilities (§911):");
    for c in Capability::builtin() {
        println!("  - {c}");
    }
    ExitCode::SUCCESS
}
