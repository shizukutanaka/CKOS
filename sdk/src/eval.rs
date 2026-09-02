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

/// Yield each id the first time it appears, paired with its **original**
/// 0-based rank so cutoffs and discounts still refer to the position the run
/// actually returned it at.
///
/// Every metric here credits a document once: a run that lists the same
/// document twice must not outscore one that lists it once. The guard lives in
/// this one place because it previously lived in [`average_precision`] alone,
/// and its three siblings each disagreed — nDCG could exceed 1.0 and recall
/// could report a document that was never retrieved.
fn first_occurrences(ranked: &[String]) -> impl Iterator<Item = (usize, &String)> {
    let mut seen: HashSet<&String> = HashSet::new();
    ranked
        .iter()
        .enumerate()
        .filter(move |(_, id)| seen.insert(id))
}

/// Count the distinct relevant documents within the top-`k` positions.
fn distinct_relevant_in_top_k(ranked: &[String], relevant: &HashSet<String>, k: usize) -> usize {
    first_occurrences(ranked)
        .take_while(|(i, _)| *i < k)
        .filter(|(_, id)| relevant.contains(*id))
        .count()
}

/// Precision@k: fraction of the top-`k` results that are relevant (divided by
/// `k`, the requested cutoff).
pub fn precision_at_k(ranked: &[String], relevant: &HashSet<String>, k: usize) -> f32 {
    if k == 0 {
        return 0.0;
    }
    distinct_relevant_in_top_k(ranked, relevant, k) as f32 / k as f32
}

