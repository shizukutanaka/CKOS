//! Retrieval evaluation metrics (§959 — measure to optimize).
//!
//! Dependency-free implementations of the standard information-retrieval metrics
//! used to judge RAG search quality: Precision@k, Recall@k, reciprocal rank
//! (MRR over many queries), average precision (MAP over many queries), and
//! nDCG@k with binary relevance. They operate on a
//! ranked list of result ids/titles and the set of ids known to be relevant, so
//! the search layer can be tuned against a labelled set rather than by feel.

use crate::retrieval::Hit;
use std::collections::HashSet;

/// All metrics for a single query at cutoff `k`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EvalScores {
    /// Cutoff used.
    pub k: usize,
    /// Relevant items in the top-k, divided by `k`.
    pub precision: f32,
    /// Relevant items in the top-k, divided by the total relevant count.
    pub recall: f32,
    /// Reciprocal rank: 1/(rank of first relevant hit), else 0.
    pub reciprocal_rank: f32,
    /// nDCG@k with binary gains (DCG normalized by the ideal DCG).
    pub ndcg: f32,
    /// Average precision over the whole ranking (TREC convention — see
    /// [`average_precision`]). Not cut at `k`: it summarizes the full ranking.
    pub average_precision: f32,
}

/// Precision@k: fraction of the top-`k` results that are relevant (divided by
/// `k`, the requested cutoff).
pub fn precision_at_k(ranked: &[String], relevant: &HashSet<String>, k: usize) -> f32 {
    if k == 0 {
        return 0.0;
    }
    let hits = ranked
        .iter()
        .take(k)
        .filter(|r| relevant.contains(*r))
        .count();
    hits as f32 / k as f32
}

/// Recall@k: fraction of all relevant items that appear in the top-`k`.
pub fn recall_at_k(ranked: &[String], relevant: &HashSet<String>, k: usize) -> f32 {
    if relevant.is_empty() {
        return 0.0;
    }
    let hits = ranked
        .iter()
        .take(k)
        .filter(|r| relevant.contains(*r))
        .count();
    hits as f32 / relevant.len() as f32
}

/// Reciprocal rank: `1 / (1-based rank of the first relevant hit)`, or 0 if none.
/// Averaging this across queries gives MRR ([`mean_reciprocal_rank`]).
pub fn reciprocal_rank(ranked: &[String], relevant: &HashSet<String>) -> f32 {
    for (i, r) in ranked.iter().enumerate() {
        if relevant.contains(r) {
            return 1.0 / (i + 1) as f32;
        }
    }
    0.0
}

/// nDCG@k with binary relevance: DCG of the ranking divided by the ideal DCG
/// (all relevant items ranked first). Returns 0 when nothing is relevant.
pub fn ndcg_at_k(ranked: &[String], relevant: &HashSet<String>, k: usize) -> f32 {
    let dcg: f32 = ranked
        .iter()
        .take(k)
        .enumerate()
        .map(|(i, r)| {
            if relevant.contains(r) {
                1.0 / ((i + 2) as f32).log2()
            } else {
                0.0
            }
        })
        .sum();
    // Ideal DCG: as many relevant items as fit in k, all at the top.
    let ideal_hits = relevant.len().min(k);
    let idcg: f32 = (0..ideal_hits).map(|i| 1.0 / ((i + 2) as f32).log2()).sum();
    if idcg == 0.0 {
        0.0
    } else {
        dcg / idcg
    }
}

