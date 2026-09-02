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
4. **Full gates before every commit** — all four, no exceptions. One command
   runs them and names the first failure:

   ```sh
   ./scripts/check.sh          # format, clippy, rustdoc, status-doc, deploy manifests, tests
   ./scripts/check.sh --fix    # reformat in place first
   ```

   The status-doc gate is not cosmetic: `docs/implementation-status.md` is the
   product's §-by-§ coverage claim, and deleting inert parts left four rows
   citing symbols that no longer existed. `scripts/check-status-doc.sh` now
   verifies every cited symbol still resolves, so that claim is machine-checked
   rather than hand-maintained. A deliberately historical mention goes in its
   `ALLOWED` map *with a reason*.

   (Equivalently by hand: `cargo fmt --all -- --check` · `RUSTFLAGS="-D
   warnings" cargo clippy --workspace --all-targets` · `RUSTDOCFLAGS="-D
   warnings" cargo doc --workspace --no-deps` · `cargo test --workspace`.
   Prefer the script — retyping the chain invites quietly dropping a gate, and
   it is what the staged CI runs.)

   Better still, stop relying on remembering: `./scripts/install-hooks.sh`
   points `core.hooksPath` at `.githooks/`, whose pre-commit hook runs the
   script, so a commit that breaks a gate cannot be created (verified: a
   deliberately misformatted file was refused). `git commit --no-verify`
   bypasses it for a genuine work-in-progress commit.
5. **Run the documented path, not just the tests.** Every gate passed while
   `ckos index` recorded no provenance and the README's own quickstart printed
   `src=<unknown>`. Unit and integration tests each verified their own layer;
   nothing verified what a new user is told to type. `./scripts/verify-quickstart.sh`
   is that check — a release build, every README command and route, plus the
   session-confinement boundary from outside. Run it after touching the CLI,
   the web routes, or the README. It is deliberately not in `check.sh`
   (release build + real server is too slow for a per-commit gate).
   When it *does* fail, check your own invocation first: a raw `+` in a
   form-encoded body decodes to a space, which made `RETURN Graph + Sources`
   look like a parser bug when the fault was the `curl` call.
6. **One logical fix per commit**, with a message that states the defect, the
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

Environment facts — **re-verified this generation, since the earlier version
of this paragraph was partly wrong.** What actually holds:

- Pull requests to `main` *do* work through the GitHub MCP tools (this
  generation merged a dozen). An earlier note claiming all GitHub access was
  denied was stale; do not repeat it without testing.
- `git push` of a **tag** returns HTTP 403 (tested directly, this generation).
- There is **no** create-release and **no** repo-settings tool in the MCP
  server (searched). So a tag, a GitHub Release, and settings changes are
  genuinely owner-only.
- `.github/workflows/` pushes are denied, so CI stays staged at
  `docs/ci-workflow.yml` until a maintainer copies it by hand.
- **The repository is already public and its default branch is already
  `main`** (`git ls-remote --symref origin HEAD`). Earlier notes listed
  "switch the default branch" as outstanding work. It is not, and repeating
  that sends the owner after a setting that needs no change.

