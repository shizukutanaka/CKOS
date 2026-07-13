# Changelog

All notable changes to CKOS are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions track the
spec generations (v2.5 core kernel → v2.6 agent mesh → v2.7 knowledge
platform).

## [Unreleased] — v2.8 groundwork

### Improved (from recent IR/Graph-RAG literature)

- **BM25+ keyword ranking** (§950): `retrieval::keyword_hits` now adds the
  Lv & Zhai (CIKM 2011) δ lower-bound to each matched term's normalized TF,
  so BM25's over-penalization of long documents can no longer drop a matched
  term's contribution below an unmatched document's. δ = 1.0 (the paper's
  recommended value); regression test covers the long-rare-term-beats-
  short-common-term case the correction targets.
- **Personalized-PageRank graph expansion** (§951/§952): the retriever's
  multi-hop graph expansion is now a HippoRAG-style Personalized PageRank
  pass (`graph::personalized_pagerank`, arXiv:2405.14831 / 2502.14802)
  seeded on the query's matched nodes, replacing a fixed per-hop geometric
  decay. Associated nodes rank by how much query mass actually flows to them,
  so a node corroborated by several short paths outranks one reached by a
  single long path — with a deterministic diamond-graph test proving exactly
  that. `graph::pagerank` was refactored onto a shared `pagerank_core`
  (uniform teleport = classic PageRank, verified behaviour-preserving).

### Fixed

- **CLI flags now consume every occurrence, last value wins**: `take_flag`/
  `take_value_flag` used to stop at the first occurrence of a flag; a repeated
  flag (`ckos history <dir> --k 3 --k 1 <query>`) leaked the later occurrence
  into the positional args, silently corrupting the query text or, for a
  repeated `--flag <dir>` pair, the session directory itself. Every command
  built on these helpers (`plan`/`run`/`search`/`eval`/`gc`/`tool`/`workflow`/
  `graph`/`serve`/`kql`/`history`) is affected; a repeated boolean flag is now
  idempotent and a repeated value flag takes its last occurrence, the usual
  CLI convention.
- **`JsonBalanceCheck` rejects mismatched delimiter types**: the check tracked
  a single aggregate depth counter, so `{"a": [1}]` and `[{]}` — equal
  open/close counts but a `]` closing a `{` — passed as balanced. It now
  tracks a stack of the closing delimiter each opener expects, catching the
  bracket-type mismatch a depth counter can't see.
- **`memory::summarize` (and therefore `compress_document`/`consolidate`/
  `ckos gc --consolidate`) panicked on CJK text**: it cut the truncated window
  at a byte index returned by `str::rfind`, but `。` (the CJK full stop the
  function explicitly lists as a sentence terminator) is 3 bytes wide, so a
  cut landing on it sliced a string mid-character and panicked. Reproduced
  directly (not just under `cargo test`) before fixing; now indexes in chars
  throughout, so a `。`-terminated sentence cuts as cleanly as a `.`-terminated
  one instead of crashing the process.
- **`security::ReplayGuard` tracked every seen nonce forever**: `seen` was a
  `HashSet` with no eviction, so a long-running guard's memory grew for the
  whole process lifetime (§930). A nonce whose timestamp has aged past the
  freshness window can never again change a verdict — a replay carrying it
  fails the `Expired` check before the nonce lookup is even reached — so it's
  now pruned the moment it ages out, bounding memory by the window instead of
  by uptime. Diagnostic `tracked_nonces()` added so the bound is directly
  testable, not just inferred.

### Added

- **`web` crate + `ckos serve`** (§902 API gateway, front-loaded from the v2.8
  roadmap): a `std`-only HTTP/JSON server (own request parser, response
  writer and `Json` value type — no `tokio`/`axum`/`serde`) exposing
  `/api/{capabilities,runtimes,plan,run,history,search,kql,graph,verify}`
  over the same `Engine`/`Session`/`Retriever`/KQL surface the CLI uses.
  Binds to `127.0.0.1` by default (least privilege by default); every
  destructive operation (`gc`) stays CLI-only.
- **Embedded browser dashboard**: a single self-contained HTML/CSS/JS page
  (no build step, no CDN, `include_str!`-embedded in the binary) with panels
  for Run, Search, History, KQL, Graph (an in-browser force-directed SVG
  layout — the v2.8 "Graph Explorer" groundwork), Verify and System (the
  v2.8 "Runtime Monitor" groundwork). Every panel works in zero-setup
  "try it" mode or against a named session directory.
- 14 new tests in `web` (JSON escaping/rendering, percent-decoding, and
  in-process HTTP round-trips for every route) plus a CLI end-to-end test
  that spawns `ckos serve --port 0` and makes a real `TcpStream` request
  against the OS-assigned port.
- **Bounded concurrency**: `ckos serve` caps connections handled at once
  (default 64); beyond the cap, a connection gets an immediate
  `503 Service Unavailable` instead of an unbounded thread spawn.
- **No silent request truncation**: a declared `Content-Length` beyond the
  per-request size cap is now rejected outright (`413`) rather than clamped
  and parsed as a corrupted, truncated body.
