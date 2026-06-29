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
//! The store keeps an in-memory index and writes through to disk, so reads and
//! searches stay fast while every mutation is durable.

use crate::{Document, Query, Storage};
use ckos_kernel::error::{KernelError, Result};
use ckos_kernel::DocumentId;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

const EXT: &str = "doc";

/// A directory of documents persisted to disk.
pub struct FileStore {
    dir: PathBuf,
    index: HashMap<DocumentId, Document>,
}

impl FileStore {
    /// Open (creating if needed) a store rooted at `dir`, loading any existing
    /// `*.doc` files into the index.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir).map_err(|e| KernelError::other(format!("create dir: {e}")))?;
        let mut index = HashMap::new();
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
            let content =
                fs::read_to_string(&path).map_err(|e| KernelError::other(format!("read: {e}")))?;
            let doc = deserialize(DocumentId::from_raw(stem), &content);
            index.insert(doc.id.clone(), doc);
        }
        Ok(FileStore { dir, index })
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
        fs::write(self.path_for(&doc.id), serialize(&doc))
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

/// Encode a document to the on-disk format.
fn serialize(doc: &Document) -> String {
    let mut s = String::new();
    s.push_str(&format!("doc_type: {}\n", doc.doc_type));
    s.push_str(&format!("title: {}\n", doc.title));
    if let Some(a) = &doc.author {
        s.push_str(&format!("author: {a}\n"));
    }
    s.push_str(&format!("confidence: {}\n", doc.confidence));
    if let Some(emb) = &doc.embedding {
        let joined: Vec<String> = emb.iter().map(|x| x.to_string()).collect();
        s.push_str(&format!("embedding: {}\n", joined.join(",")));
    }
    for (k, v) in &doc.metadata {
        s.push_str(&format!("meta.{k}: {v}\n"));
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
            "doc_type" => doc.doc_type = value.to_string(),
            "title" => doc.title = value.to_string(),
            "author" => doc.author = Some(value.to_string()),
            "confidence" => doc.confidence = value.parse().unwrap_or(0),
            "embedding" => {
                let v: Vec<f32> = value
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .filter_map(|s| s.parse().ok())
                    .collect();
                doc.embedding = Some(v);
            }
            k if k.starts_with("meta.") => {
                doc.metadata
                    .insert(k.trim_start_matches("meta.").to_string(), value.to_string());
            }
            _ => {}
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
}
