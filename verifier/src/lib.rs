//! # CKOS Verifier
//!
//! Verification runs **independently** of generation (§899) — ideally on a
//! separate runtime — so quality and safety are judged by something other than
//! the producer. Each concern is a [`Check`]; a [`Verifier`] runs a set of them
//! and aggregates the verdict.
//!
//! Checks from §899: mathematical consistency, JSON-schema conformance, source
//! integrity, static code analysis, citation validity, security policy.

/// Outcome of a single check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The check passed.
    Pass,
    /// The check failed, with an explanation.
    Fail(String),
    /// The check did not apply to this input.
    Skip,
}

impl Verdict {
    /// Whether this verdict permits the result to proceed.
    pub fn is_ok(&self) -> bool {
        !matches!(self, Verdict::Fail(_))
    }
}

/// A single verification concern (§899).
pub trait Check: Send + Sync {
    /// Stable name for audit logs (§903).
    fn name(&self) -> &str;
    /// Evaluate the candidate output.
    fn evaluate(&self, output: &str) -> Verdict;
}

/// Aggregated report over all checks.
#[derive(Debug, Clone)]
pub struct Report {
    pub results: Vec<(String, Verdict)>,
}

impl Report {
    /// True only if every check passed (or was skipped).
    pub fn passed(&self) -> bool {
        self.results.iter().all(|(_, v)| v.is_ok())
    }

    /// Names of failing checks with their reasons.
    pub fn failures(&self) -> Vec<(&str, &str)> {
        self.results
            .iter()
            .filter_map(|(name, v)| match v {
                Verdict::Fail(why) => Some((name.as_str(), why.as_str())),
                _ => None,
            })
            .collect()
    }
}

/// Runs a configured set of checks against candidate output.
#[derive(Default)]
pub struct Verifier {
    checks: Vec<Box<dyn Check>>,
}

impl Verifier {
    /// Create an empty verifier.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a check.
    pub fn with_check(mut self, check: Box<dyn Check>) -> Self {
        self.checks.push(check);
        self
    }

    /// Evaluate all registered checks and aggregate.
    pub fn verify(&self, output: &str) -> Report {
        Report {
            results: self
                .checks
                .iter()
                .map(|c| (c.name().to_string(), c.evaluate(output)))
                .collect(),
        }
    }
}

/// A check that rejects empty or whitespace-only output.
pub struct NonEmptyCheck;

impl Check for NonEmptyCheck {
    fn name(&self) -> &str {
        "non_empty"
    }
    fn evaluate(&self, output: &str) -> Verdict {
        if output.trim().is_empty() {
            Verdict::Fail("output is empty".into())
        } else {
            Verdict::Pass
        }
    }
}

/// A shallow JSON well-formedness check (balanced braces/brackets, no parser
/// dependency) standing in for full schema conformance (§899).
pub struct JsonBalanceCheck;

impl Check for JsonBalanceCheck {
    fn name(&self) -> &str {
        "json_balance"
    }
    fn evaluate(&self, output: &str) -> Verdict {
        let trimmed = output.trim();
        if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
            return Verdict::Skip; // not JSON-shaped
        }
        let mut depth = 0i32;
        let mut in_string = false;
        let mut escaped = false;
        for ch in trimmed.chars() {
            if in_string {
                match ch {
                    _ if escaped => escaped = false,
                    '\\' => escaped = true,
                    '"' => in_string = false,
                    _ => {}
                }
                continue;
            }
            match ch {
                '"' => in_string = true,
                '{' | '[' => depth += 1,
                '}' | ']' => {
                    depth -= 1;
                    if depth < 0 {
                        return Verdict::Fail("unbalanced closing delimiter".into());
                    }
                }
                _ => {}
            }
        }
        if depth == 0 {
            Verdict::Pass
        } else {
            Verdict::Fail("unbalanced delimiters".into())
        }
    }
}

