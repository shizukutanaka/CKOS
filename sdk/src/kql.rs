//! Knowledge Query Language (KQL, §962).
//!
//! A small, cross-knowledge query language that compiles to graph/vector/
//! full-text operations. Example from the spec:
//!
//! ```text
//! FIND Concept "Transformer"
//! RELATED Algorithm VIA References
//! FILTER (Confidence > 90 AND Confidence < 99) OR NOT Confidence < 50
//! BEFORE 2025-01-01
//! ORDER BY Confidence DESC
//! LIMIT 10
//! RETURN Graph + Sources
//! ```
//!
//! `RELATED <kind>` follows one hop to neighbours of a given node kind (`*` =
//! any kind). An optional `VIA <edge-kind>` qualifier restricts the hop to a
//! single typed relation (`DependsOn`, `Implements`, `CreatedBy`, `References`,
//! `RelatedTo`, §941), so e.g. `RELATED * VIA DependsOn` finds everything a
//! node depends on regardless of the neighbour's kind.
//!
//! `FILTER` accepts a boolean expression over `Confidence <op> <value>` leaves
//! combined with `AND`, `OR`, `NOT` and parentheses (precedence: `NOT` > `AND` >
//! `OR`). Multiple `FILTER` clauses are ANDed together.
//!
//! This module provides [`parse`] (tokeniser + recursive-descent parser → AST)
//! and [`execute`] (runs an AST against a [`KnowledgeGraph`]). Clauses may appear
//! in any order after `FIND`.

use ckos_graph::{EdgeKind, KnowledgeGraph, Node, NodeKind};
use std::fmt;

// ---------------------------------------------------------------------------
// AST
// ---------------------------------------------------------------------------

/// A parsed KQL query.
#[derive(Debug, Clone, PartialEq)]
pub struct KqlQuery {
    /// The `FIND` selector (required).
    pub find: NodeSelector,
    /// Optional `RELATED <kind>` traversal target (neighbour node kind, or `*`
    /// for any kind).
    pub related: Option<String>,
    /// Optional `VIA <edge-kind>` qualifier on `RELATED`: restrict the hop to
    /// neighbours reached by an edge of this [`ckos_graph::EdgeKind`] token
    /// (e.g. `DependsOn`, `References`). `None` traverses edges of any kind.
    pub related_via: Option<String>,
    /// `FILTER` predicates.
    pub filters: Vec<Filter>,
    /// Optional `BEFORE <date>` bound (ISO date, lexicographically comparable).
    pub before: Option<String>,
    /// Optional `AFTER <date>` bound.
    pub after: Option<String>,
    /// Optional `ORDER [BY] Confidence [ASC|DESC]` direction. Results are always
    /// returned in a deterministic order; this overrides the default.
    pub order: Option<SortDir>,
    /// Optional `LIMIT <n>` cap on each of the primary/related result sets.
    pub limit: Option<usize>,
    /// `RETURN` targets; defaults to `[Documents]` when omitted.
    pub returns: Vec<ReturnTarget>,
}

/// Sort direction for `ORDER BY Confidence`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    /// Lowest confidence first.
    Asc,
    /// Highest confidence first.
    Desc,
}

/// What `FIND` selects: an optional node kind and/or quoted text.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NodeSelector {
    /// Node kind token (e.g. `Concept`), or `None` / `"*"` for any.
    pub kind: Option<String>,
    /// Quoted text the label must contain.
    pub text: Option<String>,
}

/// A comparison operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    /// Strictly greater than (`>`).
    Gt,
    /// Greater than or equal (`>=`).
    Ge,
    /// Strictly less than (`<`).
    Lt,
    /// Less than or equal (`<=`).
    Le,
    /// Equal (`=` or `==`).
    Eq,
}

impl CmpOp {
    fn apply(self, lhs: u8, rhs: u8) -> bool {
        match self {
            CmpOp::Gt => lhs > rhs,
            CmpOp::Ge => lhs >= rhs,
            CmpOp::Lt => lhs < rhs,
            CmpOp::Le => lhs <= rhs,
            CmpOp::Eq => lhs == rhs,
        }
    }
}

