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
    run [--session <dir>]            Plan and execute a workflow end-to-end;
      [--role R | --token T]         --session persists + grows the graph;
      <intent…>                      --role/--token authorize sensitive
                                     capabilities (§929/§928)
    history <dir> [<query…>]         Show a session's execution history, or
      [--k N]                        (with a query) recall top --k records by
                                     Generative-Agents memory score (§896/§927)
    search [--synonyms] [--expand]   Hybrid-search a session (--synonyms: domain
      [--diverse] [--lambda N]       synonym expansion, --expand: pseudo-relevance
      <dir> <query…>                 expansion, --diverse: MMR, --lambda: tradeoff)
    workflow [--role R | --token T]  Load and execute a workflow definition file
      <file>                        (--role/--token: as above, §929/§928)
    kql [--session <dir>] <query>    Run a KQL query (demo graph, or a session's
                                     persisted graph with --session)
    eval --relevant <csv> [--k N]    Score search quality (Precision/Recall/
      <dir> <query…>                 MRR/nDCG) against known-relevant titles
    graph [--dot] <text…>            Extract a knowledge graph from text (§941)
    graph [--dot] --session <dir>    Extract a graph from a session's documents
    gc <dir> [--min-confidence N]    Garbage-collect a session: low-value docs
      [--now YYYY-MM-DD]             (--now enables expiry) + orphaned graph nodes;
      [--consolidate N]              --consolidate compresses docs over N chars first (§953)
    verify <text…>                   Run the built-in verifier checks on text
    tool --list                      List built-in tools and required permissions
    tool [--role R | --token T]      Invoke a tool; required permissions are
      <name> <input…>                authorized by RBAC+ABAC policy (§929), not
                                     self-granted (roles: admin, guest; --token
                                     authenticates via a demo provider, §928)
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
        Some("eval") => cmd_eval(&args[1..]),
        Some("graph") => cmd_graph(&args[1..]),
        Some("gc") => cmd_gc(&args[1..]),
        Some("verify") => cmd_verify(&args[1..]),
        Some("tool") => cmd_tool(&args[1..]),
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
            eprintln!(
                "error: workflow cannot be scheduled (a cycle or a reference to an unknown step)"
            );
            ExitCode::FAILURE
        }
    }
}

