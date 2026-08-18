# CKOS v2.6 — Agent Service Mesh & Capability Layer

Treats AI agents not as bare inference units but as **services** that cooperate
inside the OS. Favours existing OSS and current hardware while keeping room for
research.

## §907 Agents as OS services

```
Application → Agent → Kernel → Runtime
```
An agent is the execution unit, analogous to a process. → [`sdk::agent`](../sdk/src/agent.rs).

## §908 Agent manifest

```yaml
id: planner
version: 1.2.0
description: Planning Agent
capabilities: [planning, decomposition]
memory: [working, graph]
runtime: [photon, llama.cpp]
permissions: [graph.read, graph.write, tool.search]
priority: high
```
→ `sdk::agent::AgentManifest`; `AgentManifest::from_manifest` parses this exact
YAML-style config (dependency-free), so agents can be defined in files.

## §909 Agent lifecycle

`Install → Register → Load → Ready → Running → Suspended → Resume → Terminate`.
Managed like OS services. → `sdk::agent::AgentState`.

## §910–§912 Capability registry, interface & discovery

Agents are found by **capability**, not name, so they can be swapped without
changing workflows. Capabilities (§911): planning, reasoning, coding,
translation, embedding, retrieval, verification, simulation, vision, speech,
robotics, finance, medical, legal. Discovery (§912): task → required capability
→ registry → agent selection. → `kernel::Capability`, `sdk::CapabilityRegistry`.

## §913 Agent scheduler

Priority is decided by deadline, importance, cost, runtime, energy and
confidence — not FIFO. → `scheduler::ScoreFactors`. The `runtime_fit` factor is
fed by observed telemetry (`scheduler::runtime_fit` + `Engine::recommended_factors`),
closing the §904→§913 optimization loop.

## §914–§916 Messaging & service mesh

- **§914 Message bus**: event-driven `Planner → Bus → Reasoner → Verifier →
  Output`. → `sdk::messaging::MessageBus` (per-agent inboxes, priority-ordered
  delivery).
- **§915 Message format**: `id, source, destination, type, priority,
  payload{graph_id, memory_ref}`. → `sdk::messaging::{Message, Payload}`.
- **§916 Service mesh**: abstracts agent-to-agent comms — load balancing, retry,
  routing, security. → `sdk::messaging::ServiceMesh` routes by capability with
  round-robin load balancing and delivery accounting.

## §917–§919 Tools

- **§917 Tool registry**: Filesystem, Git, Docker, Python, SQLite, Neo4j,
  Browser, Email, Slack, GitHub, MCP, REST, gRPC.
- **§918 Tool adapter**: unified `trait Tool { execute / validate / cancel /
  metadata }`. → [`plugins::Tool`](../plugins/src/lib.rs).
- **§919 Tool permissions**: `filesystem.read/write, network.http/websocket,
  database.query, docker.run` — strict least privilege.

## §920 Workflow compiler

`natural language → planner → DAG → execution plan → kernel`.

## §921–§923 Reflection & knowledge bus

- **§921 Agent reflection**: task → result → score → improvement hint → memory.
  → `sdk::reflection::Reflector` + `sdk::Session::record_reflections`.
- **§922 Collective reflection**: planner/reasoner/verifier → consensus → memory
  update. → `sdk::reflection::consensus`.
- **§923 Knowledge bus**: graph updates flow as events so embeddings re-generate
  automatically. → `sdk::knowledge_bus::KnowledgeBus` publishes
  `Event::GraphChanged` on every mutation; `ReindexQueue` is a ready-made
  subscriber that collects changed node ids for an async re-index worker (§938).

## §924–§927 Runtime pool, edge, distribution, sessions

- **§924 Runtime pool**: CPU/GPU/Cloud/Edge; the kernel picks the best.
- **§925 Edge execution**: inference, cache search and speech work offline; cloud is auxiliary.
- **§926 Distributed workflow**: split per workflow across nodes, then merge.
- **§927 Session manager**: persists memory, runtime, workflow, graph and tool
  state for fast resume. → `sdk::session::Session` records execution history and
  reflections to a `Storage` backend; with `FileStore` they survive restarts
  (`ckos run --session <dir>` / `ckos history <dir>`).

## §928–§933 Enterprise, security, ops

- **§928 Identity**: OAuth2, OIDC, SAML, LDAP, Active Directory. →
  `policy::IdentityProvider` authenticates a token into an `Identity`
  (subject/roles/attributes) that feeds the policy engine; `StaticTokenProvider`
  is the in-memory stand-in, real directories implement the same trait.
- **§929 Authorization**: two layers — RBAC (role) + ABAC (attribute) — applied
  per agent/tool/workflow. → [`policy`](../policy).
- **§930 Distributed security**: mTLS, cert rotation, message signing, replay
  protection, audit logs. → `sdk::security`: `Signer` seals messages into a
  `SignedEnvelope` (nonce + timestamp + signature); `ReplayGuard` verifies the
  signature, freshness window and nonce uniqueness. mTLS/cert rotation are
  transport concerns layered below; audit is `kernel::audit`.
- **§931 Kubernetes**: each agent as a Deployment, autoscaled.
- **§932 Docker Compose (dev)**: kernel + planner/reasoner/verifier agents +
  graph-db + vector-db + redis.
- **§933 Observability**: OpenTelemetry, Prometheus, Grafana, Jaeger.

## §934 Positioning

At v2.6 CKOS becomes an AI-native OS integrating agents, workflows, memory,
graph, tools, runtimes, security and orchestration.
