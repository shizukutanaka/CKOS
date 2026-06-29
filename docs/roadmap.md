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

- **Persistent storage** behind `memory::Storage` (SQLite first, then Qdrant for
  vectors) — §936.
- **Real runtimes** behind `runtime::Runtime` (llama.cpp via FFI, ONNX for
  embeddings) — §900.
- **Networked event bus** behind `kernel::EventBus` for the service mesh — §916.
- **Async execution loop** wiring planner → scheduler → runtime → verifier into a
  running engine.
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