/// A filter predicate tree. The leaf is `Confidence <op> <value>` (§948);
/// leaves combine with `AND`, `OR`, `NOT` and parentheses.
#[derive(Debug, Clone, PartialEq)]
pub enum Filter {
    /// Leaf comparison against a node's confidence score (§948).
    Confidence {
        /// Comparison operator.
        op: CmpOp,
        /// Threshold in 0..=100 to compare against.
        value: u8,
    },
    /// All sub-filters must hold.
    And(Vec<Filter>),
    /// At least one sub-filter must hold.
    Or(Vec<Filter>),
    /// Inverts the inner filter.
    Not(Box<Filter>),
}

/// A `RETURN` target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReturnTarget {
    /// Return matching nodes with their graph relations.
    Graph,
    /// Return provenance/source information for matches (§947).
    Sources,
    /// Return the matching documents themselves (the default).
    Documents,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// A parse error with a human-readable message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KqlError(pub String);

impl fmt::Display for KqlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KQL parse error: {}", self.0)
    }
}

impl std::error::Error for KqlError {}

fn err<T>(msg: impl Into<String>) -> Result<T, KqlError> {
    Err(KqlError(msg.into()))
}

// ---------------------------------------------------------------------------
// Tokeniser
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    /// A bare word (keyword, kind, number).
    Word(String),
    /// A quoted string literal.
    Str(String),
    /// A comparison operator.
    Op(CmpOp),
    /// The `+` separator in RETURN.
    Plus,
    /// `(` grouping a filter sub-expression.
    LParen,
    /// `)` closing a filter sub-expression.
    RParen,
}

fn tokenize(input: &str) -> Result<Vec<Tok>, KqlError> {
    let mut toks = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            c if c.is_whitespace() => {
                chars.next();
            }
            '"' => {
                chars.next();
                let mut s = String::new();
                let mut closed = false;
                for c2 in chars.by_ref() {
                    if c2 == '"' {
                        closed = true;
                        break;
                    }
                    s.push(c2);
                }
                if !closed {
                    return err("unterminated string literal");
                }
                toks.push(Tok::Str(s));
            }
            '+' => {
                chars.next();
                toks.push(Tok::Plus);
            }
            '(' => {
                chars.next();
                toks.push(Tok::LParen);
            }
            ')' => {
                chars.next();
                toks.push(Tok::RParen);
            }
            '>' | '<' | '=' => {
                chars.next();
                let op = if matches!(chars.peek(), Some('=')) {
                    chars.next();
                    match c {
                        '>' => CmpOp::Ge,
                        '<' => CmpOp::Le,
                        _ => CmpOp::Eq,
                    }
                } else {
                    match c {
                        '>' => CmpOp::Gt,
                        '<' => CmpOp::Lt,
                        _ => CmpOp::Eq,
                    }
                };
                toks.push(Tok::Op(op));
            }
            _ => {
                let mut w = String::new();
                while let Some(&c2) = chars.peek() {
                    if c2.is_whitespace() || matches!(c2, '"' | '+' | '>' | '<' | '=' | '(' | ')') {
                        break;
                    }
                    w.push(c2);
                    chars.next();
                }
                toks.push(Tok::Word(w));
            }
        }
    }
    Ok(toks)
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    /// Consume the next token if it is a word equal (case-insensitive) to `kw`.
    fn eat_keyword(&mut self, kw: &str) -> bool {
        if let Some(Tok::Word(w)) = self.peek() {
            if w.eq_ignore_ascii_case(kw) {
                self.pos += 1;
                return true;
            }
        }
        false
    }

    fn next_word(&mut self) -> Result<String, KqlError> {
        match self.next() {
            Some(Tok::Word(w)) => Ok(w),
            other => err(format!("expected a word, found {other:?}")),
        }
    }
}

