# CKOS Roadmap

## Implementation priority (§906)

| # | Item | Status |
|---|------|--------|
| 1 | Rust kernel | ✅ implemented (`kernel`) |
| 2 | Runtime adapter | ✅ trait + registry (`runtime`); real engines pending |
| 3 | Workflow engine | ✅ DAG + topological scheduling (`workflow`) |
| 4 | Graph memory | ✅ in-memory graph + traversal (`graph`, `memory`) |
| 5 | Planner | ✅ heuristic (`planner`); model-backed pending |
| 6 | Verifier | ✅ check framework (`verifier`); richer checks pending |
| 7 | Plugin SDK | ✅ tool/registry/permissions (`plugins`); WASM sandbox pending |
| 8 | Desktop GUI | ⏳ not started |
| 9 | Mobile client | ⏳ not started |
| 10 | Distributed cluster | ⏳ not started |

The current milestone delivers a **compiling, tested kernel foundation** with a
trait seam at every point the spec asks for substitutability (see
[`architecture.md`](architecture.md)). Each subsystem ships an in-memory default
so the whole system runs offline.

## Near-term backend work

- ✅ **Persistent storage** — `memory::FileStore` gives durable, dependency-free
  document persistence behind `memory::Storage` (§936, §956). Next: a SQLite
  backend, then Qdrant for vectors.
- **Real runtimes** behind `runtime::Runtime` (llama.cpp via FFI, ONNX for
  embeddings) — §900.
- **Networked event bus** behind `kernel::EventBus` for the service mesh — §916.
- ✅ **Synchronous execution loop** (`sdk::engine::Engine`) wiring planner →
  scheduler → runtime → verifier with events; `ckos run` exercises it.
  An **async/distributed** driver (§926) can replace it behind the same surface.
- ✅ **Reflection loop** (`sdk::reflection`) — per-task self-evaluation (§921)
  and cross-agent consensus (§922), persistable to memory for the §959 learning
  loop; surfaced in `ckos run`.
- ✅ **Session manager** (`sdk::session::Session`) — persists execution history
  and reflections to a `Storage` backend for fast resume (§927); durable with
  `FileStore` via `ckos run --session` / `ckos history`.
- ✅ **Retrieval layer** (`sdk::retrieval`) — retrieval planner (§949) + hybrid
  search (§950) over documents (keyword **and vector**) and the graph (label
  match + multi-hop expansion, §951–§952), confidence-weighted; via `ckos search`.
- ✅ **Embeddings** (`memory::Embedder`/`HashingEmbedder`/`cosine`, §944) —
  dependency-free vector embeddings; sessions embed persisted outputs so
  semantic search works across restarts. Next: a real embedding-model backend.
- ✅ **KQL** (`sdk::kql`, §962) — Knowledge Query Language: tokeniser +
  recursive-descent parser → typed AST, executor over the graph
  (FIND/RELATED/FILTER/BEFORE/AFTER, RETURN Sources); via `ckos kql`.
- ✅ **Temporal knowledge & provenance** (§946/§947) — graph nodes carry an ISO
  `date` and a `provenance` source; KQL enforces `BEFORE`/`AFTER` and surfaces
  sources via `RETURN Sources`.
- ✅ **Memory hygiene** (`memory::collect`/`compress_document`, §954/§940/§953)
  — garbage collection (expired, low-confidence, duplicate, broken-embedding) and
  semantic compression (summary step); via `ckos gc`. Orphaned-graph-node GC and
  the concept/knowledge compression tiers are next.
- ✅ **Telemetry** (`kernel::telemetry`, §904) — per-task latency/token metrics
  aggregated (mean latency per runtime, token throughput) to feed scheduler
  `runtime_fit` (§913); `ResourceProbe` seam for CPU/GPU/NPU. Shown by
  `ckos run`. A real hardware probe + Prometheus export (§933) are next.
- ✅ **Audit logging** (`kernel::audit`, §903) — every task execution recorded
  with timestamp, runtime, and FNV-1a I/O hashes (raw payloads not retained);
  failures audited too. Separate from debug logging; shown by `ckos run`. A
  persistent/OpenTelemetry audit sink (§933) is the next step.
- ✅ **CI** (`docs/ci-workflow.yml`, §905) — fmt + clippy + build + test + CLI
  smoke tests on every push/PR, with `-D warnings`. Copy it to
  `.github/workflows/ci.yml` to activate (the branch automation can't push
  workflow files itself). Cross-platform/mobile build matrix is the next step.
- **API gateway** (REST/gRPC/WebSocket/MCP) over the common Task API — §902.

## v2.8 — Developer platform & ecosystem

The center of gravity shifts from "OS" to "platform developers actually use":

- Developer SDKs for **Rust, Python and TypeScript**.
- **Plugin system** maturity — WASM sandboxing, signing, a registry.
- **No-code / low-code** authoring.
- **Workflow Studio** — visual DAG authoring.
- **Agent Studio** — author, test and publish agents.
- **Graph Explorer** and **Runtime Monitor** GUIs.

The aim: third parties can build AI applications on CKOS easily.
