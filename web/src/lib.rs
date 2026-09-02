//! CKOS API gateway (§902): a `std`-only HTTP/JSON server and embedded
//! browser dashboard over [`ckos_sdk`], reached via `ckos serve`.
//!
//! **Scope, deliberately**: this is a single-operator, offline-first control
//! surface for a local CKOS installation — not an internet-facing service.
//! [`serve`] binds to whatever address it is given (the `ckos serve` CLI
//! command defaults to `127.0.0.1`, matching the "least privilege by
//! default" principle the rest of the workspace follows); exposing it beyond
//! localhost is the caller's explicit choice, made with `--host`. There is no
//! TLS, no request-rate limiting beyond the per-request size cap in
//! [`http`], and no authentication in front of the dashboard itself (the
//! `run`/`kql`/`search` handlers reuse the same demo, unauthenticated engine
//! pool as `ckos run` without `--role`/`--token`) — a production deployment
//! reachable by untrusted clients belongs behind a reverse proxy that adds
//! those.
//!
//! What that scope note does **not** excuse is filesystem reach. Sessions are
//! confined to a **session root** ([`serve_rooted`]; [`serve`] uses the
//! process's working directory), and a request's `session` parameter is
//! resolved beneath it or rejected with a 400. Before that confinement
//! existed the parameter was an unconstrained path handed to
//! `FileStore::open`, so one request could create directories and write
//! documents anywhere the process could reach — including from any page in
//! the operator's browser, since the form-encoded `POST /api/run` is a
//! CORS-simple request that needs no preflight and no readable response for
//! the write to land.

pub mod dashboard;
pub mod http;
pub mod json;
mod routes;