/// Parse a KQL string into a [`KqlQuery`].
pub fn parse(input: &str) -> Result<KqlQuery, KqlError> {
    let toks = tokenize(input)?;
    if toks.is_empty() {
        return err("empty query");
    }
    let mut p = Parser { toks, pos: 0 };

    if !p.eat_keyword("FIND") {
        return err("query must start with FIND");
    }

    // FIND selector: optional kind word (or "*" for any) then optional quoted
    // text. A clause keyword here is not a kind — it begins the next clause.
    let mut find = NodeSelector::default();
    let mut explicit_any = false;
    if let Some(Tok::Word(w)) = p.peek() {
        let is_clause = matches!(
            w.to_ascii_uppercase().as_str(),
            "RELATED" | "FILTER" | "BEFORE" | "AFTER" | "ORDER" | "LIMIT" | "RETURN"
        );
        if !is_clause {
            if w == "*" {
                explicit_any = true;
            } else {
                find.kind = Some(w.clone());
            }
            p.pos += 1;
        }
    }
    if let Some(Tok::Str(s)) = p.peek() {
        find.text = Some(s.clone());
        p.pos += 1;
    }
    if find.kind.is_none() && find.text.is_none() && !explicit_any {
        return err("FIND needs a node kind, quoted text, or *");
    }

    let mut q = KqlQuery {
        find,
        related: None,
        related_via: None,
        filters: Vec::new(),
        before: None,
        after: None,
        order: None,
        limit: None,
        returns: Vec::new(),
    };

    // Remaining clauses, in any order.
    while let Some(tok) = p.peek().cloned() {
        let Tok::Word(kw) = tok else {
            return err(format!("expected a clause keyword, found {tok:?}"));
        };
        let kw_up = kw.to_ascii_uppercase();
        p.pos += 1;
        match kw_up.as_str() {
            "RELATED" => {
                q.related = Some(p.next_word()?);
                // Optional `VIA <edge-kind>` qualifier restricts the hop to a
                // single relation type (§941 typed edges).
                if p.eat_keyword("VIA") {
                    q.related_via = Some(p.next_word()?);
                }
            }
            "FILTER" => q.filters.push(parse_filter(&mut p)?),
            "BEFORE" => q.before = Some(p.next_word()?),
            "AFTER" => q.after = Some(p.next_word()?),
            "ORDER" => q.order = Some(parse_order(&mut p)?),
            "LIMIT" => q.limit = Some(parse_limit(&mut p)?),
            "RETURN" => q.returns = parse_returns(&mut p)?,
            other => return err(format!("unknown clause: {other}")),
        }
    }

    if q.returns.is_empty() {
        q.returns.push(ReturnTarget::Documents);
    }
    Ok(q)
}

/// Parse a filter expression: `OR` (lowest precedence) over `AND` over `NOT`
/// over atoms, where an atom is `Confidence <op> <value>` or a parenthesised
/// sub-expression. Stops at the next clause keyword.
fn parse_filter(p: &mut Parser) -> Result<Filter, KqlError> {
    parse_filter_or(p)
}

fn parse_filter_or(p: &mut Parser) -> Result<Filter, KqlError> {
    let mut terms = vec![parse_filter_and(p)?];
    while p.eat_keyword("OR") {
        terms.push(parse_filter_and(p)?);
    }
    Ok(if terms.len() == 1 {
        terms.pop().unwrap()
    } else {
        Filter::Or(terms)
    })
}

fn parse_filter_and(p: &mut Parser) -> Result<Filter, KqlError> {
    let mut terms = vec![parse_filter_not(p)?];
    while p.eat_keyword("AND") {
        terms.push(parse_filter_not(p)?);
    }
    Ok(if terms.len() == 1 {
        terms.pop().unwrap()
    } else {
        Filter::And(terms)
    })
}

fn parse_filter_not(p: &mut Parser) -> Result<Filter, KqlError> {
    if p.eat_keyword("NOT") {
        Ok(Filter::Not(Box::new(parse_filter_not(p)?)))
    } else {
        parse_filter_atom(p)
    }
}

