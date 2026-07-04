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
cargo build            # build everything (std-only, no network needed)
cargo test             # run the unit + integration + doc tests
cargo run -p ckos-cli -- help    # list commands
```

## Command reference

Every command is `ckos <command> [args]`. Flags (`--dot`, `--session`) may
appear in any position, and `ckos <command> --help` prints per-command usage.

| Command | What it does | Example |
|---------|--------------|---------|
| `plan [--dot] <intent…>` | Decompose an intent into a workflow DAG | `ckos plan "research the Transformer paper"` |
| `run [--session <dir>] [--role R \| --token T] <intent…>` | Plan + execute; `--session` persists the run and grows its knowledge graph; `--role`/`--token` authorize finance/medical/legal/robotics steps (§929). `--token` authenticates via a demo identity provider (§928) carrying real ABAC attributes, unlike bare `--role`. **Caution**: the built-in planner never classifies free text into those capabilities, so neither flag has an observable effect via `run` — see `ckos workflow` | `ckos run --session ./sess "research X"` |
| `history <dir> [<query…>] [--k N]` | Show a session's past runs; with a query, recalls the top `--k` (default 5) records ranked by Generative-Agents memory score — recency × importance × relevance (§896/§927) — instead of dumping raw history | `ckos history ./sess "scheduler urgency"` |
| `search [--synonyms] [--expand] [--diverse] [--lambda N] <dir> <query…>` | Hybrid BM25 + vector + graph search (RRF-fused); `--synonyms` bridges vocabulary gaps with a built-in domain table, `--expand` widens recall (PRF), `--diverse` re-ranks for variety (MMR, tune with `--lambda` 0..1) | `ckos search --synonyms ./sess "dispatcher urgency"` |
| `graph [--dot] <text…>` / `graph [--dot] --session <dir>` | Extract a typed knowledge graph from text or a session's docs | `ckos graph --session ./sess` |
| `kql [--session <dir>] <query>` | Run a Knowledge Query Language query | `ckos kql 'FIND Concept "Transformer" RELATED Algorithm'` |
| `eval --relevant <csv> [--k N] <dir> <query…>` | Score search quality (Precision/Recall/MRR/nDCG) against known-relevant titles | `ckos eval --relevant "Transformer" ./sess Transformer` |
| `gc <dir> [--min-confidence N] [--now <date>] [--consolidate N]` | Garbage-collect low-value documents and sweep orphaned graph nodes (§954); `--now` enables expiry of documents whose `expires` metadata has passed; `--consolidate N` runs the §953 sleep-phase pass first, compressing document bodies over `N` characters | `ckos gc ./sess --consolidate 2000 --min-confidence 30` |
| `verify <text…>` | Run the independent §899 checks (non-empty, repetition, arithmetic, JSON, citations, security) | `ckos verify 'see [1]'` |
| `tool --list` / `tool [--role <role> \| --token <token>] <name> <input…>` | Invoke a tool; required permissions are authorized by RBAC+ABAC policy (§929), not self-granted (§917/§919); every run — allowed or denied — prints its §903 audit record | `ckos tool --role admin reverse hello` |
| `capabilities` | List the built-in capability vocabulary | `ckos capabilities` |
| `runtimes` | List the runtime registry table (§900): registered backends, execution locality, and the capabilities each serves | `ckos runtimes` |
| `workflow [--role <role> \| --token <token>] <file>` | Load and execute a workflow definition file; `--token` authenticates via a demo identity provider (§928: `tok-admin-hq`, `tok-admin-restricted`, `tok-guest`), carrying real ABAC attributes | `ckos workflow pipeline.wf` |
| `version` | Print the CKOS version | `ckos version` |

### A typical session flow

```sh
ckos run --session ./sess "research the Transformer paper by Vaswani"  # execute + learn
ckos search ./sess "Transformer"        # hybrid search (keyword + graph hits)
ckos kql --session ./sess 'FIND Concept "Transformer" RETURN Graph + Sources'
ckos plan --dot "research X" | dot -Tsvg > plan.svg                    # visualize a plan
```

`plan` output:

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

### Knowledge Query Language (KQL, §962)

```text
FIND Concept "Transformer"          # select by kind and/or quoted text (or *)
RELATED Algorithm VIA References    # one hop to neighbours of a kind (* = any);
                                    # optional VIA <edge-kind> filters by relation
                                    # (DependsOn/Implements/CreatedBy/References/RelatedTo)
FILTER (Confidence > 90 AND Confidence < 99) OR NOT Confidence < 50
BEFORE 2025-01-01                   # temporal bounds (also AFTER)
ORDER BY Confidence DESC            # ranking (ASC/DESC)
LIMIT 10                            # cap results
RETURN Graph + Sources             # shape output (Documents/Graph/Sources)
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