Before writing "X is impossible here" into this file, test X. Two of the
claims above were inherited rather than measured, and one of them was false.

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
| `graph/src/versioning.rs` | Union merge by semantic identity (kind + lowercased label): strategy resolution incl. deterministic `HigherConfidence` ties, conflict reporting, edge union deduped by identity triple, endpoints that fail to resolve skipped, single-parent chain / branch-pointer semantics. It does **not** claim 3-way merge, so absent delete-vs-modify detection is scope, not a defect |
| `sdk/src/kql.rs` selector | Substring label matching is **intended** here — `NodeSelector.text` is the documented `CONTAINS` operator, not a ranking signal. Do not "fix" it to token matching the way `graph_hits` was fixed; the code shape is the same but the contract is not |
| `memory::embedding::cosine` | Length mismatch and zero vectors return 0.0 (documented), so an embedder dimension change degrades to "no vector signal" rather than a silently wrong score |
| `web/src/http.rs` parser | Per-line and total size budgets actually cover the request line and headers (not just the body); `%`-escape truncation at end-of-input; malformed escapes pass through by contract; body rejected rather than clamped when over budget. The `Content-Length` header parse falls back to 0 on garbage — a body then goes unread, which is a *missing* parameter rather than a corrupted one, so it fails as a 400 from the handler |
| `cli/src/main.rs` argument surface | Swept every command with missing/extra/malformed arguments (absent dirs, empty queries, non-numeric `--k`/`--port`/`--min-confidence`/`--consolidate`, unknown tools, unreadable files): **no panics**, and each reports usage or a named error. The one gap — an unvalidated `--now` in the destructive `gc` — is fixed; see §3 |
| `sdk/src/kql.rs` parser | Swept with adversarial input (empty, truncated clauses, unterminated strings, negative/overflowing `LIMIT`, unknown `ORDER BY`/`RETURN` targets, non-ASCII kinds): **no panics**, and every malformed clause is rejected with a message naming what was expected. The one gap — unvalidated date bounds — is fixed; see §3 |
| `verifier/src/lib.rs` built-in checks | **Measured**, not read: a 27-case battery through `ckos verify` (15 must-fail, 12 must-pass) judged on exit code and per-check `FAIL` rows. Every bad output but one fails on the right check; the exception, `1,000 + 1 = 1,000`, is the *documented* grouped-number skip in `parse_operand` (non-evaluable is skipped, not guessed) and stays that way. The one false positive — a subscript read as a citation — is fixed; see §3. Do not re-run the battery to "confirm"; extend it only with a case that fails |
| Bounded `f32` quantities workspace-wide | **Swept deliberately** after the NaN defects in the scheduler and MMR, since both were "a documented range nothing enforced". Clean and not to be re-audited: `memory::cosine` (zero vector → 0, tested); `memory::rank_memories`/`MemorySignals` — the fields are public and documented `0..=1`, but every in-tree producer is already bounded (`recency_decay` clamps, `importance` is `u8/100`, `relevance` is a clamped `cosine`), so no NaN is reachable and adding a guard would be label-moving; `graph` confidence (`u8`, `.min(100)` at both the setter and the parser); `consensus` (`.min(100)`). The remaining `partial_cmp(...).unwrap_or(Equal)` sorts are only reachable with finite scores — worth re-checking only if a new *producer* of a score appears, which is the actual risk, not the sort |
| `docs/ci-workflow.yml` | **Verified, not assumed ready.** The file is the last thing between this repo and working CI, and nothing executes it, so it was checked by hand: the YAML parses; all five smoke-test commands run green verbatim (`version`, `run`, `kql`, and the `index` + `search` ingest pair on a `mktemp -d` session); and `./scripts/check.sh` passes under the workflow's own `RUSTFLAGS="-D warnings"` after touching a source file to force a rebuild — the setting local runs do not use, and the most likely cause of a red first run. Do not "fix" the `on:` key: a YAML 1.1 parser reads it as the boolean `true`, but GitHub's parser does not, and it is correct as written |
| `sdk/src/reflection.rs` constant score | The invariant `consensus score 95/100` across varied inputs is **correct**, not a stub: `HeuristicReflector` keys on structural properties (verified / has agent / non-empty output), so identically-shaped runs must score identically. Do not make it vary with content to look responsive — that is label-moving. Checked while chasing the telemetry defect in the same summary line |
| `sdk/src/engine.rs` | §893 lifecycle advanced only in `execute` so `task.state()` matches reality on every path (denial and no-runtime honestly stay `Queued`); bounded retry via `MAX_TASK_RETRIES`; a verification failure propagates instead of unblocking dependents; `WorkflowCompleted` fires only after every task ran; telemetry-scored submission; agent priority adopted only over the default. Its default-open behaviour for sensitive capabilities when no policy is attached, and the planner's inability to infer them, are **documented holes, not defects** — see the `SENSITIVE_CAPABILITIES` and `Engine::access` docs |
| `graph/src/store.rs` format | Tab/newline sanitizing on every field including the kind token; label placed last and rejoined, so embedded tabs in a hand-edited file cannot shift other fields; deterministic sort for byte-stable output; `confidence` clamped to 100; unknown line kinds skipped for forward compatibility; missing file loads empty by contract |

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
| `HashingEmbedder` sign taken from a hash bit determined by the bucket index (this round) | a "decorrelating" bit that is actually a function of the value it should be independent of |
| `fuse_rrf` merged distinct documents sharing a title (this round) | a human-readable label used as an identity key, where the real identity exists but was not carried |
| `RuntimeRegistry::register` desynced its order index from its map (this round) | two parallel collections that must agree, where only one deduplicates — grep for a `Vec` index beside a `HashMap` |
| `ckos serve`'s `session` parameter was an unconstrained filesystem path (this round) | a *documented* scope limitation ("no auth, use a proxy") mistaken for covering an *undocumented* one (unbounded filesystem reach) — when a module doc waives one property, check it does not silently waive a different one. Grep for request-supplied strings reaching `Path`/`fs` |
| 413/400 replies were RST away by their own connection close (this round) | a hazard *correctly handled in one place* (the 503 path's drain) and omitted from its siblings — when you find a subtle guard with a long comment, grep for the other sites that need it before assuming it is the only one. Its test also passed only because it avoided the very input that triggers the bug |
| A contentless synthetic record outranked the content explaining it (this round) | **emptiness can score better than substance.** A `graph_node` document whose body was its own title matched keyword exactly, embedded to a single-token vector, and was a graph node — three legs, so a corroboration bonus lifted it over authored prose matching on one. Wherever ranking rewards agreement across signals, check that a *degenerate* record cannot satisfy all of them trivially. The fix was to give the record content, not to re-weight the ranker |
| §929 was fail-open in `run`/`workflow` but fail-closed in `tool` (this round) | **a security default that differs between sibling commands — the fail-open sibling is the bug.** Also: it was pinned by a test asserting the old behaviour on purpose, so "a test covers it" did not mean "it is right". When a default protects something, check every command that shares the mechanism, and read what the test *claims* rather than that it passes |
| `gc --now <garbage>` deleted unexpired documents (this round) | the same unvalidated-date class as the KQL row below, in a **destructive** command — found by grepping for the sibling right after fixing the first. When a class turns up, sweep for every other input of that shape before moving on; here the second instance was the one that lost data |
| KQL took any word as a date and compared it lexicographically (this round) | a *documented precondition* that nothing enforces — the field's doc said "ISO date, lexicographically comparable" and the parser accepted anything. Grep for doc comments stating a format, then check the parser actually rejects violations |
| Deployment manifests marked ✅ while undeployable (this round) | config that no test reads is just prose — a Deployment ran `args: ["help"]`, which exits, so the pod CrashLoopBackOff'd forever with no Service and an HPA scaling nothing. Grep for YAML/TOML/Dockerfiles asserted by nothing |
| A renamed API field broke the dashboard silently (this round) | two artifacts coupled *by name* with no link between them — `dashboard.html` reads response fields the Rust code emits. Whenever you rename across a string boundary, grep the other side |
| Atomic writes shared one scratch path per destination (this round) | a safety mechanism whose own *intermediate state* is not unique — the rename was atomic, the temp file it renamed was not. Grep for a temp/lock/scratch name derived purely from its target |
| `Event::TaskStarted` fired before a runtime was selected (this round) | an event published at the *start of the function* rather than at the moment its own documented claim becomes true. Check where each event is emitted against what its variant's doc says it means |
| `CitationCheck` read `argv[0]` as an undefined citation and rejected the output (this round) | **a token-boundary guard present in one check and absent from its sibling in the same file** — `ArithmeticCheck` refuses a digit mid-identifier, `citation_markers` took any `[digits]`. The unguarded sibling was the one gating `run`/`/api/run`. Fourth sibling-divergence this generation: when a check carries a carefully commented guard, grep the neighbouring checks for the same hazard. Found by *measuring* the verifier with a must-pass/must-fail battery rather than reading it |
| Telemetry reported a *measured* `0.0ms` / `0 tok/s` for real work (this round) | **a unit too coarse to hold the measurement, plus a sentinel that reads as data.** `as_millis()` truncated every 3–83 µs task to 0, and the rate methods returned a bare `0.0` for "no denominator" while their sibling in the same `impl` returned `Option`. Zero is a claim; absence is not. When a metric is suspiciously round across varied inputs, check the unit before the arithmetic — and check whether a neighbouring accessor already models absence properly |
| Three eval metrics could be inflated by a duplicated hit; nDCG returned 1.307 (this round) | **fifth sibling-divergence of the generation, and the clearest: the guard existed, in the same module, with a doc comment explaining exactly why it was needed — and three neighbouring functions did not use it.** Two lessons. When a function documents a defensive measure, grep the file for peers that need the same one *before* trusting any of them. And when a metric has a defined range, assert the range: `nDCG > 1.0` is unarguable in a way that "this number looks high" is not. Found by turning the audit on the *measuring instrument* used to justify the previous round's fix |
| *(automation)* `sdk/src/eval.rs` now fuzzes its own invariants (this round) | The response to the row above. A bounded quantity should be asserted **as a range over generated input**, not only as example values: 42 000 random rankings with repeats rediscover the duplicate-credit defect unaided, including a `recall = 1.333` case no hand-written test had produced. Use `Lcg` there as the pattern for any other bounded/normalised quantity — it needs no external crate. It also disproved an invariant its author believed, which is the point of generating cases rather than choosing them |
| A `NaN` scheduling factor stalled a task in the ready queue forever (this round) | **the same unenforced-precondition class as the KQL date row, in the module whose headline property is anti-starvation.** `ScoreFactors` documented `0.0..=1.0`, five of six fields were public, and one lone builder clamped. Two general lessons: a `NaN` in a `>`-based max scan is not a crash but an *invisible* exclusion, and it defeats aging because `NaN + age` is still `NaN`; and when a doc comment states a range, enforce it at the point of *consumption*, not in setters that can each be bypassed. Found by generalising the nDCG range violation to every other bounded quantity in the workspace |
| `--lambda NaN` disabled MMR while reporting a re-rank (this round) | **the NaN class again, one layer up, plus a flag whose error message documented a range it never enforced.** `str::parse::<f32>` accepts `"NaN"`/`"inf"`; `f32::clamp` propagates NaN; a `>`-based selection loop then silently returns its input. Sweep: after finding a NaN hazard, check *every* `f32` that reaches a comparison, and every numeric flag whose help text states a range — `cosine` was checked the same way and is correct. Fix the boundary by rejecting and the library by defining the value; do not clamp at both and call it defence in depth |
| Chunk overlap sliced words in half, indexing terms absent from the corpus (this round) | **two components split the same text by different rules** — the chunker counted characters, the indexer tokenises on alphanumeric runs — so a boundary that looked arbitrary to one produced a *term* in the other. `hate` from `triphosphate` is the memorable case: not noise, a real word with an unrelated meaning, matched against a passage it has nothing to do with. When two components consume the same data, make the boundary rule the *consumer's* definition. Also: a "context preserving" mechanism that destroys the boundary token is worth measuring rather than trusting — the fix both removed a false positive and improved recall |
| Japanese search was broken end to end — keyword leg never fired, MRR 0.143 (this round) | **the headline feature did not work in the maintainer's own language, and no test noticed** — because every retrieval test was written in English. "Split on non-alphanumerics" encodes an assumption (words are space-delimited) that is simply false for Han/Kana/Hangul, so a whole clause became one term. Two lessons: when a rule *encodes an assumption about input*, name the assumption and test an input that violates it; and a test asserting the broken behaviour (`tokens("日本語のカーネル")` as one token) is not coverage — read what a test **claims**, as with the §929 default. Also the reason `memory::terms_of` now exists: one definition of a term, since the two legs' private tokenisers had already drifted apart |
| Japanese produced zero graph concepts, because detection keyed on `is_uppercase` (this round) | the same buried assumption as the row above, one layer up: **a property that silently does not exist in another script**. The fix is also the template for adding a heuristic here — candidates were *measured first* (katakana precision 1.00/recall 0.44; adding kanji gives 0.58/0.88) and the higher-recall one **rejected**, because a wrong concept is worse than a missing one when everything downstream treats it as fact. Recall 0.44 is recorded as a limitation with a lift condition (a morphological analyser), not quietly traded away for a bigger number |
| `ckos history` labelled every reflection `[FAIL]` (this round) | **absence of a verdict rendered as a negative verdict** — the telemetry lesson in a different subsystem: only executions carry `verified`, so a reflection's missing key became a failure in two CLI paths, the API and the dashboard. Whenever a display derives a boolean from an optional field, ask what a *missing* field should look like, and give it its own state. Also a live example of the test trap: the first regression test passed against the broken code because reflections rank below executions and a default-`k` recall never reached one — it now asserts it actually saw one |
| The dashboard threw on load and rendered nothing, in every commit (this round) | **an artifact no test executes is unverified however green the suite is.** 313 Rust tests, a route check and a JSON-shape tripwire all passed while the page was dead: they exercised the *server*, and the defect was in the embedded script (a `const` used above its declaration — hoisted, but in the temporal dead zone, and one uncaught top-level throw abandons the whole script). When a deliverable is a different language from the tests, ask what actually runs it; here, loading it in a browser took two minutes and found it immediately |
| Concurrent writers lost most of the session graph (this round) | **a read-modify-write split across two public calls is a race waiting for a second writer**, and `load` + `save` were paired by hand at five sites. The server made it reachable from two browser tabs. Two rules worth keeping: put the *whole* critical section behind one API (`GraphStore::update`) so no caller can hold a load without the lock; and choose the lock to match the widest concurrency the product actually has — here separate **processes** (a CLI run against a live `ckos serve` session), which a `Mutex` cannot see, so it is a lock file with a staleness break |

---

## 4. Known gaps — deliberate, with conditions for lifting them

These are NOT bugs. Each is parked with a reason; do not wire them up without
meeting the stated condition (that would be label-moving, §1):

- **Extraction never dates a node, so KQL `BEFORE`/`AFTER` are empty on real
  sessions.** `graph::Node::date` and the temporal clauses work — the demo
  graph sets dates and queries them correctly — but `graph::extract` has no
  producer for `date`, so a graph built by `ckos index` has none and every
  temporal query returns 0 results. Status doc §946 is 🟡 for this reason.

  **Not fixed on purpose.** The obvious heuristic — take a year found in the
  sentence and attach it to the entities there — is ambiguous *by construction*,
  not merely imprecise: in "Vector Labs published the Transformer paper in
  2017" the year belongs to the publication event, arguably to `Transformer`,
  and not to the organisation. Any rule assigns it to all three. Contrast the
  katakana entity heuristic, which shipped because it measured precision 1.00;
  here there is no version to measure that is right in principle.

  **Lift condition:** a relation-aware extractor that can attach a date to an
  *edge* or event rather than to every co-occurring node — or an explicit
  `meta.date` on ingested documents, which would be exact and needs no
  heuristic at all. The second is the cheaper half and is the one to do first
  if a user asks for temporal queries.

- **The library's `Engine` still defaults to no policy** (`access: None`,
  sdk/src/engine.rs) while every CLI command now defaults to `guest`. That
  split is intentional and each half says so where it is made: attaching a
  policy is an explicit decision for an embedder who may have their own
  authorization layer, whereas the CLI is a product surface with a user typing
  flags, where the safe default is the one that denies. Lift only if `Engine`
  gains a caller that is itself a product surface — and then by making the
  policy a constructor argument, not by silently defaulting one, which would
  hide an authorization decision inside a library.
