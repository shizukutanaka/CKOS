//! Synonym-based query expansion (§949) — a dependency-free, partial answer to
//! the vocabulary-mismatch gap measured in `ckos_memory::embedding`'s module
//! doc: the default [`HashingEmbedder`](ckos_memory::HashingEmbedder) cannot
//! relate a paraphrase or synonym sharing no literal words with the query
//! (measured: a true paraphrase scored no higher than unrelated text), and
//! pseudo-relevance feedback ([`crate::retrieval::expand_query`]) only pulls
//! terms from documents *already found* by literal overlap — so it cannot
//! recall a document that shares zero terms with the query either.
//!
//! A [`SynonymTable`] sidesteps both limits for the specific terms it knows
//! about: it injects a priori related terms into the query *before* any
//! search runs, so a document written with different but related vocabulary
//! becomes reachable by BM25/RRF on the very first pass. It is not a
//! substitute for real semantic embeddings — only a curated, explicit
//! mapping — but for a known domain's core vocabulary it closes a real,
//! measured gap at zero dependency cost.

use crate::retrieval::s_stem;
use std::collections::{HashMap, HashSet};

/// A table mapping a term to other terms considered related/interchangeable
/// in this domain. Symmetric: inserting a group makes every term in it map to
/// every other term.
#[derive(Debug, Clone, Default)]
pub struct SynonymTable {
    map: HashMap<String, Vec<String>>,
}

impl SynonymTable {
    /// An empty table (no expansions).
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a group of mutually-related terms (case-insensitive): each
    /// term maps to every other term in the group. Call repeatedly to build up
    /// a table; later groups add to, rather than replace, earlier ones for a
    /// shared term.
    pub fn insert_group(&mut self, terms: &[&str]) {
        let lower: Vec<String> = terms.iter().map(|t| t.to_lowercase()).collect();
        for (i, term) in lower.iter().enumerate() {
            // Key by the stemmed form so a plural query reaches a table written
            // in the singular (and vice versa); values keep their natural
            // spelling, since they are injected back into the query text.
            let entry = self.map.entry(s_stem(term)).or_default();
            for (j, other) in lower.iter().enumerate() {
                if i != j && !entry.contains(other) {
                    entry.push(other.clone());
                }
            }
        }
    }

