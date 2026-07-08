//! Route handlers bridging HTTP requests to the SDK (§902 API gateway).
//!
//! Every handler is read-mostly or additive (`run` persists new records,
//! never deletes). Destructive maintenance (`gc`, retry-budget resets) stays
//! CLI-only in this first pass — a one-click web button for a destructive
//! action needs a confirmation flow this gateway doesn't have yet.
//!
//! A request without a `session` parameter runs against transient in-memory
//! state (mirroring `ckos run`/`ckos graph` with no `--session`): nothing
//! persists, so the dashboard's "try it" panels always work with zero setup.

use crate::http::{Request, Response};
use crate::json::Json;
use ckos_sdk::prelude::*;
use std::path::Path;

const GRAPH_FILE: &str = "graph.kg";

/// Dispatch a parsed request to the matching handler, or a JSON 404.
pub fn route(req: &Request) -> Response {
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") => Response::html(crate::dashboard::PAGE),
        ("GET", "/api/capabilities") => capabilities(),
        ("GET", "/api/runtimes") => runtimes(),
        ("GET", "/api/plan") => plan(req),
        ("POST", "/api/run") => run(req),
        ("GET", "/api/history") => history(req),
        ("GET", "/api/search") => search(req),
        ("POST", "/api/kql") => kql(req),
        ("GET", "/api/graph") => graph(req),
        ("GET", "/api/verify") => verify(req),
        _ => Response::not_found(),
    }
}

/// The capability + runtime pool every handler assembles a fresh `Engine`
/// from — mirrors `cli::demo_capabilities`/`demo_tools`: a fully offline echo
/// runtime and a demo agent per built-in capability (§911).
fn demo_engine() -> Engine {
    let mut runtimes = RuntimeRegistry::new();
    let mut agents = CapabilityRegistry::new();
    for cap in Capability::builtin() {
        runtimes.register(Box::new(EchoRuntime::new(vec![cap.clone()])));
        agents.register(AgentManifest::new(format!("{cap}-agent"), cap));
    }
    let verifier = Verifier::new()
        .with_check(Box::new(NonEmptyCheck))
        .with_check(Box::new(CitationCheck));
    Engine::new(runtimes, agents, verifier)
}

fn capabilities() -> Response {
    let caps: Vec<Json> = Capability::builtin()
        .into_iter()
        .map(|c| c.to_string().into())
        .collect();
    Response::json(Json::object([("capabilities", Json::Array(caps))]))
}

fn runtimes() -> Response {
    let mut registry = RuntimeRegistry::new();
    for cap in Capability::builtin() {
        registry.register(Box::new(EchoRuntime::new(vec![cap])));
    }
    let infos: Vec<Json> = registry
        .list()
        .into_iter()
        .map(|info| {
            let caps: Vec<Json> = info
                .capabilities
                .iter()
                .map(|c| c.to_string().into())
                .collect();
            Json::object([
                ("name", info.name.into()),
                ("kind", format!("{:?}", info.kind).into()),
                ("capabilities", Json::Array(caps)),
            ])
        })
        .collect();
    Response::json(Json::object([("runtimes", Json::Array(infos))]))
}

fn plan(req: &Request) -> Response {
    let intent = req.param_or_empty("intent").trim();
    if intent.is_empty() {
        return Response::bad_request("missing `intent` parameter");
    }
    let dag = HeuristicPlanner::new().plan(intent);
    let Some(order) = dag.topological_order() else {
        return Response::json_status(
            500,
            Json::object([("error", "workflow contains a cycle".into())]),
        );
    };
    let steps: Vec<Json> = order
        .into_iter()
        .filter_map(|s| dag.task(s))
        .map(|t| {
            Json::object([
                ("capability", t.capability.to_string().into()),
                ("description", t.description.clone().into()),
            ])
        })
        .collect();
    Response::json(Json::object([
        ("intent", intent.into()),
        ("workflow", dag.name().into()),
        ("steps", Json::Array(steps)),
    ]))
}

