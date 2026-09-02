//! Chunking (§939) — split a document body into index units.
//!
//! The spec rejects fixed chunking and lists Paragraph / Semantic /
//! Hierarchical / Adaptive strategies. This module provides the dependency-free
//! ones — [`ChunkStrategy::Paragraph`], [`ChunkStrategy::Fixed`] and
//! [`ChunkStrategy::Adaptive`] (paragraph-aware, merging small paragraphs and
//! splitting oversized ones at sentence boundaries toward a target size).
//! Semantic/hierarchical strategies need embeddings/structure and remain
//! behind the same [`chunk`] entry point for later.

/// How to split text into chunks (§939).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkStrategy {
    /// One chunk per paragraph (blank-line separated).
    Paragraph,
    /// Fixed windows of at most N characters.
    Fixed(usize),
    /// Paragraph-aware, packed toward ~N characters: small paragraphs merge,
    /// oversized ones split at sentence boundaries.
    Adaptive(usize),
    /// Recursive character splitting toward N characters — the general-purpose
    /// RAG baseline. Splits on the coarsest separator that fits (paragraph →
    /// sentence → word → hard char split), then greedily packs units up to N so
    /// no chunk exceeds the target.
    Recursive(usize),
}

/// Split `text` into chunks per `strategy`. Empty/whitespace chunks are dropped.
pub fn chunk(text: &str, strategy: ChunkStrategy) -> Vec<String> {
    match strategy {
        ChunkStrategy::Paragraph => paragraphs(text),
        ChunkStrategy::Fixed(n) => fixed(text, n.max(1)),
        ChunkStrategy::Adaptive(target) => adaptive(text, target.max(1)),
        ChunkStrategy::Recursive(target) => recursive(text, target.max(1)),
    }
}

/// Chunk `text`, then add a character overlap between consecutive chunks so
/// context isn't lost at boundaries (§939). Each chunk after the first is
/// prefixed with the trailing `overlap` characters of its predecessor — the
/// standard RAG continuity technique (typically 10–20% of the chunk size).
/// `overlap` is clamped below the effective chunk size.
pub fn chunk_with_overlap(text: &str, strategy: ChunkStrategy, overlap: usize) -> Vec<String> {
    let base = chunk(text, strategy);
    if overlap == 0 || base.len() < 2 {
        return base;
    }
    let mut out = Vec::with_capacity(base.len());
    out.push(base[0].clone());
    for i in 1..base.len() {
        let prev: Vec<char> = base[i - 1].chars().collect();
        let take = overlap.min(prev.len());
        let start = prev.len() - take;
        let tail: String = prev[overlap_start(&prev, start, overlap)..]
            .iter()
            .collect();
        let tail = tail.trim_start();
        if tail.is_empty() {
            out.push(base[i].clone());
        } else {
            out.push(format!("{tail} {}", base[i]));
        }
    }
    out
}

/// Where the overlap window should really begin, given that it wants to start
/// at `start`.
///
/// Indexing tokenises on runs of alphanumerics (`memory::embedding`,
/// `memory::maintenance::keywords`), so a window opening inside a word yields a
/// fragment that is a *term of its own*: `triphosphate` cut into `triphosp` and
/// `hate` put the word "hate" into the index for a passage about cellular
/// respiration, and lost `triphosphate` — the very continuity the overlap
/// exists to provide. The boundary therefore has to use the same definition of
/// a token the indexer does, not whitespace.
///
/// The boundary is moved *backwards*, to the start of the word the window
/// opened inside, so the word is carried whole — losing it is the failure this
/// exists to prevent. That spends at most one word beyond the requested
/// `overlap`; a "word" with no boundary at all (a long base64 blob, or a
/// space-less script) would spend an unbounded amount, so the reach back is
/// capped at `overlap` characters and the boundary otherwise moves forwards
/// instead, dropping the fragment.
///
/// Scripts written without spaces (Han, Kana) are a single alphanumeric run, so
/// moving forwards would consume the whole window. There the raw start is kept:
/// some overlap beats none, and it is what those scripts already got.
fn overlap_start(prev: &[char], start: usize, overlap: usize) -> usize {
    if start == 0 || !prev[start - 1].is_alphanumeric() {
        return start; // already at a boundary
    }
    // Backwards to the start of the partial word, within budget.
    let mut back = start;
    while back > 0 && prev[back - 1].is_alphanumeric() {
        back -= 1;
        if start - back > overlap {
            break; // too far: this is not a word, fall through
        }
    }
    if start - back <= overlap && (back == 0 || !prev[back - 1].is_alphanumeric()) {
        return back;
    }
    // Otherwise forwards, past the fragment.
    let mut fwd = start;
    while fwd < prev.len() && prev[fwd].is_alphanumeric() {
        fwd += 1;
    }
    if fwd >= prev.len() {
        start // nothing would be left; keep the window as-is
    } else {
        fwd
    }
}

