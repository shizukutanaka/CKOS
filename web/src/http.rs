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
    let n = reader.read_line(&mut request_line)?;
    if n == 0 {
        return Ok(None);
    }
    budget = budget.saturating_sub(n);

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("/").to_string();
    let (raw_path, raw_query) = target.split_once('?').unwrap_or((target.as_str(), ""));
    let mut params = parse_form(raw_query);

    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            break; // client closed mid-headers
        }
        budget = budget.saturating_sub(n);
        if budget == 0 {
            return Err(io::Error::other("request too large"));
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

    let content_length = content_length.min(budget);
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

/// Handle one accepted connection: parse a single request, dispatch it to
/// `handler`, write the response, then close. A handler that panics is
/// caught so one bad request can't take down the listener thread or leave a
/// client hanging with no response.
pub fn handle_connection(mut stream: TcpStream, handler: &(dyn Fn(&Request) -> Response + Sync)) {
    let request = match parse(&mut stream) {
        Ok(Some(r)) => r,
        Ok(None) => return,
        Err(_) => {
            let _ = Response::bad_request("malformed request").write_to(&mut stream);
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
}
