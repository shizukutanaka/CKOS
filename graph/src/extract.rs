//! # Automatic knowledge-graph extraction (§941)
//!
//! A dependency-free, heuristic entity/relation extractor. It turns free text
//! into [`Concept`](crate::NodeKind::Concept) nodes and typed edges so the graph
//! can be seeded automatically from documents (§938 index pipeline) rather than
//! only by hand. The heuristics are deliberately simple and std-only:
//!
//! * **Entities** — maximal runs of capitalized words (`Knowledge Graph`) and
//!   all-caps acronyms (`CKOS`, `API`). Common capitalized stop-words at the
//!   start of a sentence (`The`, `This`, …) are filtered out. Entities whose
//!   last word is an organization marker (`Corp`, `Institute`, …) are typed as
//!   `Organization`; everything else defaults to `Concept`.
//! * **Relations** — the connective text between two adjacent entities is mapped
//!   to a typed edge (`depends on` → `DependsOn`, `implements` → `Implements`,
//!   `created by` → `CreatedBy`, `uses`/`references` → `References`); any other
//!   co-occurring pair falls back to `RelatedTo`.
//! * **Confidence** — scales with how often an entity is seen, so terms that
//!   recur across the corpus are trusted more (§948).
//!
//! A statistical model (spaCy/NER, an LLM extractor) plugs in behind the same
//! [`KnowledgeGraph::extract_concepts`] surface when one is available; this
//! gives the platform a working default in the meantime.

use crate::{EdgeKind, KnowledgeGraph, NodeKind};
use ckos_kernel::NodeId;
use std::collections::HashMap;

/// What an extraction pass produced.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ExtractReport {
    /// Number of new entity nodes created.
    pub nodes_added: usize,
    /// Number of existing nodes whose mention count (and confidence) grew.
    pub nodes_reinforced: usize,
    /// Number of co-occurrence edges created.
    pub edges_added: usize,
}

/// Capitalized words that are almost never entities on their own; filtered when
/// they appear at a sentence boundary so "The Scheduler" yields "Scheduler".
const STOP_WORDS: &[&str] = &[
    "The", "This", "That", "These", "Those", "A", "An", "It", "We", "You", "They", "He", "She",
    "I", "If", "When", "While", "And", "But", "Or", "So", "Then", "There", "Here", "Each", "Every",
    "Some", "Any", "All", "No", "Not", "For", "As", "At", "By", "In", "On", "To", "Of", "Its",
];

fn is_stop(word: &str) -> bool {
    STOP_WORDS.contains(&word)
}

/// Strip leading/trailing punctuation but keep internal characters (so
/// "CKOS's" -> "CKOS's" trimmed to "CKOS's"; "(API)" -> "API").
fn trim_token(tok: &str) -> &str {
    tok.trim_matches(|c: char| !c.is_alphanumeric())
}

/// Whether a cleaned token looks like the start/continuation of an entity.
fn is_entity_token(tok: &str) -> bool {
    let mut chars = tok.chars();
    match chars.next() {
        Some(c) if c.is_uppercase() => {
            // Single uppercase letter ("A", "I") is too noisy unless it's a
            // multi-letter acronym; require length >= 2.
            tok.chars().count() >= 2
        }
        _ => false,
    }
}

/// A parsed sentence: the entity phrases in order plus the connective text
/// between each consecutive pair (`gaps[i]` lies between `entities[i]` and
/// `entities[i+1]`), used to type the relation edge.
struct SentenceParse {
    entities: Vec<String>,
    gaps: Vec<String>,
}

/// Drop leading stop-words from a candidate entity run ("The Scheduler" ->
/// "Scheduler"); returns the joined phrase or `None` if nothing remains.
fn finish_entity(current: &mut Vec<&str>) -> Option<String> {
    while current.first().map(|w| is_stop(w)).unwrap_or(false) {
        current.remove(0);
    }
    let phrase = if current.is_empty() {
        None
    } else {
        Some(current.join(" "))
    };
    current.clear();
    phrase
}

