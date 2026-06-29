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
}

/// Split `text` into chunks per `strategy`. Empty/whitespace chunks are dropped.
pub fn chunk(text: &str, strategy: ChunkStrategy) -> Vec<String> {
    match strategy {
        ChunkStrategy::Paragraph => paragraphs(text),
        ChunkStrategy::Fixed(n) => fixed(text, n.max(1)),
        ChunkStrategy::Adaptive(target) => adaptive(text, target.max(1)),
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

#[cfg(test)]
mod tests {
    use super::*;

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