fn cmd_run(rest: &[String]) -> ExitCode {
    if wants_help(rest) {
        println!("usage: ckos run [--session <dir>] [--role <role> | --token <token>] <intent…>\n  Plan and execute end-to-end; --session persists the run and grows its graph.\n  --role/--token attach RBAC+ABAC authorization (§929) for finance/medical/\n  legal/robotics steps. --role is bare roles (admin, guest), no attributes.\n  --token authenticates via a demo identity provider (§928: tok-admin-hq,\n  tok-admin-restricted, tok-guest), carrying real ABAC attributes. CAUTION:\n  the built-in planner never classifies free text into those capabilities (a\n  keyword classifier was tested and rejected as unsafe — see planner's docs),\n  so neither flag has effect here in practice; use `ckos workflow` with an\n  explicit `step x: medical` (etc.) to actually reach a gated capability.");
        return ExitCode::SUCCESS;
    }
    // Optional flags in any position; the remainder is the intent.
    let (session_dir, rest) = match take_value_flag(rest, "--session") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (role, rest) = match take_value_flag(&rest, "--role") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (token, intent_args) = match take_value_flag(&rest, "--token") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let identity = match resolve_identity(role, token, None) {
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
    let mut engine = Engine::new(runtimes, agents, verifier);
    // Authorization (§929) is opt-in: without --role/--token, every
    // capability runs unrestricted, exactly as before this existed.
    if let Some(identity) = identity {
        engine = engine.with_identity(demo_policy(), identity);
    }

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
    if wants_help(rest) {
        println!("usage: ckos history <dir> [<query…>] [--k N]\n  Show a session's execution history, in stored order. With a query, ranks\n  records by the §896/§927 Generative-Agents memory score (recency ×\n  importance × relevance) instead — --k caps how many are returned\n  (default 5).");
        return ExitCode::SUCCESS;
    }
    let (k, rest) = match take_value_flag(rest, "--k") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let k: usize = match k {
        Some(v) => match v.parse() {
            Ok(n) => n,
            Err(_) => {
                eprintln!("error: --k needs a positive integer");
                return ExitCode::FAILURE;
            }
        },
        None => 5,
    };
    let Some((dir, query_words)) = rest.split_first() else {
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

    if !query_words.is_empty() {
        let query = query_words.join(" ");
        let session = Session::new("cli", Box::new(store))
            .with_embedder(Box::new(HashingEmbedder::default()));
        let recalled = match session.recall(&query, k) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        };
        if recalled.is_empty() {
            println!("session {dir}: no records recalled for {query:?}");
            return ExitCode::SUCCESS;
        }
        println!(
            "session {dir}: top {} recalled for {query:?} (recency × importance × relevance)",
            recalled.len()
        );
        for doc in &recalled {
            let verified = doc.metadata.get("verified").map(String::as_str) == Some("true");
            let mark = if verified { "ok" } else { "FAIL" };
            println!("  [{mark}] {} -> {}", doc.title, doc.body);
        }
        return ExitCode::SUCCESS;
    }

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
    if wants_help(rest) {
        println!("usage: ckos search [--synonyms] [--expand] [--diverse] [--lambda N] <dir> <query…>\n  Hybrid search (BM25 + vector + graph, RRF-fused). --synonyms rewrites the query\n  with a built-in domain synonym table before searching (closes vocabulary gaps\n  literal matching can't); --expand adds pseudo-relevance query expansion;\n  --diverse re-ranks for variety (MMR). --lambda (0..1, default 0.7) trades MMR\n  relevance (1.0) against diversity (0.0); only applies with --diverse.");
        return ExitCode::SUCCESS;
    }
    // Optional flags in any position.
    let (synonyms, rest) = take_flag(rest, "--synonyms");
    let (expand, rest) = take_flag(&rest, "--expand");
    let (diverse, rest) = take_flag(&rest, "--diverse");
    let (lambda_str, rest) = match take_value_flag(&rest, "--lambda") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let lambda: f32 = match lambda_str.as_deref().map(str::parse) {
        Some(Ok(n)) => n,
        Some(Err(_)) => {
            eprintln!("error: --lambda needs a number in 0.0..=1.0");
            return ExitCode::FAILURE;
        }
        None => 0.7,
    };
    let (dir, query) = match rest.as_slice() {
        [dir, q @ ..] if !q.is_empty() => (dir.clone(), q.join(" ")),
        _ => {
            eprintln!(
                "error: usage `ckos search [--synonyms] [--expand] [--diverse] [--lambda N] <dir> <query…>`"
            );
            return ExitCode::FAILURE;
        }
    };
    // --synonyms rewrites the query up front, before any other refinement, so
    // it composes with --expand/--diverse rather than competing with them.
    let query = if synonyms {
        expand_query_with_synonyms(&query, &SynonymTable::builtin(), 10)
    } else {
        query
    };
    let store = match FileStore::open(&dir) {
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
    let graph = match GraphStore::load(Path::new(&dir).join(GRAPH_FILE)) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("warning: could not load graph ({e}); searching documents only");
            KnowledgeGraph::default()
        }
    };
    let embedder = HashingEmbedder::default();
    let retriever = Retriever::with_embedder(&store, &graph, &embedder);
    // Compose the requested retrieval refinements (§949): pseudo-relevance
    // expansion to widen recall, MMR to diversify.
    let hits = match (expand, diverse) {
        (true, true) => {
            let pool = retriever.search_expanded(&query, 40, 5, 5);
            mmr_rerank(&pool, lambda, 10)
        }
        (true, false) => retriever.search_expanded(&query, 10, 5, 5),
        (false, true) => retriever.search_diverse(&query, 10, lambda),
        (false, false) => retriever.search(&query, 10),
    };
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

fn cmd_eval(rest: &[String]) -> ExitCode {
    if wants_help(rest) {
        println!("usage: ckos eval --relevant <title1,title2,…> [--k N] <dir> <query…>\n  Run search and score it (Precision@k, Recall@k, MRR, nDCG@k) against the\n  comma-separated titles you consider relevant.");
        return ExitCode::SUCCESS;
    }
    let (relevant_csv, rest) = match take_value_flag(rest, "--relevant") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (k_str, rest) = match take_value_flag(&rest, "--k") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(relevant_csv) = relevant_csv else {
        eprintln!("error: `eval` needs --relevant <title1,title2,…>");
        return ExitCode::FAILURE;
    };
    let k: usize = match k_str.as_deref().map(str::parse) {
        Some(Ok(0)) | Some(Err(_)) => {
            eprintln!("error: --k needs a positive integer");
            return ExitCode::FAILURE;
        }
        Some(Ok(n)) => n,
        None => 10,
    };
    let (dir, query) = match rest.as_slice() {
        [dir, q @ ..] if !q.is_empty() => (dir.clone(), q.join(" ")),
        _ => {
            eprintln!("error: usage `ckos eval --relevant <csv> [--k N] <dir> <query…>`");
            return ExitCode::FAILURE;
        }
    };
    let relevant: std::collections::HashSet<String> = relevant_csv
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    if relevant.is_empty() {
        eprintln!("error: --relevant listed no titles");
        return ExitCode::FAILURE;
    }

    let store = match FileStore::open(&dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: could not open session {dir}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let graph = GraphStore::load(Path::new(&dir).join(GRAPH_FILE)).unwrap_or_default();
    let embedder = HashingEmbedder::default();
    let retriever = Retriever::with_embedder(&store, &graph, &embedder);
    let hits = retriever.search(&query, k.max(1));
    let scores = evaluate_hits(&hits, &relevant, k);

    println!("eval for {query:?} (k={}):", scores.k);
    println!("  precision@{:<2} {:.3}", scores.k, scores.precision);
    println!("  recall@{:<5} {:.3}", scores.k, scores.recall);
    println!("  MRR         {:.3}", scores.reciprocal_rank);
    println!("  nDCG@{:<6} {:.3}", scores.k, scores.ndcg);
    ExitCode::SUCCESS
}

fn cmd_workflow(rest: &[String]) -> ExitCode {
    if wants_help(rest) {
        println!("usage: ckos workflow [--role <role> | --token <token>] <file>\n  Load and execute a workflow definition file. --role/--token attach RBAC+ABAC\n  authorization (§929) for finance/medical/legal/robotics steps — the only\n  reachable way to author such a step today, since the heuristic planner\n  behind `ckos plan`/`ckos run` never classifies free text into them. --token\n  authenticates via a demo identity provider (§928: tok-admin-hq,\n  tok-admin-restricted, tok-guest), carrying real ABAC attributes.");
        return ExitCode::SUCCESS;
    }
    let (role, rest) = match take_value_flag(rest, "--role") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (token, rest) = match take_value_flag(&rest, "--token") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let identity = match resolve_identity(role, token, None) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
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
    let mut engine = Engine::new(
        runtimes,
        agents,
        Verifier::new().with_check(Box::new(NonEmptyCheck)),
    );
    if let Some(identity) = identity {
        engine = engine.with_identity(demo_policy(), identity);
    }

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
    if wants_help(rest) {
        println!("usage: ckos gc <dir> [--min-confidence N] [--now YYYY-MM-DD] [--consolidate N]\n  Garbage-collect a session (§954): removes low-value documents AND sweeps\n  orphaned knowledge-graph nodes from the session's persisted graph.\n  --now enables expiry: documents whose `expires` metadata is <= the given\n  ISO date are collected (without --now, expiry is skipped).\n  --consolidate N runs the §953 sleep-phase pass first: any document body\n  over N characters is compressed (summary + keywords) and written back,\n  before garbage collection runs over the (now smaller) store.");
        return ExitCode::SUCCESS;
    }
    let (now, rest) = match take_value_flag(rest, "--now") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (min_conf, rest) = match take_value_flag(&rest, "--min-confidence") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (consolidate_max_chars, rest) = match take_value_flag(&rest, "--consolidate") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let min_confidence: u8 = match min_conf {
        Some(v) => match v.parse() {
            Ok(n) => n,
            Err(_) => {
                eprintln!("error: --min-confidence needs a number 0..=255");
                return ExitCode::FAILURE;
            }
        },
        None => 0,
    };
    let consolidate_max_chars: Option<usize> = match consolidate_max_chars {
        Some(v) => match v.parse() {
            Ok(n) => Some(n),
            Err(_) => {
                eprintln!("error: --consolidate needs a character count, e.g. --consolidate 2000");
                return ExitCode::FAILURE;
            }
        },
        None => None,
    };
    let Some(dir) = rest.first() else {
        eprintln!("error: `gc` needs a session directory, e.g. `ckos gc ./my-session`");
        return ExitCode::FAILURE;
    };

    let mut store = match FileStore::open(dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: could not open session {dir}: {e}");
            return ExitCode::FAILURE;
        }
    };

    // §953 sleep-phase pass runs first, so GC below sees the (now smaller)
    // consolidated bodies rather than acting on stale, oversized ones.
    if let Some(max_chars) = consolidate_max_chars {
        match consolidate(&mut store, max_chars) {
            Ok(n) => println!("consolidated {n} document(s) over {max_chars} chars"),
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        }
    }

    let policy = GcPolicy {
        min_confidence,
        ..GcPolicy::default()
    };
    match gc_collect(&mut store, &policy, now.as_deref()) {
        Ok(report) => {
            println!(
                "garbage-collected {} document(s) from {dir}",
                report.count()
            );
            for (id, reason) in &report.removed {
                println!("  - {id} ({reason:?})");
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    }

    // The graph half of §954: sweep orphaned nodes from the session's
    // persisted knowledge graph (nodes with no edges in either direction).
    let graph_path = Path::new(dir.as_str()).join(GRAPH_FILE);
    if graph_path.exists() {
        let mut graph = match GraphStore::load(&graph_path) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("error: could not load {}: {e}", graph_path.display());
                return ExitCode::FAILURE;
            }
        };
        let swept = graph.remove_orphans();
        if swept > 0 {
            if let Err(e) = GraphStore::save(&graph_path, &graph) {
                eprintln!("error: could not save {}: {e}", graph_path.display());
                return ExitCode::FAILURE;
            }
        }
        println!("swept {swept} orphaned graph node(s)");
    }
    ExitCode::SUCCESS
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

/// A demo tool that requires a permission, so `ckos tool` exercises the
/// least-privilege gate (§919) rather than only ever calling a permissionless
/// tool. Kept local to the CLI — a real deployment registers its own tools.
struct ReverseTool;

impl Tool for ReverseTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: "reverse".into(),
            description: "reverses its input (requires text.transform)".into(),
            permissions: vec!["text.transform".into()],
        }
    }
    fn execute(&self, input: &str) -> Result<String> {
        Ok(input.chars().rev().collect())
    }
}

