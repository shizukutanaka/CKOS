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

/// Whether `c` belongs to a script written without word spaces, where a run of
/// characters is a phrase rather than a single word: Han, Hiragana, Katakana
/// (including the prolonged-sound mark) and Hangul.
pub fn is_scriptio_continua(c: char) -> bool {
    matches!(c,
        '\u{3040}'..='\u{30ff}'   // Hiragana, Katakana
        | '\u{3400}'..='\u{4dbf}' // CJK Extension A
        | '\u{4e00}'..='\u{9fff}' // CJK Unified Ideographs
        | '\u{f900}'..='\u{faff}' // CJK Compatibility Ideographs
        | '\u{ac00}'..='\u{d7af}' // Hangul syllables
    )
}

/// Split text into the terms the index is built from — **the** definition of a
/// term for this workspace, shared by the vector and keyword legs so the two
/// cannot disagree about what a word is.
///
/// Splitting on non-alphanumerics alone is wrong for scripts written without
/// spaces: `猫は人気のペットです` is one alphanumeric run, so the whole clause
/// became a single term and a search for `猫` could never match it. Such runs
/// are emitted as overlapping **unigrams and bigrams** (the classic
/// dictionary-free CJK indexing approach, as in Lucene's CJK analyzer):
/// unigrams so a one-character query works — a single kanji is a whole word —
/// and bigrams for the precision unigrams alone would lose. Very common
/// characters are not filtered out, because BM25's own idf already discounts
/// terms that appear in every document.
///
/// Latin runs are returned whole and lowercased. Callers apply their own
/// stemming and length policy on top.
pub fn terms_of(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for run in text.split(|c: char| !c.is_alphanumeric()) {
        if run.is_empty() {
            continue;
        }
        let mut buf: Vec<char> = Vec::new();
        // Walk the run, flushing whenever the script class changes, so a mixed
        // run like `AI搭載` yields `ai` plus the CJK grams rather than one blob.
        let flush_latin = |buf: &mut Vec<char>, out: &mut Vec<String>| {
            if !buf.is_empty() {
                out.push(buf.iter().collect::<String>().to_lowercase());
                buf.clear();
            }
        };
        let mut cjk: Vec<char> = Vec::new();
        let flush_cjk = |cjk: &mut Vec<char>, out: &mut Vec<String>| {
            for (i, c) in cjk.iter().enumerate() {
                out.push(c.to_string());
                if let Some(next) = cjk.get(i + 1) {
                    out.push(format!("{c}{next}"));
                }
            }
            cjk.clear();
        };
        for c in run.chars() {
            if is_scriptio_continua(c) {
                flush_latin(&mut buf, &mut out);
                cjk.push(c);
            } else {
                flush_cjk(&mut cjk, &mut out);
                buf.push(c);
            }
        }
        flush_latin(&mut buf, &mut out);
        flush_cjk(&mut cjk, &mut out);
    }
    out
}

fn tokens(text: &str) -> impl Iterator<Item = String> + '_ {
    // A CJK gram is meaningful at one character, so the length filter applies
    // only to the space-delimited scripts it was written for.
    terms_of(text)
        .into_iter()
        .filter(|t| t.chars().count() > 1 || t.chars().all(is_scriptio_continua))
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
    fn terms_of_splits_space_less_scripts_into_grams() {
        // Splitting on non-alphanumerics leaves a whole Japanese clause as one
        // term, so no query could ever match it: measured MRR 0.143 over a
        // 7-query Japanese corpus, with 6 queries returning nothing at all.
        let t = terms_of("猫は人気");
        // Unigrams, so a single kanji — a whole word in Japanese — matches.
        assert!(t.contains(&"猫".to_string()), "{t:?}");
        // Bigrams, for the precision unigrams alone would lose.
        assert!(t.contains(&"猫は".to_string()), "{t:?}");
        assert!(t.contains(&"人気".to_string()), "{t:?}");
        // Nothing longer than a bigram, so the term count stays linear.
        assert!(t.iter().all(|g| g.chars().count() <= 2), "{t:?}");
    }

    #[test]
    fn terms_of_leaves_space_delimited_text_alone() {
        // The change must be invisible to Latin text: whole words, lowercased.
        assert_eq!(terms_of("Hello, World"), vec!["hello", "world"]);
        assert_eq!(terms_of("scheduler-v2"), vec!["scheduler", "v2"]);
        assert_eq!(terms_of(""), Vec::<String>::new());
    }

    #[test]
    fn terms_of_separates_scripts_inside_one_run() {
        // `AI搭載` is a single alphanumeric run but two scripts; the Latin part
        // must stay a word rather than being cut into grams with the kanji.
        let t = terms_of("AI搭載");
        assert!(t.contains(&"ai".to_string()), "{t:?}");
        assert!(t.contains(&"搭".to_string()), "{t:?}");
        assert!(t.contains(&"搭載".to_string()), "{t:?}");
        assert!(
            !t.iter().any(|g| g.contains('i') && g.contains('搭')),
            "{t:?}"
        );
    }

    #[test]
    fn fnv1a_matches_the_published_vectors() {
        // Bucket assignment (and, since the sign fix, the sign bit) both come
        // from this hash, so a changed constant would silently reshuffle every
        // embedding — invisible to the other tests here, which only compare
        // vectors computed by the same build. Pinned against the published
        // Fowler–Noll–Vo FNV-1a 32-bit reference vectors.
        assert_eq!(fnv1a(""), 0x811c_9dc5);
        assert_eq!(fnv1a("a"), 0xe40c_292c);
        assert_eq!(fnv1a("b"), 0xe70c_2de5);
        assert_eq!(fnv1a("foobar"), 0xbf9c_f968);
    }

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
