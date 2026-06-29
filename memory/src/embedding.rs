//! Embeddings (§944) and similarity.
//!
//! [`Embedder`] abstracts an embedding model so a real one (ONNX/MLX) can be
//! dropped in later (§900, §945). [`HashingEmbedder`] is a deterministic,
//! dependency-free default: a hashed bag-of-words projected into a fixed
//! dimension and L2-normalised, which is enough to exercise vector search
//! offline. [`cosine`] scores similarity between two vectors.

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
            let idx = (fnv1a(&token) as usize) % self.dim;
            // Sign bit decorrelates colliding tokens (a standard hashing trick).
            let sign = if fnv1a(&token) & 1 == 0 { 1.0 } else { -1.0 };
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
    fn cosine_handles_degenerate_inputs() {
        assert_eq!(cosine(&[], &[]), 0.0);
        assert_eq!(cosine(&[1.0, 2.0], &[1.0]), 0.0);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }
}
