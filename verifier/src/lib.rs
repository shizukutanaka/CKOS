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
    /// Each check's name paired with its verdict.
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

    /// A verifier preconfigured with all built-in checks (§899): non-empty,
    /// repetition/degeneration, arithmetic consistency, JSON balance, citations,
    /// and a default security policy. A single discoverable entry point so the
    /// CLI and SDK don't each hand-assemble the set.
    pub fn builtin() -> Self {
        Verifier::new()
            .with_check(Box::new(NonEmptyCheck))
            .with_check(Box::new(RepetitionCheck::new()))
            .with_check(Box::new(ArithmeticCheck))
            .with_check(Box::new(JsonBalanceCheck))
            .with_check(Box::new(CitationCheck))
            .with_check(Box::new(ForbiddenContentCheck::new([
                "begin private key",
                "password=",
                "api_key=",
            ])))
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

/// A shallow JSON well-formedness check (matched braces/brackets, no parser
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
        // Stack of the closing delimiter each open bracket expects, so a
        // closer must match the *type* of its opener — not just balance the
        // aggregate count (`{"a": [1}]` and `[{]}` have equal open/close
        // counts but mismatched types, and are not valid JSON).
        let mut expected: Vec<char> = Vec::new();
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
                '{' => expected.push('}'),
                '[' => expected.push(']'),
                '}' | ']' => match expected.pop() {
                    Some(want) if want == ch => {}
                    Some(_) => return Verdict::Fail("mismatched closing delimiter".into()),
                    None => return Verdict::Fail("unbalanced closing delimiter".into()),
                },
                _ => {}
            }
        }
        if expected.is_empty() {
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

/// Mathematical-consistency check (§899): scans text for simple
/// `A <op> B = C` equalities (where `op` is `+`, `-`, `*` or `/`) and fails on
/// the first that does not hold. Operands are non-negative integers; the result
/// may be negative. Division is only evaluated when exact — ambiguous cases
/// (`7 / 2 = 3`) are ignored rather than guessed. Text with no equations is
/// skipped. This is a focused, dependency-free stand-in for full symbolic math.
pub struct ArithmeticCheck;

fn parse_uint(bytes: &[u8], start: usize) -> Option<(i128, usize)> {
    let mut i = start;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == start {
        return None;
    }
    let v = std::str::from_utf8(&bytes[start..i]).ok()?.parse().ok()?;
    Some((v, i))
}

/// Parse a standalone non-negative integer operand — rejecting any run of
/// digits that is actually a *fragment* of a larger grouped or decimal number,
/// so a correct sentence like `1,000 + 1 = 1001` or `2 + 2 = 4.5` is never
/// mis-evaluated (`parse_uint` alone would read `000`/`4` and fabricate a wrong
/// equation). A verifier reporting a false positive — rejecting valid output —
/// is worse than skipping an equation it cannot judge, so an operand touching a
/// `,` or `.` grouping/decimal separator makes the whole equation non-evaluable.
fn parse_operand(bytes: &[u8], start: usize) -> Option<(i128, usize)> {
    // Not the start of a number if the previous byte is a digit or a
    // grouping/decimal separator (we'd be reading a fragment, e.g. the `000`
    // in `1,000`).
    if start > 0 {
        let prev = bytes[start - 1];
        if prev.is_ascii_digit() || prev == b',' || prev == b'.' {
            return None;
        }
    }
    let (v, end) = parse_uint(bytes, start)?;
    // Not a plain integer if it continues into a grouped/decimal number, e.g.
    // the `1` in `1,001` or the `4` in `4.5`.
    if matches!(bytes.get(end), Some(b',') | Some(b'.'))
        && bytes.get(end + 1).is_some_and(u8::is_ascii_digit)
    {
        return None;
    }
    Some((v, end))
}

fn skip_spaces(bytes: &[u8], mut i: usize) -> usize {
    while matches!(bytes.get(i), Some(b' ' | b'\t')) {
        i += 1;
    }
    i
}

/// Whether the byte after skipping spaces from `i` is an arithmetic operator
/// immediately followed (spaces allowed) by a digit — i.e. an expression
/// continues here. Used to detect fragments of a larger expression that must
/// not be judged in isolation.
fn continues_with_operator(bytes: &[u8], i: usize) -> bool {
    let j = skip_spaces(bytes, i);
    if !matches!(bytes.get(j), Some(b'+' | b'-' | b'*' | b'/')) {
        return false;
    }
    let k = skip_spaces(bytes, j + 1);
    bytes.get(k).is_some_and(u8::is_ascii_digit)
}

/// Try to match an `A op B = C` equation at byte offset `i`. Returns
/// `(correct, rendered)` or `None` if no (evaluable) equation starts here.
fn match_equation(bytes: &[u8], i: usize) -> Option<(bool, String)> {
    let (a, i) = parse_operand(bytes, i)?;
    let i = skip_spaces(bytes, i);
    let op = *bytes.get(i)?;
    if !matches!(op, b'+' | b'-' | b'*' | b'/') {
        return None;
    }
    let i = skip_spaces(bytes, i + 1);
    let (b, i) = parse_operand(bytes, i)?;
    let i = skip_spaces(bytes, i);
    if bytes.get(i) != Some(&b'=') {
        return None;
    }
    let i = skip_spaces(bytes, i + 1);
    let (neg, i) = if bytes.get(i) == Some(&b'-') {
        (true, i + 1)
    } else {
        (false, i)
    };
    let (c_abs, c_end) = parse_operand(bytes, i)?;
    // The result itself continues into another operator+operand (e.g.
    // `= 5 - 1`): the equation isn't `A op B = C` in isolation, so it can't
    // be judged whole — skip it rather than risk a false positive. A prose
    // hyphen (`= 5 - obviously`) has no following digit and still evaluates.
    if continues_with_operator(bytes, c_end) {
        return None;
    }
    let c = if neg { -c_abs } else { c_abs };
    let expected = match op {
        b'+' => a.checked_add(b)?,
        b'-' => a.checked_sub(b)?,
        b'*' => a.checked_mul(b)?,
        b'/' if b != 0 && a % b == 0 => a / b,
        _ => return None, // zero/non-exact division: not evaluated
    };
    Some((expected == c, format!("{a} {} {b} = {c}", op as char)))
}

impl Check for ArithmeticCheck {
    fn name(&self) -> &str {
        "arithmetic"
    }
    fn evaluate(&self, output: &str) -> Verdict {
        let bytes = output.as_bytes();
        let mut found = false;
        let mut i = 0;
        while i < bytes.len() {
            // Only attempt at a digit that begins a token (not mid-identifier).
            let boundary = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
            // …and not one that is really the right-hand term of a larger
            // expression: a digit preceded (spaces allowed) by an arithmetic
            // operator is a fragment. Evaluating it alone produces false
            // positives — a unary-minus operand (`-5 + 3 = -2`) read as
            // `5 + 3 = -2`, or a precedence term (`1 + 2 * 3 = 7`) read as
            // `2 * 3 = 7` — so it is not an equation start.
            let fragment = {
                let mut j = i;
                while j > 0 && matches!(bytes[j - 1], b' ' | b'\t') {
                    j -= 1;
                }
                j > 0 && matches!(bytes[j - 1], b'+' | b'-' | b'*' | b'/')
            };
            if bytes[i].is_ascii_digit() && boundary && !fragment {
                if let Some((correct, rendered)) = match_equation(bytes, i) {
                    found = true;
                    if !correct {
                        return Verdict::Fail(format!("incorrect arithmetic: {rendered}"));
                    }
                }
            }
            i += 1;
        }
        if found {
            Verdict::Pass
        } else {
            Verdict::Skip
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

/// Degeneration check (§899): flags pathological repetition — a common
/// generation failure mode where output collapses into a loop. Two signals,
/// both std-only:
///
/// * **Consecutive runs** — the same token repeated more than `max_run` times
///   in a row (`"go go go go go go"`).
/// * **Low diversity** — for outputs of at least `min_tokens` words, a
///   unique/total token ratio below `min_diversity`.
///
/// Short outputs are skipped, since repetition is only meaningful at length.
pub struct RepetitionCheck {
    max_run: usize,
    min_tokens: usize,
    min_diversity: f32,
}

impl RepetitionCheck {
    /// Sensible defaults: >6 consecutive identical tokens, or <25% unique words
    /// across an output of 12+ words.
    pub fn new() -> Self {
        RepetitionCheck {
            max_run: 6,
            min_tokens: 12,
            min_diversity: 0.25,
        }
    }

    /// Customise the thresholds.
    pub fn with_thresholds(max_run: usize, min_tokens: usize, min_diversity: f32) -> Self {
        RepetitionCheck {
            max_run,
            min_tokens,
            min_diversity,
        }
    }
}

impl Default for RepetitionCheck {
    fn default() -> Self {
        Self::new()
    }
}

/// Normalise a token for comparison: lowercase, outer punctuation stripped.
fn norm_token(tok: &str) -> &str {
    tok.trim_matches(|c: char| !c.is_alphanumeric())
}

impl Check for RepetitionCheck {
    fn name(&self) -> &str {
        "repetition"
    }
    fn evaluate(&self, output: &str) -> Verdict {
        let tokens: Vec<String> = output
            .split_whitespace()
            .map(|t| norm_token(t).to_lowercase())
            .filter(|t| !t.is_empty())
            .collect();
        if tokens.is_empty() {
            return Verdict::Skip;
        }

        // Longest run of consecutive identical tokens. This signal applies at any
        // length — a short all-identical loop is still degenerate.
        let mut run = 1usize;
        let mut max_seen = 1usize;
        let mut worst = tokens[0].as_str();
        for w in tokens.windows(2) {
            if w[0] == w[1] {
                run += 1;
                if run > max_seen {
                    max_seen = run;
                    worst = w[1].as_str();
                }
            } else {
                run = 1;
            }
        }
        if max_seen > self.max_run {
            return Verdict::Fail(format!(
                "token {worst:?} repeated {max_seen} times consecutively"
            ));
        }

        // The diversity ratio is only meaningful once there are enough tokens.
        if tokens.len() < self.min_tokens {
            return Verdict::Pass;
        }
        let unique: std::collections::HashSet<&str> = tokens.iter().map(String::as_str).collect();
        let diversity = unique.len() as f32 / tokens.len() as f32;
        if diversity < self.min_diversity {
            return Verdict::Fail(format!(
                "low lexical diversity ({:.0}% unique of {} tokens)",
                diversity * 100.0,
                tokens.len()
            ));
        }
        Verdict::Pass
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
    fn json_balance_rejects_mismatched_delimiter_types() {
        // Equal open/close counts with the wrong closer type are not valid
        // JSON — a bracket-counting check that only tracks aggregate depth
        // would miss both of these.
        let v = Verifier::new().with_check(Box::new(JsonBalanceCheck));
        assert!(!v.verify(r#"{"a": [1}]"#).passed(), "']' closes '{{'");
        assert!(
            !v.verify("[{]}").passed(),
            "']' closes '{{', '}}' closes '['"
        );
        // A correctly nested, mixed-type structure still passes.
        assert!(v.verify(r#"{"a": [1, {"b": 2}]}"#).passed());
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
    fn detects_degenerate_repetition() {
        let v = Verifier::new().with_check(Box::new(RepetitionCheck::new()));
        // Healthy prose (varied vocabulary, 14 words) passes the diversity path.
        assert!(v
            .verify(
                "The kernel orchestrates agents and verifies their output, but never \
                 performs any inference of its own."
            )
            .passed());
        // A consecutive-token loop fails.
        let loopy = "go go go go go go go go go go";
        let report = v.verify(loopy);
        assert!(!report.passed());
        assert_eq!(report.failures()[0].0, "repetition");
        // Low-diversity output fails on the ratio signal.
        assert!(!v
            .verify("buy buy now buy buy now buy buy now buy buy now")
            .passed());
        // Short output is skipped (too little to judge).
        assert!(v.verify("yes yes").passed());
    }

    #[test]
    fn builtin_runs_all_checks() {
        let v = Verifier::builtin();
        // Clean prose passes everything.
        assert!(v.verify("A concise, correct sentence.").passed());
        // The builtin set catches each concern.
        assert!(!v.verify("2 + 2 = 5").passed()); // arithmetic
        assert!(!v.verify("api_key=SECRET").passed()); // security policy
        let report = v.verify("x");
        // Every check ran (six concerns).
        assert_eq!(report.results.len(), 6);
    }

    #[test]
    fn checks_mathematical_consistency() {
        let v = Verifier::new().with_check(Box::new(ArithmeticCheck));
        // Correct arithmetic passes; spaced and unspaced both parse.
        assert!(v.verify("The sum 2 + 2 = 4 is right.").passed());
        assert!(v.verify("compute 10*10=100 ok").passed());
        // A negative result is handled.
        assert!(v.verify("3 - 5 = -2").passed());
        // A wrong equation fails.
        let report = v.verify("Clearly 2 + 2 = 5 here.");
        assert!(!report.passed());
        assert_eq!(report.failures()[0].0, "arithmetic");
        // No equation → skipped (e.g. a date is not an equation).
        assert!(v.verify("Released on 2024-01-01, version 2.5.").passed());
        // Non-exact division is ignored, not failed.
        assert!(v.verify("7 / 2 = 3").passed());
    }

    #[test]
    fn fragments_of_larger_expressions_do_not_cause_false_positives() {
        // Regression: an operand that is really a term of a larger expression
        // used to be evaluated as its own equation and reject correct output:
        // - negative first operand: "-5 + 3 = -2" was read as "5 + 3 = -2";
        // - operator precedence: "1 + 2 * 3 = 7" (correct) — the fragment
        //   "2 * 3 = 7" was evaluated alone and failed;
        // - the result continuing into another expression: "2 + 2 = 5 - 1"
        //   (= 4, correct) had its result read as the bare 5.
        // Same principle as the grouped-number rule: an equation the checker
        // can't judge whole is skipped, not failed.
        let v = Verifier::new().with_check(Box::new(ArithmeticCheck));
        assert!(v.verify("-5 + 3 = -2").passed(), "negative first operand");
        assert!(v.verify("1 + 2 * 3 = 7").passed(), "operator precedence");
        assert!(v.verify("2 + 2 = 5 - 1").passed(), "result continues");
        // Detection power is preserved: a prose hyphen after the result is not
        // a continuing expression, so a genuinely wrong equation still fails.
        assert!(
            !v.verify("2 + 2 = 5 - obviously wrong").passed(),
            "a prose hyphen must not disable detection"
        );
    }

    #[test]
    fn grouped_and_decimal_numbers_do_not_cause_false_positives() {
        // Regression: a comma-grouped operand used to be read as a fragment
        // (`1,000` → the scanner picked up `000`), fabricating a wrong
        // equation and rejecting perfectly correct output. A verifier that
        // fails valid text is worse than one that skips — these must pass.
        let v = Verifier::new().with_check(Box::new(ArithmeticCheck));
        assert!(
            v.verify("1,000 + 1 = 1001").passed(),
            "grouped left operand"
        );
        assert!(
            v.verify("1000 + 1 = 1,001").passed(),
            "grouped right operand"
        );
        assert!(
            v.verify("Revenue rose 1,200 + 300 = 1,500 this year.")
                .passed(),
            "grouped operands throughout"
        );
        // A decimal result is not judged as an integer equality (skipped, not
        // falsely passed or failed).
        assert!(v.verify("2 + 2 = 4.5").passed());
        // The genuine check still fires on plain-integer equations nearby.
        let report = v.verify("Totals: 1,000 items, but 2 + 2 = 5 is wrong.");
        assert!(!report.passed());
        assert_eq!(report.failures()[0].0, "arithmetic");
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