fn parse_filter_atom(p: &mut Parser) -> Result<Filter, KqlError> {
    if matches!(p.peek(), Some(Tok::LParen)) {
        p.pos += 1;
        let inner = parse_filter_or(p)?;
        match p.next() {
            Some(Tok::RParen) => Ok(inner),
            other => err(format!("expected ')', found {other:?}")),
        }
    } else {
        let field = p.next_word()?;
        if !field.eq_ignore_ascii_case("Confidence") {
            return err(format!("unsupported filter field: {field}"));
        }
        let op = match p.next() {
            Some(Tok::Op(op)) => op,
            other => return err(format!("expected a comparison operator, found {other:?}")),
        };
        let value: u8 = p
            .next_word()?
            .parse()
            .map_err(|_| KqlError("FILTER Confidence value must be 0..=255".into()))?;
        Ok(Filter::Confidence { op, value })
    }
}

/// Parse `ORDER [BY] Confidence [ASC|DESC]`. The optional `BY` and the field
/// name (only `Confidence` is sortable) are accepted for readability; direction
/// defaults to `DESC` (highest confidence first).
fn parse_order(p: &mut Parser) -> Result<SortDir, KqlError> {
    p.eat_keyword("BY"); // optional
    let field = p.next_word()?;
    if !field.eq_ignore_ascii_case("Confidence") {
        return err(format!("can only ORDER BY Confidence, found {field}"));
    }
    // Optional trailing direction.
    if p.eat_keyword("ASC") {
        Ok(SortDir::Asc)
    } else {
        p.eat_keyword("DESC");
        Ok(SortDir::Desc)
    }
}

fn parse_limit(p: &mut Parser) -> Result<usize, KqlError> {
    p.next_word()?
        .parse()
        .map_err(|_| KqlError("LIMIT needs a non-negative integer".into()))
}

fn parse_returns(p: &mut Parser) -> Result<Vec<ReturnTarget>, KqlError> {
    let mut targets = Vec::new();
    loop {
        let w = p.next_word()?;
        let target = match w.to_ascii_uppercase().as_str() {
            "GRAPH" => ReturnTarget::Graph,
            "SOURCES" => ReturnTarget::Sources,
            "DOCUMENTS" => ReturnTarget::Documents,
            other => return err(format!("unknown RETURN target: {other}")),
        };
        targets.push(target);
        if matches!(p.peek(), Some(Tok::Plus)) {
            p.pos += 1;
        } else {
            break;
        }
    }
    Ok(targets)
}

// ---------------------------------------------------------------------------
// Executor
// ---------------------------------------------------------------------------

/// A node returned from a KQL query.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeMatch {
    /// Human-readable label of the matched node.
    pub label: String,
    /// Node kind token, e.g. `concept` (§897).
    pub kind: String,
    /// Confidence 0..=100 (§948).
    pub confidence: u8,
    /// Temporal date, if the node carries one (§946).
    pub date: Option<String>,
    /// Provenance/source, surfaced for `RETURN Sources` (§947).
    pub provenance: Option<String>,
}

/// The result of executing a KQL query against a graph.
#[derive(Debug, Clone, PartialEq)]
pub struct KqlResult {
    /// Nodes matching the `FIND` selector.
    pub primary: Vec<NodeMatch>,
    /// Nodes reached via `RELATED` from the primary set.
    pub related: Vec<NodeMatch>,
}

/// Lowercase token for a node kind (mirrors §897 kinds).
fn kind_token(kind: &NodeKind) -> String {
    kind.as_token()
}

/// Resolve a user-typed edge-kind word to an [`EdgeKind`], tolerating the
/// natural spellings — `DependsOn`, `dependson`, `depends_on`, `DEPENDS_ON`
/// all resolve to [`EdgeKind::DependsOn`] — while the on-disk token format
/// (`EdgeKind::from_token`, snake_case) stays strict for round-trip fidelity.
/// An unknown relation falls through to `from_token` (→ `Other`), matching no
/// real edge rather than erroring.
fn edge_kind_from_query(token: &str) -> EdgeKind {
    let squash = |s: &str| -> String {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(|c| c.to_lowercase())
            .collect()
    };
    let want = squash(token);
    [
        EdgeKind::DependsOn,
        EdgeKind::Implements,
        EdgeKind::References,
        EdgeKind::CreatedBy,
        EdgeKind::RelatedTo,
    ]
    .into_iter()
    .find(|k| squash(&k.as_token()) == want)
    .unwrap_or_else(|| EdgeKind::from_token(token))
}

