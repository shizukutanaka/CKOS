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
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

const GRAPH_FILE: &str = "graph.kg";

/// Per-session query cache capacity (§958). Modest: this bounds memory the
/// same way `InMemoryAuditLog`/`InMemoryTelemetry` bound their retention —
/// a demo/local-operator gateway doesn't need thousands of distinct cached
/// queries per session.
const CACHE_CAPACITY_PER_SESSION: usize = 32;

/// State shared across every connection for the server's lifetime (built
/// once in [`crate::serve`], reused per request — see that module's doc for
/// why this is safe without extra locking around `Engine` itself).
///
/// Before this existed, every `/api/run` request built a brand-new `Engine`,
/// so its audit trail (§903) and telemetry (§904) were discarded the instant
/// the response was written — a long-lived `ckos serve` process had no way
/// to observe its own cumulative activity. And `/api/search` never cached
/// anything (§958's `SearchCache` had no caller anywhere), so identical
/// repeat queries against the same session always re-ran the full retrieval
/// pipeline — pointless for a process that, unlike the one-shot CLI, is
/// expected to serve the same session repeatedly.
pub struct AppState {
    engine: Engine,
    /// One LRU cache per session directory, keyed by session path.
    caches: Mutex<HashMap<String, SearchCache>>,
    /// A mutation counter per session directory, bumped whenever `run`
    /// invalidates that session's cache (see [`Self::invalidate_cache`]).
    /// `search` captures the generation before it starts computing and only
    /// writes its result into the cache if the generation is still the same
    /// when it finishes — closing a TOCTOU race where `run` invalidates and
    /// mutates a session *while* a concurrent `search` is already past its
    /// own cache-miss check, which would otherwise let that search's
    /// now-stale result resurrect the entry `run` had just invalidated.
    generations: Mutex<HashMap<String, u64>>,
}

impl AppState {
    /// Build the shared engine (the same demo capability/runtime/verifier
    /// pool every request used to rebuild from scratch) and an empty cache
    /// table.
    pub fn new() -> Self {
        let mut runtimes = RuntimeRegistry::new();
        let mut agents = CapabilityRegistry::new();
        for cap in Capability::builtin() {
            runtimes.register(Box::new(EchoRuntime::new(vec![cap.clone()])));
            agents.register(AgentManifest::new(format!("{cap}-agent"), cap));
        }
        let verifier = Verifier::new()
            .with_check(Box::new(NonEmptyCheck))
            .with_check(Box::new(CitationCheck));
        AppState {
            engine: Engine::new(runtimes, agents, verifier),
            caches: Mutex::new(HashMap::new()),
            generations: Mutex::new(HashMap::new()),
        }
    }

