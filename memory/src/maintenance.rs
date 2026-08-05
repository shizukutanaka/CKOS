//! Memory maintenance: garbage collection (§954) and semantic compression
//! (§940, §953).
//!
//! [`collect`] sweeps a [`Storage`] backend and removes documents that are no
//! longer worth keeping — expired, low-confidence, duplicate, or carrying a
//! broken embedding. [`summarize`]/[`compress_document`] shrink a document's
//! body toward a summary, the first step of the spec's full → summary → concept
//! → knowledge compression ladder.

use crate::{Document, Query, Storage};
use ckos_kernel::error::Result;
use ckos_kernel::DocumentId;
use std::collections::HashSet;

/// Why the garbage collector removed a document (§954).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcReason {
    /// An `expires` metadata date is at or before `now`.
    Expired,
    /// Confidence is below the policy threshold.
    LowConfidence,
    /// Another copy with the same (type, title, body) was kept — the one with
    /// the highest confidence (ties broken by id, so the choice is stable).
    Duplicate,
    /// The embedding is empty, wrong-dimension, all-zero, or non-finite.
    BrokenEmbedding,
}

/// Tunable garbage-collection policy. Default is conservative: keep everything
/// except exact duplicates and broken embeddings.
#[derive(Debug, Clone)]
pub struct GcPolicy {
    /// Remove documents with confidence strictly below this (0 disables).
    pub min_confidence: u8,
    /// For each group of documents sharing (doc_type, title, body), keep the
    /// highest-confidence copy and remove the rest.
    pub drop_duplicates: bool,
    /// Remove documents whose stored embedding is broken.
    pub drop_broken_embeddings: bool,
    /// If set, an embedding of a different length counts as broken.
    pub expected_dim: Option<usize>,
    /// Remove documents whose `expires` metadata date is <= `now`.
    pub drop_expired: bool,
}

impl Default for GcPolicy {
    fn default() -> Self {
        GcPolicy {
            min_confidence: 0,
            drop_duplicates: true,
            drop_broken_embeddings: true,
            expected_dim: None,
            drop_expired: true,
        }
    }
}

/// What a GC run removed.
#[derive(Debug, Clone, Default)]
pub struct GcReport {
    /// (document id, reason) for each removed document.
    pub removed: Vec<(DocumentId, GcReason)>,
}

impl GcReport {
    /// Number of documents removed.
    pub fn count(&self) -> usize {
        self.removed.len()
    }
}

fn embedding_broken(emb: &[f32], expected: Option<usize>) -> bool {
    emb.is_empty()
        || expected.is_some_and(|d| emb.len() != d)
        || emb.iter().any(|x| !x.is_finite())
        || emb.iter().all(|x| *x == 0.0)
}

/// Decide why a document should be removed, if at all (priority order).
fn removal_reason(
    doc: &Document,
    policy: &GcPolicy,
    now: Option<&str>,
    seen: &mut HashSet<(String, String, String)>,
) -> Option<GcReason> {
    if policy.drop_expired {
        if let (Some(now), Some(expires)) = (now, doc.metadata.get("expires")) {
            // ISO dates compare correctly as strings.
            if expires.as_str() <= now {
                return Some(GcReason::Expired);
            }
        }
    }
    if policy.min_confidence > 0 && doc.confidence < policy.min_confidence {
        return Some(GcReason::LowConfidence);
    }
    if policy.drop_broken_embeddings {
        if let Some(emb) = &doc.embedding {
            if embedding_broken(emb, policy.expected_dim) {
                return Some(GcReason::BrokenEmbedding);
            }
        }
    }
    if policy.drop_duplicates {
        let key = (doc.doc_type.clone(), doc.title.clone(), doc.body.clone());
        if !seen.insert(key) {
            return Some(GcReason::Duplicate);
        }
    }
    None
}

/// Run garbage collection over a store (§954). `now` is the current ISO date
/// used for expiry checks; pass `None` to skip expiry regardless of policy.
///
/// Deterministic: duplicates are resolved in a fixed order (see below), so two
/// runs over equivalent stores remove the same documents.
pub fn collect(store: &mut dyn Storage, policy: &GcPolicy, now: Option<&str>) -> Result<GcReport> {
    let mut docs = store.search(&Query::default())?;
    // Backends return documents in unspecified order — both `InMemoryStore`
    // and `FileStore` iterate a `HashMap` — while duplicate detection keeps
    // whichever copy it sees *first*. Without an explicit order, which
    // duplicate survives varies from run to run, so gc could silently discard
    // the higher-confidence copy (and a different document each time, since
    // copies sharing (type, title, body) can still differ in id, confidence,
    // metadata and embedding). Sort so the best copy is the one kept, with id
    // as a stable tie-break.
    docs.sort_by(|a, b| {
        b.confidence
            .cmp(&a.confidence)
            .then_with(|| a.id.cmp(&b.id))
    });
    let mut seen = HashSet::new();
    let mut report = GcReport::default();
    for doc in &docs {
        if let Some(reason) = removal_reason(doc, policy, now, &mut seen) {
            store.delete(&doc.id)?;
            report.removed.push((doc.id.clone(), reason));
        }
    }
    Ok(report)
}

