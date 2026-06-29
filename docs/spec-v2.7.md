# CKOS v2.7 — Unified Knowledge Platform

Integrates RAG, knowledge graph, long-term memory and workflows into a single
knowledge substrate rather than separate features.

## §935 Data layer

```
Applications → Cognitive Kernel → { Knowledge Platform | Runtime Layer }
Knowledge Platform: Vector DB · Graph DB · Object Store · SQL DB → Index Service
```

## §936 Storage abstraction

```rust
trait Storage { read; write; delete; search; watch; transaction; }
```
Backends are interchangeable: PostgreSQL, SQLite, RocksDB, Neo4j, SurrealDB,
Qdrant, Milvus, Weaviate, S3-compatible, Azure Blob, MinIO.
→ [`memory::Storage`](../memory/src/lib.rs). Implemented backends today:
`InMemoryStore` (volatile) and `FileStore` (durable, one `<id>.doc` file per
document, newline-safe header/body format) — the latter gives offline-first
persistence (§956) and the basis for session resume (§927).

## §937 Unified document model

Every artifact (Word, PDF, Markdown, HTML, CSV, JSON, Notebook) shares one
shape: `id, type, title, author, created, updated, metadata, body, embedding,
graph, attachments`. → `memory::Document`.

## §938–§940 Indexing & compression

- **§938 Index pipeline**: add → parse → chunk → embed → NER → relation
  extraction → graph build → vector insert → search-index update. Fully automatic.
- **§939 Chunk evolution**: no fixed chunking — Paragraph / Semantic /
  Hierarchical / Adaptive.
- **§940 Semantic compression**: old documents collapse full-text → summary →
  concept → knowledge to save memory. → `memory::compress_document`/`summarize`
  implement the first (summary) step, idempotently and auditably.

## §941–§943 Knowledge graph builder & versioning

- **§941 Builder** extracts: people, companies, APIs, OSS, libraries, events,
  concepts, algorithms, formulas, papers, patents, source code.
- **§942 Versioning**: the graph is managed Git-like (v1 → v2 → branch → merge).
- **§943 Merge**: AI merge / human merge / policy merge on conflict.

## §944–§946 Embeddings & temporal knowledge

- **§944 Embedding manager**: small/medium/large/multilingual/code/image/audio/
  math, switched automatically by use. → `memory::Embedder` trait;
  `HashingEmbedder` is the dependency-free default, `cosine` scores similarity.
- **§945 Cross-modal embedding**: image/audio/code unified into one space.
- **§946 Temporal knowledge**: versions carry time (API → Version → Deprecated →
  Removed); search can be time-scoped. → `graph::Node::date` + KQL
  `BEFORE`/`AFTER` enforcement (ISO dates; nodes without a date are excluded
  from temporal queries).

## §947–§948 Provenance & confidence

- **§947 Provenance engine**: every fact keeps its origin (GitHub, paper, wiki,
  conversation, PDF, URL, history). → `graph::Node::provenance`, surfaced by KQL
  `RETURN Sources`.
- **§948 Confidence score**: 0–100 on all information, used during reasoning.
  → `graph::Node::confidence`, `memory::Document::confidence`.

## §949–§952 Retrieval & reasoning

- **§949 Retrieval planner**: question → goal analysis → strategy → graph +
  vector + full-text search → merge. → `sdk::retrieval::plan_retrieval`.
- **§950 Hybrid search**: vector + keyword + graph + metadata + time, run
  together. → `sdk::retrieval::Retriever` (keyword + graph today; vector next).
- **§951 Graph reasoning**: traverse relations (A → depends → B → maintained_by →
  Company). → graph label search in the retriever.
- **§952 Multi-hop planner**: estimate hops → traverse → reason → answer.
  → `graph::KnowledgeGraph::traverse`, driven by the retriever's hop expansion.

## §953–§960 Memory ops, security, distribution, API

- **§953 Consolidation**: working memory → "sleep phase" → long memory.
  → `memory::compress_document`/`summarize` (first compression step).
- **§954 Garbage collection**: expired, low-confidence, duplicate, broken
  embeddings, orphaned graph nodes. → `memory::collect` with a `GcPolicy`
  (expired/low-confidence/duplicate/broken-embedding); via `ckos gc`.
- **§955 Encryption**: AES-256 at rest, TLS 1.3 in transit, keys in OS secret store.
- **§956 Offline-first**: sync + diff + conflict resolution; usable without cloud.
- **§957 Distributed knowledge**: knowledge sharded across nodes, partial sync.
- **§958 Search cache**: query/result/graph/embedding caches.
- **§959 Learning pipeline**: search → use → evaluate → improve → re-embed →
  graph update (closed loop).
- **§960 Unified knowledge API**: `/search /document /graph /vector /history
  /embedding /entity /source` over REST/gRPC/MCP/SDK.

## §961–§962 Proposals

- **§961 AI-native filesystem**: sidecar files (`api.rs.embedding`,
  `api.rs.graph`, `api.rs.history`, `api.rs.metadata`) tie the filesystem to the
  knowledge platform.
- **§962 Knowledge Query Language (KQL)**: a cross-knowledge query language, e.g.

  ```
  FIND Concept "Transformer"
  RELATED Algorithm
  FILTER Confidence > 90
  BEFORE 2025-01-01
  RETURN Graph + Sources
  ```

  compiled down to graph/vector/full-text searches.
  → `sdk::kql`: `parse` (tokeniser + recursive-descent → AST) and `execute`
  (runs against the knowledge graph); `ckos kql "<query>"` demos it. Supports
  `FIND`, `RELATED`, `FILTER Confidence`, `BEFORE`/`AFTER`, `RETURN`. Temporal
  bounds parse but are not yet enforced (§946 pending).