/// Order a result set deterministically and apply any `LIMIT`. Without an
/// explicit `ORDER`, results are sorted by descending confidence then label, so
/// output is stable regardless of the graph's internal hash order.
fn order_and_limit(mut v: Vec<NodeMatch>, query: &KqlQuery) -> Vec<NodeMatch> {
    let dir = query.order.unwrap_or(SortDir::Desc);
    v.sort_by(|a, b| {
        let by_conf = match dir {
            SortDir::Desc => b.confidence.cmp(&a.confidence),
            SortDir::Asc => a.confidence.cmp(&b.confidence),
        };
        by_conf.then_with(|| a.label.cmp(&b.label))
    });
    if let Some(n) = query.limit {
        v.truncate(n);
    }
    v
}

fn eval_filter(node: &Node, filter: &Filter) -> bool {
    match filter {
        Filter::Confidence { op, value } => op.apply(node.confidence, *value),
        Filter::And(fs) => fs.iter().all(|f| eval_filter(node, f)),
        Filter::Or(fs) => fs.iter().any(|f| eval_filter(node, f)),
        Filter::Not(inner) => !eval_filter(node, inner),
    }
}

fn passes_filters(node: &Node, filters: &[Filter]) -> bool {
    filters.iter().all(|f| eval_filter(node, f))
}

/// Enforce `BEFORE`/`AFTER` temporal bounds against a node's date (§946).
///
/// When a bound is present but the node has no date, the node is excluded — a
/// temporal query should not return knowledge whose time is unknown. ISO dates
/// compare correctly as strings.
fn passes_temporal(node: &Node, query: &KqlQuery) -> bool {
    if query.before.is_none() && query.after.is_none() {
        return true;
    }
    let Some(date) = &node.date else {
        return false;
    };
    if let Some(before) = &query.before {
        if date.as_str() >= before.as_str() {
            return false;
        }
    }
    if let Some(after) = &query.after {
        if date.as_str() <= after.as_str() {
            return false;
        }
    }
    true
}

fn selector_matches(node: &Node, sel: &NodeSelector) -> bool {
    if let Some(kind) = &sel.kind {
        if !kind_token(&node.kind).eq_ignore_ascii_case(kind) {
            return false;
        }
    }
    if let Some(text) = &sel.text {
        if !node.label.to_lowercase().contains(&text.to_lowercase()) {
            return false;
        }
    }
    true
}

fn to_match(node: &Node) -> NodeMatch {
    NodeMatch {
        label: node.label.clone(),
        kind: kind_token(&node.kind),
        confidence: node.confidence,
        date: node.date.clone(),
        provenance: node.provenance.clone(),
    }
}

