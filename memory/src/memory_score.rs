//! Generative-Agents memory scoring (§896 hierarchy / §953 consolidation).
//!
//! Ranks memories by the Stanford "Generative Agents" retrieval score
//! (Park et al., 2023), the de-facto standard later adopted by MemGPT, Mem0 and
//! LangGraph:
//!
//! ```text
//! score = α_recency·recency + α_importance·importance + α_relevance·relevance
//! ```
//!
//! * **recency** decays exponentially with age since last access
//!   ([`recency_decay`], γ≈0.995 in the paper);
//! * **importance** is an authored 0..1 weight (here, document confidence);
//! * **relevance** is query similarity (cosine), supplied by the caller.
//!
//! Each component is min-max normalized across the candidate set before the
//! weighted sum, exactly as the paper does, so no single component's raw scale
//! dominates. All std-only.

/// Relative weights for the three score components (default: equal, as in the
/// reference implementation).
#[derive(Debug, Clone, Copy)]
pub struct MemoryWeights {
    /// Weight of the recency component (α_recency).
    pub recency: f32,
    /// Weight of the importance component (α_importance).
    pub importance: f32,
    /// Weight of the relevance component (α_relevance).
    pub relevance: f32,
}

impl Default for MemoryWeights {
    fn default() -> Self {
        MemoryWeights {
            recency: 1.0,
            importance: 1.0,
            relevance: 1.0,
        }
    }
}

/// The three raw signals for one memory.
#[derive(Debug, Clone, Copy)]
pub struct MemorySignals {
    /// Recency in 0..=1 (1 = just accessed); typically from [`recency_decay`].
    pub recency: f32,
    /// Importance in 0..=1 (1 = most important); e.g. confidence/100.
    pub importance: f32,
    /// Relevance in 0..=1 (1 = perfect match); e.g. cosine similarity.
    pub relevance: f32,
}

/// Exponential recency: `decay^age`, clamped. `age` is elapsed time (any unit
/// matching `decay`'s per-step factor); `decay` in `(0,1]` — the paper uses
/// ~0.995 per hour. Age 0 → 1.0 (fully recent).
pub fn recency_decay(age: f32, decay: f32) -> f32 {
    let decay = decay.clamp(0.0, 1.0);
    decay.powf(age.max(0.0))
}

/// Min-max normalize a slice to `0..=1`. When all values are equal the range is
/// zero, so every entry maps to 1.0 (the component then contributes uniformly
/// rather than vanishing).
fn normalize(values: &[f32]) -> Vec<f32> {
    let min = values.iter().cloned().fold(f32::INFINITY, f32::min);
    let max = values.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let range = max - min;
    if range.abs() < f32::EPSILON {
        return vec![1.0; values.len()];
    }
    values.iter().map(|v| (v - min) / range).collect()
}

/// Score every memory and return `(index, score)` sorted by descending score
/// (ties broken by original index for determinism). Each component is min-max
/// normalized across `items` before the weighted sum (Park et al., 2023).
pub fn rank_memories(items: &[MemorySignals], weights: &MemoryWeights) -> Vec<(usize, f32)> {
    if items.is_empty() {
        return Vec::new();
    }
    let rec = normalize(&items.iter().map(|m| m.recency).collect::<Vec<_>>());
    let imp = normalize(&items.iter().map(|m| m.importance).collect::<Vec<_>>());
    let rel = normalize(&items.iter().map(|m| m.relevance).collect::<Vec<_>>());

    let mut scored: Vec<(usize, f32)> = (0..items.len())
        .map(|i| {
            let s =
                weights.recency * rec[i] + weights.importance * imp[i] + weights.relevance * rel[i];
            (i, s)
        })
        .collect();
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    scored
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recency_decays_exponentially() {
        assert_eq!(recency_decay(0.0, 0.5), 1.0);
        assert_eq!(recency_decay(1.0, 0.5), 0.5);
        assert_eq!(recency_decay(2.0, 0.5), 0.25);
        // Negative age is treated as fully recent.
        assert_eq!(recency_decay(-3.0, 0.9), 1.0);
    }

    #[test]
    fn ranking_blends_components() {
        // A: very recent but unimportant/irrelevant; B: stale but important and
        // relevant. With equal weights B (two strong components) outranks A.
        let items = [
            MemorySignals {
                recency: 1.0,
                importance: 0.0,
                relevance: 0.0,
            },
            MemorySignals {
                recency: 0.0,
                importance: 1.0,
                relevance: 1.0,
            },
        ];
        let ranked = rank_memories(&items, &MemoryWeights::default());
        assert_eq!(ranked[0].0, 1);
    }

    #[test]
    fn weights_can_favor_recency() {
        let items = [
            MemorySignals {
                recency: 1.0,
                importance: 0.0,
                relevance: 0.0,
            },
            MemorySignals {
                recency: 0.0,
                importance: 1.0,
                relevance: 1.0,
            },
        ];
        // Heavily weighting recency flips the order in favour of the fresh memory.
        let w = MemoryWeights {
            recency: 5.0,
            importance: 1.0,
            relevance: 1.0,
        };
        let ranked = rank_memories(&items, &w);
        assert_eq!(ranked[0].0, 0);
    }

    #[test]
    fn empty_input_is_empty() {
        assert!(rank_memories(&[], &MemoryWeights::default()).is_empty());
    }
}