fn paragraphs(text: &str) -> Vec<String> {
    text.split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(String::from)
        .collect()
}

fn fixed(text: &str, n: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    chars
        .chunks(n)
        .map(|c| c.iter().collect::<String>())
        .filter(|s| !s.trim().is_empty())
        .collect()
}

/// Split a paragraph into sentences (keeping terminators).
fn sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    for ch in text.chars() {
        buf.push(ch);
        if matches!(ch, '.' | '!' | '?' | '。') {
            out.push(buf.trim().to_string());
            buf.clear();
        }
    }
    if !buf.trim().is_empty() {
        out.push(buf.trim().to_string());
    }
    out
}

fn adaptive(text: &str, target: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let flush = |buf: &mut String, out: &mut Vec<String>| {
        if !buf.trim().is_empty() {
            out.push(buf.trim().to_string());
        }
        buf.clear();
    };

    for para in paragraphs(text) {
        if para.chars().count() > target {
            flush(&mut buf, &mut out);
            // Split the oversized paragraph by sentences toward the target.
            let mut sbuf = String::new();
            for sent in sentences(&para) {
                if !sbuf.is_empty() && sbuf.chars().count() + sent.chars().count() > target {
                    flush(&mut sbuf, &mut out);
                }
                if !sbuf.is_empty() {
                    sbuf.push(' ');
                }
                sbuf.push_str(&sent);
            }
            flush(&mut sbuf, &mut out);
        } else if !buf.is_empty() && buf.chars().count() + para.chars().count() > target {
            flush(&mut buf, &mut out);
            buf.push_str(&para);
        } else {
            if !buf.is_empty() {
                buf.push_str("\n\n");
            }
            buf.push_str(&para);
        }
    }
    flush(&mut buf, &mut out);
    out
}

/// Hard-split a token that itself exceeds `target` into char windows.
fn hard_split(token: &str, target: usize) -> Vec<String> {
    token
        .chars()
        .collect::<Vec<_>>()
        .chunks(target)
        .map(|c| c.iter().collect())
        .collect()
}

/// Break text into atoms no larger than `target`, trying the coarsest unit
/// first: paragraph → sentence → word → hard char split.
fn atomize(text: &str, target: usize) -> Vec<String> {
    let mut atoms = Vec::new();
    for para in paragraphs(text) {
        if para.chars().count() <= target {
            atoms.push(para);
            continue;
        }
        for sent in sentences(&para) {
            if sent.chars().count() <= target {
                atoms.push(sent);
                continue;
            }
            // Pack words toward the target; hard-split any over-long word.
            let mut wbuf = String::new();
            for w in sent.split_whitespace() {
                if w.chars().count() > target {
                    if !wbuf.is_empty() {
                        atoms.push(std::mem::take(&mut wbuf));
                    }
                    atoms.extend(hard_split(w, target));
                    continue;
                }
                if !wbuf.is_empty() && wbuf.chars().count() + 1 + w.chars().count() > target {
                    atoms.push(std::mem::take(&mut wbuf));
                }
                if !wbuf.is_empty() {
                    wbuf.push(' ');
                }
                wbuf.push_str(w);
            }
            if !wbuf.is_empty() {
                atoms.push(wbuf);
            }
        }
    }
    atoms
}

