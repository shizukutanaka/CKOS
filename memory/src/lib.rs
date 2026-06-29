//! # CKOS Memory
//!
//! Implements the memory hierarchy (§896), the unified document model (§937)
//! and the storage abstraction (§936). Caches and persistent data are kept
//! cleanly separated by [`MemoryTier`], so consolidation (§953) and garbage
//! collection (§954) can reason about each level independently.

use ckos_kernel::error::Result;
use ckos_kernel::DocumentId;
use std::collections::HashMap;

mod embedding;
mod file_store;
mod maintenance;
pub use embedding::{cosine, Embedder, HashingEmbedder};
pub use file_store::FileStore;
pub use maintenance::{
    collect, compress_document, keywords, summarize, GcPolicy, GcReason, GcReport,
};

/// The six-level memory hierarchy from §896.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MemoryTier {
    /// L0 — registers / hottest scratch state.
    Register,
    /// L1 — context cache.
    ContextCache,
    /// L2 — working memory.
    Working,
    /// L3 — semantic memory.
    Semantic,
    /// L4 — knowledge graph.
    KnowledgeGraph,
    /// L5 — cold archive.
    Archive,
}

impl MemoryTier {
    /// Whether this tier is a volatile cache rather than durable storage.
    pub fn is_cache(self) -> bool {
        matches!(self, MemoryTier::Register | MemoryTier::ContextCache)
    }
}

/// The unified document model (§937): every artifact — Markdown, PDF, code,
/// notebook — shares this shape.
#[derive(Debug, Clone)]
pub struct Document {
    pub id: DocumentId,
    pub doc_type: String,
    pub title: String,
    pub author: Option<String>,
    pub body: String,
    /// Free-form metadata.
    pub metadata: HashMap<String, String>,
    /// Optional embedding vector (§944).
    pub embedding: Option<Vec<f32>>,
    /// Confidence 0..=100 (§948).
    pub confidence: u8,
}

impl Document {
    /// Create a minimal document.
    pub fn new(
        doc_type: impl Into<String>,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        Document {
            id: DocumentId::new(),
            doc_type: doc_type.into(),
            title: title.into(),
            author: None,
            body: body.into(),
            metadata: HashMap::new(),
            embedding: None,
            confidence: 100,
        }
    }
}

/// A query passed to a [`Storage`] backend.
#[derive(Debug, Clone, Default)]
pub struct Query {
    /// Free-text match against title/body.
    pub text: Option<String>,
    /// Restrict to a document type.
    pub doc_type: Option<String>,
    /// Maximum results.
    pub limit: usize,
}

/// Storage abstraction (§936). Backends (SQLite, RocksDB, Qdrant, S3, …) are
/// interchangeable behind this trait.
pub trait Storage: Send + Sync {
    /// Persist or replace a document.
    fn write(&mut self, doc: Document) -> Result<()>;
    /// Fetch a document by id.
    fn read(&self, id: &DocumentId) -> Result<Option<Document>>;
    /// Delete a document.
    fn delete(&mut self, id: &DocumentId) -> Result<()>;
    /// Search documents.
    fn search(&self, query: &Query) -> Result<Vec<Document>>;
}

/// In-memory storage backend — the default for tests and offline use.
#[derive(Default)]
pub struct InMemoryStore {
    docs: HashMap<DocumentId, Document>,
}

impl InMemoryStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of stored documents.
    pub fn len(&self) -> usize {
        self.docs.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }
}

impl Storage for InMemoryStore {
    fn write(&mut self, doc: Document) -> Result<()> {
        self.docs.insert(doc.id.clone(), doc);
        Ok(())
    }

    fn read(&self, id: &DocumentId) -> Result<Option<Document>> {
        Ok(self.docs.get(id).cloned())
    }

    fn delete(&mut self, id: &DocumentId) -> Result<()> {
        self.docs.remove(id);
        Ok(())
    }

    fn search(&self, query: &Query) -> Result<Vec<Document>> {
        let needle = query.text.as_deref().map(str::to_lowercase);
        let mut hits: Vec<Document> = self
            .docs
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_tiers_classified() {
        assert!(MemoryTier::ContextCache.is_cache());
        assert!(!MemoryTier::Archive.is_cache());
        assert!(MemoryTier::Register < MemoryTier::Archive);
    }

    #[test]
    fn store_round_trip_and_search() {
        let mut store = InMemoryStore::new();
        let doc = Document::new("markdown", "Design", "the CKOS kernel never infers");
        let id = doc.id.clone();
        store.write(doc).unwrap();
        assert_eq!(store.read(&id).unwrap().unwrap().title, "Design");

        let results = store
            .search(&Query {
                text: Some("kernel".into()),
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(results.len(), 1);

        store.delete(&id).unwrap();
        assert!(store.is_empty());
    }
}