fn run(req: &Request) -> Response {
    let intent = req.param_or_empty("intent").trim();
    if intent.is_empty() {
        return Response::bad_request("missing `intent` parameter");
    }
    let session_dir = req.param("session").filter(|s| !s.is_empty());

    let dag = HeuristicPlanner::new().plan(intent);
    let engine = demo_engine();
    let results = match engine.run_workflow(&dag) {
        Ok(r) => r,
        Err(e) => {
            return Response::json_status(500, Json::object([("error", e.to_string().into())]))
        }
    };

    let mut warnings: Vec<Json> = Vec::new();
    if let Some(dir) = session_dir {
        match FileStore::open(dir) {
            Ok(store) => {
                if store.skipped() > 0 {
                    warnings.push(
                        format!(
                            "{} unreadable document(s) skipped in this session",
                            store.skipped()
                        )
                        .into(),
                    );
                }
                let mut session = Session::new("web", Box::new(store))
                    .with_embedder(Box::new(HashingEmbedder::default()));
                let reflections = engine.reflect(&HeuristicReflector::new(), &results);
                if let Err(e) = session
                    .record_run(&results)
                    .and_then(|_| session.record_reflections(&reflections))
                {
                    warnings.push(format!("failed to persist session: {e}").into());
                }

                let graph_path = Path::new(dir).join(GRAPH_FILE);
                match GraphStore::load(&graph_path) {
                    Ok(mut graph) => {
                        graph.extract_concepts_with_provenance(intent, Some("run:intent"));
                        for r in &results {
                            graph.extract_concepts_with_provenance(&r.output, Some("run:output"));
                        }
                        if let Err(e) = GraphStore::save(&graph_path, &graph) {
                            warnings.push(format!("failed to save graph: {e}").into());
                        }
                    }
                    Err(e) => warnings.push(format!("failed to load graph: {e}").into()),
                }
            }
            Err(e) => warnings.push(format!("could not open session {dir}: {e}").into()),
        }
    }

    let results_json: Vec<Json> = results
        .iter()
        .map(|r| {
            Json::object([
                ("capability", r.capability.to_string().into()),
                ("agent", r.agent.clone().into()),
                ("runtime", r.runtime.clone().into()),
                ("output", r.output.clone().into()),
                ("verified", r.verified.into()),
                ("state", format!("{:?}", r.state).into()),
            ])
        })
        .collect();

    Response::json(Json::object([
        ("intent", intent.into()),
        ("results", Json::Array(results_json)),
        ("audit_records", engine.audit().len().into()),
        ("audit_errors", engine.audit().error_count().into()),
        ("total_tokens", engine.telemetry().total_tokens().into()),
        ("warnings", Json::Array(warnings)),
    ]))
}

fn open_session(dir: &str) -> std::result::Result<FileStore, Response> {
    FileStore::open(dir).map_err(|e| {
        Response::json_status(
            400,
            Json::object([("error", format!("could not open session {dir}: {e}").into())]),
        )
    })
}

fn history(req: &Request) -> Response {
    let Some(dir) = req.param("session").filter(|s| !s.is_empty()) else {
        return Response::bad_request("missing `session` parameter");
    };
    let store = match open_session(dir) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    let skipped = store.skipped();
    let query = req.param_or_empty("q").trim();
    let k: usize = req.param("k").and_then(|v| v.parse().ok()).unwrap_or(5);

    let docs = if query.is_empty() {
        let session = Session::new("web", Box::new(store));
        session.history()
    } else {
        let session = Session::new("web", Box::new(store))
            .with_embedder(Box::new(HashingEmbedder::default()));
        session.recall(query, k)
    };
    let docs = match docs {
        Ok(d) => d,
        Err(e) => {
            return Response::json_status(500, Json::object([("error", e.to_string().into())]))
        }
    };

    let items: Vec<Json> = docs
        .iter()
        .map(|d| {
            Json::object([
                ("title", d.title.clone().into()),
                ("body", d.body.clone().into()),
                ("confidence", d.confidence.into()),
                (
                    "verified",
                    (d.metadata.get("verified").map(String::as_str) == Some("true")).into(),
                ),
            ])
        })
        .collect();
    Response::json(Json::object([
        ("recalled", (!query.is_empty()).into()),
        ("skipped", skipped.into()),
        ("items", Json::Array(items)),
    ]))
}

