# CKOS v2.5 — Core Kernel Implementation Spec

The market-grade core kernel. Goal: a high-performance, highly extensible AI
execution foundation.

## §889 System overview

```
                User / API / GUI
                       │
                Workflow Engine
                       │
               Cognitive Kernel
                       │
 ┌────────┬────────┬────────┬────────┐
 │Scheduler│Memory │Graph   │Policy  │
 └────────┴────────┴────────┴────────┘
                       │
             Runtime Abstraction Layer
                       │
       llama.cpp / vLLM / ONNX / MLX ...
                       │
        CPU / GPU / NPU / Remote Cluster
```

## §890 Rust workspace

`kernel, scheduler, runtime, graph, memory, planner, verifier, policy,
workflow, plugins, sdk, cli, desktop, mobile, server, docs` — each managed as an
independent Cargo workspace member. *(This repo implements the library/CLI
crates; `desktop`/`mobile`/`server` are app targets on the roadmap.)*

## §891 Kernel responsibilities

The kernel performs **no inference**. It is limited to: task management,
scheduling, memory management, runtime management, event management, security,
resource allocation. This keeps runtime changes from affecting the kernel.
→ implemented in [`kernel`](../kernel).

## §892 Scheduler architecture

Four layers: `Task Queue → Priority Queue → Dependency Resolver → Execution
Dispatcher`. → [`scheduler`](../scheduler).

## §893 Task state machine

```
Created → Queued → Planning → Running → Verifying → Completed
                                   │
                       Failed → Rollback → Retry
```
→ `kernel::task::TaskState`.

## §894 Event bus

Inter-module communication is event-driven for loose coupling. Representative
events: `TaskCreated, TaskStarted, TaskCompleted, RuntimeLoaded, MemoryUpdated,
GraphChanged, PluginInstalled, PolicyViolation`. → `kernel::event`.

## §895 Workflow engine

Workflows are DAGs; nodes run in parallel where dependencies allow. Canonical
example: search → embed → summarize → verify citations → report.
→ [`workflow`](../workflow), driven by [`planner`](../planner).

## §896 Memory hierarchy

`L0 Register · L1 Context Cache · L2 Working · L3 Semantic · L4 Knowledge Graph
· L5 Archive`. Cache and persistent data are cleanly separated.
→ `memory::MemoryTier`.

## §897 Knowledge graph

Node kinds: Concept, Document, Person, Organization, Tool, API, Project.
Edge kinds: depends_on, implements, references, created_by, related_to.
→ [`graph`](../graph).

## §898 Planner

`input → intent analysis → subtask decomposition → dependency analysis → DAG
generation → scheduler`. → [`planner`](../planner).

## §899 Verifier

Runs on an independent runtime. Checks: mathematical consistency, JSON-schema
conformance, source integrity, static code analysis, citation validity,
security policy. Separating generation from verification raises quality.
→ [`verifier`](../verifier).

## §900 Runtime registry

| Runtime | Use |
|---------|-----|
| llama.cpp | local LLM |
| vLLM | GPU server |
| ONNX Runtime | embedding / classification |
| MLX | Apple Silicon |
| OpenVINO | Intel NPU |
| DirectML | Windows GPU |

Additional runtimes register as plugins. → [`runtime`](../runtime).

## §901 Plugin SDK

Plugin kinds: Runtime, Memory, Graph, Tool, UI, Workflow. Each runs in a
WASM-based sandbox. → [`plugins`](../plugins).

## §902 API gateway

Interfaces: REST, gRPC, WebSocket, MCP, CLI — over a common Task API.
→ [`cli`](../cli) implemented; network gateways on the roadmap.

## §903–§905 Logging, telemetry, CI/CD

- **§903 Audit/log**: time, runtime, model version, plugin, tool, I/O hash,
  errors. Audit and debug logs are kept separate. → `kernel::audit`
  (`AuditRecord`/`AuditSink`/`InMemoryAuditLog`); the engine audits every task
  execution (I/O hashed, not stored raw); shown by `ckos run`.
- **§904 Telemetry**: CPU/GPU/NPU usage, memory, latency, token rate, power —
  fed back into scheduler optimization. → `kernel::telemetry`: per-task
  `TaskMetrics` (latency/tokens) aggregated by `InMemoryTelemetry` (mean latency
  per runtime feeds `ScoreFactors.runtime_fit`); hardware counters via the
  `ResourceProbe` seam (`NullProbe` default). The engine records each run;
  `ckos run` prints the summary.
- **§905 CI/CD**: Windows/Linux/macOS (x64+ARM), Android, iOS; unit,
  integration, workflow, performance and regression tests.

## §906 Implementation priority

1. Rust kernel → 2. Runtime adapter → 3. Workflow engine → 4. Graph memory →
5. Planner → 6. Verifier → 7. Plugin SDK → 8. Desktop GUI → 9. Mobile client →
10. Distributed cluster.