/// Summarise text to at most `max_chars`, preferring to cut at a sentence
/// boundary, appending an ellipsis when truncated (§940).
pub fn summarize(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= max_chars {
        return trimmed.to_string();
    }
    let head = &chars[..max_chars];
    // Prefer the last sentence terminator within the window. Indexed in
    // chars, not bytes: `。` (U+3002, the CJK full stop this deliberately
    // supports) is 3 bytes wide, so a byte-index cut through the middle of
    // it would panic on the non-char-boundary slice below.
    let cut = head
        .iter()
        .rposition(|c| matches!(c, '.' | '!' | '?' | '。'))
        .map(|i| i + 1)
        .filter(|&i| i >= max_chars / 2)
        .unwrap_or(head.len());
    let collected: String = head[..cut].iter().collect();
    let mut out = collected.trim_end().to_string();
    out.push('…');
    out
}

/// Extract up to `top_n` "concept" keywords from text — the concept tier of the
/// §940 compression ladder (full-text → summary → concept → knowledge). Words
/// shorter than 4 characters and a small stop-word set are ignored; results are
/// ranked by frequency, ties broken alphabetically for determinism.
pub fn keywords(text: &str, top_n: usize) -> Vec<String> {
    const STOP: &[&str] = &[
        "this", "that", "with", "from", "have", "will", "into", "over", "they", "them", "then",
        "than", "your", "which", "their", "there", "about", "would", "could", "should", "been",
        "were", "what", "when", "where",
    ];
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for word in text.split(|c: char| !c.is_alphanumeric()) {
        let w = word.to_lowercase();
        if w.len() >= 4 && !STOP.contains(&w.as_str()) {
            *counts.entry(w).or_insert(0) += 1;
        }
    }
    let mut ranked: Vec<(String, usize)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked.into_iter().take(top_n).map(|(w, _)| w).collect()
}

/// Compress a document's body in place to a summary, recording the extracted
/// concepts, and tagging it so the compression is auditable and not repeated
/// (§940 summary + concept tiers, §953).
pub fn compress_document(doc: &mut Document, max_chars: usize) {
    if doc.metadata.contains_key("compressed") {
        return;
    }
    let original_len = doc.body.chars().count();
    if original_len <= max_chars {
        return;
    }
    let concepts = keywords(&doc.body, 5);
    doc.body = summarize(&doc.body, max_chars);
    doc.metadata.insert("compressed".into(), "true".into());
    doc.metadata
        .insert("original_len".into(), original_len.to_string());
    if !concepts.is_empty() {
        doc.metadata.insert("concepts".into(), concepts.join(","));
    }
}