/// Recall@k: fraction of all relevant items that appear in the top-`k`.
pub fn recall_at_k(ranked: &[String], relevant: &HashSet<String>, k: usize) -> f32 {
    if relevant.is_empty() {
        return 0.0;
    }
    distinct_relevant_in_top_k(ranked, relevant, k) as f32 / relevant.len() as f32
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
///
/// A repeated document earns its gain once, at the rank it first appeared, so
/// the result stays inside `0.0..=1.0`.
pub fn ndcg_at_k(ranked: &[String], relevant: &HashSet<String>, k: usize) -> f32 {
    let dcg: f32 = first_occurrences(ranked)
        .take_while(|(i, _)| *i < k)
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
        return 0.0;
    }
    // `+ 0.0` normalises a signed zero: `Sum for f32` folds from -0.0, so an
    // empty ranking yields -0.0 here and the report read "nDCG  -0.000" while
    // every other metric on the same input read "0.000".
    dcg / idcg + 0.0
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
    let mut found = 0usize;
    let mut sum = 0.0f32;
    for (i, id) in first_occurrences(ranked) {
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

/// Deterministic, dependency-free pseudo-random source for the invariant tests
/// below (the workspace takes no external crates, so no `proptest`). A fixed
/// seed keeps failures reproducible: the same run always generates the same
/// cases.
#[cfg(test)]
struct Lcg(u64);

#[cfg(test)]
impl Lcg {
    fn next(&mut self) -> u64 {
        // Numerical Recipes constants; adequate for generating test cases.
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 11
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

/// Invariants every metric in this module must satisfy, checked over generated
/// inputs rather than hand-picked ones.
///
/// These exist because a defect slipped past example-based tests: three metrics
/// credited a repeated document more than once, and `nDCG` returned 1.307 — a
/// value its own normalisation makes impossible. Each example test asserted a
/// number it had been given; none asserted the *range*. Randomised cases with
/// duplicates catch that class without anyone having to think of the example.
#[cfg(test)]
mod invariants {
    use super::*;

    /// Build a random ranking over a small id pool, deliberately allowing
    /// repeats — the case the example tests never covered.
    fn case(rng: &mut Lcg) -> (Vec<String>, HashSet<String>, usize) {
        let pool = ["a", "b", "c", "d", "e"];
        let len = rng.below(8);
        let ranked: Vec<String> = (0..len)
            .map(|_| pool[rng.below(pool.len())].to_string())
            .collect();
        let relevant: HashSet<String> = pool
            .iter()
            .filter(|_| rng.next() % 2 == 0)
            .map(|s| s.to_string())
            .collect();
        (ranked, relevant, rng.below(6))
    }

    #[test]
    fn every_metric_stays_within_zero_and_one() {
        let mut rng = Lcg(0x5EED);
        for _ in 0..20_000 {
            let (ranked, relevant, k) = case(&mut rng);
            let scores = evaluate(&ranked, &relevant, k);
            for (name, v) in [
                ("precision", scores.precision),
                ("recall", scores.recall),
                ("reciprocal_rank", scores.reciprocal_rank),
                ("ndcg", scores.ndcg),
                ("average_precision", scores.average_precision),
            ] {
                assert!(
                    v.is_finite() && (0.0..=1.0).contains(&v),
                    "{name} = {v} outside [0,1] for ranked={ranked:?} relevant={relevant:?} k={k}"
                );
            }
        }
    }

    #[test]
    fn repeating_a_hit_never_improves_a_score() {
        // The property the duplicate-credit defect violated. Every metric here
        // scores a repeat at the rank the run actually returned it at, so a
        // repeat *wastes a slot*: collapsing repeats can only move a score up,
        // never down. Before the fix, repeats raised nDCG (to 1.307), recall
        // and precision instead.
        //
        // Note this is a one-sided property. An earlier draft asserted that
        // recall and MAP could not move at all; a generated case disproved it
        // in under a second — both credit relevance at the *original* rank, so
        // removing a repeat shifts later documents up and legitimately
        // improves them. The generator corrected the assumption, which is the
        // reason it is here.
        let mut rng = Lcg(0xC0FFEE);
        let mut cases_with_repeats = 0;
        for _ in 0..20_000 {
            let (ranked, relevant, k) = case(&mut rng);
            let mut seen = HashSet::new();
            let deduped: Vec<String> = ranked
                .iter()
                .filter(|id| seen.insert((*id).clone()))
                .cloned()
                .collect();
            if deduped.len() == ranked.len() {
                continue; // no repeats in this case
            }
            cases_with_repeats += 1;
            let with = evaluate(&ranked, &relevant, k);
            let without = evaluate(&deduped, &relevant, k);
            for (name, a, b) in [
                ("precision", with.precision, without.precision),
                ("recall", with.recall, without.recall),
                (
                    "reciprocal_rank",
                    with.reciprocal_rank,
                    without.reciprocal_rank,
                ),
                ("ndcg", with.ndcg, without.ndcg),
                (
                    "average_precision",
                    with.average_precision,
                    without.average_precision,
                ),
            ] {
                assert!(
                    a <= b + 1e-6,
                    "a repeat raised {name}: {a} with repeats vs {b} without, \
                     ranked={ranked:?} relevant={relevant:?} k={k}"
                );
            }
        }
        assert!(
            cases_with_repeats > 1_000,
            "the generator produced only {cases_with_repeats} cases with repeats; \
             it is no longer exercising the property"
        );
    }

    #[test]
    fn a_perfect_ranking_scores_one_and_an_empty_one_scores_zero() {
        let mut rng = Lcg(0xBEEF);
        for _ in 0..2_000 {
            let (_, relevant, _) = case(&mut rng);
            if relevant.is_empty() {
                continue;
            }
            let mut ideal: Vec<String> = relevant.iter().cloned().collect();
            ideal.sort(); // deterministic order; all are relevant either way
            let k = ideal.len();
            let s = evaluate(&ideal, &relevant, k);
            assert!((s.ndcg - 1.0).abs() < 1e-6, "ideal nDCG = {}", s.ndcg);
            assert!((s.recall - 1.0).abs() < 1e-6, "ideal recall = {}", s.recall);
            assert!(
                (s.average_precision - 1.0).abs() < 1e-6,
                "ideal MAP = {}",
                s.average_precision
            );

            let nothing = evaluate(&[], &relevant, k);
            assert_eq!(nothing.ndcg, 0.0);
            assert_eq!(nothing.recall, 0.0);
            assert_eq!(nothing.average_precision, 0.0);
        }
    }
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
    fn a_zero_score_is_never_reported_as_negative_zero() {
        // `ckos eval` printed "nDCG@10  -0.000" for a query that found nothing,
        // while every sibling metric printed "0.000" on the same input.
        // Rust's `Sum for f32` folds from -0.0 (the true additive identity, so
        // that signed zeros survive), so an *empty* ranking sums to -0.0 while
        // a non-empty one reaches +0.0 via -0.0 + 0.0. Harmless arithmetically,
        // but a measuring tool that reports a negative score for a metric
        // defined on [0,1] invites doubt about the numbers that matter.
        let relevant = set(&["x"]);
        for ranked in [Vec::new(), list(&["a", "b"])] {
            let s = evaluate(&ranked, &relevant, 10);
            for (name, v) in [
                ("precision", s.precision),
                ("recall", s.recall),
                ("reciprocal_rank", s.reciprocal_rank),
                ("ndcg", s.ndcg),
                ("average_precision", s.average_precision),
            ] {
                assert_eq!(v, 0.0, "{name} should be zero here");
                assert!(
                    !v.is_sign_negative(),
                    "{name} reported negative zero for ranked={ranked:?}"
                );
            }
        }
    }

    #[test]
    fn no_metric_can_be_inflated_by_a_duplicated_hit() {
        // Regression: only `average_precision` credited a document once. Its
        // siblings counted every occurrence, so a run listing the same
        // document twice scored *better* than an honest one — nDCG above 1.0,
        // which the metric's own normalisation makes impossible, and full
        // recall for a document that was never retrieved.
        let relevant = set(&["a", "b"]);

        let ndcg = ndcg_at_k(&list(&["a", "a", "b"]), &relevant, 3);
        assert!(ndcg <= 1.0, "nDCG is normalised to [0,1], got {ndcg}");
        // `a` is credited once at rank 1 and `b` at rank 3 (the repeat holds
        // rank 2 and earns nothing): DCG = 1/log2(2) + 1/log2(4) = 1.5, over an
        // ideal DCG of 1/log2(2) + 1/log2(3).
        let expected = 1.5 / (1.0 + 1.0 / 3.0_f32.log2());
        assert!(
            (ndcg - expected).abs() < 1e-6,
            "expected {expected}, got {ndcg}"
        );

        // Recall must not claim a document it never returned.
        let recall = recall_at_k(&list(&["a", "a"]), &relevant, 2);
        assert!(
            (recall - 0.5).abs() < 1e-6,
            "b was never retrieved, recall is 0.5, got {recall}"
        );

        // Precision counts distinct relevant results, not repeats.
        let precision = precision_at_k(&list(&["a", "a"]), &set(&["a"]), 2);
        assert!(
            (precision - 0.5).abs() < 1e-6,
            "one of two slots was a repeat, got {precision}"
        );

        // The already-correct sibling is unchanged.
        assert!((average_precision(&list(&["a", "a", "b"]), &relevant) - 0.8333333).abs() < 1e-6);
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
                sources: vec![HitSource::Keyword],
            },
            Hit {
                id: None,
                title: "z".into(),
                snippet: String::new(),
                score: 1.0,
                sources: vec![HitSource::Keyword],
            },
        ];
        let scores = evaluate_hits(&hits, &set(&["a"]), 2);
        assert_eq!(scores.precision, 0.5);
        assert_eq!(scores.reciprocal_rank, 1.0);
    }
}
