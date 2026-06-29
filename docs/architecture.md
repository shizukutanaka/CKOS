# CKOS Architecture

How the specification maps onto the implementation, and how the crates depend on
each other.

## Dependency graph

```
              kernel  (no internal deps — the foundation)
                │
   ┌──────┬─────┼──────┬───────┬────────┬────────┐
scheduler runtime graph memory verifier policy  workflow
                                                   │
                                               planner
   └──────┴─────┴──────┴───────┴────────┴────────┘
                │
               sdk  (re-exports everything + agent layer)
                │
               cli
```

`kernel` depends on nothing internal, enforcing §891: the core is independent of
runtimes and higher layers. Everything else builds upward; `sdk` is the single
import surface and `cli` is a thin consumer.

## Why these seams

The spec repeatedly calls for substitutability. Each is expressed as a Rust
trait so an in-memory default can be swapped for a production backend without
changing callers:

| Trait | Spec | In-memory default | Intended production backend |
|-------|------|-------------------|------------------------------|
| `kernel::EventBus` | §894, §916 | `InMemoryEventBus` | NATS / Kafka via the service mesh |
| `runtime::Runtime` | §900 | `EchoRuntime` | llama.cpp / vLLM / ONNX / MLX |
| `memory::Storage` | §936 | `InMemoryStore` | SQLite / RocksDB / Qdrant / S3 |
| `planner::Planner` | §898 | `HeuristicPlanner` | model-backed decomposition |
| `verifier::Check` | §899 | `NonEmptyCheck`, `JsonBalanceCheck` | schema / static-analysis / citation checks |
| `plugins::Tool` | §918 | `UppercaseTool` | filesystem / git / docker / HTTP, WASM-sandboxed |

## End-to-end flow (implemented)

1. **Plan** — `planner::HeuristicPlanner::plan(intent)` decomposes intent into a
   `workflow::Dag` (§898, §920).
2. **Order** — `Dag::topological_order()` produces a schedulable order and
   rejects cycles (§895).
3. **Schedule** — `scheduler::Scheduler` scores ready tasks by the §913 factors
   and dispatches them once dependencies clear (§892).
4. **Discover** — `sdk::CapabilityRegistry` selects an agent by capability, not
   name (§910).
5. **Select runtime** — `runtime::RuntimeRegistry::select` picks the best
   (local-preferred) runtime for the capability (§924–§925).
6. **Verify** — `verifier::Verifier` runs independent checks on the output (§899).
7. **Authorize** — `policy::PolicyEngine` and `plugins::ToolRegistry` enforce
   RBAC/ABAC and least-privilege tool permissions (§919, §929).

The `ckos plan …` CLI command walks steps 1–4 today.

## Design constraints honoured

- **std-only core.** No external crates yet, so the workspace builds and tests
  with no registry/network access — important for the offline-first goal
  (§925, §956) and for reproducible CI.
- **Validated state machines.** Task (§893) and agent (§909) transitions are
  checked centrally; illegal transitions are errors, not silent no-ops.
- **Default-deny security.** Both the policy engine (§929) and the tool registry
  (§919) deny unless explicitly granted.
