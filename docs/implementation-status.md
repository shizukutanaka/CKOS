# CKOS Implementation Status

Traceability from every spec section to the code that implements it. Legend:
✅ implemented · 🟡 partial (core done, noted gap) · ⏳ design only / pending.

The workspace is intentionally `std`-only, so it builds and tests offline; items
marked ⏳ are those whose realistic implementation needs external crates
(networking, real crypto, model runtimes, databases) or app targets
(desktop/mobile/k8s) outside that constraint.

## v2.5 — Core Kernel

| § | Topic | Status | Where |
|---|-------|--------|-------|
| 889 | System overview | ✅ | `README.md`, `docs/architecture.md` |
| 890 | Rust workspace | ✅ | `Cargo.toml` (13 crates) |
| 891 | Kernel responsibilities (no inference) | ✅ | `kernel` |
| 892 | Four-layer scheduler | ✅ | `scheduler::Scheduler` (multi-factor score + priority aging; note aging only matters under continuous arrivals — `run_workflow` drains a batch, where retries are the only mid-loop arrivals) |
| 893 | Task state machine | ✅ | `kernel::task::TaskState`, driven live by `Engine::execute`; the recovery loop (`Failed → Rollback → Retry → Queued`, bounded by `MAX_TASK_RETRIES`) is driven by `Engine::run_workflow`. `Planning` remains a declared-but-unentered state (execute goes `Queued → Running` directly) |
| 894 | Event bus | 🟡 | `kernel::event` — published & consumed: TaskStarted/TaskCompleted/TaskFailed (both failure paths)/PolicyViolation (on §929 denial)/GraphChanged/WorkflowCompleted. Never published anywhere yet: TaskCreated, RuntimeLoaded, MemoryUpdated, PluginInstalled, AgentRegistered |
| 895 | Workflow DAG | ✅ | `workflow::Dag` (Kahn's-algorithm topological order; rejects duplicate step names) |
| 896 | Memory hierarchy L0–L5 | 🟡 | `rank_memories` (Generative-Agents recency×importance×relevance; consumed by `Session::recall`, reachable via `ckos history <dir> <query…>`, see §927). The six-level *tier* vocabulary was removed: it classified nothing — documents carried no tier and no code path routed by one — so it satisfied this section in name only. Real tiering needs a tier on `Document` plus promote/demote logic with a consumer |
| 897 | Knowledge graph | ✅ | `graph` (+ `GraphStore` file persistence) |
| 898 | Planner | ✅ | `planner` — deliberately never infers regulated capabilities (finance/medical/legal/robotics) from free text; a keyword classifier was tested and rejected as unsafe (see module doc) |
| 899 | Verifier (independent) | ✅ | `verifier` (non-empty, repetition/degeneration, arithmetic, JSON, citation, security-policy) |
| 900 | Runtime registry | ✅ | `runtime` (trait + registry; the `list`/`RuntimeInfo` table is surfaced by `ckos runtimes`; real engines ⏳) |
| 901 | Plugin SDK | 🟡 | `plugins` (tool/registry/permissions, `ckos tool`; WASM sandbox ⏳) |
| 902 | API gateway | 🟡 | `cli` (done) + `web` (`ckos serve`: `std`-only HTTP/JSON REST-style API + embedded browser dashboard, no TLS/auth — see README); a single `Engine` is shared server-lifetime (`routes::AppState`), so its audit/telemetry accumulate across requests and are exposed at `GET /api/status`; gRPC/WebSocket/MCP ⏳ |
| 903 | Audit logging | ✅ | `kernel::audit` — task execution (`Engine::execute`, incl. policy denials) and tool runs (`ckos tool`, allowed *and* denied, trail printed on exit); `ckos serve`'s `GET /api/status` reports the running total across the server's lifetime. `.plugin()` field still has no producer |
| 904 | Telemetry | ✅ | `kernel::telemetry` — latency feeds scheduling via `run_workflow`'s telemetry-scored submission (§913); `ckos serve`'s `GET /api/status` reports cumulative tokens/mean latency. Hardware counters (CPU/GPU/NPU/power) ⏳ — the former `ResourceProbe` seam was removed because nothing ever called `sample()`, so implementing it produced a snapshot no consumer read |
| 905 | CI/CD | 🟡 | `docs/ci-workflow.yml` (fmt → clippy `-D warnings` → build → test → CLI smoke) — complete and ready, but the automation lacks the `workflows` permission to push `.github/workflows/` (verified: the push was rejected), so a maintainer must copy it to `.github/workflows/ci.yml` once |
| 906 | Implementation priority | ✅ | `docs/roadmap.md` |

## v2.6 — Agent Service Mesh

| § | Topic | Status | Where |
|---|-------|--------|-------|
| 907–909 | Agent as service / manifest / lifecycle | ✅ | `sdk::agent` — `AgentState::transition` validates the §909 graph and `discover` now actually honours it (excludes `Suspended`/`Terminated`; was previously ignored entirely) |
| 910–912 | Capability registry / discovery | ✅ | `sdk::CapabilityRegistry`, `kernel::Capability` |
| 913 | Multi-factor agent scheduler | ✅ | `scheduler::ScoreFactors`, fed live: `Engine::run_workflow` submits every task via `submit_scored` with `recommended_factors` (observed runtime latency → `runtime_fit`, closing §904→§913) and adopts the serving agent's `AgentManifest.priority` as the task priority. The remaining factors (deadline/importance/cost/energy/confidence) still have no producer |
| 914–916 | Message bus / format / service mesh | ✅ | `sdk::messaging` |
| 917–919 | Tool registry / adapter / permissions | ✅ | `plugins` (permission gate incl. `.*` wildcards, shared with `policy` via `kernel::permission_matches`); `ckos tool` grants are authorized by `PolicyEngine`, not self-asserted |
| 920 | Workflow compiler | ✅ | `planner` (intent → DAG) |
| 921–922 | Agent / collective reflection | ✅ | `sdk::reflection` (confidence-weighted majority-vote consensus, self-consistency) |
| 923 | Knowledge bus | ✅ | `sdk::knowledge_bus` |
| 924–925 | Runtime pool / edge | 🟡 | `runtime::select` (local-preferred, deterministic ties, tested across all `RuntimeKind`s); real edge runtimes ⏳ |
| 926 | Distributed workflow | ⏳ | sync engine done; distributed driver pending |
| 927 | Session manager | ✅ | `sdk::session` (history/reflections persisted by `ckos run --session`); `recall` (Generative-Agents scoring) reachable via `ckos history <dir> <query…> [--k N]`, which recalls instead of dumping raw history |
| 928 | Enterprise identity | 🟡 | `policy::IdentityProvider` + `StaticTokenProvider`, with a real caller: `ckos tool`/`run`/`workflow --token <token>` authenticates against a demo provider (`cli::demo_identity_provider`), producing an `Identity` with roles *and* attributes. OIDC/LDAP verification ⏳ |
| 929 | Authorization (RBAC + ABAC) | ✅ | `policy` — RBAC+ABAC authorize `ckos tool` and `Engine`'s sensitive capabilities (finance/medical/legal/robotics) via opt-in `Engine::with_identity`/`with_policy` + `ckos run`/`ckos workflow`/`ckos tool --role\|--token`; denials emit `Event::PolicyViolation`. `--token` (§928) carries real ABAC attributes end-to-end — `Engine::with_identity` + `Identity::request` — proven by a demo rule that denies `capability.medical` for a `region=restricted` attribute even though the RBAC role alone would allow it; `--role` remains a bare-roles convenience with no attributes |
| 930 | Distributed security | 🟡 | `sdk::security` (signing + replay; mTLS/cert rotation ⏳) |
| 931–932 | Kubernetes / Docker Compose | ✅ | `Dockerfile` + `docker-compose.yml` (dev stack) + `deploy/k8s/ckos.yaml` (Deployment + HPA autoscale) |
| 933 | Observability | 🟡 | `audit` + `telemetry`; OpenTelemetry/Prometheus export ⏳ |
| 934 | Positioning | — | narrative |

## v2.7 — Unified Knowledge Platform

| § | Topic | Status | Where |
|---|-------|--------|-------|
| 935 | Data layer | ✅ | `memory` + `graph` |
| 936 | Storage abstraction | ✅ | `memory::Storage` (`InMemoryStore`, `FileStore`) + `graph::GraphStore` (graph persistence); SQL/vector DBs ⏳ |
| 937 | Unified document model | ✅ | `memory::Document` |
| 938 | Index pipeline | 🟡 | `knowledge_bus::ingest_text` (extract → queue) + `Reindexer` (embed + insert); deep parse/chunk ⏳ |
| 939 | Chunk evolution | 🟡 | `memory::chunk` (Paragraph/Fixed/Adaptive/Recursive + `chunk_with_overlap`; semantic & hierarchical ⏳) |
| 940 | Semantic compression | 🟡 | `memory::compress_document`/`summarize`/`keywords` (summary + concept tiers; knowledge tier ⏳), reachable via `ckos gc --consolidate` (§953) |
| 941 | Knowledge graph builder | 🟡 | `graph::extract` (heuristic entities + typed-relation edges, now queryable by relation type via KQL `RELATED … VIA <edge-kind>`; `ckos graph` and auto-built by `ckos run --session`; re-extraction over a persisted graph reinforces nodes without duplicating edges; statistical NER ⏳) |
| 942–943 | Graph versioning / merge | 🟡 | `graph::versioning` (complete, tested library — commits/branches/3 merge strategies — but no CLI or engine path uses it yet) |
| 944 | Embedding manager | ✅ | `memory::Embedder` / `HashingEmbedder` — a lexical hash, **not semantic**: cannot match paraphrases/synonyms (measured; see embedding.rs); real model ⏳ |
| 945 | Cross-modal embedding | ⏳ | single-space design; modality encoders pending |
| 946 | Temporal knowledge | ✅ | `graph::Node::date` + KQL `BEFORE`/`AFTER` |
| 947 | Provenance engine | ✅ | `graph::Node::provenance`; extraction stamps source (`extract_concepts_with_provenance`); KQL `RETURN Sources` |
| 948 | Confidence score | ✅ | `Node::confidence`, `Document::confidence` |
| 949 | Retrieval planner | ✅ | `retrieval::plan_retrieval` + `search_diverse`/`mmr_rerank` (MMR) + `expand_query`/`search_expanded` (PRF) + `sdk::synonyms::SynonymTable`/`expand_query_with_synonyms` (a priori domain-term expansion; mitigates the §944 lexical-only gap) |
| 950 | Hybrid search | ✅ | `retrieval::Retriever` (BM25+ keyword [Lv & Zhai 2011 δ lower-bound] + vector + graph, Reciprocal Rank Fusion) |
| 951 | Graph reasoning | ✅ | retrieval graph hits + `graph::traverse`/`neighbors_via` (typed-relation hop) + `pagerank`/`personalized_pagerank`/`central_nodes` (node importance); KQL `RELATED … VIA <edge-kind>` reasons over relation types |
| 952 | Multi-hop planner | ✅ | retriever graph expansion is a **Personalized PageRank** pass seeded on the query's matched nodes (HippoRAG, arXiv:2405.14831/2502.14802) — associated nodes rank by query-mass flow, so multi-path corroboration outranks single long paths; `graph::traverse`/`traverse_with_hops` remain for distance-annotated walks |
| 953 | Memory consolidation | ✅ | `memory::consolidate` (sleep-phase pass compressing oversized docs), reachable via `ckos gc --consolidate N`, which runs before the document/graph GC passes; also driving the §940 compression ladder it calls into |
| 954 | Garbage collection | ✅ | `ckos gc`: `memory::collect` (documents; expiry via `--now <date>`) + `graph::KnowledgeGraph::remove_orphans` sweeping the session's persisted graph |
| 955 | Data encryption | ⏳ | at-rest/in-transit pending (transport layer) |
| 956 | Offline-first | ✅ | `FileStore` + std-only build; header fields (title/author/metadata) are backslash-escaped on write so an embedded newline can't shift real headers into the body on reload; writes are atomic (temp + fsync + rename) and one unreadable `.doc` is skipped with a warning instead of blocking the whole session |
| 957 | Distributed knowledge | ⏳ | sharding/partial-sync pending |
| 958 | Search cache | ✅ | `sdk::retrieval::SearchCache` (LRU query→hits cache), one per session directory, held in `ckos serve`'s shared `AppState` and warmed across `/api/search` requests (the CLI's one-shot processes still have no cache to warm) — invalidated whenever `/api/run` mutates that session |
| 959 | Learning pipeline | 🟡 | reflection persistence + auto-reindex + `sdk::eval` (Precision/Recall/MRR/nDCG); full closed loop ⏳ |
| 960 | Unified knowledge API | 🟡 | `cli` (`search`/`kql`/`history`/`eval`); network API ⏳ |
| 961 | AI-native filesystem | ⏳ | proposal |
| 962 | Knowledge Query Language | ✅ | `sdk::kql` — FIND/RELATED (with `VIA <edge-kind>` typed-relation filter)/FILTER (AND/OR/NOT)/BEFORE/AFTER/ORDER/LIMIT/RETURN; `ckos kql` incl. `--session` |

## Summary

Every core mechanism the spec describes has a working, tested implementation
behind a trait seam where a production backend will later plug in. The ⏳ items
fall into three buckets, all deliberate:

1. **External-dependency backends** — real inference runtimes, SQL/vector
   databases, HMAC/mTLS crypto, OpenTelemetry export, an async runtime. The
   traits (`Runtime`, `Storage`, `Embedder`, `AuditSink`, `EventBus`,
   `IdentityProvider`) exist; only the concrete impls are pending.
2. **App / deployment targets** — desktop, mobile, Kubernetes, Docker Compose.
3. **Advanced data-pipeline features** — statistical graph extraction
   (heuristic extraction shipped in `graph::extract`; NER model pending),
   semantic/hierarchical chunking, cross-modal encoders, sharding.

See [`roadmap.md`](roadmap.md) for sequencing.