    /// Lock the cache table, recovering from poisoning — one panicking
    /// request must not turn every later cache lookup/insert into a panic
    /// too (the same rule applied to `InMemoryAuditLog`/`InMemoryTelemetry`
    /// in `ckos_kernel`).
    fn caches(&self) -> std::sync::MutexGuard<'_, HashMap<String, SearchCache>> {
        self.caches.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn generations(&self) -> std::sync::MutexGuard<'_, HashMap<String, u64>> {
        self.generations.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Current mutation generation for a session directory (0 if it has
    /// never been invalidated).
    fn generation(&self, dir: &str) -> u64 {
        self.generations().get(dir).copied().unwrap_or(0)
    }

    /// Evict a session's cached searches and bump its generation, so any
    /// search already past its own cache-miss check for this session knows,
    /// when it later checks, not to write its (possibly now stale) result
    /// back in (see the field doc on `generations`).
    fn invalidate_cache(&self, dir: &str) {
        self.caches().remove(dir);
        *self.generations().entry(dir.to_string()).or_insert(0) += 1;
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

/// Dispatch a parsed request to the matching handler, or a JSON 404.
pub fn route(state: &AppState, req: &Request) -> Response {
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") => Response::html(crate::dashboard::PAGE),
        ("GET", "/api/capabilities") => capabilities(),
        ("GET", "/api/runtimes") => runtimes(),
        ("GET", "/api/status") => status(state),
        ("GET", "/api/plan") => plan(req),
        ("POST", "/api/run") => run(state, req),
        ("GET", "/api/history") => history(req),
        ("GET", "/api/search") => search(state, req),
        ("POST", "/api/kql") => kql(req),
        ("GET", "/api/graph") => graph(req),
        ("GET", "/api/verify") => verify(req),
        _ => Response::not_found(),
    }
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

/// Cumulative server-lifetime activity: the shared engine's audit log and
/// telemetry (§903/§904), plus how many sessions currently have a warm
/// search cache (§958) and how many queries are cached in total. This is
/// the "Runtime Monitor" groundwork the v2.8 roadmap calls for — a
/// long-lived `ckos serve` process finally has something to observe beyond
/// a single request/response.
fn status(state: &AppState) -> Response {
    let (cached_sessions, cached_queries) = {
        let caches = state.caches();
        (
            caches.len(),
            caches.values().map(SearchCache::len).sum::<usize>(),
        )
    };
    Response::json(Json::object([
        ("audit_records", state.engine.audit().len().into()),
        ("audit_errors", state.engine.audit().error_count().into()),
        (
            "total_tokens",
            state.engine.telemetry().total_tokens().into(),
        ),
        (
            "mean_latency_ms",
            state
                .engine
                .telemetry()
                .mean_latency_ms()
                .unwrap_or(0.0)
                .into(),
        ),
        ("cached_sessions", cached_sessions.into()),
        ("cached_queries", cached_queries.into()),
    ]))
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

fn run(state: &AppState, req: &Request) -> Response {
    let intent = req.param_or_empty("intent").trim();
    if intent.is_empty() {
        return Response::bad_request("missing `intent` parameter");
    }
    let session_dir = req.param("session").filter(|s| !s.is_empty());

    let dag = HeuristicPlanner::new().plan(intent);
    // The shared, server-lifetime engine (see AppState's doc) — its audit
    // log and telemetry accumulate across every /api/run call, not just
    // this one; `GET /api/status` reports the running totals.
    let engine = &state.engine;
    let results = match engine.run_workflow(&dag) {
        Ok(r) => r,
        Err(e) => {
            return Response::json_status(500, Json::object([("error", e.to_string().into())]))
        }
    };

    let mut warnings: Vec<Json> = Vec::new();
    if let Some(dir) = session_dir {
        // This run is about to add documents and/or graph nodes to `dir`;
        // any cached search results for it are now stale (§958 cache —
        // see `search`'s cache_key doc). Evict before mutating, not after,
        // so a search racing this request never observes a half-updated
        // cache entry; bumping the generation here (not just evicting) also
        // stops a search that started even earlier from writing a stale
        // result back in after this point (see `AppState::generations`).
        state.invalidate_cache(dir);
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
        // Cumulative since the server started (the engine is shared across
        // every /api/run call — see AppState), not just this request; the
        // key names spell that out so a caller doesn't mistake it for a
        // per-call count. GET /api/status reports the same totals without
        // needing to run anything.
        ("server_audit_records", engine.audit().len().into()),
        ("server_audit_errors", engine.audit().error_count().into()),
        (
            "server_total_tokens",
            engine.telemetry().total_tokens().into(),
        ),
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

fn search(state: &AppState, req: &Request) -> Response {
    let Some(dir) = req.param("session").filter(|s| !s.is_empty()) else {
        return Response::bad_request("missing `session` parameter");
    };
    let raw_query = req.param_or_empty("q").trim();
    if raw_query.is_empty() {
        return Response::bad_request("missing `q` parameter");
    }
    let synonyms = req.flag("synonyms");
    let expand = req.flag("expand");
    let diverse = req.flag("diverse");
    let k: usize = req.param("k").and_then(|v| v.parse().ok()).unwrap_or(10);
    let lambda: f32 = req
        .param("lambda")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.7);
    // Every parameter that can change the result set feeds the cache key
    // (using \u{1}, which can't appear in a query typed into the dashboard,
    // as an unambiguous separator), so two requests differing only in a flag
    // never share a cached answer. Invalidated by `run` — see its call to
    // `invalidate_cache` — whenever that session's documents/graph change.
    let cache_key =
        format!("{synonyms}\u{1}{expand}\u{1}{diverse}\u{1}{k}\u{1}{lambda}\u{1}{raw_query}");

    // Captured before the cache-miss check so it covers the entire window
    // this computation takes — see `AppState::generations`.
    let gen_before = state.generation(dir);
    if let Some(hits) = state.caches().get_mut(dir).and_then(|c| c.get(&cache_key)) {
        return search_response(&hits, 0, true);
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
    let query = if synonyms {
        expand_query_with_synonyms(raw_query, &SynonymTable::builtin(), 4)
    } else {
        raw_query.to_string()
    };
    let retriever = Retriever::with_embedder(&store, &graph, &embedder);
    let mut hits = if expand {
        retriever.search_expanded(&query, k, 3, 4)
    } else {
        retriever.search(&query, k)
    };
    if diverse {
        hits = mmr_rerank(&hits, lambda, k);
    }

    // Test-only fault injection (compiled out of the real binary entirely):
    // lets a test hold this search open across a concurrent `run` so the
    // invalidation race below is deterministic instead of timing-dependent.
    #[cfg(test)]
    if req.flag("test_stall_before_cache_write") {
        std::thread::sleep(std::time::Duration::from_millis(300));
    }

    // Only cache if nothing invalidated this session while we were computing
    // — otherwise this result may already be stale, and writing it now would
    // resurrect exactly what `run`'s invalidation was meant to discard.
    if state.generation(dir) == gen_before {
        state
            .caches()
            .entry(dir.to_string())
            .or_insert_with(|| SearchCache::new(CACHE_CAPACITY_PER_SESSION))
            .put(cache_key, hits.clone());
    }

    search_response(&hits, skipped, false)
}

fn search_response(hits: &[Hit], skipped: usize, cached: bool) -> Response {
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
        ("cached", cached.into()),
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