/// Citation validity (§899): every `[n]` reference in the text must have a
/// matching definition line (a line whose trimmed start is `[n]`). Output with
/// no citation markers is skipped.
pub struct CitationCheck;

/// Extract the integer inside every `[n]` marker in `text`.
fn citation_markers(text: &str) -> Vec<u32> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 1 && j < bytes.len() && bytes[j] == b']' {
                if let Ok(n) = text[i + 1..j].parse::<u32>() {
                    out.push(n);
                }
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

impl Check for CitationCheck {
    fn name(&self) -> &str {
        "citations"
    }
    fn evaluate(&self, output: &str) -> Verdict {
        let referenced = citation_markers(output);
        if referenced.is_empty() {
            return Verdict::Skip;
        }
        // A definition is a line whose trimmed start is a `[n]` marker.
        let defined: std::collections::HashSet<u32> = output
            .lines()
            .filter_map(|line| {
                let t = line.trim_start();
                if t.starts_with('[') {
                    citation_markers(t).into_iter().next()
                } else {
                    None
                }
            })
            .collect();
        let mut missing: Vec<u32> = referenced
            .into_iter()
            .filter(|n| !defined.contains(n))
            .collect();
        if missing.is_empty() {
            Verdict::Pass
        } else {
            missing.sort_unstable();
            missing.dedup();
            Verdict::Fail(format!("undefined citation(s): {missing:?}"))
        }
    }
}

/// Security-policy check (§899): reject output containing any forbidden
/// substring (case-insensitive) — e.g. leaked secrets or disallowed content.
pub struct ForbiddenContentCheck {
    patterns: Vec<String>,
}

impl ForbiddenContentCheck {
    /// Create a check from a list of forbidden substrings.
    pub fn new(patterns: impl IntoIterator<Item = impl Into<String>>) -> Self {
        ForbiddenContentCheck {
            patterns: patterns
                .into_iter()
                .map(|p| p.into().to_lowercase())
                .collect(),
        }
    }
}

impl Check for ForbiddenContentCheck {
    fn name(&self) -> &str {
        "security_policy"
    }
    fn evaluate(&self, output: &str) -> Verdict {
        let haystack = output.to_lowercase();
        match self.patterns.iter().find(|p| haystack.contains(p.as_str())) {
            Some(p) => Verdict::Fail(format!("forbidden content: {p}")),
            None => Verdict::Pass,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_output() {
        let v = Verifier::new().with_check(Box::new(NonEmptyCheck));
        assert!(!v.verify("   ").passed());
        assert!(v.verify("ok").passed());
    }

    #[test]
    fn validates_json_balance() {
        let v = Verifier::new().with_check(Box::new(JsonBalanceCheck));
        assert!(v.verify(r#"{"a": [1, 2, {"b": "x}"}]}"#).passed());
        let report = v.verify("{\"a\": [1, 2}");
        assert!(!report.passed());
        assert_eq!(report.failures()[0].0, "json_balance");
        // Non-JSON input is skipped, not failed.
        assert!(v.verify("plain text").passed());
    }

    #[test]
    fn validates_citations() {
        let v = Verifier::new().with_check(Box::new(CitationCheck));
        // Defined citation passes.
        let ok = "As shown [1].\n\n[1] Smith et al., 2020";
        assert!(v.verify(ok).passed());
        // Dangling citation fails.
        let report = v.verify("As shown [2], but no reference exists.");
        assert!(!report.passed());
        assert_eq!(report.failures()[0].0, "citations");
        // No citations → skipped.
        assert!(v.verify("plain prose").passed());
    }

    #[test]
    fn enforces_security_policy() {
        let v = Verifier::new().with_check(Box::new(ForbiddenContentCheck::new([
            "BEGIN PRIVATE KEY",
            "password=",
        ])));
        assert!(v.verify("normal output").passed());
        assert!(!v.verify("-----BEGIN PRIVATE KEY-----").passed());
        // Case-insensitive.
        assert!(!v.verify("Password=hunter2").passed());
    }
}
