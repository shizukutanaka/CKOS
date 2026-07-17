//! File-backed [`Storage`] — durable document persistence (§956 offline-first;
//! the basis for §927 session resume).
//!
//! Each document is one `<id>.doc` file in a directory. The format is a header
//! block (`key: value` lines) followed by a blank line and then the raw body,
//! so the body may contain anything — including blank lines — without escaping:
//!
//! ```text
//! doc_type: markdown
//! title: Design
//! author: alice
//! confidence: 100
//! embedding: 0.1,0.2,0.3
//! meta.project: ckos
//!
//! the body starts here and runs verbatim to end of file
//! ```
//!
//! Header **field values** (`doc_type`, `title`, `author`, and every
//! `meta.*` key/value) are backslash-escaped on write and unescaped on read
//! (`\` -> `\\`, newline -> `\n`, CR -> `\r`) — arbitrary `Document` content
//! (§937) is not guaranteed to be newline-free, and an unescaped title or
//! metadata value containing a blank line would otherwise be split at the
//! wrong point, silently shifting real headers (confidence, embedding, other
//! metadata) into the body on the next load. Metadata **keys** additionally
//! escape `:` (as `\c`), so a key containing the `": "` header delimiter
//! round-trips instead of being mis-split; the `meta.` prefix is likewise
//! stripped exactly once on read, so a key that itself begins with `meta.`
//! survives. The **body** itself is never escaped: it always follows the first
//! unescaped blank line, unambiguously.
//!
//! The store keeps an in-memory index and writes through to disk, so reads and
//! searches stay fast while every mutation is durable.

use crate::{Document, Query, Storage};
use ckos_kernel::error::{KernelError, Result};
use ckos_kernel::DocumentId;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

const EXT: &str = "doc";

/// Write `contents` to `path` atomically: write a sibling `<path>.tmp`, flush
/// it to disk, then rename over the destination. On POSIX the rename is
/// atomic, so a crash mid-write can never leave a truncated/corrupt file in
/// place of the previous good copy — readers see either the old file or the
/// complete new one.
fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|s| s.to_str()).unwrap_or("")
    ));
    {
        use std::io::Write;
        let mut f = fs::File::create(&tmp)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)
}

/// A directory of documents persisted to disk.
pub struct FileStore {
    dir: PathBuf,
    index: HashMap<DocumentId, Document>,
    /// How many `.doc` files were skipped as unreadable (I/O error or
    /// non-UTF-8) when the store was opened. See [`FileStore::skipped`].
    skipped: usize,
}

impl FileStore {
    /// Open (creating if needed) a store rooted at `dir`, loading any existing
    /// `*.doc` files into the index.
    ///
    /// A file that cannot be read (OS error, non-UTF-8 bytes) is **skipped**,
    /// not fatal: one damaged or foreign file must not make every other valid
    /// document in the session unreachable. The number of skipped files is
    /// reported by [`skipped`](Self::skipped) so callers can warn.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir).map_err(|e| KernelError::other(format!("create dir: {e}")))?;
        let mut index = HashMap::new();
        let mut skipped = 0usize;
        for entry in fs::read_dir(&dir).map_err(|e| KernelError::other(format!("read dir: {e}")))? {
            let entry = entry.map_err(|e| KernelError::other(format!("dir entry: {e}")))?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some(EXT) {
                continue;
            }
            let stem = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            let content = match fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            let doc = deserialize(DocumentId::from_raw(stem), &content);
            index.insert(doc.id.clone(), doc);
        }
        Ok(FileStore {
            dir,
            index,
            skipped,
        })
    }

    /// Number of `.doc` files skipped as unreadable when the store was opened
    /// (0 for a healthy directory). Callers surfacing sessions to users should
    /// warn when this is non-zero.
    pub fn skipped(&self) -> usize {
        self.skipped
    }

    /// Number of documents currently stored.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    fn path_for(&self, id: &DocumentId) -> PathBuf {
        self.dir.join(format!("{}.{EXT}", id.as_str()))
    }
}

