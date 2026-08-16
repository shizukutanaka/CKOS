//! Embeddings (§944) and similarity.
//!
//! [`Embedder`] abstracts an embedding model so a real one (ONNX/MLX) can be
//! dropped in later (§900, §945). [`HashingEmbedder`] is a deterministic,
//! dependency-free default: a hashed bag-of-words projected into a fixed
//! dimension and L2-normalised.
//!
//! **Honest limitation**: this is *not* a semantic embedding. It only
//! recognises literal token overlap (modulo hash collisions), so it cannot
//! relate a paraphrase or synonym to the text it restates. With signed hashing
//! working correctly the collision noise is centred near zero: a paraphrase
//! sharing zero content words scores only at noise level (~0.13, and unrelated
//! text often lands slightly negative), far below the ~0.77 a genuine
//! literal-overlap match earns — see
//! `paraphrase_with_no_shared_words_shows_no_semantic_recall` below. Its
//! purpose is to exercise the vector-search code path offline, not to provide
//! meaning-aware retrieval — in the hybrid pipeline (§950) it is effectively a
//! *second, noisier lexical signal* alongside BM25, not an independent semantic
//! one. Real synonym/paraphrase matching needs an actual trained embedding
//! model, which is necessarily an external dependency (§944 "real model").

/// Produces embedding vectors for text (§944).
pub trait Embedder: Send + Sync {
    /// Embed `text` into a fixed-dimension vector.
    fn embed(&self, text: &str) -> Vec<f32>;
    /// Dimensionality of produced vectors.
    fn dim(&self) -> usize;
}

/// FNV-1a 32-bit hash — small, fast, std-only.
fn fnv1a(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

fn tokens(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 1)
        .map(|t| t.to_lowercase())
}

/// A deterministic hashing embedder (the "hashing trick"), L2-normalised.
pub struct HashingEmbedder {
    dim: usize,
}

impl HashingEmbedder {
    /// Create an embedder producing `dim`-dimensional vectors (min 1).
    pub fn new(dim: usize) -> Self {
        HashingEmbedder { dim: dim.max(1) }
    }
}

impl Default for HashingEmbedder {
    /// A 64-dimensional embedder — a reasonable default for the demo corpus.
    fn default() -> Self {
        HashingEmbedder::new(64)
    }
}

impl Embedder for HashingEmbedder {
    fn embed(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0.0f32; self.dim];
        for token in tokens(text) {
            let h = fnv1a(&token);
            let idx = (h as usize) % self.dim;
            // Signed hashing (Weinberger et al. 2009): the ±1 sign must come from
            // a bit INDEPENDENT of the bucket index, or colliding tokens can only
            // add — never cancel — and the collision-bias correction the sign
            // exists to provide is lost. FNV-1a's low bit is merely the parity of
            // the input bytes (the odd-prime multiply preserves it), so for any
            // even `dim` it equals `idx & 1`: fully determined by the bucket, the
            // opposite of independent. The top bit is well-mixed by the multiply
            // chain and is not among the low bits `% dim` consumes.
            let sign = if (h >> 31) & 1 == 0 { 1.0 } else { -1.0 };
            v[idx] += sign;
        }
        l2_normalize(&mut v);
        v
    }

    fn dim(&self) -> usize {
        self.dim
    }
}

/// Scale a vector to unit L2 length in place (no-op for the zero vector).
fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// Cosine similarity in `-1.0..=1.0`. Returns 0 for length mismatch or a zero
/// vector, so callers can treat 0 as "no signal".
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0;
    let mut na = 0.0;
    let mut nb = 0.0;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_hash_is_independent_of_the_bucket_index() {
        // Regression: the ±1 sign came from FNV-1a's low bit, which for an even
        // `dim` (the default 64) is exactly the bucket index's parity — so every
        // token in a bucket shared a sign, colliding tokens could only add, and
        // the signed-hashing collision correction (Weinberger et al. 2009) was
        // fully defeated (measured: sign matched bucket parity for 100% of
        // tokens; colliding pairs cancelled 0% of the time, vs the ~50% the
        // technique needs). A single token lands in exactly one bucket, so its
        // sign and bucket are both readable from its embedding; across many
        // tokens the sign must NOT be a function of `idx & 1`.
        let e = HashingEmbedder::new(64);
        let n = 400;
        let mut matches_parity = 0;
        for i in 0..n {
            let v = e.embed(&format!("token{i}"));
            let (idx, val) = v
                .iter()
                .enumerate()
                .find(|(_, x)| x.abs() > 1e-6)
                .expect("a single token yields one nonzero component");
            let sign_positive = *val > 0.0;
            let parity_even = idx % 2 == 0;
            if sign_positive == parity_even {
                matches_parity += 1;
            }
        }
        let frac = matches_parity as f32 / n as f32;
        // The bug pins this at 1.0; an index-independent sign sits near 0.5.
        assert!(
            (frac - 0.5).abs() < 0.2,
            "sign must not be determined by bucket parity; matched {:.0}%",
            frac * 100.0
        );
    }

    #[test]
    fn embedding_is_unit_length_and_deterministic() {
        let e = HashingEmbedder::new(32);
        let a = e.embed("the cognitive kernel schedules tasks");
        let b = e.embed("the cognitive kernel schedules tasks");
        assert_eq!(a, b);
        assert_eq!(a.len(), 32);
        let norm = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);
    }

    #[test]
    fn similar_text_scores_higher_than_unrelated() {
        let e = HashingEmbedder::new(128);
        let q = e.embed("kernel task scheduling");
        let close = e.embed("the kernel schedules a task");
        let far = e.embed("banana smoothie recipe with mango");
        assert!(cosine(&q, &close) > cosine(&q, &far));
    }

    #[test]
    fn paraphrase_with_no_shared_words_shows_no_semantic_recall() {
        // Documents this embedder's real boundary (see the module doc): it keys
        // on literal tokens, not meaning. A paraphrase sharing no content words
        // with the original earns only noise-level similarity — far below a
        // variant that reuses the original's words. If this ever changes (a real
        // semantic model dropped in behind the same trait), the paraphrase gap
        // would close; that would be good news, and the module doc should be
        // updated alongside this test.
        //
        // (An earlier form asserted paraphrase ≈ unrelated within 0.1. That held
        // only because a sign-hashing bug pinned every collision positive, giving
        // both a ~0.13 floor. With signed hashing corrected the noise centres on
        // zero — unrelated text can be negative — so the honest invariant is that
        // the paraphrase stays at noise level and literal overlap dominates it.)
        let e = HashingEmbedder::new(64);
        let original = e.embed("The scheduler dispatches ready tasks by priority.");
        // Same meaning, zero shared content words:
        let paraphrase = e.embed("Ready work is ordered and run according to importance.");
        // Genuine literal overlap with the original:
        let lexical = e.embed("The scheduler dispatches ready tasks quickly.");
        let sim_paraphrase = cosine(&original, &paraphrase);
        let sim_lexical = cosine(&original, &lexical);
        assert!(
            sim_lexical > sim_paraphrase + 0.3,
            "literal overlap ({sim_lexical:.3}) must dominate a no-shared-word \
             paraphrase ({sim_paraphrase:.3}) — the embedder has no semantic recall"
        );
        assert!(
            sim_paraphrase.abs() < 0.4,
            "a no-shared-word paraphrase should score only at noise level, got {sim_paraphrase:.3}"
        );
    }

    #[test]
    fn cosine_handles_degenerate_inputs() {
        assert_eq!(cosine(&[], &[]), 0.0);
        assert_eq!(cosine(&[1.0, 2.0], &[1.0]), 0.0);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }
}
