# CKOS Agent Handbook — Strengths, Weaknesses, and Improvement Instructions

Audience: an AI coding agent (Claude Opus / Sonnet class) continuing work on
this repository. Written so it can be followed without any prior session
context. Every claim below carries a `file:line`-level pointer or a commit
reference so you can verify it instead of trusting it.

State: all four gates green at every commit — `cargo fmt --all --check`,
`RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets`,
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`,
`cargo test --workspace`. 13 std-only crates, zero external dependencies.
For the current test count and the full fix list, read `CHANGELOG.md`'s
Unreleased section — this document deliberately carries no running totals,
because a hand-maintained count silently goes stale and a handbook whose own
numbers are wrong cannot be trusted on anything else.

---

## 1. How to work on this codebase (mandatory discipline)

Follow this loop for every change. It produced every fix in §3, with zero
regressions:

1. **Audit** — read one module closely against its own doc comments. The doc
   comment is the contract; a mismatch between doc and behavior is a bug even
   when tests pass.
2. **Reproduce before you claim** — never report or fix a bug you have not
   made fail: write the failing test (or a standalone `rustc` scratch repro)
   FIRST, watch it fail, then fix. If you cannot reproduce it, it is not a
   finding.
3. **Prove the test catches the bug** — after fixing, temporarily revert the
   fix and confirm the new test fails, then restore. (Example: the search-cache
   race test in `web/src/lib.rs` was validated exactly this way.)
4. **Full gates before every commit** — all four, no exceptions:
   `cargo fmt --all` · `RUSTFLAGS="-D warnings" cargo clippy --workspace
   --all-targets` · `RUSTDOCFLAGS="-D warnings" cargo doc --workspace
   --no-deps` · `cargo test --workspace`.
5. **One logical fix per commit**, with a message that states the defect, the
   reproduction, and the fix rationale. Update `CHANGELOG.md`'s Unreleased
   `### Fixed` section and the test count in the same commit.

Standing principles (each has already prevented or caught real bugs here):

- **std-only, dependency-free.** No external crates anywhere, including tests.
  The `web` crate hand-rolls HTTP and JSON for this reason. Do not add serde,
  tokio, or "just one small crate".
- **No label-moving.** Never wire up a dormant feature just to make a state
  machine look used. A change must have a testable behavioral payoff. (The
  planner's regulated-domain keyword classifier was rejected after measuring a
  1-of-4 catch rate — see `planner/src/lib.rs` module doc; `AgentState`
  transitions are deliberately not driven by the engine because nothing
  consumes the state yet.)
- **Silent truncation is worse than explicit rejection.** Applied in:
  oversized HTTP bodies → 413 (`web/src/http.rs`), corrupt embeddings → `None`
  (`memory/src/file_store.rs`), non-evaluable arithmetic → `Skip`
  (`verifier/src/lib.rs`).
- **Bound every resource.** Retention caps on audit/telemetry
  (`kernel/src/audit.rs`, `kernel/src/telemetry.rs`), connection cap and
  per-line read caps in `web`, nonce-window pruning in `sdk/src/security.rs`.
- **Poison-recovering locks** (`unwrap_or_else(|e| e.into_inner())`) so one
  panicking thread cannot cascade — pattern used in audit, telemetry, event
  bus, reindex queue, and `web::AppState`.