/// Recursive character splitting: atomize to units ≤ target, then greedily pack
/// adjacent units up to the target.
fn recursive(text: &str, target: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    for atom in atomize(text, target) {
        if !buf.is_empty() && buf.chars().count() + 1 + atom.chars().count() > target {
            out.push(std::mem::take(&mut buf));
        }
        if !buf.is_empty() {
            buf.push(' ');
        }
        buf.push_str(&atom);
    }
    if !buf.trim().is_empty() {
        out.push(buf.trim().to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlap_does_not_begin_mid_token() {
        // Regression: the overlap window was `overlap` raw characters of the
        // previous chunk, so it could open inside a word. Indexing tokenises on
        // runs of alphanumerics, so the fragment became a term that appears
        // nowhere in the source — `triphosphate` split as `triphosp`/`hate`
        // put the real English word "hate" into a document about cellular
        // respiration, and lost the term the overlap existed to preserve.
        let text = "alpha beta gamma\n\ndelta epsilon";
        let out = chunk_with_overlap(text, ChunkStrategy::Paragraph, 8);
        assert_eq!(out.len(), 2);
        let second = &out[1];
        // The carried-over context starts at a whole word.
        assert!(
            second.starts_with("beta gamma "),
            "overlap began mid-token: {second:?}"
        );
        // And no token exists in the chunk that is absent from the source.
        for token in second.split(|c: char| !c.is_alphanumeric()) {
            if token.is_empty() {
                continue;
            }
            assert!(
                text.contains(token),
                "token {token:?} is not in the source text"
            );
        }
    }

    #[test]
    fn overlap_survives_a_script_without_spaces() {
        // Japanese has no word spaces and Han characters are alphanumeric, so
        // the whole tail is a single token. Dropping a "partial token" would
        // delete the overlap entirely; the raw window is kept instead, which
        // is the behaviour these scripts already had.
        let text = "日本語のテキストです\n\n次の段落";
        let out = chunk_with_overlap(text, ChunkStrategy::Paragraph, 4);
        assert_eq!(out.len(), 2);
        // Assert the property, not a hand-counted prefix: the second chunk
        // carries context from the first in addition to its own text.
        assert!(
            out[1].ends_with("次の段落") && out[1].chars().count() > "次の段落".chars().count(),
            "overlap was dropped for a space-less script: {:?}",
            out[1]
        );
    }

    #[test]
    fn paragraph_strategy_splits_on_blank_lines() {
        let text = "First para.\n\nSecond para.\n\n\n  ";
        let chunks = chunk(text, ChunkStrategy::Paragraph);
        assert_eq!(chunks, vec!["First para.", "Second para."]);
    }

    #[test]
    fn fixed_strategy_windows_by_chars() {
        let chunks = chunk("abcdefg", ChunkStrategy::Fixed(3));
        assert_eq!(chunks, vec!["abc", "def", "g"]);
    }

    #[test]
    fn recursive_keeps_chunks_under_target() {
        let text = "Alpha beta gamma delta. Epsilon zeta eta theta. Iota kappa lambda mu nu.";
        let chunks = chunk(text, ChunkStrategy::Recursive(30));
        assert!(chunks.len() >= 2);
        assert!(
            chunks.iter().all(|c| c.chars().count() <= 30),
            "every chunk within target: {chunks:?}"
        );
        // A single word longer than the target is hard-split, never exceeding it.
        let long = chunk(&"x".repeat(25), ChunkStrategy::Recursive(10));
        assert!(long.iter().all(|c| c.chars().count() <= 10));
    }

    #[test]
    fn overlap_carries_context_between_chunks() {
        let chunks = chunk_with_overlap("abcdefghij", ChunkStrategy::Fixed(5), 2);
        // Base: ["abcde", "fghij"]; with overlap 2 the 2nd carries "de ".
        assert_eq!(chunks[0], "abcde");
        assert_eq!(chunks[1], "de fghij");
        // No overlap or single chunk returns the base unchanged.
        assert_eq!(
            chunk_with_overlap("abcde", ChunkStrategy::Fixed(5), 2),
            vec!["abcde"]
        );
    }

    #[test]
    fn adaptive_merges_small_and_splits_large() {
        // Two tiny paragraphs merge under a generous target.
        let merged = chunk("a.\n\nb.", ChunkStrategy::Adaptive(50));
        assert_eq!(merged.len(), 1);

        // A long paragraph splits at sentence boundaries toward the target.
        let long = "Sentence one is here. Sentence two is here. Sentence three is here.";
        let split = chunk(long, ChunkStrategy::Adaptive(25));
        assert!(split.len() >= 2);
        assert!(split.iter().all(|c| !c.is_empty()));
    }
}