/// The demo tool registry `ckos tool` operates on.
fn demo_tools() -> ToolRegistry {
    let mut reg = ToolRegistry::new();
    reg.register(Box::new(UppercaseTool));
    reg.register(Box::new(ReverseTool));
    reg
}

/// The demo RBAC+ABAC policy `ckos tool`/`ckos run`/`ckos workflow` authorize
/// against (§929). Two built-in roles: `admin` (granted `text.*` for tools
/// and `capability.*` for the Engine's sensitive capabilities) and `guest`
/// (nothing — PolicyEngine defaults to deny). One ABAC rule demonstrates
/// attribute-based override: `capability.medical` is explicitly denied when
/// the requester's `region` attribute is `restricted`, regardless of the
/// `admin` RBAC grant (deny always wins, §929) — only reachable when the
/// identity actually carries that attribute, i.e. via `--token`
/// (see [`demo_identity_provider`]), since `--role` carries no attributes.
/// A real deployment would load roles/rules from its own policy store.
fn demo_policy() -> PolicyEngine {
    let mut p = PolicyEngine::new();
    p.grant("admin", "text.*");
    p.grant("admin", "capability.*"); // sensitive Engine capabilities (§929)
    p.add_rule(AbacRule {
        action: "capability.medical".into(),
        attribute_key: "region".into(),
        attribute_value: "restricted".into(),
        deny: true,
    });
    p
}