/// **Average precision** over the full ranking (Manning, Raghavan & Schütze,
/// *Introduction to Information Retrieval* §8.4; the TREC `map` metric):
///
/// ```text
/// AP = (1/|R|) · Σ  P@i   over each rank i holding a relevant item
/// ```
///
/// Unlike P@k it rewards ranking relevant items *early* rather than merely
/// retrieving them, and unlike reciprocal rank it accounts for every relevant
/// item, not just the first. Following the TREC convention the divisor is the
/// total number of relevant items `|R|`, so relevant items that were never
/// retrieved contribute 0 rather than being silently excluded — an evaluation
/// that ignores what a run missed would flatter a short, incomplete ranking.
/// Returns 0 when nothing is relevant.
///
/// Duplicate ids in `ranked` are counted once: a run listing the same document
/// twice must not be able to inflate its own score.
pub fn average_precision(ranked: &[String], relevant: &HashSet<String>) -> f32 {
    if relevant.is_empty() {
        return 0.0;
    }
    let mut seen: HashSet<&String> = HashSet::new();
    let mut found = 0usize;
    let mut sum = 0.0f32;
    for (i, id) in ranked.iter().enumerate() {
        if !seen.insert(id) {
            continue; // already credited this document
        }
        if relevant.contains(id) {
            found += 1;
            sum += found as f32 / (i + 1) as f32; // precision at this rank
        }
    }
    sum / relevant.len() as f32
}

/// **Mean average precision** across many queries: the mean of each query's
/// [`average_precision`]. The standard single-number summary of a retrieval
/// run's quality. Empty input yields 0.
pub fn mean_average_precision(per_query: &[(Vec<String>, HashSet<String>)]) -> f32 {
    if per_query.is_empty() {
        return 0.0;
    }
    let total: f32 = per_query
        .iter()
        .map(|(ranked, relevant)| average_precision(ranked, relevant))
        .sum();
    total / per_query.len() as f32
}

/// Compute every metric at cutoff `k` for one ranked list.
pub fn evaluate(ranked: &[String], relevant: &HashSet<String>, k: usize) -> EvalScores {
    EvalScores {
        k,
        precision: precision_at_k(ranked, relevant, k),
        recall: recall_at_k(ranked, relevant, k),
        reciprocal_rank: reciprocal_rank(ranked, relevant),
        ndcg: ndcg_at_k(ranked, relevant, k),
        average_precision: average_precision(ranked, relevant),
    }
}

/// Evaluate a [`Retriever`](crate::retrieval::Retriever) result directly, using
/// each hit's title as its id.
pub fn evaluate_hits(hits: &[Hit], relevant: &HashSet<String>, k: usize) -> EvalScores {
    let ranked: Vec<String> = hits.iter().map(|h| h.title.clone()).collect();
    evaluate(&ranked, relevant, k)
}

