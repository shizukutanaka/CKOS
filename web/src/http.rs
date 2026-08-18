//! A minimal, dependency-free HTTP/1.1 server (§902 API gateway).
//!
//! Not a general-purpose web server: it understands exactly what the CKOS
//! dashboard needs — `GET` with a query string, `POST` with an
//! `application/x-www-form-urlencoded` body — over a bounded request size,
//! one short-lived connection per request (`Connection: close`, no
//! keep-alive). This keeps the workspace's `std`-only, dependency-free
//! guarantee. A deployment fronting real internet traffic should sit behind a
//! reverse proxy (nginx/Caddy) rather than rely on this server's HTTP
//! compliance or its lack of TLS.

use crate::json::Json;
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// Hard cap on a request's total size (request line + headers + body) — a
/// bounded-input guard so a malformed or hostile client can't exhaust memory
/// or hang a connection thread forever reading headers.
const MAX_REQUEST_BYTES: usize = 1 << 20; // 1 MiB

/// How long a connection may sit idle before the server gives up on it.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// How long [`write_early_response`] waits for more unread request bytes
/// before concluding the peer has stopped sending.
const DRAIN_TIMEOUT: Duration = Duration::from_millis(200);

/// Hard cap on bytes [`write_early_response`] will read and discard. The
/// idle timeout alone does not bound the drain: it only fires when the peer
/// goes *quiet*, so a peer that keeps streaming would hold the connection
/// thread forever with every read succeeding.
const MAX_DRAIN_BYTES: usize = MAX_REQUEST_BYTES;

/// A parsed HTTP request. Query-string parameters and (for `POST`)
/// form-encoded body parameters are merged into one lookup, since every
/// route in this gateway treats them the same way.
pub struct Request {
    /// The HTTP method, e.g. `"GET"` or `"POST"`.
    pub method: String,
    /// The request path, percent-decoded (query string stripped).
    pub path: String,
    params: HashMap<String, String>,
}

impl Request {
    /// A parameter's value (query string, or form body for `POST`).
    pub fn param(&self, key: &str) -> Option<&str> {
        self.params.get(key).map(String::as_str)
    }

    /// A parameter's value, or `""` if absent.
    pub fn param_or_empty(&self, key: &str) -> &str {
        self.param(key).unwrap_or("")
    }

    /// Whether a boolean flag parameter is present and not literally
    /// `""`/`"0"`/`"false"` — matches how an HTML checkbox/form typically
    /// submits a flag.
    pub fn flag(&self, key: &str) -> bool {
        matches!(self.param(key), Some(v) if !matches!(v, "" | "0" | "false"))
    }
}

/// Percent-decode a `application/x-www-form-urlencoded` component: `+` is a
/// space, `%XX` is a byte in hex. An invalid `%` escape (truncated or
/// non-hex) is passed through literally rather than treated as an error —
/// this server only ever produces best-effort decoding, never a 400 for a
/// slightly malformed query string.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 3 <= bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok());
                match hex {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_form(input: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for pair in input.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        map.insert(percent_decode(k), percent_decode(v));
    }
    map
}