- **Shared server-lifetime state (`routes::AppState`)**: `ckos serve` now
  holds one `Engine` for its whole process lifetime instead of building a
  fresh one per `/api/run` call, so its audit trail (§903) and telemetry
  (§904) finally accumulate across requests — exposed at the new
  `GET /api/status` endpoint and the dashboard's System tab. `/api/search`
  also gets `sdk::retrieval::SearchCache` (§958) wired up for the first time
  anywhere in the workspace: one LRU cache per session directory, hit on an
  identical repeat query and invalidated the moment `/api/run` adds anything
  new to that session.

245 tests passing (was 216); fmt, clippy `-D warnings`, and rustdoc
`-D warnings` all clean. Manually verified end-to-end with `curl` against
every route, including a full `run` → `history` → `search` → `graph` cycle
against a real session directory (atomic-write and corrupt-file hardening
from 2.7.0 apply automatically, since the web handlers call the same
`FileStore`/`GraphStore`), and a `run` → `search` → `search` → `run` → `search`
sequence proving the cache hits on repeat and invalidates after a mutation.

## [2.7.0]

### Fixed

- **Knowledge-graph edge duplication**: re-extracting over a persisted graph
  (`ckos run --session`) appended a parallel copy of every existing edge on
  each run, skewing PageRank centrality (§951) and growing `graph.kg` without
  bound. Extraction now seeds its dedup set from the graph's existing edges.
- **FileStore header corruption**: a document title or metadata value
  containing a blank line shifted real headers (confidence, embedding) into
  the body on reload. Header fields are now backslash-escaped on write and
  unescaped on read; bodies remain verbatim.
- **Verifier false positive on grouped numbers**: `ArithmeticCheck` misread
  digit fragments of grouped/decimal numbers (`1,000 + 1 = 1001`) as wrong
  equations and rejected correct output. Operands touching a `,`/`.` are now
  treated as non-evaluable (skipped) rather than mis-evaluated.
- **`cmd_eval` silently swallowed graph load errors**, scoring against an
  empty graph; it now fails loudly like every other session command.
- **Corrupt persisted embeddings**: a vector with any unparseable component
  is now dropped whole (`None`) instead of silently loading at the wrong
  dimension.

### Added

- **§893 recovery loop**: `Engine::run_workflow` recovers a `Failed` task via
  `Failed → Rollback → Retry → Queued` with a bounded retry budget;
  deterministic denials are not retried.
- **§904→§913 closed loop**: tasks are submitted with telemetry-derived
  scoring (`runtime_fit` from observed latency); the serving agent's declared
  manifest priority is adopted when a task has none.
- **§928/§929 identity + ABAC**: `ckos run`/`workflow`/`tool --token`
  authenticate against an identity provider, carrying real ABAC attributes
  end-to-end (demo rule: `capability.medical` denied for `region=restricted`
  even with an admin role). `--role` remains the bare-roles convenience.
- **§953/§954 maintenance entry points**: `ckos gc` gains `--consolidate N`
  (sleep-phase compression), `--now <date>` (expiry) and an orphaned-graph-
  node sweep.
- **§962 KQL `VIA <edge-kind>`**: `RELATED … VIA DependsOn` restricts the hop
  to one typed relation, making §941 typed edges queryable.
- **§927 scored recall**: `ckos history <dir> <query…> [--k N]` recalls
  records by the Generative-Agents memory score instead of dumping raw
  history.
- **Operator surfaces**: `ckos runtimes` (the §900 registry table) and a
  printed §903 audit trail for every `ckos tool` run, allowed or denied.
- **Policy observability**: §929 denials publish `Event::PolicyViolation`.

### Hardened (commercial-grade pass)

- Atomic persistence: `FileStore` and `GraphStore` write via temp-file +
  fsync + rename, so a crash mid-write can no longer truncate stored state.
- Corrupt-file isolation: one unreadable `.doc` no longer prevents a session
  from opening; it is skipped, counted and surfaced as a CLI warning.
- Poisoned-lock recovery in audit/telemetry/event-bus/reindex-queue: a panic
  in one thread no longer cascades into every later call.
- Bounded in-memory retention (drop-oldest, default 10 000) for the audit log
  and telemetry.
- Workspace-enforced lints: `unsafe_code = "forbid"` (the workspace has zero
  unsafe) and `missing_docs = "warn"` (escalated to errors in CI); every
  public item is documented.
- Release profile strips symbols; crates carry full publication metadata;
  a complete CI workflow (fmt → clippy `-D warnings` → build → test → CLI
  smoke) is staged at `docs/ci-workflow.yml` — copy to
  `.github/workflows/ci.yml` to activate (the branch automation lacks the
  `workflows` push permission).

## [2.6.0]

Agent service mesh (§907–§934): agent manifests and lifecycle, capability
registry and discovery, multi-factor scheduler, message bus, tool registry
with least-privilege permission gate, reflection consensus, session manager,
RBAC+ABAC policy engine, message signing utilities.

## [2.5.0]

Core kernel (§889–§906): task lifecycle state machine, four-layer scheduler,
event bus, workflow DAG engine, memory hierarchy and document model, knowledge
graph, heuristic planner, independent verifier, runtime registry, plugin SDK,
audit logging, telemetry, CLI.