/// Mean reciprocal rank across many queries: the average of each query's
/// [`reciprocal_rank`]. Empty input yields 0.
pub fn mean_reciprocal_rank(per_query: &[(Vec<String>, HashSet<String>)]) -> f32 {
    if per_query.is_empty() {
        return 0.0;
    }
    let sum: f32 = per_query
        .iter()
        .map(|(ranked, rel)| reciprocal_rank(ranked, rel))
        .sum();
    sum / per_query.len() as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }
    fn list(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn precision_and_recall() {
        let ranked = list(&["a", "x", "b", "y"]);
        let relevant = set(&["a", "b", "c"]);
        // 2 of top-4 relevant → 0.5; 2 of 3 relevant found → 0.667.
        assert_eq!(precision_at_k(&ranked, &relevant, 4), 0.5);
        assert!((recall_at_k(&ranked, &relevant, 4) - 2.0 / 3.0).abs() < 1e-6);
        // Tighter cutoff: only "a" in top-1.
        assert_eq!(precision_at_k(&ranked, &relevant, 1), 1.0);
    }

    #[test]
    fn reciprocal_rank_finds_first_relevant() {
        let relevant = set(&["b"]);
        assert_eq!(reciprocal_rank(&list(&["a", "b", "c"]), &relevant), 0.5);
        assert_eq!(reciprocal_rank(&list(&["b", "a"]), &relevant), 1.0);
        assert_eq!(reciprocal_rank(&list(&["x", "y"]), &relevant), 0.0);
    }

    #[test]
    fn average_precision_matches_hand_computed_values() {
        // Textbook example (IIR §8.4): relevant at ranks 1 and 3 of 4, with
        // three relevant documents in total.
        //   P@1 = 1/1, P@3 = 2/3, the third relevant is never retrieved (0).
        //   AP = (1.0 + 0.6667) / 3 = 0.5556
        let ap = average_precision(&list(&["a", "x", "b", "y"]), &set(&["a", "b", "c"]));
        assert!((ap - (1.0 + 2.0 / 3.0) / 3.0).abs() < 1e-6, "got {ap}");

        // A perfect ranking scores 1.0; reversing it must score strictly less
        // even though both retrieve every relevant item — the property P@k and
        // recall@k cannot express.
        let relevant = set(&["a", "b"]);
        assert!((average_precision(&list(&["a", "b", "x"]), &relevant) - 1.0).abs() < 1e-6);
        let late = average_precision(&list(&["x", "a", "b"]), &relevant);
        assert!(late < 1.0, "ranking relevant items later must cost: {late}");
        // P@2 = 1/2 and P@3 = 2/3 → AP = (0.5 + 0.6667)/2 = 0.5833
        assert!((late - (0.5 + 2.0 / 3.0) / 2.0).abs() < 1e-6);

        // Nothing relevant, or nothing found → 0.
        assert_eq!(average_precision(&list(&["a"]), &set(&[])), 0.0);
        assert_eq!(average_precision(&list(&["x"]), &set(&["a"])), 0.0);
    }

    #[test]
    fn average_precision_ignores_repeated_ids() {
        // A run that lists the same relevant document twice must not score
        // higher than one that lists it once.
        let relevant = set(&["a", "b"]);
        let once = average_precision(&list(&["a", "b"]), &relevant);
        let twice = average_precision(&list(&["a", "a", "b"]), &relevant);
        assert!((once - 1.0).abs() < 1e-6);
        assert!(twice <= once, "duplicates must not inflate AP: {twice}");
    }

    #[test]
    fn mean_average_precision_averages_per_query() {
        let queries = vec![
            (list(&["a", "x"]), set(&["a"])), // AP = 1.0
            (list(&["x", "b"]), set(&["b"])), // AP = 0.5
        ];
        assert!((mean_average_precision(&queries) - 0.75).abs() < 1e-6);
        assert_eq!(mean_average_precision(&[]), 0.0);
    }

    #[test]
    fn ndcg_is_one_for_ideal_ranking() {
        let relevant = set(&["a", "b"]);
        // Both relevant items ranked first → perfect nDCG.
        assert!((ndcg_at_k(&list(&["a", "b", "c"]), &relevant, 3) - 1.0).abs() < 1e-6);
        // A worse ranking scores lower.
        let worse = ndcg_at_k(&list(&["c", "a", "b"]), &relevant, 3);
        assert!(worse < 1.0 && worse > 0.0);
        // Nothing relevant → 0.
        assert_eq!(ndcg_at_k(&list(&["x", "y"]), &relevant, 2), 0.0);
    }

    #[test]
    fn mrr_averages_over_queries() {
        let q1 = (list(&["a", "b"]), set(&["a"])); // RR 1.0
        let q2 = (list(&["x", "b"]), set(&["b"])); // RR 0.5
        assert!((mean_reciprocal_rank(&[q1, q2]) - 0.75).abs() < 1e-6);
        assert_eq!(mean_reciprocal_rank(&[]), 0.0);
    }

    #[test]
    fn evaluate_hits_uses_titles() {
        use crate::retrieval::HitSource;
        let hits = vec![
            Hit {
                id: None,
                title: "a".into(),
                snippet: String::new(),
                score: 2.0,
                source: HitSource::Keyword,
            },
            Hit {
                id: None,
                title: "z".into(),
                snippet: String::new(),
                score: 1.0,
                source: HitSource::Keyword,
            },
        ];
        let scores = evaluate_hits(&hits, &set(&["a"]), 2);
        assert_eq!(scores.precision, 0.5);
        assert_eq!(scores.reciprocal_rank, 1.0);
    }
}