/// Extract entity phrases and the connective text between them from a sentence.
fn parse_sentence(sentence: &str) -> SentenceParse {
    let mut entities: Vec<String> = Vec::new();
    let mut gaps: Vec<String> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    let mut gap: Vec<String> = Vec::new();

    for raw in sentence.split_whitespace() {
        let tok = trim_token(raw);
        if is_entity_token(tok) {
            current.push(tok);
        } else {
            if let Some(e) = finish_entity(&mut current) {
                if !entities.is_empty() {
                    gaps.push(std::mem::take(&mut gap).join(" "));
                }
                entities.push(e);
            }
            // Connective words count toward the gap once we have an entity.
            if !entities.is_empty() && !tok.is_empty() {
                gap.push(tok.to_lowercase());
            }
        }
    }
    if let Some(e) = finish_entity(&mut current) {
        if !entities.is_empty() {
            gaps.push(std::mem::take(&mut gap).join(" "));
        }
        entities.push(e);
    }
    SentenceParse { entities, gaps }
}

/// Map connective text between two entities to a typed relation (§897 edges).
/// Falls back to `RelatedTo` for plain co-occurrence.
fn relation_for(gap: &str) -> EdgeKind {
    if gap.contains("created by")
        || gap.contains("authored by")
        || gap.contains("developed by")
        || gap.contains("written by")
        || gap.contains("built by")
        || gap.contains("made by")
    {
        EdgeKind::CreatedBy
    } else if gap.contains("depend") || gap.contains("require") || gap.contains("needs") {
        EdgeKind::DependsOn
    } else if gap.contains("implement") || gap.contains("provides") || gap.contains("realizes") {
        EdgeKind::Implements
    } else if gap.contains("reference")
        || gap.contains("uses")
        || gap.contains("calls")
        || gap.contains("cites")
        || gap.contains("invokes")
    {
        EdgeKind::References
    } else {
        EdgeKind::RelatedTo
    }
}