/// Consolidation pass (§953, the "sleep phase"): compress every stored document
/// whose body exceeds `max_chars` and write it back, moving bulky working memory
/// toward compact long-term memory. Already-compressed documents are skipped.
/// Returns the number compressed.
pub fn consolidate(store: &mut dyn Storage, max_chars: usize) -> Result<usize> {
    let docs = store.search(&Query::default())?;
    let mut count = 0;
    for mut doc in docs {
        if doc.metadata.contains_key("compressed") || doc.body.chars().count() <= max_chars {
            continue;
        }
        compress_document(&mut doc, max_chars);
        store.write(doc)?;
        count += 1;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryStore;

    #[test]
    fn duplicate_survivor_is_deterministic_and_the_best_copy() {
        // Regression: `collect` consumed the backend's `search` order, but both
        // in-tree backends iterate a `HashMap`, so *which* copy survived varied
        // between runs — silently discarding a different document each time,
        // including the higher-confidence one. Copies sharing (type, title,
        // body) can still differ in id, confidence, metadata and embedding, so
        // this is real data loss, not a no-op choice. Repeated over fresh
        // stores (each with its own hash seed) and with both insertion orders,
        // the highest-confidence copy must always be the survivor.
        for flip in [false, true] {
            for _ in 0..50 {
                let mut store = InMemoryStore::new();
                let mut low = Document::new("note", "same", "same body");
                low.confidence = 10;
                let mut high = Document::new("note", "same", "same body");
                high.confidence = 100;
                if flip {
                    store.write(high).unwrap();
                    store.write(low).unwrap();
                } else {
                    store.write(low).unwrap();
                    store.write(high).unwrap();
                }

                let report = collect(&mut store, &GcPolicy::default(), None).unwrap();
                assert_eq!(report.count(), 1);
                assert_eq!(report.removed[0].1, GcReason::Duplicate);
                let left = store.search(&Query::default()).unwrap();
                assert_eq!(left.len(), 1);
                assert_eq!(
                    left[0].confidence, 100,
                    "gc must keep the highest-confidence duplicate, deterministically"
                );
            }
        }
    }

    #[test]
    fn removes_duplicates_keeping_first() {
        let mut store = InMemoryStore::new();
        store.write(Document::new("note", "a", "same")).unwrap();
        store.write(Document::new("note", "a", "same")).unwrap();
        store.write(Document::new("note", "b", "other")).unwrap();
        let report = collect(&mut store, &GcPolicy::default(), None).unwrap();
        assert_eq!(report.count(), 1);
        assert_eq!(report.removed[0].1, GcReason::Duplicate);
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn removes_low_confidence_and_broken_embeddings() {
        let mut store = InMemoryStore::new();
        let mut weak = Document::new("note", "weak", "x");
        weak.confidence = 10;
        let mut broken = Document::new("note", "broken", "y");
        broken.embedding = Some(vec![0.0, 0.0, 0.0]); // all-zero → broken
        let good = Document::new("note", "good", "z");
        store.write(weak).unwrap();
        store.write(broken).unwrap();
        store.write(good).unwrap();

        let policy = GcPolicy {
            min_confidence: 50,
            ..GcPolicy::default()
        };
        let report = collect(&mut store, &policy, None).unwrap();
        assert_eq!(report.count(), 2);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn removes_expired_documents() {
        let mut store = InMemoryStore::new();
        let mut old = Document::new("note", "old", "x");
        old.metadata.insert("expires".into(), "2024-01-01".into());
        let mut fresh = Document::new("note", "fresh", "y");
        fresh.metadata.insert("expires".into(), "2099-01-01".into());
        store.write(old).unwrap();
        store.write(fresh).unwrap();

        let report = collect(&mut store, &GcPolicy::default(), Some("2026-06-29")).unwrap();
        assert_eq!(report.count(), 1);
        assert_eq!(report.removed[0].1, GcReason::Expired);
    }

    #[test]
    fn summarize_cuts_at_sentence_boundary() {
        let text = "First sentence here. Second sentence is longer and runs on and on.";
        let s = summarize(text, 30);
        assert!(s.ends_with('…'));
        assert!(s.starts_with("First sentence here."));
        // Short text is returned unchanged.
        assert_eq!(summarize("short", 30), "short");
    }

    #[test]
    fn summarize_does_not_panic_on_a_cjk_full_stop() {
        // `。` (U+3002) is 3 bytes in UTF-8; a byte-index cut computed from
        // `str::rfind` lands mid-character and panics on the slice below it.
        // Indexing in chars throughout avoids that entirely.
        let text = "这是一个句子。这是另一个句子用来填充长度直到超过上限的字数。";
        let s = summarize(text, 8);
        assert!(s.ends_with('…'));
        assert!(text.starts_with(s.trim_end_matches('…')));
    }

    #[test]
    fn consolidate_compresses_oversized_documents() {
        let mut store = InMemoryStore::new();
        store
            .write(Document::new("note", "big", "word ".repeat(50)))
            .unwrap();
        store.write(Document::new("note", "small", "tiny")).unwrap();

        assert_eq!(consolidate(&mut store, 40).unwrap(), 1);
        // Re-running is a no-op: the big doc is now tagged compressed.
        assert_eq!(consolidate(&mut store, 40).unwrap(), 0);
        let big = store
            .search(&Query {
                text: Some("big".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            big[0].metadata.get("compressed").map(String::as_str),
            Some("true")
        );
    }

    #[test]
    fn keywords_rank_by_frequency() {
        let text = "kernel kernel scheduler kernel scheduler graph the a an";
        let kw = keywords(text, 2);
        assert_eq!(kw, vec!["kernel", "scheduler"]); // freq 3, 2; short words dropped
                                                     // Stop words and short tokens are excluded.
        assert!(!keywords("this that with from", 5).contains(&"this".to_string()));
    }

    #[test]
    fn compress_document_tags_and_shortens() {
        let mut doc = Document::new("note", "t", "word ".repeat(100));
        compress_document(&mut doc, 40);
        assert_eq!(
            doc.metadata.get("compressed").map(String::as_str),
            Some("true")
        );
        assert!(doc.body.chars().count() <= 41); // 40 + ellipsis
                                                 // Idempotent: a second pass is a no-op.
        let once = doc.body.clone();
        compress_document(&mut doc, 40);
        assert_eq!(doc.body, once);
    }
}
