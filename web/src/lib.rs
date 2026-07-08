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

pub mod dashboard;
pub mod http;
pub mod json;
mod routes;

use std::io;
use std::net::{TcpListener, ToSocketAddrs};
use std::thread;

/// Bind a listener at `addr` (e.g. `"127.0.0.1:8080"`, or port `0` to let the
/// OS pick a free port). Split from [`serve`] so callers — including tests —
/// can discover the actual bound port via [`TcpListener::local_addr`] before
/// entering the accept loop.
pub fn bind(addr: impl ToSocketAddrs) -> io::Result<TcpListener> {
    TcpListener::bind(addr)
}

/// Accept connections on `listener` forever, dispatching each to the
/// dashboard/API routes (§902) on its own thread. Never returns under normal
/// operation; an individual connection's I/O error or a route handler panic
/// is contained to that connection (see [`http::handle_connection`]) and
/// does not stop the server.
pub fn serve(listener: TcpListener) -> ! {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        thread::spawn(move || {
            http::handle_connection(stream, &routes::route);
        });
    }
    unreachable!("TcpListener::incoming() only ends if the listener itself is dropped")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;

    fn start_test_server() -> std::net::SocketAddr {
        let listener = bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        thread::spawn(move || serve(listener));
        addr
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
    fn unknown_route_is_404() {
        let addr = start_test_server();
        let resp = raw_request(addr, "GET /nope HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(resp.starts_with("HTTP/1.1 404 Not Found"));
    }

    #[test]
    fn missing_required_param_is_400() {
        let addr = start_test_server();
        let resp = raw_request(
            addr,
            "GET /api/search?session=%2Ftmp%2Fx HTTP/1.1\r\nHost: x\r\n\r\n",
        );
        assert!(resp.starts_with("HTTP/1.1 400 Bad Request"));
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