use json::Json;
use std::io;
use std::net::{TcpListener, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

/// Hard cap on connections handled concurrently. Without one, a connection
/// flood (a retry storm, a port scanner, even an eager browser) would spawn
/// an unbounded number of OS threads — the same "bounded input" discipline
/// already applied to request size ([`http`]) and in-memory retention
/// (`InMemoryAuditLog`/`InMemoryTelemetry` in `ckos_kernel`) extends to
/// concurrency here. A connection beyond the cap gets an immediate
/// `503 Service Unavailable` rather than an unbounded thread spawn or a
/// silent drop.
const MAX_CONCURRENT_CONNECTIONS: usize = 64;

/// Bind a listener at `addr` (e.g. `"127.0.0.1:8080"`, or port `0` to let the
/// OS pick a free port). Split from [`serve`] so callers — including tests —
/// can discover the actual bound port via [`TcpListener::local_addr`] before
/// entering the accept loop.
pub fn bind(addr: impl ToSocketAddrs) -> io::Result<TcpListener> {
    TcpListener::bind(addr)
}

/// Accept connections on `listener` forever, dispatching each to the
/// dashboard/API routes (§902) on its own thread, up to
/// `MAX_CONCURRENT_CONNECTIONS` at a time. Never returns under normal
/// operation; an individual connection's I/O error or a route handler panic
/// is contained to that connection (see [`http::handle_connection`]) and
/// does not stop the server.
///
/// Sessions are rooted at the process's current working directory. Use
/// [`serve_rooted`] to put them somewhere else.
pub fn serve(listener: TcpListener) -> ! {
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    serve_rooted(listener, root)
}

/// [`serve`], with an explicit session root: every request's `session`
/// parameter names a directory *beneath* `session_root` and cannot escape it
/// (see the crate doc). Nothing outside this directory is reachable through
/// the API, so pointing it at a dedicated sessions directory is strictly
/// safer than serving from a home directory.
pub fn serve_rooted(listener: TcpListener, session_root: impl Into<PathBuf>) -> ! {
    serve_bounded(listener, MAX_CONCURRENT_CONNECTIONS, session_root.into())
}

/// [`serve_rooted`], with the concurrency cap as a parameter — split out so
/// tests can exercise the cap deterministically at a small size instead of
/// opening 64 real connections.
fn serve_bounded(listener: TcpListener, max_concurrent: usize, session_root: PathBuf) -> ! {
    // Built once and shared for the server's whole lifetime — see
    // `routes::AppState`'s doc for why (the engine's audit/telemetry and the
    // per-session search cache need to persist across requests, not reset on
    // every connection).
    let state = Arc::new(routes::AppState::new(session_root));
    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        if active.load(Ordering::Acquire) >= max_concurrent {
            // A well-behaved client (or an eager browser) may have already
            // written its request before we could respond, and closing on
            // unread data RSTs our 503 away. `write_early_response` is the
            // one place that rule lives — it was duplicated here and missing
            // from `handle_connection`'s 413/400 paths, where a truncated
            // reply was actually measured. Its byte cap also fixes what this
            // copy got wrong: an idle timeout alone does not stop a peer that
            // keeps streaming, since every read succeeds.
            http::write_early_response(
                &mut stream,
                &http::Response::json_status(
                    503,
                    Json::object([("error", "server busy, try again".into())]),
                ),
            );
            continue;
        }
        active.fetch_add(1, Ordering::AcqRel);
        let active = Arc::clone(&active);
        let state = Arc::clone(&state);
        thread::spawn(move || {
            let handler = move |req: &http::Request| routes::route(&state, req);
            http::handle_connection(stream, &handler);
            active.fetch_sub(1, Ordering::AcqRel);
        });
    }
    unreachable!("TcpListener::incoming() only ends if the listener itself is dropped")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;

    /// A server whose session root is the process working directory — fine
    /// for the handlers that never touch a session.
    fn start_test_server() -> std::net::SocketAddr {
        let listener = bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        thread::spawn(move || serve(listener));
        addr
    }

    /// A server rooted at `root`, for the session-backed handlers.
    fn start_test_server_rooted(root: std::path::PathBuf) -> std::net::SocketAddr {
        let listener = bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        thread::spawn(move || serve_rooted(listener, root));
        addr
    }

    /// Monotonic counter for unique temp-directory names across tests
    /// (which `cargo test` may run concurrently in the same process).
    fn addr_seq() -> usize {
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        SEQ.fetch_add(1, Ordering::SeqCst)
    }

    struct TempDir(std::path::PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A unique, self-cleaning temp directory to use as a session root.
    fn temp_root(tag: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!(
            "ckos-web-{tag}-{}-{}",
            std::process::id(),
            addr_seq()
        ));
        std::fs::create_dir_all(&dir).expect("create temp root");
        TempDir(dir)
    }

    fn raw_request(addr: std::net::SocketAddr, request: &str) -> String {
        let mut stream = TcpStream::connect(addr).expect("connect");
        stream.write_all(request.as_bytes()).expect("write");
        let mut resp = String::new();
        stream.read_to_string(&mut resp).expect("read");
        resp
    }

    #[test]
    fn serves_the_dashboard_at_root() {
        let addr = start_test_server();
        let resp = raw_request(addr, "GET / HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(resp.starts_with("HTTP/1.1 200 OK"));
        assert!(resp.contains("text/html"));
        assert!(resp.contains("CKOS"));
    }

    #[test]
    fn serves_capabilities_as_json() {
        let addr = start_test_server();
        let resp = raw_request(addr, "GET /api/capabilities HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(resp.starts_with("HTTP/1.1 200 OK"));
        assert!(resp.contains("application/json"));
        assert!(resp.contains("\"capabilities\":["));
        assert!(resp.contains("planning"));
    }

    #[test]
    fn plan_endpoint_decomposes_an_intent() {
        let addr = start_test_server();
        let resp = raw_request(
            addr,
            "GET /api/plan?intent=research%20the%20Transformer%20paper HTTP/1.1\r\nHost: x\r\n\r\n",
        );
        assert!(resp.starts_with("HTTP/1.1 200 OK"));
        assert!(resp.contains("\"steps\":["));
        assert!(resp.contains("retrieval"));
    }

    #[test]
    fn run_endpoint_executes_and_returns_results() {
        let addr = start_test_server();
        let body = "intent=say+hello";
        let req = format!(
            "POST /api/run HTTP/1.1\r\nHost: x\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let resp = raw_request(addr, &req);
        assert!(resp.starts_with("HTTP/1.1 200 OK"));
        assert!(resp.contains("\"results\":["));
        assert!(resp.contains("\"verified\":true"));
    }

    #[test]
    fn audit_and_telemetry_accumulate_across_requests_via_shared_engine() {
        // Regression: /api/run used to build a brand-new Engine per request,
        // so its audit/telemetry were discarded the instant the response was
        // written — a long-lived `ckos serve` process had no way to observe
        // its own cumulative activity. Two runs against the same server must
        // now both be reflected in GET /api/status.
        let addr = start_test_server();
        let run = |intent: &str| {
            let body = format!("intent={intent}");
            let req = format!(
                "POST /api/run HTTP/1.1\r\nHost: x\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            raw_request(addr, &req)
        };
        assert!(run("say+hello").starts_with("HTTP/1.1 200 OK"));
        assert!(run("say+goodbye").starts_with("HTTP/1.1 200 OK"));

        let status = raw_request(addr, "GET /api/status HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(status.starts_with("HTTP/1.1 200 OK"));
        let body = status.rsplit("\r\n\r\n").next().unwrap();
        let records: usize = body
            .split("\"audit_records\":")
            .nth(1)
            .and_then(|s| s.split(',').next())
            .and_then(|s| s.trim().parse().ok())
            .expect("audit_records field");
        assert!(
            records >= 2,
            "expected at least 2 accumulated audit records from 2 runs, got {records}: {body}"
        );
    }

    #[test]
    fn search_cache_hits_on_repeat_query_and_invalidates_after_run() {
        let root = temp_root("cache");
        let dir_str = "s";

        let addr = start_test_server_rooted(root.0.clone());
        let run = |dir: &str| {
            let body = format!("intent=study+the+Transformer+paper&session={dir}");
            let req = format!(
                "POST /api/run HTTP/1.1\r\nHost: x\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            raw_request(addr, &req)
        };
        let search = |dir: &str| {
            raw_request(
                addr,
                &format!("GET /api/search?session={dir}&q=Transformer HTTP/1.1\r\nHost: x\r\n\r\n"),
            )
        };

        assert!(run(dir_str).starts_with("HTTP/1.1 200 OK"));

        let first = search(dir_str);
        assert!(first.starts_with("HTTP/1.1 200 OK"), "got: {first}");
        assert!(first.contains("\"cached\":false"), "got: {first}");

        let second = search(dir_str);
        assert!(
            second.contains("\"cached\":true"),
            "identical repeat query must hit the cache: {second}"
        );

        // A new run touches this session (new document + graph nodes) —
        // the stale cache entry must not survive it.
        assert!(run(dir_str).starts_with("HTTP/1.1 200 OK"));
        let third = search(dir_str);
        assert!(
            third.contains("\"cached\":false"),
            "a run against the session must invalidate its search cache: {third}"
        );
    }

    #[test]
    fn a_search_racing_a_concurrent_run_never_resurrects_stale_cache_data() {
        // Regression: search()'s cache-check, compute, and cache-write used
        // to be three separate lock acquisitions. If a `run` invalidated and
        // mutated a session *while* a concurrent `search` was already past
        // its own cache-miss check (already computing from the pre-run
        // state), that search's now-stale result got written into the cache
        // *after* the invalidation — resurrecting exactly the data `run`'s
        // invalidation was meant to discard. `test_stall_before_cache_write`
        // (compiled only in test builds) makes this race deterministic
        // instead of timing-dependent.
        let root = temp_root("race");
        let dir_str = "s".to_string();

        let addr = start_test_server_rooted(root.0.clone());
        let run = move |dir: &str, intent: &str| {
            let body = format!("intent={intent}&session={dir}");
            let req = format!(
                "POST /api/run HTTP/1.1\r\nHost: x\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            raw_request(addr, &req)
        };
        let search = move |dir: &str, stall: bool| {
            let flag = if stall {
                "&test_stall_before_cache_write=1"
            } else {
                ""
            };
            raw_request(
                addr,
                &format!(
                    "GET /api/search?session={dir}&q=Transformer{flag} HTTP/1.1\r\nHost: x\r\n\r\n"
                ),
            )
        };

        assert!(
            run(&dir_str, "study+the+Transformer+paper").starts_with("HTTP/1.1 200 OK"),
            "seed run failed"
        );

        // A stalled search: it computes its (pre-second-run) hits, then
        // sleeps 300ms before deciding whether to cache them.
        let stalled_dir = dir_str.clone();
        let stalled = thread::spawn(move || search(&stalled_dir, true));

        // Give the stalled search time to pass its own cache-miss check and
        // finish computing — well inside the 300ms stall window.
        thread::sleep(std::time::Duration::from_millis(50));

        // A second, concurrent run mutates the same session — this must
        // invalidate the cache such that the stalled search's now-stale
        // result, written after this point, is never cached.
        assert!(
            run(&dir_str, "learn+about+Vaswani+Attention").starts_with("HTTP/1.1 200 OK"),
            "concurrent run failed"
        );

        let stalled_resp = stalled.join().expect("stalled search thread panicked");
        assert!(stalled_resp.starts_with("HTTP/1.1 200 OK"));
        assert!(
            stalled_resp.contains("\"cached\":false"),
            "the stalled search itself computed fresh (uncached) results: {stalled_resp}"
        );

        // A third, plain search must not find a cached entry — if the race
        // still existed, the stalled search's write (after the concurrent
        // run's invalidation) would have resurrected a stale entry here.
        let third = search(&dir_str, false);
        assert!(
            third.contains("\"cached\":false"),
            "the invalidated-mid-computation search must not have cached a stale result: {third}"
        );
    }

    #[test]
    fn unknown_route_is_404() {
        let addr = start_test_server();
        let resp = raw_request(addr, "GET /nope HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(resp.starts_with("HTTP/1.1 404 Not Found"));
    }

    #[test]
    fn missing_required_param_is_400() {
        // A valid (in-root) session, so the 400 can only come from the
        // missing `q` — not from session confinement, which has its own test.
        let root = temp_root("missing-param");
        let addr = start_test_server_rooted(root.0.clone());
        let resp = raw_request(
            addr,
            "GET /api/search?session=s HTTP/1.1\r\nHost: x\r\n\r\n",
        );
        assert!(resp.starts_with("HTTP/1.1 400 Bad Request"), "got: {resp}");
        assert!(resp.contains("missing `q`"), "got: {resp}");
    }

    #[test]
    fn graph_try_it_mode_extracts_from_text() {
        let addr = start_test_server();
        let resp = raw_request(
            addr,
            "GET /api/graph?text=CKOS%20depends%20on%20the%20Scheduler. HTTP/1.1\r\nHost: x\r\n\r\n",
        );
        assert!(resp.starts_with("HTTP/1.1 200 OK"));
        assert!(resp.contains("\"nodes\":["));
        assert!(resp.contains("Scheduler"));
    }

    #[test]
    fn connections_beyond_the_cap_get_503_not_an_unbounded_thread_spawn() {
        // A cap of 1: hold the first connection open (send nothing, so its
        // handler thread blocks in the header read) to occupy the single
        // slot, then a second connection must be rejected immediately with
        // 503 rather than queued or spawned unboundedly.
        let listener = bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        thread::spawn(move || serve_bounded(listener, 1, PathBuf::from(".")));

        let _held = TcpStream::connect(addr).expect("connect first");
        // Give the accept loop's spawned thread a moment to actually start
        // and increment the active counter before the second connection.
        thread::sleep(std::time::Duration::from_millis(100));

        let mut second = TcpStream::connect(addr).expect("connect second");
        second
            .write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        let mut resp = String::new();
        second.read_to_string(&mut resp).expect("read");
        assert!(resp.starts_with("HTTP/1.1 503"), "got: {resp}");
    }

    #[test]
    fn a_session_parameter_cannot_reach_outside_the_session_root() {
        // Regression: `session` was passed straight to `FileStore::open`,
        // which `create_dir_all`s the path and writes `*.doc` + `graph.kg`
        // into it — so it was an unconstrained filesystem write primitive
        // reachable from any page in the operator's browser (the form-encoded
        // POST is CORS-simple; the opaque response doesn't stop the write).
        //
        // Reproduced before the fix with exactly this request shape: it
        // answered `200 OK`, created all three levels of
        // `<tmp>/…/deep/nested`, and wrote two documents plus a graph there.
        let root = temp_root("confine");
        let outside = root.0.parent().unwrap().join(format!(
            "ckos-web-escapee-{}-{}",
            std::process::id(),
            addr_seq()
        ));
        let _outside_guard = TempDir(outside.clone());
        assert!(!outside.exists(), "escape target must not exist up front");

        let addr = start_test_server_rooted(root.0.clone());
        let post_run = |session: &str| {
            let body = format!("intent=say+hello&session={session}");
            let req = format!(
                "POST /api/run HTTP/1.1\r\nHost: x\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            raw_request(addr, &req)
        };

        // Absolute path: the shape the original repro used.
        let absolute = post_run(&outside.display().to_string());
        assert!(
            absolute.starts_with("HTTP/1.1 400 Bad Request"),
            "an absolute session path must be rejected, got: {absolute}"
        );

        // Traversal out of the root, including a form that only escapes
        // after descending — the partial case a naive `starts_with("..")`
        // guard would miss.
        for traversal in ["..%2Fescapee", "a%2F..%2F..%2Fescapee"] {
            let resp = post_run(traversal);
            assert!(
                resp.starts_with("HTTP/1.1 400 Bad Request"),
                "traversal {traversal} must be rejected, got: {resp}"
            );
        }

        assert!(
            !outside.exists(),
            "nothing may be created outside the session root: {} exists",
            outside.display()
        );

        // The confinement must not break the legitimate case: a plain
        // relative name still works, and lands inside the root.
        let ok = post_run("s");
        assert!(ok.starts_with("HTTP/1.1 200 OK"), "got: {ok}");
        assert!(
            root.0.join("s").join("graph.kg").is_file(),
            "a relative session must still persist under the root"
        );
    }

    #[test]
    fn a_rejected_session_is_reported_on_every_handler_that_takes_one() {
        // The resolver is a security boundary, so it must sit in front of
        // *every* handler that accepts `session`, not just the one the
        // vulnerability was demonstrated on. Two of these (kql, graph) treat
        // the parameter as optional and previously fell through to the demo
        // mode's code path with the raw string.
        let root = temp_root("confine-all");
        let addr = start_test_server_rooted(root.0.clone());

        let cases = [
            "GET /api/history?session=%2Fetc&q= HTTP/1.1\r\nHost: x\r\n\r\n".to_string(),
            "GET /api/search?session=%2Fetc&q=x HTTP/1.1\r\nHost: x\r\n\r\n".to_string(),
            "GET /api/graph?session=..%2Fescapee HTTP/1.1\r\nHost: x\r\n\r\n".to_string(),
        ];
        for req in cases {
            let resp = raw_request(addr, &req);
            assert!(
                resp.starts_with("HTTP/1.1 400 Bad Request"),
                "expected 400 for {req:?}, got: {resp}"
            );
        }

        let body = "query=FIND+Concept+%22Transformer%22&session=%2Fetc";
        let req = format!(
            "POST /api/kql HTTP/1.1\r\nHost: x\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let resp = raw_request(addr, &req);
        assert!(
            resp.starts_with("HTTP/1.1 400 Bad Request"),
            "kql must reject an escaping session too, got: {resp}"
        );
    }

    #[test]
    fn api_response_shapes_are_locked_to_what_the_dashboard_reads() {
        // Tripwire, not a proof. `dashboard.html` is a static string compiled
        // into this binary that reads response fields *by name*, and nothing
        // links the two: renaming a field in `routes.rs` leaves the dashboard
        // silently rendering `undefined`, with every existing test still
        // green. That is not hypothetical — renaming `source` to `sources` on
        // the search hit did exactly that to the results table, and it was
        // caught by reading the HTML, not by the suite.
        //
        // So: pin the field names each route emits. Any change here fails
        // loudly and the message tells you to check the dashboard. Both
        // directions are checked, because either side can drift — the API can
        // drop a field the page reads, and the page can be edited to read a
        // field the API never had.
        let root = temp_root("shape");
        let addr = start_test_server_rooted(root.0.clone());
        let page = crate::dashboard::PAGE;

        // (route, response, field names the dashboard renders from it)
        let cases: Vec<(&str, String, Vec<&str>)> = vec![
            (
                "/api/search",
                {
                    let body = "intent=study+the+Transformer&session=s";
                    raw_request(addr, &format!(
                        "POST /api/run HTTP/1.1\r\nHost: x\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{}",
                        body.len(), body));
                    raw_request(
                        addr,
                        "GET /api/search?session=s&q=Transformer HTTP/1.1\r\nHost: x\r\n\r\n",
                    )
                },
                vec!["hits", "title", "snippet", "score", "sources"],
            ),
            (
                "/api/history",
                raw_request(
                    addr,
                    "GET /api/history?session=s HTTP/1.1\r\nHost: x\r\n\r\n",
                ),
                vec!["items", "title", "body", "confidence", "verified"],
            ),
            (
                "/api/graph",
                raw_request(
                    addr,
                    "GET /api/graph?text=CKOS%20uses%20a%20Scheduler. HTTP/1.1\r\nHost: x\r\n\r\n",
                ),
                vec![
                    "nodes",
                    "edges",
                    "id",
                    "kind",
                    "label",
                    "confidence",
                    "from",
                    "to",
                ],
            ),
            (
                "/api/status",
                raw_request(addr, "GET /api/status HTTP/1.1\r\nHost: x\r\n\r\n"),
                vec![
                    "audit_records",
                    "total_tokens",
                    "mean_latency_ms",
                    "mean_tokens_per_sec",
                    "cached_sessions",
                ],
            ),
            (
                "/api/verify",
                raw_request(
                    addr,
                    "GET /api/verify?text=hello HTTP/1.1\r\nHost: x\r\n\r\n",
                ),
                vec!["passed", "checks", "name", "status", "reason"],
            ),
        ];

        for (route, response, fields) in cases {
            assert!(
                response.starts_with("HTTP/1.1 200 OK"),
                "{route} should answer 200: {response}"
            );
            for field in fields {
                assert!(
                    response.contains(&format!("\"{field}\"")),
                    "{route} no longer emits `{field}`. If that rename was \
                     deliberate, update web/src/dashboard.html — it reads this \
                     field by name and will render `undefined` otherwise. \
                     Response: {response}"
                );
                assert!(
                    page.contains(field),
                    "dashboard.html no longer mentions `{field}`, which {route} \
                     emits — did a rename land on only one side?"
                );
            }
        }
    }

    #[test]
    fn kql_runs_against_the_demo_graph_with_no_session() {
        let addr = start_test_server();
        let body = "query=FIND+Concept+%22Transformer%22+RELATED+Algorithm";
        let req = format!(
            "POST /api/kql HTTP/1.1\r\nHost: x\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let resp = raw_request(addr, &req);
        assert!(resp.starts_with("HTTP/1.1 200 OK"));
        assert!(resp.contains("\"primary\":["));
    }
}
