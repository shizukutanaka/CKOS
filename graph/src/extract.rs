//! # Automatic knowledge-graph extraction (§941)
//!
//! A dependency-free, heuristic entity/relation extractor. It turns free text
//! into [`Concept`](crate::NodeKind::Concept) nodes and `RelatedTo` edges so the
//! graph can be seeded automatically from documents (§938 index pipeline) rather
//! than only by hand. The heuristics are deliberately simple and std-only:
//!
//! * **Entities** — maximal runs of capitalized words (`Knowledge Graph`) and
//!   all-caps acronyms (`CKOS`, `API`). Common capitalized stop-words at the
//!   start of a sentence (`The`, `This`, …) are filtered out.
//! * **Relations** — two distinct entities mentioned in the same sentence are
//!   linked with a `RelatedTo` edge (co-occurrence).
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

/// Extract maximal capitalized phrases from one sentence.
fn entities_in_sentence(sentence: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    let tokens: Vec<&str> = sentence.split_whitespace().collect();

    let flush = |current: &mut Vec<&str>, out: &mut Vec<String>| {
        if current.is_empty() {
            return;
        }
        // Drop a leading stop-word ("The Scheduler" -> "Scheduler").
        while current.first().map(|w| is_stop(w)).unwrap_or(false) {
            current.remove(0);
        }
        if !current.is_empty() {
            out.push(current.join(" "));
        }
        current.clear();
    };

    for raw in tokens {
        let tok = trim_token(raw);
        if is_entity_token(tok) {
            current.push(tok);
        } else {
            flush(&mut current, &mut out);
        }
    }
    flush(&mut current, &mut out);
    out
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

impl KnowledgeGraph {
    /// Heuristically extract concept nodes and co-occurrence edges from `text`,
    /// accumulating into this graph (§941). Entities already present (matched by
    /// label, case-insensitively) are reinforced — their mention count grows and
    /// confidence is bumped — instead of being duplicated, so calling this across
    /// a corpus builds a richer graph over time.
    ///
    /// Returns an [`ExtractReport`] describing what changed.
    pub fn extract_concepts(&mut self, text: &str) -> ExtractReport {
        // Existing label (lowercased) -> node id, so repeated runs reuse nodes.
        let mut index: HashMap<String, NodeId> = self
            .nodes()
            .map(|n| (n.label.to_lowercase(), n.id.clone()))
            .collect();

        // Count mentions across the whole text first, so confidence reflects the
        // full corpus rather than first-seen order.
        let mut counts: HashMap<String, usize> = HashMap::new();
        let parsed: Vec<Vec<String>> = sentences(text)
            .into_iter()
            .map(|s| {
                let ents = entities_in_sentence(s);
                for e in &ents {
                    *counts.entry(e.to_lowercase()).or_default() += 1;
                }
                ents
            })
            .collect();

        let mut report = ExtractReport::default();
        // Track edges we have already drawn this pass to avoid duplicates.
        let mut edge_seen: std::collections::HashSet<(NodeId, NodeId)> =
            std::collections::HashSet::new();

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
                    .flatten()
                    .find(|e| e.to_lowercase() == *lower)
                    .cloned()
                    .unwrap_or_else(|| lower.clone());
                let id = self.add_node(NodeKind::Concept, display, conf);
                index.insert(lower.clone(), id.clone());
                label_for.insert(lower.clone(), id.clone());
                report.nodes_added += 1;
            }
        }

        // Then draw co-occurrence edges within each sentence.
        for ents in &parsed {
            let ids: Vec<NodeId> = {
                let mut seen = std::collections::HashSet::new();
                ents.iter()
                    .filter_map(|e| label_for.get(&e.to_lowercase()).cloned())
                    .filter(|id| seen.insert(id.clone()))
                    .collect()
            };
            for i in 0..ids.len() {
                for j in (i + 1)..ids.len() {
                    let pair = (ids[i].clone(), ids[j].clone());
                    if edge_seen.insert(pair.clone()) {
                        self.connect(&ids[i], &ids[j], EdgeKind::RelatedTo);
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
        g.extract_concepts("CKOS depends on the Scheduler.");
        // Two entities in one sentence -> one RelatedTo edge.
        assert_eq!(
            g.edges().filter(|e| e.kind == EdgeKind::RelatedTo).count(),
            1
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