- **`graph::GraphRepo` (§942/§943) and `messaging::ServiceMesh` (§912) are
  SDK-level, with no CLI surface — deliberately, and this was decided, not
  drifted into.** The test applied when auditing every dormant part was: *can a
  consumer use this as-is to accomplish something real?* Both pass — graph
  branch/merge and multi-agent routing both work standalone, in-process, for a
  library user today. (`MemoryTier` and `ResourceProbe` failed the same test —
  nothing routed by tier, nothing ever called `sample()` — and were deleted;
  see §3.) `GraphRepo` additionally has no on-disk format, so a CLI surface
  would first require inventing repo persistence; do not add a `ckos graph
  branch` that silently loses every branch between invocations.
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
  **Caveat on timing:** this stranding risk applies *once embeddings are
  released/persisted with a compat guarantee*. The signed-hashing correctness
  fix (see §3) was made *before* any release precisely to avoid needing that
  marker — there were no protected persisted vectors yet. A genuine embedder
  bug found pre-release should still be fixed now; only post-release changes
  need the version-marker infrastructure first.
- **`Event` now carries only variants with a real publisher.** The five that
  had none (`TaskCreated`, `RuntimeLoaded`, `MemoryUpdated`, `PluginInstalled`,
  `AgentRegistered`) are deleted, as is `Event::topic()`. Add one back
  *together with its publisher, in the same change* — an event is observable
  only by being published, so a variant nothing publishes is a promise the bus
  silently breaks. Note the dividing line used here: `Task::workflow` is
  written and never read, and it **stays**, because a consumer reading it gets
  a true answer; an unpublished event is unobservable by construction.
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
- **`RepetitionCheck`'s diversity signal is a raw type-token ratio**, which is
  length-dependent by construction (Heaps' law), so it is not comparable across
  outputs of different sizes. Measured on this repo's own documentation: 0.55 at
  1 000 tokens, 0.38 at 4 000, 0.32 at 8 000 — above the 0.25 default, but
  trending toward it. **No false positive was reproducible on real prose at
  those sizes, so the code was deliberately left unchanged** (§1: an
  unreproducible suspicion is not a finding). Lift only if a genuine false
  positive appears on a real long output, and then by switching to a
  length-independent measure — moving-average TTR (Covington & McFall 2010) —
  not by retuning the constant, which just moves the cliff.
- **Task retries have no backoff** (`sdk/src/engine.rs::requeue`), unlike every
  production retry loop (AWS backoff-and-jitter, Kubernetes CrashLoopBackOff,
  gRPC retry policies). Checked, not assumed: with an always-failing task and a
  healthy sibling in one workflow the observed order was `completed,
  bad-attempt, bad-attempt, bad-attempt` — the scheduler already dispatches
  ready work ahead of the doomed task's retries, so nothing starves. Note the
  naive fix is *worse*: sleeping in `requeue` would stall the single-threaded
  dispatch loop and create the head-of-line blocking that measurably does not
  exist today. Lift only when a genuinely remote runtime makes rapid retries
  harmful, and then as a delayed requeue — a "not before" dispatch cycle on the
  task (the scheduler already has a monotonic clock) that `dispatch_next`
  skips past — never a blocking sleep.
- **`ckos serve` has no TLS/auth by design** (`web/src/lib.rs` module doc):
  local single-operator surface; a reverse proxy adds transport security.
  Destructive ops (gc) stay CLI-only. **This waiver covers transport and
  identity only** — it does not extend to what a request can reach. Sessions
  are confined under `--session-root` (`AppState::resolve_session`); treating
  the auth waiver as covering filesystem reach is exactly the mistake that
  produced the traversal bug in §3. There is likewise **no CSRF token**: the
  confinement bounds *where* a cross-origin `POST /api/run` can write, not
  *whether* it can. Lift when the gateway grows any authenticated or
  destructive route — then an origin check or token is required, not
  optional.
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