    /// Related terms for `term`, or an empty slice if none are known.
    /// Case-insensitive and morphology-insensitive: the lookup is stemmed the
    /// same way the retrieval tokenizer stems, so `caches` finds the entry
    /// registered as `cache`.
    pub fn related(&self, term: &str) -> &[String] {
        self.map
            .get(&s_stem(&term.to_lowercase()))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// A small built-in table of common software/orchestration vocabulary —
    /// exactly the domain this kernel's own sessions and documents use.
    /// Callers with a different domain should build their own via
    /// [`insert_group`](Self::insert_group).
    pub fn builtin() -> Self {
        let mut t = Self::new();
        for group in [
            &["scheduler", "dispatcher", "dispatch"][..],
            &["priority", "importance", "urgency"][..],
            &["task", "job", "work"][..],
            &["delete", "remove", "drop"][..],
            &["search", "retrieval", "query", "lookup"][..],
            &["config", "configuration", "settings"][..],
            &["error", "failure", "exception", "fault"][..],
            &["verify", "validate", "check"][..],
            &["memory", "storage", "store"][..],
            &["agent", "worker"][..],
            &["workflow", "pipeline"][..],
            &["graph", "network"][..],
            &["node", "vertex"][..],
            &["edge", "relation", "link"][..],
            &["rank", "score", "order"][..],
            &["cache", "buffer"][..],
            &["runtime", "engine"][..],
        ] {
            t.insert_group(group);
        }
        t
    }
}

/// Expand `query` with terms from `table` related to any of its own terms,
/// case-insensitively, up to `max_terms` additions. Unlike
/// [`crate::retrieval::expand_query`] (pseudo-relevance feedback, which mines
/// terms from documents already found), this injects terms the table knows
/// about regardless of what the corpus contains — closing the vocabulary gap
/// on the very first search pass. Deterministic: candidates are considered in
/// the query's own term order, falling back to alphabetical among a term's
/// own synonym list.
pub fn expand_query_with_synonyms(query: &str, table: &SynonymTable, max_terms: usize) -> String {
    if max_terms == 0 {
        return query.to_string();
    }
    // Stem exactly as `retrieval::tokens` does, so lookups and the
    // already-present check agree with the search that follows: without this a
    // plural query silently got no expansion at all, because the table is
    // written in the singular.
    let query_terms: Vec<String> = query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(s_stem)
        .filter(|t| t.chars().count() > 1)
        .collect();
    let existing: HashSet<String> = query_terms.iter().cloned().collect();

    let mut added = Vec::new();
    let mut seen: HashSet<String> = existing.clone();
    'outer: for term in &query_terms {
        let mut related: Vec<&String> = table.related(term).iter().collect();
        related.sort();
        for r in related {
            // Dedup on the stem so a synonym already present in another
            // inflection is not appended again.
            if seen.insert(s_stem(r)) {
                added.push(r.clone());
                if added.len() >= max_terms {
                    break 'outer;
                }
            }
        }
    }
    if added.is_empty() {
        query.to_string()
    } else {
        format!("{query} {}", added.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_group_is_symmetric_and_excludes_self() {
        let mut t = SynonymTable::new();
        t.insert_group(&["scheduler", "dispatcher"]);
        assert_eq!(t.related("scheduler"), &["dispatcher".to_string()]);
        assert_eq!(t.related("dispatcher"), &["scheduler".to_string()]);
        assert!(t.related("scheduler").iter().all(|s| s != "scheduler"));
    }

    #[test]
    fn related_is_case_insensitive_and_unknown_terms_are_empty() {
        let mut t = SynonymTable::new();
        t.insert_group(&["priority", "importance"]);
        assert_eq!(t.related("PRIORITY"), &["importance".to_string()]);
        assert!(t.related("nonexistent").is_empty());
    }

    #[test]
    fn a_plural_query_still_reaches_a_singular_table() {
        // Regression: the table is written in the singular and lookups used the
        // raw token, so `schedulers` matched no entry and got *no* expansion at
        // all, while `scheduler` got the full set — an arbitrary split that also
        // disagreed with the retrieval tokenizer, which stems both sides.
        let table = SynonymTable::builtin();
        let plural = expand_query_with_synonyms("schedulers", &table, 3);
        assert!(
            plural.contains("dispatcher") || plural.contains("dispatch"),
            "plural query must still expand: {plural}"
        );
        assert!(
            expand_query_with_synonyms("caches", &table, 3).contains("buffer"),
            "plural of a table key must reach its synonyms"
        );
        // The reverse: a singular query against a plural table entry.
        let mut t = SynonymTable::new();
        t.insert_group(&["queries", "lookups"]);
        assert!(!t.related("query").is_empty(), "singular must find plural");

        // A synonym already present in another inflection is not re-added.
        let expanded = expand_query_with_synonyms("caches cache", &table, 5);
        assert_eq!(
            expanded.matches("buffer").count(),
            1,
            "a synonym must be added once: {expanded}"
        );
    }

    #[test]
    fn expand_adds_related_terms_deterministically() {
        let table = SynonymTable::builtin();
        let expanded = expand_query_with_synonyms("scheduler priority", &table, 10);
        assert!(expanded.contains("dispatcher") || expanded.contains("dispatch"));
        assert!(expanded.contains("importance"));
        // Re-running produces byte-identical output.
        assert_eq!(
            expanded,
            expand_query_with_synonyms("scheduler priority", &table, 10)
        );
    }

    #[test]
    fn expand_respects_max_terms_and_empty_table() {
        let table = SynonymTable::builtin();
        let capped = expand_query_with_synonyms("scheduler", &table, 1);
        assert_eq!(capped.split_whitespace().count(), 2); // original + 1 addition
        let empty = SynonymTable::new();
        assert_eq!(
            expand_query_with_synonyms("scheduler", &empty, 5),
            "scheduler"
        );
        assert_eq!(
            expand_query_with_synonyms("scheduler", &table, 0),
            "scheduler"
        );
    }

    #[test]
    fn closes_the_measured_vocabulary_gap() {
        // The exact scenario from the embedding module's boundary test: a true
        // paraphrase sharing zero content words with the query. Plain keyword
        // matching (BM25) finds nothing; synonym expansion bridges it.
        use crate::retrieval::Retriever;
        use ckos_graph::KnowledgeGraph;
        use ckos_memory::{Document, InMemoryStore, Storage};

        let mut store = InMemoryStore::new();
        store
            .write(Document::new(
                "note",
                "Task Prioritization",
                "Ready work is ordered and run according to importance.",
            ))
            .unwrap();
        let graph = KnowledgeGraph::new();
        let retriever = Retriever::new(&store, &graph);

        // The unexpanded query shares no content words with the document.
        let baseline = retriever.search("scheduler priority", 10);
        assert!(
            baseline.is_empty(),
            "baseline should find nothing: zero term overlap"
        );

        // Expanding with the builtin table injects "importance" (via
        // priority) and "work"/"job" (via task, if mentioned) — enough for
        // BM25 to recall the paraphrase on the very first pass.
        let table = SynonymTable::builtin();
        let expanded_query = expand_query_with_synonyms("scheduler priority", &table, 10);
        let expanded = retriever.search(&expanded_query, 10);
        assert!(
            expanded.iter().any(|h| h.title == "Task Prioritization"),
            "synonym expansion should recall the paraphrase; got {expanded:?}"
        );
    }
}