impl Storage for FileStore {
    fn write(&mut self, doc: Document) -> Result<()> {
        write_atomic(&self.path_for(&doc.id), &serialize(&doc))
            .map_err(|e| KernelError::other(format!("write: {e}")))?;
        self.index.insert(doc.id.clone(), doc);
        Ok(())
    }

    fn read(&self, id: &DocumentId) -> Result<Option<Document>> {
        Ok(self.index.get(id).cloned())
    }

    fn delete(&mut self, id: &DocumentId) -> Result<()> {
        if self.index.remove(id).is_some() {
            let path = self.path_for(id);
            if path.exists() {
                fs::remove_file(path).map_err(|e| KernelError::other(format!("delete: {e}")))?;
            }
        }
        Ok(())
    }

    fn search(&self, query: &Query) -> Result<Vec<Document>> {
        let needle = query.text.as_deref().map(str::to_lowercase);
        let mut hits: Vec<Document> = self
            .index
            .values()
            .filter(|d| query.doc_type.as_ref().map_or(true, |t| &d.doc_type == t))
            .filter(|d| match &needle {
                Some(n) => d.title.to_lowercase().contains(n) || d.body.to_lowercase().contains(n),
                None => true,
            })
            .cloned()
            .collect();
        if query.limit > 0 {
            hits.truncate(query.limit);
        }
        Ok(hits)
    }
}

/// Escape a header field value so an embedded newline/CR/backslash can't be
/// mistaken for the header/body separator or corrupt line-by-line parsing:
/// `\` -> `\\`, `\n` -> `\n` (literal backslash-n), `\r` -> `\r`.
fn escape_header(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out
}

/// Escape a metadata **key** for the left side of a `key: value` header line.
/// Beyond what [`escape_header`] does, this also escapes `:` (as `\c`), so a
/// key containing the `": "` delimiter — legal in the public
/// `Document.metadata` map — can't make [`deserialize`]'s `split_once(": ")`
/// cut inside the key and shift its tail into the value. `\c` leaves no bare
/// colon in the on-disk key at all, so the first `": "` on the line is always
/// the real delimiter. Values keep bare colons (readable, and unambiguous
/// since everything after the first delimiter is the value).
fn escape_meta_key(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    for c in key.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            ':' => out.push_str("\\c"),
            _ => out.push(c),
        }
    }
    out
}