fn search(req: &Request) -> Response {
    let Some(dir) = req.param("session").filter(|s| !s.is_empty()) else {
        return Response::bad_request("missing `session` parameter");
    };
    let query = req.param_or_empty("q").trim();
    if query.is_empty() {
        return Response::bad_request("missing `q` parameter");
    }
    let store = match open_session(dir) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    let skipped = store.skipped();
    let graph = match GraphStore::load(Path::new(dir).join(GRAPH_FILE)) {
        Ok(g) => g,
        Err(e) => {
            return Response::json_status(500, Json::object([("error", e.to_string().into())]))
        }
    };
    let embedder = HashingEmbedder::default();
    let query = if req.flag("synonyms") {
        expand_query_with_synonyms(query, &SynonymTable::builtin(), 4)
    } else {
        query.to_string()
    };
    let retriever = Retriever::with_embedder(&store, &graph, &embedder);
    let k = req.param("k").and_then(|v| v.parse().ok()).unwrap_or(10);
    let mut hits = if req.flag("expand") {
        retriever.search_expanded(&query, k, 3, 4)
    } else {
        retriever.search(&query, k)
    };
    if req.flag("diverse") {
        let lambda: f32 = req
            .param("lambda")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.7);
        hits = mmr_rerank(&hits, lambda, k);
    }

    let items: Vec<Json> = hits
        .iter()
        .map(|h| {
            Json::object([
                ("title", h.title.clone().into()),
                ("snippet", h.snippet.clone().into()),
                ("score", h.score.into()),
                ("source", format!("{:?}", h.source).into()),
            ])
        })
        .collect();
    Response::json(Json::object([
        ("skipped", skipped.into()),
        ("hits", Json::Array(items)),
    ]))
}

/// A small demo graph (§897) with temporal dates (§946) and provenance
/// (§947), queried when no `session` is given — mirrors `ckos kql`'s
/// built-in demo graph, so the dashboard's KQL panel works with zero setup.
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

fn kql(req: &Request) -> Response {
    let query_text = req.param_or_empty("query").trim();
    if query_text.is_empty() {
        return Response::bad_request("missing `query` parameter");
    }
    let query = match kql_parse(query_text) {
        Ok(q) => q,
        Err(e) => return Response::bad_request(&e.to_string()),
    };
    let graph = match req.param("session").filter(|s| !s.is_empty()) {
        Some(dir) => match GraphStore::load(Path::new(dir).join(GRAPH_FILE)) {
            Ok(g) => g,
            Err(e) => {
                return Response::json_status(500, Json::object([("error", e.to_string().into())]))
            }
        },
        None => demo_graph(),
    };
    let result = kql_execute(&query, &graph);
    let to_json = |ms: &[NodeMatch]| -> Json {
        Json::Array(
            ms.iter()
                .map(|m| {
                    Json::object([
                        ("label", m.label.clone().into()),
                        ("kind", m.kind.clone().into()),
                        ("confidence", m.confidence.into()),
                        ("date", m.date.clone().into()),
                        ("provenance", m.provenance.clone().into()),
                    ])
                })
                .collect(),
        )
    };
    Response::json(Json::object([
        ("primary", to_json(&result.primary)),
        ("related", to_json(&result.related)),
    ]))
}

fn graph(req: &Request) -> Response {
    let g = match req.param("session").filter(|s| !s.is_empty()) {
        Some(dir) => match GraphStore::load(Path::new(dir).join(GRAPH_FILE)) {
            Ok(g) => g,
            Err(e) => {
                return Response::json_status(500, Json::object([("error", e.to_string().into())]))
            }
        },
        None => {
            let text = req.param_or_empty("text").trim();
            if text.is_empty() {
                return Response::bad_request("provide `session` or `text`");
            }
            let mut g = KnowledgeGraph::new();
            g.extract_concepts(text);
            g
        }
    };
    let nodes: Vec<Json> = g
        .nodes()
        .map(|n| {
            Json::object([
                ("id", n.id.as_str().into()),
                ("kind", n.kind.as_token().into()),
                ("label", n.label.clone().into()),
                ("confidence", n.confidence.into()),
            ])
        })
        .collect();
    let edges: Vec<Json> = g
        .edges()
        .map(|e| {
            Json::object([
                ("from", e.from.as_str().into()),
                ("to", e.to.as_str().into()),
                ("kind", e.kind.as_token().into()),
            ])
        })
        .collect();
    Response::json(Json::object([
        ("nodes", Json::Array(nodes)),
        ("edges", Json::Array(edges)),
    ]))
}

fn verify(req: &Request) -> Response {
    let text = req.param_or_empty("text");
    if text.trim().is_empty() {
        return Response::bad_request("missing `text` parameter");
    }
    let report = Verifier::builtin().verify(text);
    let checks: Vec<Json> = report
        .results
        .iter()
        .map(|(name, verdict)| {
            let (status, reason): (Json, Json) = match verdict {
                Verdict::Pass => ("pass".into(), Json::Null),
                Verdict::Skip => ("skip".into(), Json::Null),
                Verdict::Fail(why) => ("fail".into(), why.clone().into()),
            };
            Json::object([
                ("name", name.clone().into()),
                ("status", status),
                ("reason", reason),
            ])
        })
        .collect();
    Response::json(Json::object([
        ("passed", report.passed().into()),
        ("checks", Json::Array(checks)),
    ]))
}
