//! A minimal, dependency-free JSON value and serializer.
//!
//! CKOS is deliberately `std`-only (no `serde`), so the API gateway (§902)
//! needs its own small JSON writer. This is intentionally narrow: it only
//! *writes* JSON (the server never needs to parse a JSON request body — every
//! endpoint that takes input reads plain query-string or form values), and it
//! only supports the value shapes the routes actually produce.

use std::fmt;

/// A JSON value that knows how to render itself.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    /// The JSON `null` literal.
    Null,
    /// The JSON `true`/`false` literal.
    Bool(bool),
    /// Rendered with `{}` formatting (integral floats print without a
    /// trailing `.0`, matching what a browser's `JSON.parse` round-trips).
    Number(f64),
    /// A JSON string, escaped on render.
    String(String),
    /// A JSON array.
    Array(Vec<Json>),
    /// Insertion-ordered so responses are stable and diffable.
    Object(Vec<(String, Json)>),
}

impl Json {
    /// Build an object from an iterator of key/value pairs — the usual way to
    /// construct a response.
    pub fn object(fields: impl IntoIterator<Item = (&'static str, Json)>) -> Json {
        Json::Object(
            fields
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        )
    }

    /// Build an array.
    pub fn array(items: impl IntoIterator<Item = Json>) -> Json {
        Json::Array(items.into_iter().collect())
    }
}

impl From<&str> for Json {
    fn from(s: &str) -> Json {
        Json::String(s.to_string())
    }
}

impl From<String> for Json {
    fn from(s: String) -> Json {
        Json::String(s)
    }
}

impl From<bool> for Json {
    fn from(b: bool) -> Json {
        Json::Bool(b)
    }
}

impl From<u8> for Json {
    fn from(n: u8) -> Json {
        Json::Number(n as f64)
    }
}

impl From<usize> for Json {
    fn from(n: usize) -> Json {
        Json::Number(n as f64)
    }
}

impl From<u64> for Json {
    fn from(n: u64) -> Json {
        Json::Number(n as f64)
    }
}

impl From<f32> for Json {
    fn from(n: f32) -> Json {
        Json::Number(n as f64)
    }
}

impl<T: Into<Json>> From<Option<T>> for Json {
    fn from(v: Option<T>) -> Json {
        match v {
            Some(v) => v.into(),
            None => Json::Null,
        }
    }
}

/// Escape a string for embedding inside a JSON string literal (RFC 8259 §7):
/// quote, backslash and control characters are escaped; everything else
/// (including non-ASCII UTF-8) passes through unchanged.
fn escape(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

impl fmt::Display for Json {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Json::Null => write!(f, "null"),
            Json::Bool(b) => write!(f, "{b}"),
            Json::Number(n) => {
                if n.fract() == 0.0 && n.abs() < 1e15 {
                    write!(f, "{}", *n as i64)
                } else {
                    write!(f, "{n}")
                }
            }
            Json::String(s) => {
                let mut out = String::with_capacity(s.len() + 2);
                escape(s, &mut out);
                write!(f, "{out}")
            }
            Json::Array(items) => {
                write!(f, "[")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ",")?;
                    }
                    write!(f, "{item}")?;
                }
                write!(f, "]")
            }
            Json::Object(fields) => {
                write!(f, "{{")?;
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ",")?;
                    }
                    let mut key = String::new();
                    escape(k, &mut key);
                    write!(f, "{key}:{v}")?;
                }
                write!(f, "}}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_control_characters_and_quotes() {
        let j = Json::from("line1\nline2\t\"quoted\"\\backslash");
        assert_eq!(j.to_string(), r#""line1\nline2\t\"quoted\"\\backslash""#);
    }

    #[test]
    fn integral_numbers_render_without_a_trailing_dot_zero() {
        assert_eq!(Json::Number(100.0).to_string(), "100");
        assert_eq!(Json::Number(0.125).to_string(), "0.125");
    }

    #[test]
    fn builds_nested_objects_and_arrays() {
        let j = Json::object([
            ("name", Json::from("CKOS")),
            ("count", Json::from(3u64)),
            ("tags", Json::array([Json::from("a"), Json::from("b")])),
            ("missing", Json::from(Option::<String>::None)),
        ]);
        assert_eq!(
            j.to_string(),
            r#"{"name":"CKOS","count":3,"tags":["a","b"],"missing":null}"#
        );
    }

    #[test]
    fn non_ascii_passes_through_unescaped() {
        assert_eq!(Json::from("こんにちは").to_string(), "\"こんにちは\"");
    }
}