/// Execute a parsed query against a knowledge graph.
///
/// `FILTER` predicates and `BEFORE`/`AFTER` temporal bounds (§946) apply to both
/// primary and related nodes. `RETURN` shapes presentation, not the result set
/// returned by this function (provenance for `RETURN Sources` rides on each
/// [`NodeMatch`]).
pub fn execute(query: &KqlQuery, graph: &KnowledgeGraph) -> KqlResult {
    let primary_nodes: Vec<&Node> = graph
        .nodes()
        .filter(|n| {
            selector_matches(n, &query.find)
                && passes_filters(n, &query.filters)
                && passes_temporal(n, query)
        })
        .collect();

    let mut related = Vec::new();
    // Dedup by node identity, not by the rendered NodeMatch value — two
    // distinct nodes can share label/kind/confidence/date/provenance (e.g.
    // two different "Config" or "README" concepts with neither a date nor
    // provenance set), and deduping by value would silently drop one.
    let mut related_ids: std::collections::HashSet<_> = std::collections::HashSet::new();
    if let Some(related_kind) = &query.related {
        // A `VIA <edge-kind>` qualifier restricts the hop to one relation type
        // (§941 typed edges); otherwise every outgoing edge is followed.
        let via = query.related_via.as_deref().map(edge_kind_from_query);
        // `*` matches any neighbour kind, mirroring the FIND selector.
        let any_kind = related_kind == "*";
        for n in &primary_nodes {
            let neighbors = match &via {
                Some(edge_kind) => graph.neighbors_via(&n.id, edge_kind),
                None => graph.traverse(&n.id, 1),
            };
            for neighbor in neighbors {
                // A self-loop edge is not "related to itself" — excluded
                // explicitly so `VIA <edge-kind>` (graph::neighbors_via) and
                // plain `RELATED` (graph::traverse, which excludes the start
                // node as an incidental side effect of seeding its BFS
                // `visited` set with it) agree, instead of only one of the
                // two underlying primitives happening to exclude it.
                if neighbor.id == n.id {
                    continue;
                }
                if (any_kind || kind_token(&neighbor.kind).eq_ignore_ascii_case(related_kind))
                    && passes_filters(neighbor, &query.filters)
                    && passes_temporal(neighbor, query)
                    && related_ids.insert(neighbor.id.clone())
                {
                    related.push(to_match(neighbor));
                }
            }
        }
    }

    let primary = order_and_limit(primary_nodes.iter().map(|n| to_match(n)).collect(), query);
    let related = order_and_limit(related, query);
    KqlResult { primary, related }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ckos_graph::EdgeKind;

    #[test]
    fn parses_the_spec_example() {
        let q = parse(
            "FIND Concept \"Transformer\"\nRELATED Algorithm\nFILTER Confidence > 90\nBEFORE 2025-01-01\nRETURN Graph + Sources",
        )
        .unwrap();
        assert_eq!(q.find.kind.as_deref(), Some("Concept"));
        assert_eq!(q.find.text.as_deref(), Some("Transformer"));
        assert_eq!(q.related.as_deref(), Some("Algorithm"));
        assert_eq!(
            q.filters,
            vec![Filter::Confidence {
                op: CmpOp::Gt,
                value: 90
            }]
        );
        assert_eq!(q.before.as_deref(), Some("2025-01-01"));
        assert_eq!(q.returns, vec![ReturnTarget::Graph, ReturnTarget::Sources]);
    }

    #[test]
    fn defaults_return_to_documents() {
        let q = parse("FIND \"kernel\"").unwrap();
        assert!(q.find.kind.is_none());
        assert_eq!(q.find.text.as_deref(), Some("kernel"));
        assert_eq!(q.returns, vec![ReturnTarget::Documents]);
    }

    #[test]
    fn parses_related_via_edge_kind() {
        let q = parse("FIND Concept \"X\" RELATED Concept VIA DependsOn").unwrap();
        assert_eq!(q.related.as_deref(), Some("Concept"));
        assert_eq!(q.related_via.as_deref(), Some("DependsOn"));
        // RELATED without VIA leaves the qualifier unset (any edge kind).
        let q2 = parse("FIND Concept \"X\" RELATED Concept").unwrap();
        assert_eq!(q2.related_via, None);
    }

    #[test]
    fn related_via_filters_by_edge_kind() {
        // A node with two outgoing edges of different types; VIA must select
        // only the matching relation — this is the first query path where the
        // §941 typed edges actually change the result set.
        let mut g = KnowledgeGraph::new();
        let ckos = g.add_node(NodeKind::Concept, "CKOS", 95);
        let sched = g.add_node(NodeKind::Concept, "Scheduler", 90);
        let paper = g.add_node(NodeKind::Concept, "Paper", 90);
        g.connect(&ckos, &sched, EdgeKind::DependsOn);
        g.connect(&ckos, &paper, EdgeKind::References);

        // VIA DependsOn → only the Scheduler.
        let dep = execute(
            &parse("FIND Concept \"CKOS\" RELATED Concept VIA DependsOn").unwrap(),
            &g,
        );
        assert_eq!(dep.related.len(), 1);
        assert_eq!(dep.related[0].label, "Scheduler");

        // VIA References → only the Paper.
        let refs = execute(
            &parse("FIND Concept \"CKOS\" RELATED Concept VIA References").unwrap(),
            &g,
        );
        assert_eq!(refs.related.len(), 1);
        assert_eq!(refs.related[0].label, "Paper");

        // No VIA → both neighbours (any edge kind).
        let any = execute(&parse("FIND Concept \"CKOS\" RELATED Concept").unwrap(), &g);
        assert_eq!(any.related.len(), 2);

        // `RELATED * VIA DependsOn` ignores neighbour kind, keeps the edge filter.
        let star = execute(
            &parse("FIND Concept \"CKOS\" RELATED * VIA DependsOn").unwrap(),
            &g,
        );
        assert_eq!(star.related.len(), 1);
        assert_eq!(star.related[0].label, "Scheduler");
    }

    #[test]
    fn related_dedups_by_node_identity_not_by_rendered_value() {
        // Two distinct nodes that happen to share label/kind/confidence (no
        // date/provenance to tell them apart) — a real, plausible knowledge-
        // graph situation (e.g. two different "Utils" or "Config" concepts).
        // Deduping RELATED results by the rendered NodeMatch value collapses
        // them into one, silently dropping a genuinely distinct node.
        let mut g = KnowledgeGraph::new();
        let root = g.add_node(NodeKind::Concept, "Root", 90);
        let u1 = g.add_node(NodeKind::Concept, "Utils", 80);
        let u2 = g.add_node(NodeKind::Concept, "Utils", 80);
        assert_ne!(u1, u2, "the two Utils nodes must be genuinely distinct");
        g.connect(&root, &u1, EdgeKind::References);
        g.connect(&root, &u2, EdgeKind::References);

        let result = execute(&parse("FIND Concept \"Root\" RELATED Concept").unwrap(), &g);
        assert_eq!(
            result.related.len(),
            2,
            "both distinct Utils nodes must survive, got {:?}",
            result.related
        );
    }

    #[test]
    fn related_via_and_plain_related_agree_on_self_loops() {
        // `RELATED <kind> VIA <edge>` (graph::neighbors_via) and plain
        // `RELATED <kind>` (graph::traverse, which excludes the start node by
        // seeding its BFS `visited` set with it) used to disagree on whether
        // a node's own self-loop counts as "related to itself": neighbors_via
        // has no such exclusion, so only the VIA path returned the query
        // subject as its own result. Self-loops are a legal graph shape
        // (`connect` has no from == to guard), so both paths must behave the
        // same way for one.
        let mut g = KnowledgeGraph::new();
        let a = g.add_node(NodeKind::Concept, "Self", 90);
        g.connect(&a, &a, EdgeKind::DependsOn);

        let no_via = execute(&parse("FIND Concept \"Self\" RELATED Concept").unwrap(), &g);
        let via = execute(
            &parse("FIND Concept \"Self\" RELATED Concept VIA DependsOn").unwrap(),
            &g,
        );
        assert_eq!(
            no_via.related.len(),
            via.related.len(),
            "VIA and non-VIA must agree on a self-loop: no_via={:?} via={:?}",
            no_via.related,
            via.related
        );
    }

    #[test]
    fn rejects_malformed_queries() {
        assert!(parse("").is_err());
        assert!(parse("RELATED Tool").is_err()); // must start with FIND
        assert!(parse("FIND").is_err()); // selector required
        assert!(parse("FIND Concept FILTER Confidence ! 5").is_err()); // bad op
        assert!(parse("FIND Concept \"x\" WOBBLE").is_err()); // unknown clause
    }

    #[test]
    fn executes_against_a_graph() {
        let mut g = KnowledgeGraph::new();
        let t = g.add_node(NodeKind::Concept, "Transformer", 95);
        let attn = g.add_node(NodeKind::Other("algorithm".into()), "Attention", 92);
        let low = g.add_node(NodeKind::Other("algorithm".into()), "Legacy", 40);
        g.connect(&t, &attn, EdgeKind::References);
        g.connect(&t, &low, EdgeKind::References);

        let q = parse("FIND Concept \"Transformer\" RELATED Algorithm FILTER Confidence >= 90")
            .unwrap();
        let res = execute(&q, &g);
        assert_eq!(res.primary.len(), 1);
        assert_eq!(res.primary[0].label, "Transformer");
        // Only the high-confidence algorithm passes the filter.
        assert_eq!(res.related.len(), 1);
        assert_eq!(res.related[0].label, "Attention");
    }

    #[test]
    fn parses_and_evaluates_compound_filters() {
        let mut g = KnowledgeGraph::new();
        g.add_node(NodeKind::Concept, "Low", 20);
        g.add_node(NodeKind::Concept, "Mid", 60);
        g.add_node(NodeKind::Concept, "High", 95);

        // Range via AND: only Mid (60) is in (50, 90).
        let q = parse("FIND Concept FILTER Confidence > 50 AND Confidence < 90").unwrap();
        let res = execute(&q, &g);
        assert_eq!(res.primary.len(), 1);
        assert_eq!(res.primary[0].label, "Mid");

        // OR of two ranges: Low (<30) or High (>90).
        let q =
            parse("FIND Concept FILTER Confidence < 30 OR Confidence > 90 ORDER BY Confidence ASC")
                .unwrap();
        let res = execute(&q, &g);
        let labels: Vec<&str> = res.primary.iter().map(|m| m.label.as_str()).collect();
        assert_eq!(labels, vec!["Low", "High"]);

        // NOT and parentheses: NOT (Confidence < 50) keeps Mid and High.
        let q = parse("FIND Concept FILTER NOT (Confidence < 50)").unwrap();
        let res = execute(&q, &g);
        let mut labels: Vec<&str> = res.primary.iter().map(|m| m.label.as_str()).collect();
        labels.sort_unstable();
        assert_eq!(labels, vec!["High", "Mid"]);
    }

    #[test]
    fn orders_and_limits_results() {
        let mut g = KnowledgeGraph::new();
        g.add_node(NodeKind::Concept, "Low", 30);
        g.add_node(NodeKind::Concept, "High", 95);
        g.add_node(NodeKind::Concept, "Mid", 60);

        // Default ordering is by descending confidence, deterministically.
        let res = execute(&parse("FIND Concept").unwrap(), &g);
        let labels: Vec<&str> = res.primary.iter().map(|m| m.label.as_str()).collect();
        assert_eq!(labels, vec!["High", "Mid", "Low"]);

        // LIMIT caps the set after ordering.
        let res = execute(&parse("FIND Concept LIMIT 2").unwrap(), &g);
        let labels: Vec<&str> = res.primary.iter().map(|m| m.label.as_str()).collect();
        assert_eq!(labels, vec!["High", "Mid"]);

        // ORDER BY Confidence ASC reverses it.
        let res = execute(
            &parse("FIND Concept ORDER BY Confidence ASC LIMIT 1").unwrap(),
            &g,
        );
        assert_eq!(res.primary.len(), 1);
        assert_eq!(res.primary[0].label, "Low");
    }

    #[test]
    fn rejects_bad_order_and_limit() {
        assert!(parse("FIND Concept ORDER BY Label").is_err()); // only Confidence
        assert!(parse("FIND Concept LIMIT abc").is_err()); // non-numeric
    }

    #[test]
    fn enforces_temporal_bounds_and_carries_provenance() {
        let mut g = KnowledgeGraph::new();
        let old = g.add_node(NodeKind::Concept, "Perceptron", 90);
        g.set_date(&old, "1958-01-01");
        g.set_provenance(&old, "paper:Rosenblatt");
        let new = g.add_node(NodeKind::Concept, "Transformer", 96);
        g.set_date(&new, "2017-06-12");
        g.set_provenance(&new, "paper:Vaswani");
        let undated = g.add_node(NodeKind::Concept, "Mystery", 99);

        // BEFORE 2000 keeps only the Perceptron; the undated node is excluded.
        let res = execute(&parse("FIND Concept BEFORE 2000-01-01").unwrap(), &g);
        assert_eq!(res.primary.len(), 1);
        assert_eq!(res.primary[0].label, "Perceptron");
        assert_eq!(
            res.primary[0].provenance.as_deref(),
            Some("paper:Rosenblatt")
        );
        let _ = undated;

        // AFTER 2000 keeps only the Transformer.
        let res = execute(&parse("FIND Concept AFTER 2000-01-01").unwrap(), &g);
        assert_eq!(res.primary.len(), 1);
        assert_eq!(res.primary[0].label, "Transformer");
    }
}