/// The demo identity provider (§928) `--token` authenticates against — an
/// in-memory stand-in for a real OIDC/LDAP directory. `tok-admin-restricted`
/// carries the same `admin` role as `tok-admin-hq` but adds a `region`
/// attribute the demo policy's ABAC rule denies `capability.medical` for
/// (see [`demo_policy`]), so the two tokens authorize differently even
/// though their RBAC role is identical — proving `--token` carries real
/// attributes that `--role` cannot.
fn demo_identity_provider() -> StaticTokenProvider {
    let mut p = StaticTokenProvider::new();
    p.add_token("tok-admin-hq", Identity::new("admin-hq").with_role("admin"));
    p.add_token(
        "tok-admin-restricted",
        Identity::new("admin-restricted")
            .with_role("admin")
            .with_attribute("region", "restricted"),
    );
    p.add_token("tok-guest", Identity::new("guest-user").with_role("guest"));
    p
}

/// Resolve `--role`/`--token` into an [`Identity`] for §929 authorization.
/// `--token` authenticates through [`demo_identity_provider`] (§928),
/// producing an identity with real ABAC attributes; `--role` is a bare-roles
/// convenience carrying no attributes. The two flags are mutually exclusive.
/// `default_role` applies when neither is given — callers that always
/// authorize (`ckos tool`) pass `Some("guest")`; callers where authorization
/// is opt-in (`ckos run`/`ckos workflow`) pass `None` and skip attaching a
/// policy at all when the result is `None`.
fn resolve_identity(
    role: Option<String>,
    token: Option<String>,
    default_role: Option<&str>,
) -> std::result::Result<Option<Identity>, String> {
    match (role, token) {
        (Some(_), Some(_)) => Err("--role and --token are mutually exclusive".into()),
        (Some(role), None) => Ok(Some(Identity::new("cli-user").with_role(role))),
        (None, Some(token)) => demo_identity_provider()
            .authenticate(&token)
            .map(Some)
            .map_err(|e| format!("token authentication failed: {e}")),
        (None, None) => Ok(default_role.map(|r| Identity::new("cli-user").with_role(r))),
    }
}