/// Split text into sentences on terminators and newlines.
fn sentences(text: &str) -> Vec<&str> {
    text.split(['.', '!', '?', '\n', ';'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// Confidence for an entity seen `count` times: starts at 45, +15 per extra
/// mention, capped at 100.
fn confidence_for(count: usize) -> u8 {
    (45 + (count.saturating_sub(1) * 15)).min(100) as u8
}

/// Final-word markers that reliably indicate an organization (§897). Matched
/// case-insensitively against the last word of an entity phrase, so the rule is
/// conservative — "Knowledge Graph" or "Transformer" stay [`NodeKind::Concept`].
const ORG_SUFFIXES: &[&str] = &[
    "inc",
    "corp",
    "corporation",
    "llc",
    "ltd",
    "co",
    "company",
    "foundation",
    "institute",
    "university",
    "college",
    "lab",
    "labs",
    "group",
    "systems",
    "technologies",
    "industries",
    "ventures",
    "partners",
    "association",
    "consortium",
    "organization",
];

/// Classify an entity label into a [`NodeKind`]. Defaults to
/// [`NodeKind::Concept`]; only promotes to [`NodeKind::Organization`] when the
/// last word is an unambiguous organization marker, keeping false positives low.
fn classify(label: &str) -> NodeKind {
    let last = label
        .split_whitespace()
        .next_back()
        .unwrap_or("")
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    if ORG_SUFFIXES.contains(&last.as_str()) {
        NodeKind::Organization
    } else {
        NodeKind::Concept
    }
}

impl KnowledgeGraph {
    /// Heuristically extract concept nodes and co-occurrence edges from `text`,
    /// accumulating into this graph (§941). Entities already present (matched by
    /// label, case-insensitively) are reinforced — their mention count grows and
    /// confidence is bumped — instead of being duplicated, so calling this across
    /// a corpus builds a richer graph over time.
    ///
    /// Returns an [`ExtractReport`] describing what changed.
    pub fn extract_concepts(&mut self, text: &str) -> ExtractReport {
        self.extract_concepts_with_provenance(text, None)
    }

    /// Like [`extract_concepts`](Self::extract_concepts), but stamps every
    /// *newly created* node with `provenance` (§947) — e.g. the document, run or
    /// session the knowledge came from. Reinforced nodes keep their original
    /// source. `None` leaves new nodes unsourced.
    pub fn extract_concepts_with_provenance(
        &mut self,
        text: &str,
        provenance: Option<&str>,
    ) -> ExtractReport {
        // Existing label (lowercased) -> node id, so repeated runs reuse nodes.
        let mut index: HashMap<String, NodeId> = self
            .nodes()
            .map(|n| (n.label.to_lowercase(), n.id.clone()))
            .collect();

        // Count mentions across the whole text first, so confidence reflects the
        // full corpus rather than first-seen order.
        let mut counts: HashMap<String, usize> = HashMap::new();
        let parsed: Vec<SentenceParse> = sentences(text)
            .into_iter()
            .map(|s| {
                let parse = parse_sentence(s);
                for e in &parse.entities {
                    *counts.entry(e.to_lowercase()).or_default() += 1;
                }
                parse
            })
            .collect();

        let mut report = ExtractReport::default();
        // Track edges we have already drawn to avoid duplicates — seeded with
        // the edges already in the graph, so re-extracting the same corpus
        // (e.g. every `ckos run --session` over a persisted graph) reinforces
        // nodes without accumulating parallel copies of the same edge, which
        // would skew PageRank centrality (§951) and grow the store unboundedly.
        let mut edge_seen: std::collections::HashSet<(NodeId, NodeId)> = self
            .edges()
            .map(|e| (e.from.clone(), e.to.clone()))
            .collect();

        // First ensure every entity has a node with the right confidence.
        let mut label_for: HashMap<String, NodeId> = HashMap::new();
        for (lower, count) in &counts {
            let conf = confidence_for(*count);
            if let Some(existing) = index.get(lower) {
                self.bump_confidence(existing, conf);
                report.nodes_reinforced += 1;
                label_for.insert(lower.clone(), existing.clone());
            } else {
                // Recover a display label (first-seen casing) from the parses.
                let display = parsed
                    .iter()
                    .flat_map(|p| &p.entities)
                    .find(|e| e.to_lowercase() == *lower)
                    .cloned()
                    .unwrap_or_else(|| lower.clone());
                let id = self.add_node(classify(&display), display, conf);
                if let Some(src) = provenance {
                    self.set_provenance(&id, src);
                }
                index.insert(lower.clone(), id.clone());
                label_for.insert(lower.clone(), id.clone());
                report.nodes_added += 1;
            }
        }

        // Draw edges per sentence. Adjacent entities get a *typed* relation
        // inferred from the connective text between them; remaining co-occurring
        // pairs fall back to RelatedTo. Typed edges are added first so the
        // (from, to) dedup keeps the more specific kind.
        for parse in &parsed {
            // Resolve every mention to its node id, in order (with duplicates,
            // so gap alignment holds), then a deduped set for co-occurrence.
            let ordered: Vec<NodeId> = parse
                .entities
                .iter()
                .filter_map(|e| label_for.get(&e.to_lowercase()).cloned())
                .collect();

            // 1) Typed adjacent relations.
            for i in 0..ordered.len().saturating_sub(1) {
                let (from, to) = (&ordered[i], &ordered[i + 1]);
                if from == to {
                    continue;
                }
                let gap = parse.gaps.get(i).map(String::as_str).unwrap_or("");
                if edge_seen.insert((from.clone(), to.clone())) {
                    self.connect(from, to, relation_for(gap));
                    report.edges_added += 1;
                }
            }

            // 2) Co-occurrence fallback for any pair not already linked.
            let unique: Vec<NodeId> = {
                let mut seen = std::collections::HashSet::new();
                ordered
                    .iter()
                    .filter(|id| seen.insert((*id).clone()))
                    .cloned()
                    .collect()
            };
            for i in 0..unique.len() {
                for j in (i + 1)..unique.len() {
                    let pair = (unique[i].clone(), unique[j].clone());
                    if edge_seen.insert(pair) {
                        self.connect(&unique[i], &unique[j], EdgeKind::RelatedTo);
                        report.edges_added += 1;
                    }
                }
            }
        }

        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_entities_and_filters_stopwords() {
        let mut g = KnowledgeGraph::new();
        let r = g.extract_concepts("The Scheduler dispatches tasks. CKOS uses a Knowledge Graph.");
        // "The" is dropped; "Scheduler", "CKOS", "Knowledge Graph" remain.
        let labels: Vec<String> = g.nodes().map(|n| n.label.clone()).collect();
        assert!(labels.iter().any(|l| l == "Scheduler"));
        assert!(labels.iter().any(|l| l == "CKOS"));
        assert!(labels.iter().any(|l| l == "Knowledge Graph"));
        assert!(!labels.iter().any(|l| l.starts_with("The")));
        assert_eq!(r.nodes_added, 3);
    }

    #[test]
    fn co_occurring_entities_are_linked() {
        let mut g = KnowledgeGraph::new();
        g.extract_concepts("The Scheduler dispatches tasks to the Runtime.");
        // Two entities, no relation verb -> a single RelatedTo co-occurrence edge.
        assert_eq!(g.edges().count(), 1);
        assert_eq!(
            g.edges().filter(|e| e.kind == EdgeKind::RelatedTo).count(),
            1
        );
    }

    #[test]
    fn organization_suffixes_are_classified() {
        let mut g = KnowledgeGraph::new();
        g.extract_concepts(
            "The Allen Institute studies AI. Acme Corp ships products. \
             The Transformer is a model.",
        );
        let kind = |label: &str| g.nodes().find(|n| n.label == label).map(|n| n.kind.clone());
        assert_eq!(kind("Allen Institute"), Some(NodeKind::Organization));
        assert_eq!(kind("Acme Corp"), Some(NodeKind::Organization));
        // Plain concepts are not misclassified.
        assert_eq!(kind("Transformer"), Some(NodeKind::Concept));
    }

    #[test]
    fn provenance_is_stamped_on_new_nodes_only() {
        let mut g = KnowledgeGraph::new();
        g.extract_concepts_with_provenance("CKOS is great.", Some("doc:intro"));
        let ckos = g.nodes().find(|n| n.label == "CKOS").unwrap();
        assert_eq!(ckos.provenance.as_deref(), Some("doc:intro"));

        // A later pass from a different source reinforces CKOS but keeps its
        // original provenance, while a brand-new node gets the new source.
        g.extract_concepts_with_provenance("CKOS ships Telemetry.", Some("doc:v2"));
        let ckos = g.nodes().find(|n| n.label == "CKOS").unwrap();
        assert_eq!(ckos.provenance.as_deref(), Some("doc:intro"));
        let tel = g.nodes().find(|n| n.label == "Telemetry").unwrap();
        assert_eq!(tel.provenance.as_deref(), Some("doc:v2"));
    }

    #[test]
    fn relation_verbs_produce_typed_edges() {
        let mut g = KnowledgeGraph::new();
        g.extract_concepts(
            "CKOS depends on the Scheduler. The Engine implements the Runtime. \
             The Transformer was created by Vaswani. The Planner uses the Graph.",
        );
        let kinds: Vec<EdgeKind> = g.edges().map(|e| e.kind.clone()).collect();
        assert!(kinds.contains(&EdgeKind::DependsOn));
        assert!(kinds.contains(&EdgeKind::Implements));
        assert!(kinds.contains(&EdgeKind::CreatedBy));
        assert!(kinds.contains(&EdgeKind::References));
        // Every pair had a recognized verb, so none fell back to RelatedTo.
        assert_eq!(
            kinds.iter().filter(|k| **k == EdgeKind::RelatedTo).count(),
            0
        );
    }

    #[test]
    fn repeated_mentions_reinforce_confidence() {
        let mut g = KnowledgeGraph::new();
        g.extract_concepts("CKOS is fast. CKOS is safe. CKOS is open.");
        let node = g.nodes().find(|n| n.label == "CKOS").unwrap();
        // Seen 3 times -> 45 + 2*15 = 75.
        assert_eq!(node.confidence, 75);

        // A second pass reinforces the existing node rather than duplicating it.
        let before = g.len();
        let r = g.extract_concepts("CKOS ships.");
        assert_eq!(g.len(), before);
        assert_eq!(r.nodes_added, 0);
        assert_eq!(r.nodes_reinforced, 1);
    }

    #[test]
    fn re_extraction_does_not_duplicate_edges() {
        // Regression: edge_seen used to start empty each pass, so re-running
        // extraction over a persisted graph appended a parallel copy of every
        // edge per run (observed empirically: two passes -> 2 identical
        // CKOS->Scheduler edges), inflating PageRank weight and the .kg file.
        let mut g = KnowledgeGraph::new();
        let first = g.extract_concepts("CKOS depends on the Scheduler.");
        assert_eq!(first.edges_added, 1);
        assert_eq!(g.edges().count(), 1);

        let second = g.extract_concepts("CKOS depends on the Scheduler.");
        assert_eq!(second.edges_added, 0);
        assert_eq!(second.nodes_reinforced, 2);
        assert_eq!(g.edges().count(), 1, "identical edge must not accumulate");
    }

    #[test]
    fn acronyms_kept_single_letters_dropped() {
        let mut g = KnowledgeGraph::new();
        g.extract_concepts("API and I work. A test of TLS.");
        let labels: Vec<String> = g.nodes().map(|n| n.label.clone()).collect();
        assert!(labels.iter().any(|l| l == "API"));
        assert!(labels.iter().any(|l| l == "TLS"));
        assert!(!labels.iter().any(|l| l == "I"));
        assert!(!labels.iter().any(|l| l == "A"));
    }
}