/// Read and parse one HTTP request from `stream`. Returns `Ok(None)` if the
/// client closed the connection before sending anything (a normal outcome —
/// idle keep-alive-less connections end this way).
pub fn parse(stream: &mut TcpStream) -> io::Result<Option<Request>> {
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    let mut reader = BufReader::new(stream);
    let mut budget = MAX_REQUEST_BYTES;

    let mut request_line = String::new();
    // Cap the read at the remaining budget: `read_line` on its own buffers an
    // entire line before returning, so a single line with no terminator would
    // exhaust memory *before* any size check. Bounding each read makes the
    // documented per-request cap actually cover the request line and headers,
    // not just the body.
    let n = (&mut reader)
        .take(budget as u64)
        .read_line(&mut request_line)?;
    if n == 0 {
        return Ok(None);
    }
    if !request_line.ends_with('\n') {
        // Reached the cap without a line terminator → over budget ("too
        // large" maps to a 413 in handle_connection).
        return Err(io::Error::other("request line too large"));
    }
    budget -= n;

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("/").to_string();
    let (raw_path, raw_query) = target.split_once('?').unwrap_or((target.as_str(), ""));
    let mut params = parse_form(raw_query);

    let mut content_length = 0usize;
    loop {
        if budget == 0 {
            // Headers consumed the whole budget without the terminating blank
            // line — reject rather than read further unbounded.
            return Err(io::Error::other("request headers too large"));
        }
        let mut line = String::new();
        let n = (&mut reader).take(budget as u64).read_line(&mut line)?;
        if n == 0 {
            break; // client closed mid-headers
        }
        budget -= n;
        if !line.ends_with('\n') {
            // Hit the cap mid-line → over budget (413).
            return Err(io::Error::other("request headers too large"));
        }
        let line = line.trim_end();
        if line.is_empty() {
            break; // end of headers
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
    }

    // A declared Content-Length beyond the remaining budget is rejected
    // outright, never silently truncated: reading only the first `budget`
    // bytes of a larger body and parsing that as if it were the whole,
    // legitimate request would corrupt form data without any error — the
    // same "silent truncation is worse than a clear rejection" rule applied
    // to persisted embeddings and header fields elsewhere in this workspace.
    if content_length > budget {
        return Err(io::Error::other("request too large"));
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;

    if method.eq_ignore_ascii_case("POST") {
        if let Ok(text) = std::str::from_utf8(&body) {
            for (k, v) in parse_form(text) {
                params.insert(k, v);
            }
        }
    }

    Ok(Some(Request {
        method,
        path: percent_decode(raw_path),
        params,
    }))
}

/// An HTTP response to write back over the connection.
pub struct Response {
    /// HTTP status code, e.g. `200`, `404`.
    pub status: u16,
    /// The `Content-Type` header value.
    pub content_type: &'static str,
    /// The response body bytes.
    pub body: Vec<u8>,
}

impl Response {
    /// A `200 OK` JSON response.
    pub fn json(value: Json) -> Response {
        Response {
            status: 200,
            content_type: "application/json; charset=utf-8",
            body: value.to_string().into_bytes(),
        }
    }

    /// A JSON response with an explicit status (errors, denials).
    pub fn json_status(status: u16, value: Json) -> Response {
        Response {
            status,
            content_type: "application/json; charset=utf-8",
            body: value.to_string().into_bytes(),
        }
    }

    /// A `200 OK` HTML response — used only for the embedded dashboard page.
    pub fn html(body: &str) -> Response {
        Response {
            status: 200,
            content_type: "text/html; charset=utf-8",
            body: body.as_bytes().to_vec(),
        }
    }

    /// A `404 Not Found` JSON error body.
    pub fn not_found() -> Response {
        Response::json_status(404, Json::object([("error", "not found".into())]))
    }

    /// A `400 Bad Request` JSON error body with a human-readable reason.
    pub fn bad_request(reason: &str) -> Response {
        Response::json_status(400, Json::object([("error", reason.into())]))
    }

    fn status_text(status: u16) -> &'static str {
        match status {
            200 => "OK",
            400 => "Bad Request",
            404 => "Not Found",
            405 => "Method Not Allowed",
            413 => "Payload Too Large",
            500 => "Internal Server Error",
            _ => "Unknown",
        }
    }

    /// Serialize and write the full response (status line, headers, body).
    pub fn write_to(&self, stream: &mut TcpStream) -> io::Result<()> {
        write!(
            stream,
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\n\r\n",
            self.status,
            Self::status_text(self.status),
            self.content_type,
            self.body.len(),
        )?;
        stream.write_all(&self.body)?;
        stream.flush()
    }
}

/// Write `response` on a connection we are rejecting *before* having read the
/// request the peer is still sending, then discard what remains of it.
///
/// Closing a socket that still has unread data makes the OS send RST rather
/// than a clean FIN, and an RST discards whatever of our response is still in
/// flight. Measured, not assumed: replying 413 to a client actually streaming
/// the oversized body it announced delivered a *truncated* response in 3 of 5
/// runs — as little as `"HTTP/1.1 "`, 9 bytes, with the client's read
/// returning `Ok`, so the peer sees a successful read of a fragment carrying
/// no status code at all. Reading the rest first lets the close be clean.
///
/// Bounded in both directions, because this runs on the reject path where the
/// peer is by definition uncooperative: `DRAIN_TIMEOUT` (200 ms) caps how
/// long we wait for a peer that stops sending, and `MAX_DRAIN_BYTES` (the
/// per-request cap) caps a peer that never stops.
///
/// The normal response path deliberately does *not* drain: `parse` consumed
/// exactly the announced body, so nothing is pending, and a drain there would
/// add the idle timeout to every single response.
pub fn write_early_response(stream: &mut TcpStream, response: &Response) {
    let _ = response.write_to(stream);
    let _ = stream.set_read_timeout(Some(DRAIN_TIMEOUT));
    let mut discard = [0u8; 8192];
    let mut drained = 0usize;
    while drained < MAX_DRAIN_BYTES {
        match stream.read(&mut discard) {
            Ok(0) | Err(_) => break,
            Ok(n) => drained += n,
        }
    }
}

/// Handle one accepted connection: parse a single request, dispatch it to
/// `handler`, write the response, then close. A handler that panics is
/// caught so one bad request can't take down the listener thread or leave a
/// client hanging with no response.
pub fn handle_connection(mut stream: TcpStream, handler: &(dyn Fn(&Request) -> Response + Sync)) {
    let request = match parse(&mut stream) {
        Ok(Some(r)) => r,
        Ok(None) => return,
        // Both arms below reject a request the peer may still be streaming,
        // so they must drain before closing or the reply gets RST away —
        // see `write_early_response`.
        Err(e) if e.to_string().contains("too large") => {
            write_early_response(
                &mut stream,
                &Response::json_status(413, Json::object([("error", "request too large".into())])),
            );
            return;
        }
        Err(_) => {
            write_early_response(&mut stream, &Response::bad_request("malformed request"));
            return;
        }
    };
    let response = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(&request)))
        .unwrap_or_else(|_| {
            Response::json_status(500, Json::object([("error", "internal error".into())]))
        });
    let _ = response.write_to(&mut stream);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_handles_plus_and_hex_and_bad_escapes() {
        assert_eq!(percent_decode("a+b"), "a b");
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("100%25"), "100%");
        // Truncated/invalid escape passes through literally, not an error.
        assert_eq!(percent_decode("bad%2"), "bad%2");
        assert_eq!(percent_decode("bad%zz"), "bad%zz");
    }

    #[test]
    fn parse_form_decodes_pairs() {
        let m = parse_form("intent=research+X&session=%2Ftmp%2Fs");
        assert_eq!(m.get("intent").map(String::as_str), Some("research X"));
        assert_eq!(m.get("session").map(String::as_str), Some("/tmp/s"));
    }

    #[test]
    fn oversized_content_length_is_rejected_not_silently_truncated() {
        // Regression: a declared Content-Length beyond MAX_REQUEST_BYTES used
        // to be silently clamped (`content_length.min(budget)`), so the
        // server would read only the first ~1 MiB of a larger body and parse
        // that truncated, corrupted fragment as if it were the whole request
        // — never erroring. It must now be rejected outright (413) before
        // any body bytes are read.
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            handle_connection(stream, &|_req| Response::json(Json::Bool(true)));
        });

        let mut stream = TcpStream::connect(addr).unwrap();
        let oversized = MAX_REQUEST_BYTES + 1;
        // Send only the headers claiming a huge body — never the body itself.
        // A correct implementation rejects before attempting to read it, so
        // the test doesn't need to actually transfer megabytes of data.
        write!(
            stream,
            "POST /api/run HTTP/1.1\r\nHost: x\r\nContent-Length: {oversized}\r\n\r\n"
        )
        .unwrap();

        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 413"), "got: {resp}");
        assert!(
            !resp.contains("\"error\":null"),
            "must carry a real error body"
        );
    }

    #[test]
    fn a_413_reaches_a_client_that_is_still_streaming_its_oversized_body() {
        // Regression: the test above deliberately never sends the body, so it
        // could not see this. A *real* client — a browser posting a large
        // form — streams the body it announced. The server used to write 413
        // and drop the socket with that data still unread, which makes the OS
        // send RST and discard the reply already in flight: measured 3 of 5
        // runs delivering a truncated response, as little as `"HTTP/1.1 "`,
        // with the client's read returning `Ok` rather than an error. So the
        // peer sees a *successful* read of a fragment with no status code.
        //
        // Repeated, because the failure is a race with the peer's writes: a
        // single attempt succeeds by luck often enough that a one-shot test
        // would be worse than none. Calibrated against the reverted fix —
        // 5 attempts missed the bug in 1 of 3 runs, 30 attempts tripped it
        // within the first four every time across 4 runs; with the drain in
        // place, 150 attempts produced no truncation at all. It stays cheap
        // (~30 ms) because the drain lets each peer write fail promptly.
        use std::net::TcpListener;
        for attempt in 0..30 {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            std::thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                handle_connection(stream, &|_req| Response::json(Json::Bool(true)));
            });

            let mut stream = TcpStream::connect(addr).unwrap();
            let oversized = MAX_REQUEST_BYTES * 4;
            write!(
                stream,
                "POST /api/run HTTP/1.1\r\nHost: x\r\nContent-Length: {oversized}\r\n\r\n"
            )
            .unwrap();
            let chunk = vec![b'x'; 64 * 1024];
            let mut sent = 0usize;
            while sent < oversized {
                if stream.write_all(&chunk).is_err() {
                    break;
                }
                sent += chunk.len();
            }
            let _ = stream.flush();

            let mut resp = String::new();
            let _ = stream.read_to_string(&mut resp);
            // The whole response, not a prefix of it: a truncated reply is the
            // exact failure, and `starts_with` alone would accept 30 bytes cut
            // off mid-header.
            assert!(
                resp.starts_with("HTTP/1.1 413 Payload Too Large"),
                "attempt {attempt}: got {resp:?}"
            );
            assert!(
                resp.contains("\r\n\r\n") && resp.trim_end().ends_with('}'),
                "attempt {attempt}: response truncated before its body: {resp:?}"
            );
        }
    }

    #[test]
    fn an_unterminated_oversized_line_is_bounded_and_rejected() {
        // Regression: `read_line` buffers a whole line before returning, so a
        // single line with no terminator used to allocate unbounded memory
        // before any size check — defeating the per-request cap the module doc
        // promises covers the request line and headers. Each line read is now
        // capped at the remaining budget. Targeting the *request line* also
        // proves the behavior change: the old code buffered it in full and
        // then handled a garbage request (a 404), whereas the cap now rejects
        // it outright as 413 the moment `budget` bytes arrive without a
        // terminator — no need to even close the connection.
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            handle_connection(stream, &|_req| Response::json(Json::Bool(true)));
        });

        let mut stream = TcpStream::connect(addr).unwrap();
        // An unterminated request line larger than the cap (no CRLF at all).
        let chunk = "A".repeat(64 * 1024);
        let mut sent = 0usize;
        while sent < MAX_REQUEST_BYTES + 64 * 1024 {
            // Once the server hits its cap it responds and closes, so a peer
            // write may fail partway — exactly the bounded behavior we want.
            if stream.write_all(chunk.as_bytes()).is_err() {
                break;
            }
            sent += chunk.len();
        }
        let _ = stream.flush();

        let mut resp = String::new();
        let _ = stream.read_to_string(&mut resp);
        assert!(
            resp.starts_with("HTTP/1.1 413"),
            "an oversized unterminated request line must be rejected as 413, got: {resp:?}"
        );
    }
}