fn cmd_tool(rest: &[String]) -> ExitCode {
    if wants_help(rest) || rest.is_empty() {
        println!("usage: ckos tool --list | ckos tool [--role <role> | --token <token>] <name> <input…>\n  Invoke a registered tool (§917/§918). Each permission the tool requires is\n  authorized against a role+attribute policy (§929, PolicyEngine) — not\n  self-granted — before the tool's own least-privilege gate runs (§919).\n  --role is bare roles (admin, guest; default guest), no attributes. --token\n  authenticates via a demo identity provider (§928: tok-admin-hq,\n  tok-admin-restricted, tok-guest), carrying real ABAC attributes.");
        return ExitCode::SUCCESS;
    }
    if rest[0] == "--list" {
        println!("built-in tools (§917):");
        let reg = demo_tools();
        for name in reg.names() {
            println!("  - {name}");
        }
        println!("(authorize with --role <admin|guest> or --token <tok-…>, default guest)");
        return ExitCode::SUCCESS;
    }
    let (role, rest) = match take_value_flag(rest, "--role") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (token, rest) = match take_value_flag(&rest, "--token") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let identity = match resolve_identity(role, token, Some("guest")) {
        Ok(Some(v)) => v,
        Ok(None) => unreachable!("default_role always yields Some"),
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (name, input) = match rest.as_slice() {
        [name, i @ ..] if !i.is_empty() => (name.clone(), i.join(" ")),
        _ => {
            eprintln!(
                "error: usage `ckos tool [--role <role>] <name> <input…>` (see `ckos tool --list`)"
            );
            return ExitCode::FAILURE;
        }
    };

    let tools = demo_tools();
    let Some(required) = tools.metadata(&name).map(|m| m.permissions) else {
        eprintln!("error: unknown tool {name} (see `ckos tool --list`)");
        return ExitCode::FAILURE;
    };

    // Every tool run — allowed or denied — leaves an audit record (§903); the
    // trail is printed on exit so it is actually observable, not write-only.
    let audit = InMemoryAuditLog::new();

    // Authorize each required permission against the RBAC+ABAC policy
    // (§929) — the tool registry never trusts a self-asserted grant. Built
    // from the resolved identity, so a --token grant carries real attributes
    // an ABAC rule can key off, not just bare roles.
    let policy = demo_policy();
    let mut reg = tools;
    for perm in &required {
        let req = identity.request(perm.clone());
        match policy.evaluate(&req) {
            Ok(()) => reg.grant(perm.clone()),
            Err(e) => {
                audit.record(
                    AuditRecord::new("tool.invoke")
                        .tool(&name)
                        .input(&input)
                        .error(format!("denied for {}: {e}", identity.subject)),
                );
                print_audit(&audit);
                eprintln!(
                    "error: {} (roles {:?}) may not use {name}: {e}",
                    identity.subject, identity.roles
                );
                return ExitCode::FAILURE;
            }
        }
    }

    match reg.invoke(&name, &input) {
        Ok(output) => {
            audit.record(
                AuditRecord::new("tool.invoke")
                    .tool(&name)
                    .input(&input)
                    .output(&output),
            );
            print_audit(&audit);
            println!("{output}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            audit.record(
                AuditRecord::new("tool.invoke")
                    .tool(&name)
                    .input(&input)
                    .error(e.to_string()),
            );
            print_audit(&audit);
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Print an audit trail (§903) to stderr in a compact single-line-per-record
/// form: hashes rather than payloads, so the trail is verifiable without
/// leaking content — mirroring what a file/SIEM sink would receive.
fn print_audit(log: &InMemoryAuditLog) {
    for r in log.snapshot() {
        let subject = r.tool.or(r.plugin).or(r.runtime).unwrap_or_default();
        let outcome = match &r.error {
            Some(e) => format!("error: {e}"),
            None => "ok".to_string(),
        };
        eprintln!(
            "audit: {} {} in#{:016x} out#{:016x} @{} — {}",
            r.action, subject, r.input_hash, r.output_hash, r.timestamp_ms, outcome
        );
    }
}

fn cmd_capabilities() -> ExitCode {
    println!("built-in capabilities (§911):");
    for c in Capability::builtin() {
        println!("  - {c}");
    }
    ExitCode::SUCCESS
}