Environment facts (verified, do not re-litigate): pushes are restricted to the
designated work branch; pushing tags, `.github/workflows/` files, direct
GitHub REST calls, release/tag creation, and repo-settings changes are all
permission-denied (eight channels tested; the REST gateway answers "GitHub
access is not enabled for this session"). CI therefore stays staged at `docs/ci-workflow.yml` until a
maintainer copies it to `.github/workflows/ci.yml` by hand.

---

## 2. Strengths — audited clean; do not churn

Each of these was read closely this generation, most with adversarial inputs,
and found correct. Do not "improve" them without a failing test proving a
defect:

| Area | What was verified |
|---|---|
| `scheduler/src/lib.rs` | Multi-factor scoring weights, priority aging (anti-starvation proven by test), dependency gating, `runtime_fit` mapping, deterministic tie-breaks |
| `kernel/src/task.rs` | §893 state machine: legal/illegal transitions, retry counting |
| `policy/src/lib.rs` | Default-deny, ABAC deny-overrides-RBAC, wildcard grants via shared `permission_matches` |
| `plugins/src/lib.rs` | Least-privilege gate, multi-permission AND, validate-before-execute |
| `workflow/src/lib.rs` | Kahn topological order, duplicate/unknown-step rejection, by-construction acyclicity |
| `sdk/src/reflection.rs` | Confidence-weighted majority vote, deterministic ties, +1 smoothing |
| `sdk/src/session.rs` | Session isolation, `next_seq` restart-monotonicity |
| `sdk/src/messaging.rs` | Priority-ordered delivery, undeliverable-fails-loudly, round-robin mesh |
| `sdk/src/agent.rs` | Manifest parsing (block + inline lists), lifecycle validation, discovery excluding Suspended/Terminated |
| `sdk/src/retrieval.rs` helpers | `SearchCache` LRU invariant (order/entries key sets always equal), `expand_query` PRF, BM25+ δ-floor, PPR expansion |
| `memory/src/memory_score.rs` | Min-max normalization incl. all-equal guard, Generative-Agents blend, clamped decay |
| `graph/src/extract.rs` | Entity runs, stop-word stripping, typed-relation mapping, edge dedup seeded from existing edges |
| `web/src/dashboard.html` | All dynamic content via `textContent` (no XSS), force-layout math guarded against div-by-zero, clamped coordinates |
| `sdk/src/eval.rs` | P@k / R@k / MRR / nDCG / AP formulas against hand-checked values |
| IR primitives vs. their papers | BM25+ (Lv & Zhai 2011, δ applied only to matched terms), RRF (Cormack 2009, k=60, 1-based rank), MMR (Carbonell & Goldstein 1998), nDCG (Järvelin & Kekäläinen 2002) — all four match the published formulas |
| `kernel/src/telemetry.rs`, `kernel/src/audit.rs` | Poison-recovering locks (panic-cascade test), drop-oldest retention at the cap, every mean guards its zero divisor, FNV-1a determinism |
| `memory/src/chunk.rs` | Every size argument clamped to ≥1 (no `chunks(0)` panic); `Recursive`'s documented "no chunk exceeds target" invariant holds empirically for targets 1..40 across CJK, long-word and no-terminator inputs; overlap slices by chars |
| `sdk/src/crypto.rs` | SHA-256 and HMAC-SHA256 against the published FIPS 180-4 and RFC 4231 vectors (not self-consistency) |
| `sdk/src/kql.rs` selector | Substring label matching is **intended** here — `NodeSelector.text` is the documented `CONTAINS` operator, not a ranking signal. Do not "fix" it to token matching the way `graph_hits` was fixed; the code shape is the same but the contract is not |
| `memory::embedding::cosine` | Length mismatch and zero vectors return 0.0 (documented), so an embedder dimension change degrades to "no vector signal" rather than a silently wrong score |

---

## 3. Fixed this generation — bug classes to watch for recurrence

Every defect below was reproduced first, then fixed with a regression test.
The *class* column is what you should grep for elsewhere before assuming a
module is clean:

| Fix (commit) | Class |
|---|---|
| CLI flags: repeated flag leaked into positionals (`c68f58d`) | first-occurrence-only parsing |
| `JsonBalanceCheck` type-blind bracket depth (`9822a4a`) | counter where a stack is needed |
| `memory::summarize` panic on `。` (`cf82123`) | byte index from `rfind` used to slice char boundaries |
| `ReplayGuard` unbounded nonce set (`9f9608a`) | grow-forever collection with no eviction tied to its own correctness window |
| `Reindexer` duplicated docs on re-index (`7e6c5e5`) | doc promised replace, code minted fresh ids |
| `run_workflow` reported exhausted verification failure as success (`4462c0f`) | asymmetric Ok/Err arms around the same terminal condition |
| `ckos graph --session` overwrote the persisted graph (`68f292b`) | one caller loads-then-merges, sibling caller starts empty |
| `GraphStore` unsanitized kind tokens (`cda7380`) | one field skipped the sanitizer every other field goes through |
| web search-cache stale-resurrection race (`6004d8e`) | check/compute/write under three separate lock acquisitions |
| KQL RELATED value-dedup + VIA self-loop divergence (`d119df8`) | dedup by rendered value instead of identity; two primitives with silently different edge semantics |
| `FileStore` metadata keys with `": "` or `meta.` prefix (`d9a51c9`) | delimiter legal inside the field it delimits; `trim_start_matches` where `strip_prefix` is meant |
| Graph label substring matching, HTTP unbounded `read_line` (`0a17f17`, `5fd552d`) | substring vs token matching inconsistency; size cap not covering the read that precedes the check |
| `ArithmeticCheck` judged fragments of larger expressions (`30eb86c`) | a sub-term evaluated as if it were the whole expression |
| `web::json` emitted `NaN`/`inf` (`2c531b1`) | serializer contract broken by a value the type allows but the format forbids |
| Message signing forgeable without the key (`2a0aa73`) | *linear* keyed hash — a key-only term that cancels between two outputs |
| `gc` deleted a nondeterministic choice of duplicate (`42d68a0`) | iteration order of an unordered container used where the contract implies a defined order |
| PageRank leaked mass on edges to missing nodes (this round) | a guard that only covers the *total* case (no out-edges) while the *partial* case (some edges dead) slips past |

---

## 4. Known gaps — deliberate, with conditions for lifting them

These are NOT bugs. Each is parked with a reason; do not wire them up without
meeting the stated condition (that would be label-moving, §1):

- **`graph::GraphRepo` versioning** (`graph/src/versioning.rs`): complete,
  tested library; no CLI/engine caller. Lift when a real user workflow needs
  branch/merge of graphs (e.g. a `ckos graph branch/merge` command with a
  concrete use case).
- **`memory::MemoryTier`**: classification vocabulary only; documents carry no
  tier. Lift only together with real promote/demote logic and a consumer.
- **`kernel::ResourceProbe`**: seam with no consumer. Lift when a real
  hardware probe exists.
- **`HashingEmbedder` deliberately does *not* stem**, while `retrieval::tokens`
  does (S-stemmer, Harman 1991). Leaving the asymmetry is the safe choice, not
  an oversight: document embeddings are computed at write time and *persisted*,
  then compared against a freshly computed query vector. Changing the
  embedder's tokenizer would leave every stored vector on the old
  normalization at the *same dimension*, so `cosine`'s length guard cannot
  catch it and relevance would degrade silently — precisely the failure mode
  §1's "explicit rejection over silent corruption" rule exists to prevent.
  Lift only together with an embedding-version marker on documents plus
  re-embedding (or explicit rejection) of vectors carrying an older marker.
- **`Event::{TaskCreated, RuntimeLoaded, MemoryUpdated, PluginInstalled,
  AgentRegistered}`** are never published; `Event::topic()` has no caller
  (`kernel/src/event.rs`). Lift per-variant when a genuine publisher exists.
- **`AgentState` lifecycle is not driven by the engine** — discovery honors
  it, but nothing suspends/terminates agents automatically. Lift when a
  consumer (e.g. circuit breaker on repeated failure) justifies transitions.
- **Nonces are caller-supplied, not generated**: `Signer::seal` takes the
  nonce; nothing in-tree produces cryptographically random ones (std has no
  CSPRNG). A deployment must supply them. Lift = an optional feature reading
  the OS entropy source.
- **`HashingEmbedder` is lexical, not semantic** — measured: a true paraphrase
  scored no higher than unrelated text (`memory/src/embedding.rs` module doc).
  `sdk/src/synonyms.rs` is the documented partial mitigation. Lift = real
  embedding model behind the `Embedder` trait (FFI/ONNX), which breaks
  std-only, so it must be an optional, feature-gated crate.
- **`ckos serve` has no TLS/auth by design** (`web/src/lib.rs` module doc):
  local single-operator surface; a reverse proxy adds transport security.
  Destructive ops (gc) stay CLI-only.
- **CI staged, not active** (`docs/ci-workflow.yml`): workflow-file pushes are
  permission-denied from automation (verified). A maintainer must copy it to
  `.github/workflows/ci.yml` once.

---

## 5. Improvement proposals — priority-ordered, with evidence

### P1 — no known unfixed defect

Every verified defect found so far is fixed (§3). The `ArithmeticCheck`
fragment false positive that previously sat here was fixed in `30eb86c`.
Start from §1's audit loop on a module not yet listed in §2 rather than
re-reading audited-clean code.

### P2 — high value, moderate size

- **SQLite `Storage` backend** (behind `memory::Storage`,
  `memory/src/lib.rs`): first real persistence upgrade the roadmap names.
  Must be an optional feature/crate to preserve the std-only default build.
- **Real embedding model** behind `memory::Embedder` (see §4) — closes the
  measured paraphrase gap that `synonyms.rs` only partially covers.
- **Cross-mention confidence accumulation in extraction**
  (`graph/src/extract.rs`): `bump_confidence` is max-based, so an entity seen
  once per call never rises above base 45 across many calls. Persisting a
  mention count per node would make §948 confidence reflect the corpus.
  Design decision needed (node schema change) — propose before implementing.

### P3 — worthwhile, larger or blocked

- **OIDC/LDAP `IdentityProvider`** (`policy/src/identity.rs`): real token
  verification behind the existing trait; network + crypto ⇒ feature-gated.
- **A-MEM-style evolving memory notes** (arXiv:2502.12110, noted in
  `docs/roadmap.md` v2.8): requires a note-structure redesign; research
  candidate, not a retrofit.
- **gRPC/WebSocket/MCP legs for §902**: blocked on the dependency policy;
  needs an explicit decision to relax it for a gateway crate.

### Anti-proposals — measured or reasoned rejections; do not resubmit without new evidence

- Keyword classifier for regulated domains in the planner (measured 1-of-4
  catch rate — worse than no gate; see `planner/src/lib.rs` doc).
- Driving `AgentState` transitions from `Engine::execute` with no consumer.
- "Activating" CI from automation (permission-denied, verified seven ways).

---

## 6. Repository conventions

- Branch: work on the designated `claude/…` branch; push after every commit.
- `main` mirrors the finished state via sync PRs; keep it current after
  merging fixes.
- Commit messages: imperative summary line; body explains defect →
  reproduction → fix; end with the session's `Co-Authored-By` and
  `Claude-Session` trailers (see `git log` for the exact format).
- `CHANGELOG.md`: every user-visible fix goes under Unreleased `### Fixed`
  with the reproduction described; keep the trailing test-count line accurate.
- `docs/implementation-status.md` is the §-by-§ truth table — update it in
  the same commit as any change that alters a section's status, and never
  claim ✅ for behavior that lacks a consumer (use 🟡 with the gap named).