/// Reverse [`escape_header`] / [`escape_meta_key`]. An unrecognised escape (a
/// lone trailing backslash, or `\` followed by anything else) is kept literally
/// rather than dropped, so still-valid-looking-but-foreign content never
/// vanishes. `\c` (colon, only ever produced for keys) decodes back to `:`;
/// a bare `\c` never appears in a value on disk, since a literal backslash in a
/// value is written doubled (`\\`), so this rule can't corrupt a value.
fn unescape_header(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('c') => out.push(':'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Format one escaped `key: value\n` header line.
fn header_line(key: &str, value: &str) -> String {
    format!("{key}: {}\n", escape_header(value))
}

/// Encode a document to the on-disk format.
fn serialize(doc: &Document) -> String {
    let mut s = String::new();
    s.push_str(&header_line("doc_type", &doc.doc_type));
    s.push_str(&header_line("title", &doc.title));
    if let Some(a) = &doc.author {
        s.push_str(&header_line("author", a));
    }
    s.push_str(&format!("confidence: {}\n", doc.confidence));
    if let Some(emb) = &doc.embedding {
        let joined: Vec<String> = emb.iter().map(|x| x.to_string()).collect();
        s.push_str(&format!("embedding: {}\n", joined.join(",")));
    }
    for (k, v) in &doc.metadata {
        s.push_str(&header_line(&format!("meta.{}", escape_meta_key(k)), v));
    }
    s.push('\n'); // header/body separator
    s.push_str(&doc.body);
    s
}

/// Decode a document from the on-disk format. Tolerant: unknown headers are
/// ignored and missing fields fall back to defaults.
fn deserialize(id: DocumentId, content: &str) -> Document {
    let (header, body) = content.split_once("\n\n").unwrap_or((content, ""));

    let mut doc = Document::new("", "", body.to_string());
    doc.id = id;
    doc.author = None;
    doc.confidence = 0;
    doc.metadata.clear();

    for line in header.lines() {
        let Some((key, value)) = line.split_once(": ") else {
            continue;
        };
        match key {
            "doc_type" => doc.doc_type = unescape_header(value),
            "title" => doc.title = unescape_header(value),
            "author" => doc.author = Some(unescape_header(value)),
            "confidence" => doc.confidence = value.parse().unwrap_or(0),
            "embedding" => {
                // All-or-nothing: silently dropping unparseable components
                // would yield a wrong-dimension vector that skews similarity.
                // A corrupt embedding becomes None — the document stays fully
                // usable through keyword search and can be re-embedded.
                let parsed: std::result::Result<Vec<f32>, _> = value
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(str::parse)
                    .collect();
                doc.embedding = parsed.ok();
            }
            k => {
                // `strip_prefix` removes exactly one `meta.`, unlike
                // `trim_start_matches`, which strips it repeatedly and would
                // mangle a key that itself starts with `meta.` (e.g. the
                // on-disk `meta.meta.x` must decode to key `meta.x`, not `x`).
                if let Some(raw_key) = k.strip_prefix("meta.") {
                    doc.metadata
                        .insert(unescape_header(raw_key), unescape_header(value));
                }
                // Any other unknown header is ignored (forward compatibility).
            }
        }
    }
    doc
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static SEQ: AtomicU32 = AtomicU32::new(0);

    /// A unique temp directory for an isolated test, removed on drop.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            let n = SEQ.fetch_add(1, Ordering::SeqCst);
            let pid = std::process::id();
            let dir = std::env::temp_dir().join(format!("ckos-filestore-{pid}-{n}"));
            TempDir(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn persists_across_reopen() {
        let tmp = TempDir::new();
        let id;
        {
            let mut store = FileStore::open(&tmp.0).unwrap();
            let mut doc = Document::new("markdown", "Design", "line one\n\nline two");
            doc.author = Some("alice".into());
            doc.confidence = 88;
            doc.embedding = Some(vec![0.5, 0.25]);
            doc.metadata.insert("project".into(), "ckos".into());
            id = doc.id.clone();
            store.write(doc).unwrap();
        }
        // Reopen: the document must come back intact, body and all.
        let store = FileStore::open(&tmp.0).unwrap();
        let got = store.read(&id).unwrap().unwrap();
        assert_eq!(got.title, "Design");
        assert_eq!(got.author.as_deref(), Some("alice"));
        assert_eq!(got.confidence, 88);
        assert_eq!(got.body, "line one\n\nline two"); // blank line in body preserved
        assert_eq!(got.embedding, Some(vec![0.5, 0.25]));
        assert_eq!(
            got.metadata.get("project").map(String::as_str),
            Some("ckos")
        );
    }

    #[test]
    fn delete_removes_from_disk_and_index() {
        let tmp = TempDir::new();
        let mut store = FileStore::open(&tmp.0).unwrap();
        let doc = Document::new("note", "x", "y");
        let id = doc.id.clone();
        store.write(doc).unwrap();
        store.delete(&id).unwrap();
        assert!(store.is_empty());
        assert!(FileStore::open(&tmp.0).unwrap().is_empty());
    }

    #[test]
    fn search_filters_by_type_and_text() {
        let tmp = TempDir::new();
        let mut store = FileStore::open(&tmp.0).unwrap();
        store
            .write(Document::new("note", "alpha", "kernel design"))
            .unwrap();
        store
            .write(Document::new("log", "beta", "kernel boot"))
            .unwrap();
        let hits = store
            .search(&Query {
                text: Some("kernel".into()),
                doc_type: Some("note".into()),
                limit: 10,
            })
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "alpha");
    }

    #[test]
    fn title_with_embedded_blank_line_does_not_corrupt_other_fields() {
        // Regression: an unescaped title/metadata value containing "\n\n"
        // used to be split at the wrong point by deserialize's naive
        // `split_once("\n\n")`, silently shifting confidence/embedding/other
        // metadata into what became the body, and defaulting confidence to 0.
        let tmp = TempDir::new();
        let mut store = FileStore::open(&tmp.0).unwrap();
        let mut doc = Document::new(
            "note",
            "Title\n\nwith a blank line inside it",
            "the real body",
        );
        doc.confidence = 77;
        doc.embedding = Some(vec![0.1, 0.2]);
        doc.metadata
            .insert("summary".into(), "line one\n\nline two".into());
        let id = doc.id.clone();
        store.write(doc).unwrap();

        let reopened = FileStore::open(&tmp.0).unwrap();
        let got = reopened.read(&id).unwrap().unwrap();
        assert_eq!(got.title, "Title\n\nwith a blank line inside it");
        assert_eq!(got.body, "the real body");
        assert_eq!(got.confidence, 77, "confidence must survive intact");
        assert_eq!(got.embedding, Some(vec![0.1, 0.2]));
        assert_eq!(
            got.metadata.get("summary").map(String::as_str),
            Some("line one\n\nline two")
        );
    }

    #[test]
    fn backslashes_and_crlf_round_trip_in_header_fields() {
        let tmp = TempDir::new();
        let mut store = FileStore::open(&tmp.0).unwrap();
        let mut doc = Document::new(
            "note",
            r"a title with a \backslash\ and \n literal text",
            "body",
        );
        doc.author = Some("line1\r\nline2".into());
        doc.metadata
            .insert(r"weird\key".into(), r"value\with\backslashes".into());
        let id = doc.id.clone();
        store.write(doc).unwrap();

        let reopened = FileStore::open(&tmp.0).unwrap();
        let got = reopened.read(&id).unwrap().unwrap();
        assert_eq!(got.title, r"a title with a \backslash\ and \n literal text");
        assert_eq!(got.author.as_deref(), Some("line1\r\nline2"));
        assert_eq!(
            got.metadata.get(r"weird\key").map(String::as_str),
            Some(r"value\with\backslashes")
        );
    }

    #[test]
    fn metadata_key_containing_the_delimiter_round_trips() {
        // A metadata key containing ": " (the header delimiter) must survive a
        // write/reload cycle. `Document.metadata` is a public `HashMap<String,
        // String>`, and the module doc promises round-trip safety for "every
        // meta.* key" — but a bare ": " inside the key used to make
        // `deserialize`'s `split_once(": ")` cut in the wrong place, truncating
        // the key and prepending its tail to the value.
        let tmp = TempDir::new();
        let mut store = FileStore::open(&tmp.0).unwrap();
        let mut doc = Document::new("note", "t", "body");
        doc.metadata.insert("ratio: a:b".into(), "v1".into());
        let id = doc.id.clone();
        store.write(doc).unwrap();

        let reopened = FileStore::open(&tmp.0).unwrap();
        let got = reopened.read(&id).unwrap().unwrap();
        assert_eq!(
            got.metadata.get("ratio: a:b").map(String::as_str),
            Some("v1"),
            "metadata key with an embedded delimiter must round-trip: {:?}",
            got.metadata
        );
    }

    #[test]
    fn metadata_key_starting_with_the_meta_prefix_round_trips() {
        // On disk a metadata key `k` is stored as `meta.<k>`, so a key that
        // itself starts with `meta.` lands as `meta.meta.x`. The parser used
        // `trim_start_matches("meta.")`, which strips the prefix *repeatedly*
        // and decoded that back to `x` instead of `meta.x`. `strip_prefix`
        // removes exactly one.
        let tmp = TempDir::new();
        let mut store = FileStore::open(&tmp.0).unwrap();
        let mut doc = Document::new("note", "t", "body");
        doc.metadata.insert("meta.x".into(), "v1".into());
        let id = doc.id.clone();
        store.write(doc).unwrap();

        let reopened = FileStore::open(&tmp.0).unwrap();
        let got = reopened.read(&id).unwrap().unwrap();
        assert_eq!(
            got.metadata.get("meta.x").map(String::as_str),
            Some("v1"),
            "a metadata key starting with `meta.` must round-trip: {:?}",
            got.metadata
        );
    }

    #[test]
    fn writes_are_atomic_and_leave_no_temp_residue() {
        let tmp = TempDir::new();
        let mut store = FileStore::open(&tmp.0).unwrap();
        let doc = Document::new("note", "durable", "body");
        let id = doc.id.clone();
        store.write(doc).unwrap();

        // The final file exists with the full content; no `.tmp` sibling
        // survives the rename.
        let entries: Vec<String> = fs::read_dir(&tmp.0)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(entries.iter().any(|n| n.ends_with(".doc")));
        assert!(
            !entries.iter().any(|n| n.ends_with(".tmp")),
            "temp file must be renamed away, found: {entries:?}"
        );
        let got = FileStore::open(&tmp.0).unwrap().read(&id).unwrap().unwrap();
        assert_eq!(got.body, "body");
    }

    #[test]
    fn one_unreadable_doc_does_not_take_down_the_session() {
        // Regression: open() used to propagate the first read error, so a
        // single damaged/foreign .doc file made every other valid document
        // in the session unreachable.
        let tmp = TempDir::new();
        let id;
        {
            let mut store = FileStore::open(&tmp.0).unwrap();
            let doc = Document::new("note", "good", "healthy content");
            id = doc.id.clone();
            store.write(doc).unwrap();
        }
        // Drop a non-UTF-8 file with the .doc extension next to it.
        fs::write(tmp.0.join("damaged.doc"), [0xFF, 0xFE, 0x00, 0x80]).unwrap();

        let store = FileStore::open(&tmp.0).unwrap();
        assert_eq!(store.skipped(), 1, "the damaged file is skipped, counted");
        let got = store.read(&id).unwrap().unwrap();
        assert_eq!(got.title, "good", "healthy documents still load");
    }

    #[test]
    fn corrupt_embedding_component_drops_the_whole_vector() {
        // All-or-nothing: a partially unparseable embedding must become None,
        // never a silently shorter (wrong-dimension) vector.
        let doc = deserialize(
            DocumentId::from_raw("x"),
            "doc_type: note\ntitle: t\nembedding: 0.5,not_a_float,0.25\n\nbody",
        );
        assert_eq!(doc.embedding, None);
        // A fully valid vector still parses.
        let ok = deserialize(
            DocumentId::from_raw("y"),
            "doc_type: note\ntitle: t\nembedding: 0.5,0.25\n\nbody",
        );
        assert_eq!(ok.embedding, Some(vec![0.5, 0.25]));
    }

    #[test]
    fn escape_unescape_round_trips_arbitrary_strings() {
        for s in [
            "",
            "plain",
            "back\\slash",
            "new\nline",
            "carriage\rreturn",
            "mixed\\\n\r weird \\n \\\\ end",
            // A colon is left bare in values (readable, unambiguous) and must
            // still round-trip; a literal backslash-c must survive the new
            // `\c` unescape rule, since the backslash is written doubled.
            "ratio 3:1",
            "literal \\c sequence",
        ] {
            assert_eq!(unescape_header(&escape_header(s)), s, "round-trip: {s:?}");
        }
    }
}
