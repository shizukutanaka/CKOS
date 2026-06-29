# CKOS — Cognitive Kernel Operating System

> An AI-native cognitive kernel for orchestrating agents, workflows, memory and
> runtimes. The kernel **does not perform inference** (§891) — it manages tasks,
> scheduling, memory, runtimes, events, security and resources, so the inference
> backend (llama.cpp / vLLM / ONNX / MLX …) can change without touching the core.

This repository is the implementation foundation for the CKOS design specified
in [`docs/`](docs/) (versions v2.5–v2.7). It is a Rust Cargo workspace whose
crates mirror the module layout in §890 of the spec.

```
                User / API / GUI
                       │
                Workflow Engine            (workflow)
                       │
               Cognitive Kernel            (kernel)
                       │
 ┌────────┬────────┬────────┬────────┐
 │Scheduler│Memory │Graph   │Policy  │     (scheduler / memory / graph / policy)
 └────────┴────────┴────────┴────────┘
                       │
             Runtime Abstraction Layer     (runtime)
                       │
       llama.cpp / vLLM / ONNX / MLX ...
```

## Workspace layout

| Crate           | Spec | Responsibility |
|-----------------|------|----------------|
| `kernel`        | §891–§894 | Task lifecycle, capabilities, event bus, typed ids, errors |
| `scheduler`     | §892, §913 | Four-layer scheduler with multi-factor priority scoring |
| `runtime`       | §900, §924 | Runtime abstraction + registry, capability-based selection |
| `graph`         | §897, §951–§952 | Typed knowledge graph with multi-hop traversal |
| `memory`        | §896, §936–§937 | Memory hierarchy, unified document model, storage trait |
| `planner`       | §898, §920 | Intent → decomposition → dependency-ordered DAG |
| `verifier`      | §899 | Independent quality/safety checks, decoupled from generation |
| `policy`        | §929 | RBAC + ABAC authorization, least-privilege by default |
| `workflow`      | §895 | DAG engine with cycle detection and topological scheduling |
| `plugins`       | §901, §917–§919 | Tool abstraction, tool registry, permission gate |
| `sdk`           | §907–§910, §921–§922, §927 | Agent manifests, lifecycle, capability registry, execution engine, reflection, sessions, prelude |
| `cli`           | §902 | `ckos` command-line interface |

## Build & test

```sh
cargo build            # build everything
cargo test             # run the unit + doc tests
cargo run -p ckos-cli -- plan "research the Transformer paper"   # plan only
cargo run -p ckos-cli -- run  "research the Transformer paper"   # plan + execute
cargo run -p ckos-cli -- run --session ./sess "research X"       # execute + persist
cargo run -p ckos-cli -- history ./sess                          # resume: show past runs
cargo run -p ckos-cli -- search ./sess "summary report"          # hybrid search
cargo run -p ckos-cli -- kql 'FIND Concept "Transformer" RELATED Algorithm'  # KQL
cargo run -p ckos-cli -- gc ./sess                               # garbage-collect
cargo run -p ckos-cli -- verify 'see [1]'                        # run verifier checks
cargo run -p ckos-cli -- plan --dot "research X" | dot -Tsvg     # Graphviz workflow
cargo run -p ckos-cli -- workflow pipeline.wf                    # run a workflow file
```

Example output:

```
intent : research the Transformer paper
workflow: research the Transformer paper (5 step(s))

execution order:
  1. [retrieval] search sources  (agents available: 1)
  2. [embedding] generate embeddings  (agents available: 1)
  3. [reasoning] summarize  (agents available: 1)
  4. [verification] verify citations  (agents available: 1)
  5. [reasoning] generate report  (agents available: 1)
```

## Design principles

- **The kernel never infers.** Orchestration only (§891) — runtimes are pluggable.
- **Discovery by capability, not name** (§910). Swap an agent without editing workflows.
- **Generation and verification are separated** (§899) for quality.
- **Least privilege everywhere** (§919, §929). Tools and agents request scoped permissions.
- **Offline-first** (§925, §956). Local runtimes rank above remote; in-memory backends are the default.
- **Dependency-light core.** The current crates are `std`-only, so the workspace builds and tests with no network access.

## Status & roadmap

Every subsystem across the v2.5–v2.7 spec has a working, tested implementation
behind a trait seam for richer backends (persistent storage, networked event
bus, WASM-sandboxed plugins, model-backed runtimes). See
[`docs/implementation-status.md`](docs/implementation-status.md) for a
section-by-section (§889–§962) traceability matrix, and
[`docs/roadmap.md`](docs/roadmap.md) for sequencing and the v2.8 plan.

## License

Apache-2.0.
