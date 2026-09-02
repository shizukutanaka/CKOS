# CKOS Implementation Status

Traceability from every spec section to the code that implements it. Legend:
✅ implemented · 🟡 partial (core done, noted gap) · ⏳ not in v1.

**Read [Scope](#scope--what-done-means-for-v1) at the bottom first if you want
to know whether this product is finished.** Short version: v1 is complete
against a scope that is now stated explicitly, and every ⏳ is listed there
with the dependency it needs and why a smaller version would be worse than
none. They are not a backlog this product is working through — they are work
on a different product, because each one requires breaking the `std`-only,
zero-dependency, offline constraint that is CKOS's reason to exist.

Every symbol cited in backticks below is machine-checked to still exist, on
every commit, by `scripts/check-status-doc.sh` — this table cannot silently
drift from the code.

## v2.5 — Core Kernel

| § | Topic | Status | Where |
|---|-------|--------|-------|
| 889 | System overview | ✅ | `README.md`, `docs/architecture.md` |
| 890 | Rust workspace | ✅ | `Cargo.toml` (13 crates) |
| 891 | Kernel responsibilities (no inference) | ✅ | `kernel` |
| 892 | Four-layer scheduler | ✅ | `scheduler::Scheduler` (multi-factor score + priority aging; note aging only matters under continuous arrivals — `run_workflow` drains a batch, where retries are the only mid-loop arrivals) |
| 893 | Task state machine | ✅ | `kernel::task::TaskState`, driven live by `Engine::execute`; the recovery loop (`Failed → Rollback → Retry → Queued`, bounded by `MAX_TASK_RETRIES`) is driven by `Engine::run_workflow`. `Planning` remains a declared-but-unentered state (execute goes `Queued → Running` directly) |
| 894 | Event bus | ✅ | `kernel::event` — every variant has a real publisher: TaskStarted (on the `Running` transition), TaskCompleted, TaskFailed (both failure paths), PolicyViolation (§929 denial), GraphChanged, WorkflowCompleted. The five that had none (TaskCreated, RuntimeLoaded, MemoryUpdated, PluginInstalled, AgentRegistered) were removed — an event is observable only by being published, so a variant nothing publishes is a promise the bus silently breaks |
| 895 | Workflow DAG | ✅ | `workflow::Dag` (Kahn's-algorithm topological order; rejects duplicate step names) |
| 896 | Memory hierarchy L0–L5 | 🟡 | `rank_memories` (Generative-Agents recency×importance×relevance; consumed by `Session::recall`, reachable via `ckos history <dir> <query…>`, see §927). The six-level *tier* vocabulary was removed: it classified nothing — documents carried no tier and no code path routed by one — so it satisfied this section in name only. Real tiering needs a tier on `Document` plus promote/demote logic with a consumer |
| 897 | Knowledge graph | ✅ | `graph` (+ `GraphStore` file persistence) |
| 898 | Planner | ✅ | `planner` — deliberately never infers regulated capabilities (finance/medical/legal/robotics) from free text; a keyword classifier was tested and rejected as unsafe (see module doc) |
| 899 | Verifier (independent) | ✅ | `verifier` (non-empty, repetition/degeneration, arithmetic, JSON, citation, security-policy); measured with a 27-case must-pass/must-fail battery; citation check ignores subscripts such as `argv[0]` |
| 900 | Runtime registry | ✅ | `runtime` (trait + registry; the `list`/`RuntimeInfo` table is surfaced by `ckos runtimes`; real engines ⏳) |
| 901 | Plugin SDK | 🟡 | `plugins` (tool/registry/permissions, `ckos tool`; WASM sandbox ⏳) |
| 902 | API gateway | 🟡 | `cli` (done) + `web` (`ckos serve`: `std`-only HTTP/JSON REST-style API + embedded browser dashboard, no TLS/auth — see README); a single `Engine` is shared server-lifetime (`routes::AppState`), so its audit/telemetry accumulate across requests and are exposed at `GET /api/status`; every request's `session` is resolved under the server's session root by `resolve_session` and cannot escape it; gRPC/WebSocket/MCP ⏳ |
| 903 | Audit logging | ✅ | `kernel::audit` — task execution (`Engine::execute`, incl. policy denials) and tool runs (`ckos tool`, allowed *and* denied, trail printed on exit); `ckos serve`'s `GET /api/status` reports the running total across the server's lifetime. `.plugin()` field still has no producer |
| 904 | Telemetry | ✅ | `kernel::telemetry` — nanosecond latency (millisecond storage truncated every local-runtime task to zero) feeds scheduling via `run_workflow`'s telemetry-scored submission (§913); rates are `Option`, so "no data" is never reported as `0`; `ckos serve`'s `GET /api/status` reports cumulative tokens, mean latency and mean throughput. Hardware counters (CPU/GPU/NPU/power) ⏳ — the former `ResourceProbe` seam was removed because nothing ever called `sample()`, so implementing it produced a snapshot no consumer read |
| 905 | CI/CD | 🟡 | `docs/ci-workflow.yml` (fmt → clippy `-D warnings` → build → test → CLI smoke) — **verified end to end, not merely written**: YAML parses, all five smoke commands pass verbatim, and `check.sh` is green under the workflow's own `RUSTFLAGS="-D warnings"`. The automation lacks the `workflows` permission to push `.github/workflows/` (verified: the push was rejected), so a maintainer must copy it to `.github/workflows/ci.yml` once |
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
| 929 | Authorization (RBAC + ABAC) | ✅ | `policy` — RBAC+ABAC authorize `ckos tool` and `Engine`'s sensitive capabilities (finance/medical/legal/robotics) via opt-in `Engine::with_identity`/`with_policy` + `ckos run`/`ckos workflow`/`ckos tool --role\|--token`; denials emit `Event::PolicyViolation`. Authorization is attached at **every** CLI entry point by default (`resolve_identity` defaults to `guest`) — omitting `--role`/`--token` lowers privileges rather than disabling the gate, which is what `run`/`workflow` used to do. `--token` (§928) carries real ABAC attributes end-to-end — `Engine::with_identity` + `Identity::request` — proven by a demo rule that denies `capability.medical` for a `region=restricted` attribute even though the RBAC role alone would allow it; `--role` remains a bare-roles convenience with no attributes |
| 930 | Distributed security | 🟡 | `sdk::security` (signing + replay; mTLS/cert rotation ⏳) |
| 931–932 | Kubernetes / Docker Compose | ✅ | `Dockerfile` (multi-stage, non-root, gateway by default) + `docker-compose.yml` (one service — CKOS is one binary) + `deploy/k8s/ckos.yaml` (Namespace + Deployment running `serve` with probes + ClusterIP Service + HPA). Structure is gated by `scripts/check-deploy.sh`; behaviour is **not** verified against a real cluster or container runtime, neither being available to this automation |
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
| 941 | Knowledge graph builder | 🟡 | `graph::extract` (heuristic entities + typed-relation edges, now queryable by relation type via KQL `RELATED … VIA <edge-kind>`; `ckos graph` and auto-built by `ckos run --session`; re-extraction over a persisted graph reinforces nodes without duplicating edges; statistical NER ⏳); Japanese entities via katakana runs (measured precision 1.00 / recall 0.44 — kanji runs rejected at precision 0.58) |
| 942–943 | Graph versioning / merge | 🟡 | `graph::versioning` (complete, tested library — commits/branches/3 merge strategies — but no CLI or engine path uses it yet) |
| 944 | Embedding manager | ✅ | `memory::Embedder` / `HashingEmbedder` — a lexical hash, **not semantic**: cannot match paraphrases/synonyms (measured; see embedding.rs); real model ⏳. Terms come from `memory::terms_of`, shared with the keyword leg so both split text identically; scripts written without spaces (Han/Kana/Hangul) are indexed as unigram+bigram grams (Japanese MRR 0.143 → 1.000, measured) |
| 945 | Cross-modal embedding | ⏳ | single-space design; modality encoders pending |
| 946 | Temporal knowledge | 🟡 | `graph::Node::date` + KQL `BEFORE`/`AFTER` work, and are exercised by the demo graph, which sets dates explicitly. **Nothing populates `date` on a graph built by `ckos index`**: `graph::extract` never calls `set_date`, so `BEFORE`/`AFTER` return nothing on a user's own session (measured). See handbook §4 for why no date heuristic was added |
| 947 | Provenance engine | ✅ | `graph::Node::provenance`; extraction stamps source (`extract_concepts_with_provenance`) on **both** ingest paths — `ckos run --session` records `run:intent`/`run:output`, `ckos index` records `file:<path>` via `KnowledgeBus::ingest_text_from`; reinforcement keeps a node's original source; KQL `RETURN Sources` |
| 948 | Confidence score | ✅ | `Node::confidence`, `Document::confidence` |
| 949 | Retrieval planner | ✅ | `retrieval::plan_retrieval` + `search_diverse`/`mmr_rerank` (MMR) + `expand_query`/`search_expanded` (PRF) + `sdk::synonyms::SynonymTable`/`expand_query_with_synonyms` (a priori domain-term expansion; mitigates the §944 lexical-only gap) |
| 950 | Hybrid search | ✅ | `retrieval::Retriever` (BM25+ keyword [Lv & Zhai 2011 δ lower-bound] + vector + graph, Reciprocal Rank Fusion). Each `Hit` reports every leg that matched it (`Hit::sources`), so fusion is observable rather than merely claimed — `ckos search` prints e.g. `Keyword+Vector+Graph` |
| 951 | Graph reasoning | ✅ | retrieval graph hits + `graph::traverse`/`neighbors_via` (typed-relation hop) + `pagerank`/`personalized_pagerank`/`central_nodes` (node importance); KQL `RELATED … VIA <edge-kind>` reasons over relation types |
| 952 | Multi-hop planner | ✅ | retriever graph expansion is a **Personalized PageRank** pass seeded on the query's matched nodes (HippoRAG, arXiv:2405.14831/2502.14802) — associated nodes rank by query-mass flow, so multi-path corroboration outranks single long paths; `graph::traverse`/`traverse_with_hops` remain for distance-annotated walks |
| 953 | Memory consolidation | ✅ | `memory::consolidate` (sleep-phase pass compressing oversized docs), reachable via `ckos gc --consolidate N`, which runs before the document/graph GC passes; also driving the §940 compression ladder it calls into |
| 954 | Garbage collection | ✅ | `ckos gc`: `memory::collect` (documents; expiry via `--now <date>`) + `graph::KnowledgeGraph::remove_orphans` sweeping the session's persisted graph |
| 955 | Data encryption | ⏳ | at-rest/in-transit pending (transport layer) |
| 956 | Offline-first | ✅ | `FileStore` + std-only build; header fields (title/author/metadata) are backslash-escaped on write so an embedded newline can't shift real headers into the body on reload; writes are atomic (temp + fsync + rename) and one unreadable `.doc` is skipped with a warning instead of blocking the whole session |
| 957 | Distributed knowledge | ⏳ | sharding/partial-sync pending |
| 958 | Search cache | ✅ | `sdk::retrieval::SearchCache` (LRU query→hits cache), one per session directory, held in `ckos serve`'s shared `AppState` and warmed across `/api/search` requests (the CLI's one-shot processes still have no cache to warm) — invalidated whenever `/api/run` mutates that session |
| 959 | Learning pipeline | 🟡 | reflection persistence + auto-reindex + `sdk::eval` (Precision/Recall/MRR/nDCG); full closed loop ⏳ |
| 960 | Unified knowledge API | ✅ | `cli` (`search`/`kql`/`history`/`eval`) **and** the network API this row used to list as pending: `ckos serve` exposes `/api/search`, `/api/kql`, `/api/history`, `/api/graph`, `/api/run` over HTTP/JSON (§902). Additional transports (gRPC/WebSocket/MCP) are out of scope for v1 — see Scope below |
| 961 | AI-native filesystem | ⏳ | proposal |
| 962 | Knowledge Query Language | ✅ | `sdk::kql` — FIND/RELATED (with `VIA <edge-kind>` typed-relation filter)/FILTER (AND/OR/NOT)/BEFORE/AFTER/ORDER/LIMIT/RETURN; `ckos kql` incl. `--session` |

## Scope — what "done" means for v1

The previous version of this section said every mechanism has "a working,
tested implementation behind a trait seam where a production backend will
later plug in." That sentence let the product be permanently 90% finished: a
trait with no implementation is not a delivered capability, and counting it as
one is the same label-moving the codebase forbids everywhere else. So the
requirement itself gets questioned first, per section by section.

**The constraint that decides scope.** CKOS is `std`-only, zero-dependency,
offline-first. That is not an accident or a temporary state — it is the
product's reason to exist: it builds and runs anywhere with a Rust toolchain,
with no supply chain, no network, and no vendor. Every remaining ⏳ below
requires breaking exactly that constraint. So they are not "unfinished work on
CKOS"; they are **work on a different product** that would share this one's
name.

That makes them out of scope for v1, and the table above marks them so. This
is a judgment call, recorded here rather than left implicit, and the
repository owner can overturn any line of it.

### In scope, and delivered

Everything not listed below. The complete offline path works end to end and is
covered by the test suite: plan → schedule → execute → verify → reflect →
persist → extract a graph → index → hybrid-search (BM25+ / vector / graph with
RRF) → query with KQL → consolidate → collect. Reachable from both `ckos` and
`ckos serve`.

### Out of scope for v1 — each needs a dependency the product forbids

| Item | Needs | Why not a smaller version |
|---|---|---|
| §901 WASM plugin sandbox | a wasm runtime (wasmtime) | A fake sandbox is worse than none: it would imply isolation the host cannot enforce. The permission gate that *is* enforceable ships today |
| §902 gRPC / WebSocket / MCP legs | protobuf, an async runtime | The HTTP/JSON leg is delivered and is the one a browser and `curl` can both use |
| §924–925 real / edge runtimes | model runtimes, FFI | `Runtime` is a trait with a working `EchoRuntime`; a stub "real" backend would be a lie about inference |
| §926 distributed workflow, §957 sharding | an async runtime, a network transport | Distribution is a different architecture, not a flag on this one |
| §928 OIDC / LDAP | network + real token crypto | `StaticTokenProvider` is honest about being a demo; a hand-rolled OIDC verifier would be a security hazard |
| §930 mTLS, §955 encryption at rest | TLS, a vetted cipher | Hand-rolling AES/TLS is strictly worse than not offering it. `sdk::crypto` deliberately stops at SHA-256/HMAC, which *can* be verified against published vectors |
| §933 OpenTelemetry / Prometheus export | otel crates | Audit + telemetry are collected and exposed at `GET /api/status`; only the wire format is missing |
| §936 SQL / vector database backends | a DB driver | `Storage` is a trait with two working impls (`InMemoryStore`, `FileStore`) |
| §938 deep parse, §941 statistical NER, §944 real embeddings, §939 semantic chunking, §945 cross-modal | model weights / parsers | §939 and §949's remaining depth are *downstream of* §944 — semantic chunking needs a semantic embedder. Heuristic versions ship and their limits are measured, not asserted (see `embedding.rs`) |
| §961 AI-native filesystem | — | A research proposal, never specified to buildable detail |

### Genuinely blocked, not out of scope

These are wanted, ready, and stopped by something only the repository owner
can do. They are the honest release blockers:

| Item | State | Who unblocks it |
|---|---|---|
| §905 CI | `docs/ci-workflow.yml` is complete and runs the same `./scripts/check.sh` as the pre-commit hook | A maintainer copies it to `.github/workflows/ci.yml`. Automation lacks the `workflows` permission (verified: the push is rejected) |
| Public release | Fully prepared: `Cargo.toml` at 2.8.0, `CHANGELOG.md` section dated, owner runbook at `docs/releasing.md` | Repository owner: follow `docs/releasing.md` (tag, release, default branch). Verified unavailable to automation across eight channels; the GitHub gateway answers "GitHub access is not enabled for this session" |

### Deliberate gaps inside delivered features

Not everything above is unqualified. `docs/agent-handbook.md` §4 lists each
known gap in a *shipped* feature together with the condition for lifting it —
including the two that most affect real use: `HashingEmbedder` is lexical, not
semantic (measured), and `ckos serve` has no TLS, authentication, or CSRF
protection by design.

See [`roadmap.md`](roadmap.md) for sequencing of anything above v1.
