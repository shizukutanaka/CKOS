# Changelog

All notable changes to CKOS are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions track the
spec generations (v2.5 core kernel → v2.6 agent mesh → v2.7 knowledge
platform).

## [Unreleased]

### Added

- **Generated-input invariant tests for the retrieval metrics.** Musk's last
  step is *automate*: the duplicate-credit defect above was found by hand, and
  the same class had recurred five times this generation, so the next one
  should be found by a machine. `sdk/src/eval.rs` gains a deterministic LCG
  (no external crates) and three properties checked over 42 000 generated
  rankings that deliberately include repeats, empty lists and `k = 0`:

  1. every metric is finite and within `0.0..=1.0`;
  2. collapsing repeats out of a ranking never *lowers* a score — a repeat
     wastes a slot, it must not buy one;
  3. an ideal ranking scores exactly 1.0 and an empty one exactly 0.0.

  Checked against the pre-fix implementation, these rediscover the defect
  unaided in under a second — and surface a case the hand-written test never
  did: `recall = 1.333`. Every example test asserted a number it had been
  given; none asserted the *range*, which is why the class survived.

  The generator also corrected its author: a first draft asserted that
  removing repeats could not move recall or MAP at all, and a generated case
  disproved it immediately (both credit relevance at the *original* rank, so
  closing a gap legitimately improves them). The property was weakened to the
  one-sided form, which is the true one.

### Improved

- **A re-indexed concept now carries what the graph knows about it, instead of
  repeating its own name.** Found by measuring the product's headline feature
  rather than assuming it: a 4-document corpus scored with `ckos eval` gave
  MRR/nDCG@5 of 1.000 for `attention`, `starvation` and `diversity`, but
  **0.500 / 0.631 for `LSTM`** — a *literal* match, the best case this design
  has. The reason:

  ```
  [Keyword+Vector+Graph 0.05] LSTM — LSTM              ← contentless stub
  [Keyword 0.02] corpus/rnn.md#0 — Recurrent neural…   ← the passage explaining it
  ```

  `Reindexer::process` built the document as
  `Document::new("graph_node", label, label)` — the body *was* the title — and
  embedded only the label. **The stub outranked real content precisely because
  it was empty**: a one-word document is an exact keyword match, embeds to a
  single-token vector that cosines ~1.0 against that word, and *is* a graph
  node, so it scored on all three retrieval legs while authored prose scored on
  one, and RRF's corroboration bonus did the rest. Systematic, not incidental —
  every extracted concept returned its stub first, all at an identical 0.05,
  and stubs outnumbered passages 12:4 in the store. Meanwhile the graph knew
  the node's kind, confidence, date, provenance (`file:corpus/rnn.md`, added
  in the provenance fix below) and its edges, and none of it reached the
  document.

  The body is now a deterministic summary — `LSTM — concept (confidence 45),
  source file:corpus/rnn.md, related to Recurrent` — and *that* is what gets
  embedded. Neighbours are sorted, deduplicated and capped at 8, so the output
  is stable across re-indexing and a hub node cannot produce an unbounded body
  (the same "bound every resource" rule as audit/telemetry retention).
  Provenance and date also go into metadata, so they are filterable rather than
  only searchable — a concept becomes findable by its source file or by a
  related concept, which it was not before.

  **Reported honestly: nDCG@5 for `LSTM` is unchanged at 0.631.** RRF is
  rank-based, so a document present in three ranked lists still outranks one
  present in a single list regardless of how much content it gained. What
  changed is that the top result now *says something* — the defect was a
  useless first hit, and that is fixed and demonstrable. Whether an entity card
  should outrank the prose explaining it is a relevance judgement (a search
  engine showing a knowledge panel above web results makes the same choice),
  not a measured miss; the eval's judgement counts only the passage as
  relevant, which is arguable now that the card is informative. Tuning RRF or
  down-weighting `graph_node` until the number moved would have treated a
  symptom and silently shifted every other ranking.

  Persisted `graph_node` embeddings from an earlier build are now inconsistent
  with freshly computed ones. Per handbook §4 this is acceptable pre-release —
  `ckos index` regenerates them — and that rule is cited rather than
  re-litigated.

### Fixed

- **Three of the four retrieval metrics could be inflated by a duplicated hit;
  nDCG could exceed 1.0.** The Socratic follow-up to the retrieval work: the
  previous round used `ckos eval` as its measuring instrument, so the
  instrument itself was put on the bench.

  | case | before | truth |
  |---|---|---|
  | `ndcg_at_k(["a","a","b"], {a,b}, 3)` | **1.307** | ≤ 1.0 by construction |
  | `recall_at_k(["a","a"], {a,b}, 2)` | **1.000** | 0.5 — `b` was never retrieved |
  | `precision_at_k(["a","a"], {a}, 2)` | **1.000** | 0.5 |
  | `average_precision` (same input) | 0.833 | 0.833 — already correct |

  `average_precision` deduplicates with a `seen` set and its doc comment says
  why: *"a run listing the same document twice must not be able to inflate its
  own score."* Its three siblings in the same module each counted every
  occurrence. A normalised metric returning 1.307 is not a bad score, it is an
  impossible one; recall claiming 1.0 for a document that was never returned is
  worse, because it looks plausible.

  Fixed by moving the guard into one shared `first_occurrences` helper — which
  yields each id once at the **original** rank, so cutoffs and log-discounts
  still refer to the position the run really returned it at — and routing all
  four metrics through it, `average_precision` included, so they cannot
  disagree again.

  **Scope, stated honestly:** not reachable through `ckos eval` today. Retrieval
  deduplicates by document id and chunk titles carry a `#N` suffix; no duplicate
  title appeared across the plain, `--expand`, `--synonyms` and `--diverse`
  paths when checked. So the previous round's published nDCG figures stand
  unchanged. The defect is in the **public library API** (`ckos_sdk::eval`),
  which is the documented surface for scoring an external retrieval system with
  a caller-supplied ranking.

- **Telemetry reported `0.0ms` and `0 tok/s` for work that had really run.**
  Found by interrogating the run summary rather than trusting it: token counts
  moved with the input (1→9), so the pipeline was live, yet throughput was `0
  tok/s` on every query. A task's real duration is **3.6 µs – 83 µs** (measured;
  a 22× spread between samples), and `latency_ms` stored whole milliseconds via
  `as_millis()`, so every sample truncated to `0`.

  Two defects with one root — the layer could not represent what it measured:

  1. **Truncation.** All sub-millisecond work — which is *all* local-runtime
     work — recorded as zero.
  2. **Sentinel conflation.** `tokens_per_sec`/`mean_tokens_per_sec` returned a
     bare `0.0` for "nothing to divide by", while the sibling
     `mean_latency_ms()` in the same `impl` honestly returned `Option`. So
     `mean_latency_ms()` answered `Some(0.0)` — asserting a *measured* zero —
     and the CLI and dashboard printed it as fact.

  Fix: record **nanoseconds** (`latency_ns`, the resolution `Instant` already
  provides), keep milliseconds as the display unit via `latency_ms()`, and
  return `Option` from both rate methods so absence is stated as absence.
  `runtime_fit` and `mean_latency_ns_for` move to the same unit — in
  milliseconds every local runtime hit the function's "unknown latency → 1.0"
  branch, so the §904→§913 loop could not separate a 4 µs runtime from an
  830 µs one. `recommended_factors` keeps its millisecond argument and converts
  at the boundary. The CLI now picks a readable unit (`623ns`, `1.4µs`,
  `5.3ms`) instead of printing `0.0ms`, and `/api/status` sends `null` — with
  the dashboard rendering `—` — rather than a zero it did not measure. It also
  gained `mean_tokens_per_sec`, locked into the dashboard-contract test.

  Before: `telemetry: 3 tokens, mean latency 0.0ms, 0 tok/s`.
  After: `telemetry: 3 tokens, mean latency 623ns, 4815409 tok/s`.

  Also examined and found **honest**: the constant `consensus score 95/100`.
  `HeuristicReflector` scores structural properties (verified / has agent /
  non-empty), so identically-shaped runs *should* score identically; making it
  vary with content would be label-moving, not a fix.

- **`CitationCheck` rejected code: `argv[0]` was an "undefined citation".**
  Found by measuring the other headline claim, §899 independent verification,
  the way retrieval was measured: a 27-case battery through `ckos verify`
  (15 outputs that must fail, 12 that must pass), judged on the exit code and
  the per-check `FAIL` rows. 14/15 bad outputs failed on the right check (the
  one pass, `1,000 + 1 = 1,000`, is the documented grouped-number skip, not a
  defect). 11/12 good outputs passed. The twelfth:

  ```
  $ ckos verify 'Use the { key } syntax :) and array[0].'
    citations        FAIL — undefined citation(s): [0]
  ```

  `citation_markers` took any `[digits]` as a citation, so every subscript —
  `argv[0]`, `items[1]`, `m[0][1]`; this repository's own sources contain 41
  — was a dangling reference and the whole output was rejected.
  `ArithmeticCheck` in the same file already has the token-boundary rule
  ("only a digit that begins a token, not mid-identifier"); its sibling never
  got it. Consequential because `CitationCheck` is one of the two checks
  gating every step of `ckos run` and the dashboard's `/api/run`: a runtime
  answering a coding question with `sys.argv[0]` had its step marked `Failed`.
  A verifier that rejects valid output is the failure mode the module's own
  rules call worse than skipping. Fix: a `[` directly after a token character
  (alphanumeric, `_`, `]`, `)`) is a subscript and is not a marker; citation
  markers follow whitespace, punctuation or line start, which is also how
  definition lines were already found, so nothing else moves. Detection is
  unchanged — the battery re-run is 14/15 bad → FAIL, **12/12** good → PASS —
  and `argv[0] proves it [2]` still fails, naming only `[2]`. Regression tests
  at both layers (`subscripts_are_not_citation_markers`; the CLI verify test
  now includes a code sample that must exit 0).

- **§929 authorization was fail-open in `ckos run` and `ckos workflow`:
  omitting `--role`/`--token` disabled the gate rather than lowering
  privileges.** Both passed no default role to `resolve_identity`, which meant
  *no policy was attached at all*, so `Engine::execute`'s check was skipped
  entirely. Measured on a workflow with a gated `medical` step — one of the
  four `SENSITIVE_CAPABILITIES` the gate exists for:

  | Invocation | Before |
  |---|---|
  | `--role admin` / `--token tok-admin-hq` | 2/2 verified (correct) |
  | `--role guest` / `--token tok-guest` | denied (correct) |
  | **(no flag)** | **2/2 verified — the gate was absent** |

  `ckos tool` already defaulted to `guest` and failed closed, so one binary
  shipped both defaults for the same mechanism — the third sibling-divergence
  found this cycle, after the RST drain and the date validators. The
  `workflow` help said the flags "attach authorization" without saying that
  omitting them ran gated steps unauthorized.

  **This reverses a deliberate prior decision, not an oversight.** The
  behaviour was pinned by an assertion in
  `sensitive_capability_requires_an_authorized_role` reading *"the engine has
  no policy attached: unrestricted, as it always was before this authorization
  gate existed"*. That backward compatibility is compatibility with the
  ungated state the gate exists to end, and nothing external depends on it —
  no tags, no releases, not on crates.io. The assertion is flipped in place
  with the reversal recorded beside it, rather than deleted.

  All three commands now default to `guest`. Since no caller can any longer
  produce "no identity", `resolve_identity` returns a bare `Identity` instead
  of an `Option` and the two `if let Some(identity)` attach sites collapse —
  an `Option` that is never `None` invites a reader to assume the
  unauthenticated path still exists. `cmd_tool` had already needed an
  `unreachable!("default_role always yields Some")` there, which was the type
  system saying so out loud.

  Deliberately unchanged: the library's `Engine::access` stays `None` by
  default. Attaching a policy is an explicit decision for an embedder; the CLI
  is a product surface where a user types flags, and there the safe default is
  the one that denies. Both positions are now stated where they are made.

  The `workflow` help also gained the definition-file format, which it never
  documented — awkward for the command the help itself calls "the only
  reachable way" to author a gated step.

- **`ckos gc --now <garbage>` silently deleted documents that had not
  expired.** The most serious defect found this cycle: destructive, silent,
  and triggered by an ordinary typo. A document's `expires` metadata is
  compared *lexicographically* against `--now`, so a malformed value did not
  fail — it changed which documents counted as expired. Measured against a
  document whose `expires` was `2999-12-31`:
  `--now today` and `--now notadate` each **collected it, reported as
  "Expired"**, because `"2999-12-31" < "today"`. Other typos (`2026-8-25`,
  `08/25/2026`) happened to be harmless — they sort *below* the expiry rather
  than above it — which is exactly the point: the outcome depended on where
  the garbage sorted, not on any rule, and `gc` deletes files.
  `--now` is now validated before anything is collected.
  Found immediately after the KQL date fix below, by grepping for the sibling
  of a class just fixed — the handbook's standing instruction. The validation
  now lives once, in `graph::validate_iso_date`, next to the temporal model
  both callers share, rather than being copied into the second site.
  The regression test sweeps five malformed values, asserts the document
  survives each, **and** asserts that a genuinely past expiry is still
  collected — otherwise the test would pass by disabling the feature it
  guards.
- **KQL accepted any word as a `BEFORE`/`AFTER` date and compared it anyway.**
  The bound is compared lexicographically against a node's date, which equals
  chronological order *only* for well-formed `YYYY-MM-DD` — a precondition the
  field's own doc has always stated (`ISO date, lexicographically comparable`)
  and nothing enforced. Garbage therefore produced a confident, meaningless
  answer rather than an error. Measured against the demo graph (Transformer
  dated 2017-06-12):
  `BEFORE notadate` → 1 hit (digits sort before letters), `AFTER notadate` → 0
  hits, `BEFORE 99` → 1 hit, `BEFORE 2017-13-45` → 1 hit. A user typing
  `2017/06/12`, or a date in their locale's format, got a plausible result that
  meant nothing, with nothing to indicate anything was wrong — on input
  reachable from both `ckos kql` and `POST /api/kql`.
  `LIMIT` and `ORDER BY` already rejected their malformed inputs loudly; the
  date clauses were the one place that guessed. They now validate the format
  and report `BEFORE needs an ISO date (YYYY-MM-DD), found "…"`.
  Full calendar validation is deliberately *not* done, and the reason is
  recorded on `parse_date`: ordering is the only use of this value, and a
  well-formed but non-existent date such as `2017-02-30` still orders
  correctly against every real ISO date, so leap-year arithmetic would add
  complexity for no gain in correctness. The coarse month/day ranges exist to
  catch transpositions like `2017-31-06`, not to validate history — asserted
  in the test so the boundary is an explicit choice rather than an accident.
  Found by throwing adversarial input at the parser (no panics, but this
  silently wrong acceptance); the test sweeps nine malformed shapes across
  both clauses, plus the boundary values `0001-01-01` and `9999-12-31` that
  the range check must not exclude.

### Corrected — a "blocker" that was already done

Re-tested the release path instead of trusting an earlier session's notes, and
one inherited claim turned out to be **false**:

- `docs/releasing.md` and `docs/agent-handbook.md` both listed "switch the
  default branch to `main`" as outstanding owner work. It is not: the default
  branch is already `main` (`git ls-remote --symref origin HEAD` →
  `refs/heads/main`) and the repository is public. So a plain `git clone`
  already yields the released state, which `scripts/verify-quickstart.sh`
  confirms builds and works. **CKOS is distributed as source, so the software
  is already available** — the outstanding items add a *named, citable
  version*, not the delivery.
- An older note claimed all GitHub access was denied. Also stale: pull
  requests to `main` work fine. What *is* genuinely blocked, re-verified
  directly this time: pushing a tag returns **HTTP 403**, and the MCP server
  exposes no create-release and no repo-settings tool.
- The handbook's "environment facts" section now says to test a claim before
  writing "X is impossible here" into it, since two of its claims had been
  inherited rather than measured.

### Added (delivery)

- **README `## Install` section** — `cargo install --path cli`, verified to
  produce `ckos 2.8.0`, plus the no-install alternatives. For a
  source-distributed tool the README *is* the storefront, and it previously
  had no install instruction at all.
- `verify-quickstart.sh` now asserts the version the README advertises matches
  what the binary reports. A version hand-written in prose goes stale the
  first time `Cargo.toml` is bumped, and a wrong number in the install
  instructions is the first thing a new user sees. Verified by setting the
  README back to 2.7.0, which fails the check by name.

### Changed

- **`Hit` now reports every source that matched it, not one.** `Hit.source:
  HitSource` became `Hit.sources: Vec<HitSource>` (sorted, deduplicated, never
  empty), unioned across both fusion passes. `ckos search` renders it as
  `Keyword+Vector+Graph`; `/api/search` returns a `sources` array in place of
  the old `source` string, and the dashboard's results table joins it the same
  way (that last part was initially missed — see the tripwire under Added).
  Fusion was already *correct* — an item corroborated by several legs got the
  combined RRF score and rose accordingly. What it did not do was say so, and
  the representative it kept for display was whichever single leg scored
  highest. So every result printed `[Keyword]`, and the hybrid BM25 + vector +
  graph search this product is built around was invisible in its own output:
  a user tuning relevance could not tell whether the graph or vector legs
  contributed at all, and the README's headline claim was unverifiable from
  the CLI. On the quickstart corpus the same query now reads
  `[Keyword+Vector+Graph 0.05] Transformer` and `[Keyword+Vector 0.03]
  paper.md#0` — the three legs had been fusing the whole time.
  Deliberately a replacement rather than a second field beside `source`: two
  collections that must agree is the exact bug class already fixed once here
  (`RuntimeRegistry`'s order index desyncing from its map).
  Two regression tests, each proven to fail against the branch it guards —
  the identity merge and the graph-by-name fold union sources separately, and
  reverting either leaves the other's test passing.

### Fixed (deployment)

- **The Kubernetes manifest could never have run, and the Compose stack
  started three databases nothing connects to.** §931–932 was marked ✅.
  - `deploy/k8s/ckos.yaml` ran `args: ["help"]`. `ckos help` prints and exits
    0 (verified locally), and a Deployment restarts a container that exits —
    so the pod would sit in **CrashLoopBackOff** permanently. There was also
    no `Service`, making it unreachable even if it had run, and no probes; the
    HPA scaled on the CPU of a workload that never started. It now runs
    `serve` (the only long-running mode CKOS has) with `--host 0.0.0.0`
    (127.0.0.1 is unreachable from outside a pod's network namespace),
    readiness/liveness probes on `/api/status`, a **ClusterIP** Service —
    deliberately not LoadBalancer, since the gateway has no TLS or auth — and
    an `emptyDir` for sessions, with a note on why a shared PVC would be wrong
    while the HPA can run several replicas over uncoordinated session files.
  - `docker-compose.yml` started Neo4j, Qdrant and Redis, described as "the
    real public images that back the §935 data layer". They backed nothing:
    there is no driver for any of them anywhere in the workspace, CKOS having
    zero external dependencies by design — and `docs/implementation-status.md`
    already places those backends outside v1. Deleted. The one remaining
    service runs the gateway and publishes it on **loopback only**.
  - `Dockerfile` now defaults to the gateway rather than `help`, declares the
    session volume (without it, every session dies with the container),
    exposes the port, and creates `/data` owned by the non-root user.
  - **`scripts/check-deploy.sh`** (wired into `scripts/check.sh`) asserts the
    properties that make these deployable, because YAML that no test reads is
    just prose. Verified by re-introducing both original defects, which it
    reports by name. It checks *structure*, not behaviour, and says so — no
    cluster or container runtime is available to this automation, so
    `kubectl apply` and `docker build` remain unverified here.

### Added

- **A response-shape tripwire for the dashboard** (`web/src/lib.rs`).
  `dashboard.html` is a static string compiled into the binary that reads API
  fields *by name*, and nothing linked the two: renaming a field in
  `routes.rs` left the page silently rendering `undefined` with every test
  still green. Not hypothetical — the `source` → `sources` rename in this same
  section did exactly that to the search results table, and it was caught by
  reading the HTML afterwards, not by the suite or by
  `verify-quickstart.sh` (which checks the route answers, not that the page
  can consume it). The new test pins the field names each route emits and
  checks **both directions**, since either side can drift: the API dropping a
  field the page reads, and the page being edited to read a field the API
  never had. Verified by re-introducing the exact rename, which now fails with
  a message naming the route, the field, and the file to update.
- **`scripts/verify-quickstart.sh`** — runs the exact command sequence
  `README.md` tells a new user to run, against a `--release` build, and
  asserts each produces what the README says it does: every CLI command, every
  HTTP route, and the session-confinement boundary probed from outside.
  It exists because of the provenance bug below. Every gate in `check.sh`
  passed while `ckos index` silently recorded no source — unit and integration
  tests each verified their own layer, and nothing verified the documented user
  path end to end. That bug was found by hand from a clean clone of `main`;
  this is that check automated, and it is verified to catch it (reverting the
  fix makes the script fail on exactly that assertion).
  Deliberately *not* part of `check.sh`: a release build plus a real server is
  far too slow for a per-commit gate. It is wired into `docs/releasing.md`
  instead, and into the handbook's discipline list.

### Fixed

- **`ckos index` recorded no provenance, so the README's own quickstart query
  answered `<unknown>`.** `KnowledgeBus::ingest_text` called
  `extract_concepts`, the *non*-provenance extraction path, unconditionally.
  So the one command whose entire job is loading a corpus — and where
  provenance matters most, because the user wants to know which file a fact
  came from — was the only command that recorded no source. §947 meanwhile
  claimed ✅ "extraction stamps source".
  Found by running the documented quickstart end to end from a clean clone of
  `main`, exactly as a new user would: `ckos index ./sess paper.md` followed by
  `ckos kql --session ./sess 'FIND Concept "Transformer" RETURN Graph +
  Sources'` printed `src=<unknown>`, and the persisted `graph.kg` showed an
  empty provenance column on every node.
  `ingest_text_from(text, source)` now carries it through; `ckos index` passes
  `file:<path>`, matching the `kind:value` convention `run:intent` already
  uses. `ingest_text` remains as the explicit no-source form. Reinforcement
  deliberately keeps a node's *original* source rather than overwriting it,
  so "the source" does not silently become "the most recent source" — asserted
  by re-ingesting the same entity from a second file.
  Two regression tests, each proven to fail against its own layer reverted:
  one on the SDK API, one through the CLI, because the SDK test passes even
  with the CLI wiring removed and the wiring is where the user meets this.
  The CLI assertion's first draft used `FIND Concept "Vector Labs"` and matched
  nothing — the extractor classifies an `… Labs` entity as an Organization.
  That was a **fixture error, not a code defect**: dumping `graph.kg` confirmed
  all four nodes carried `file:`. The fixture was corrected and the reason
  recorded inline.

## [2.8.0] — 2026-08-24

Hardening and honesty release. Everything below was developed against the
v2.7 knowledge platform: 20+ reproduced-then-fixed defects (each with a
regression test proven to fail without its fix), IR/RAG improvements from the
literature, an explicit v1 scope declaration in
`docs/implementation-status.md`, and the removal of every subsystem that
satisfied its spec section in name only. Breaking API changes are listed
under Removed; no compatibility shims were kept because nothing external
consumes these APIs yet (no tags, no published binaries, no crates.io
release).

### Improved (from recent IR/Graph-RAG literature)

- **BM25+ keyword ranking** (§950): `retrieval::keyword_hits` now adds the
  Lv & Zhai (CIKM 2011) δ lower-bound to each matched term's normalized TF,
  so BM25's over-penalization of long documents can no longer drop a matched
  term's contribution below an unmatched document's. δ = 1.0 (the paper's
  recommended value); regression test covers the long-rare-term-beats-
  short-common-term case the correction targets.
- **Light stemming in the retrieval tokenizer** (§950): `tokens` matched terms
  by exact string, so a query for `schedulers` could not reach a document that
  only ever writes `scheduler` — a plain recall gap on the live search path
  (`ckos search`, `/api/search`), and one neither pseudo-relevance feedback nor
  the synonym table closes (PRF only pulls terms from documents already found
  by literal overlap; the synonym table is a curated list, not morphology).
  Documents and queries now pass through an **S-stemmer** (Harman, *How
  effective is suffixing?*, JASIS 42(1), 1991): plural `-s` forms only, with
  the published exceptions (`-us`, `-ss`, `-aes`, `-ees`, `-oes`, `-eies`,
  `-aies`). Deliberately not Porter/Lovins — Harman measured that aggressive
  suffix stripping helps and harms about equally often, while plural-only
  stemming is close to free. Exceptions terminate rather than falling through,
  since a fall-through would let the bare `-s` rule strip the very character
  the exception guards. Tested against the rule table, for idempotence (so
  folding feedback terms back into a query is a no-op), and for multi-byte
  safety — every slice offset lands on an ASCII suffix byte, the same class of
  bug as the earlier CJK cut in `memory::summarize`.
- **tf × idf term selection for pseudo-relevance feedback** (§950): expansion
  candidates were ranked by raw frequency in the feedback set, so the (small)
  expansion budget went to whatever those documents happened to repeat — which
  is usually a term the *whole corpus* shares. A term present in every document
  discriminates between none of them, so it recalls nothing while displacing a
  term that would. Reproduced: with one expansion slot over a six-document
  store, `system` (in five of six) won and `photon` — the only term reaching
  the target document — was displaced, so the target was never recalled.
  `expand_query_with_corpus` now weights candidates by feedback frequency times
  inverse document frequency over the store (Rocchio/RM3 term selection), using
  the same non-negative BM25 idf form as the keyword ranker; `search_expanded`
  uses it. `expand_query` stays as the frequency-only fallback for callers with
  no corpus, where idf is not computable at all.
- **Signed hashing in `HashingEmbedder` now actually decorrelates collisions**
  (§944): the ±1 sign for each token was taken from FNV-1a's *low* bit, but
  that bit is merely the parity of the input bytes (the odd-prime multiply
  preserves it), so for any even `dim` — the default is 64 — it is exactly the
  bucket index's parity. Every token landing in a bucket therefore shared a
  sign, so colliding distinct tokens could only add, never cancel: the sign
  matched the bucket parity for 100% of tokens and colliding pairs cancelled 0%
  of the time, versus the ~50% signed hashing (Weinberger et al. 2009) needs to
  make the inner product an unbiased collision estimate. The result was a
  spurious positive-similarity floor between documents sharing no tokens. The
  sign now comes from the *top* bit, which the multiply chain mixes well and
  which `% dim` does not consume; measured cancellation returns to ~50%. Vectors
  persisted by an earlier build should be regenerated (`ckos gc` drops broken
  ones; re-indexing recomputes) — done pre-release, so no compat marker was
  required (see `docs/agent-handbook.md` §4). The module's "honest limitation"
  test and doc were updated: a no-shared-word paraphrase still shows no semantic
  recall (~0.13, now vs the ~0.77 of literal overlap), but the old "~0.13 either
  way" figure was itself the bug's positive-collision floor.
- **Personalized-PageRank graph expansion** (§951/§952): the retriever's
  multi-hop graph expansion is now a HippoRAG-style Personalized PageRank
  pass (`graph::personalized_pagerank`, arXiv:2405.14831 / 2502.14802)
  seeded on the query's matched nodes, replacing a fixed per-hop geometric
  decay. Associated nodes rank by how much query mass actually flows to them,
  so a node corroborated by several short paths outranks one reached by a
  single long path — with a deterministic diamond-graph test proving exactly
  that. `graph::pagerank` was refactored onto a shared `pagerank_core`
  (uniform teleport = classic PageRank, verified behaviour-preserving).

### Scope — v1 declared complete against a stated boundary

`docs/implementation-status.md` used to end by saying every mechanism has "a
working, tested implementation behind a trait seam where a production backend
will later plug in." That sentence let the product be permanently 90%
finished: a trait with no implementation is not a delivered capability, and
counting it as one is the same label-moving the codebase forbids everywhere
else. Fourteen rows sat at 🟡/⏳ with no statement of whether they ever had to
be filled.

They are now answered one at a time. Every remaining ⏳ requires breaking the
`std`-only, zero-dependency, offline constraint — which is not a temporary
state but the product's reason to exist — so each is **out of scope for v1**,
listed with the dependency it needs and why a smaller version would be worse
than none (a fake WASM sandbox implies isolation the host cannot enforce; a
hand-rolled OIDC verifier or AES is a security hazard, which is exactly why
`sdk::crypto` stops at SHA-256/HMAC, where published test vectors can prove it
right). Two items are genuinely blocked rather than out of scope, and both are
named with who can unblock them: §905 CI needs a maintainer to copy the ready
workflow file, and the public release needs the repository owner.

This is a judgment call, recorded rather than left implicit, and any line of
it can be overturned by the owner.

### Removed

- **Five `Event` variants that nothing published**: `TaskCreated`,
  `RuntimeLoaded`, `MemoryUpdated`, `PluginInstalled`, `AgentRegistered`. Four
  had exactly one reference in the entire workspace — their own declaration;
  the fifth appeared only as filler in the bus's own test. An event is
  observable *only* by being published, so a variant with no publisher is not
  a feature a subscriber can ever use — it is a promise the bus silently
  breaks, and it was what held §894 at 🟡. §894 is now ✅ honestly: every
  remaining variant has a real publisher.
  The same sweep deliberately **kept** `Task::workflow` (written by `Dag::add`,
  read by nothing) and five unused public accessors such as
  `VersionedGraph::current_branch` and `Scheduler::pending_len`. The dividing
  line is whether a consumer can get a true answer out of it today: those are
  populated with correct data and readable, whereas an unpublished event is
  unobservable by construction. Deleting coherent public accessors to inflate
  a deletion count would be deletion theater.

### Fixed

- **`docs/implementation-status.md` §960 claimed its own network API was
  pending**, listing only the CLI, when `ckos serve` had already been
  delivering `/api/search`, `/api/kql`, `/api/history`, `/api/graph` and
  `/api/run` over HTTP/JSON. The row predated the `web` crate and was never
  revisited. Now ✅ with the routes named.
- **`scripts/check-status-doc.sh` reported a backticked *filename* as a
  missing symbol**: it extracts capitalised tokens from inside backticks, so
  `` `CHANGELOG.md` `` yielded a lookup for a type named `CHANGELOG`. Found by
  the gate firing on this very commit's edits. `` `README.md` `` had escaped
  only because the bare word "README" happens to appear in a doc comment, so
  this was a live false-positive class rather than one bad row. A backticked
  token that names a real file is now skipped as the file reference it is;
  re-verified in both directions (83 symbols resolve; a row edited to cite
  `TotallyGoneType` still exits 1).
- **`Event::TaskStarted` was published for tasks that never reached a
  runtime.** The event is documented as "a task began executing on a runtime",
  but `Engine::execute` published it as its very first statement — before the
  §929 policy gate and before runtime selection. A task stopped by either gate
  therefore announced a runtime execution that never happened, contradicting
  the module's own invariant that the observable state reflects reality.
  Reproduced with an empty `RuntimeRegistry`: `execute` returned `Err`, the
  task ended `Queued` (correctly — §893 has no `Queued -> Failed` edge), and
  `TaskStarted` had already fired once. A consumer counting starts against
  completions leaks one per denial, with nothing to close it.
  Now published immediately after the `Running` transition, where the claim is
  true. Note the fix is *not* to emit `TaskFailed` on those paths: the task is
  still `Queued`, so that would make the event stream contradict the state
  machine instead of the doc. `execute`'s doc now states plainly which events
  each path emits, rather than the previous blanket "emitting events … on
  every path". The regression test covers both never-started paths **and** the
  positive case, since a fix that merely stopped publishing the event would
  pass a negative-only test.
- **The atomic write's scratch path was shared, so two writers could corrupt
  the file the rename was meant to protect** (`graph::GraphStore::save`,
  `memory::file_store::write_atomic`). Both derived the temp path from the
  destination (`<dest>.tmp`), making it identical for every writer of that
  file. `File::create` opens `O_TRUNC`, so two writers truncate each other's
  partial file while each keeps writing at *its own* offset, and the rename
  then installs the mixture — the exact corruption the rename exists to
  prevent, making the documented "readers see either the old file or the
  complete new one" guarantee conditional on nobody writing concurrently.
  Verified at the syscall level rather than argued: with writer A at offset
  2048 of 4096 when writer B truncated and wrote 1024 bytes, the renamed file
  was neither A's nor B's content but B's 1024 bytes, then **1024 NUL bytes**
  (the hole A's truncated prefix left), then A's tail. Concurrency here is not
  hypothetical — `ckos serve` handles requests on separate threads, two
  `POST /api/run` calls against one session both save that session's graph,
  and `Reindexer` addresses documents by a *deterministic* id.
  Each `save`/`write_atomic` now takes a scratch path unique to the call
  (`.<name>.<pid>.<seq>.tmp`); the `.tmp` extension still keeps it out of
  `FileStore::open`'s `*.doc` scan.
  Honest note on how this was found: 40 rounds of two genuinely concurrent
  `save`s, plus 20 more with a synchronized start and a 60 000-node graph,
  never tripped it — the window is narrow, not absent, which is why the
  syscall-level check was done before concluding anything. The regression
  tests (one per crate) therefore model the interleaving *deterministically*
  instead of racing: they hold an open handle at a nonzero offset on the old
  scratch path — exactly the state a writer mid-`write_all` is in — and assert
  the saved file contains neither NUL bytes nor the other writer's data. Both
  fail with the fix reverted (3 932 and 4 045 NUL bytes respectively).
- **A 413/400 rejection was destroyed by its own connection close.** Closing a
  socket that still holds unread data makes the OS send RST rather than FIN,
  and the RST discards whatever of the response is still in flight.
  `serve_bounded`'s 503 path already knew this and drained before closing;
  `handle_connection`'s 413 and 400 paths did not — and they are where it
  matters most, since by construction the peer is mid-request. Measured, not
  assumed: a client actually streaming the oversized body it announced
  received a **truncated** response in 3 of 5 runs — as little as
  `"HTTP/1.1 "`, 9 bytes, and in other runs full headers promising
  `Content-Length: 29` with no body — while its read returned `Ok`, so the
  peer sees a *successful* read of a fragment carrying no status code.
  The existing oversize test could not catch this because it deliberately
  never sends the body ("a correct implementation rejects before attempting
  to read it") — true of the parse, but the close still races the peer's
  writes. Both reject paths now go through one `http::write_early_response`,
  which the 503 path also uses, so the rule lives in a single place instead of
  one correct copy and two omissions. Its **byte** cap additionally fixes what
  the 503 copy got wrong: an idle timeout alone does not bound a drain,
  because it only fires when the peer goes quiet — a peer that keeps streaming
  makes every read succeed and holds the connection thread indefinitely.
  Regression test calibrated in both directions rather than assumed: against
  the reverted fix, 5 attempts missed the race in 1 of 3 runs while 30
  attempts tripped it within the first four every time across 4 runs; with the
  drain in place, 150 attempts produced no truncation.
- **`ckos serve`: a request's `session` was an unconstrained filesystem path**
  (path traversal, CWE-22). Every session handler passes the parameter to
  `FileStore::open`, which `create_dir_all`s it and then writes `*.doc` files
  and `graph.kg` there — so one unauthenticated request could create
  directories and write documents anywhere the server process could reach.
  Reproduced before fixing: `POST /api/run` with
  `intent=say+hello&session=/tmp/ckos-repro-victim/deep/nested` answered
  `200 OK` after creating all three directory levels and writing two
  documents plus a graph into them. This is *not* covered by the crate's
  "no TLS, no auth — put it behind a reverse proxy" scope note: a proxy adds
  transport security and authentication, but an authenticated operator still
  expects `session` to name a session rather than to be a filesystem-wide
  write primitive. It is also reachable without any operator action at all —
  the handler accepts `application/x-www-form-urlencoded`, a CORS-simple
  content type that any web page can `POST` cross-origin to `127.0.0.1`
  without a preflight, and the write lands whether or not the page can read
  the (opaque) response.
  Sessions are now confined to a **session root**: `ckos serve
  [--session-root <dir>]` (default: the current directory, printed at
  startup), `ckos_web::serve_rooted` for library callers, and
  `AppState::resolve_session` resolving each name beneath it. The rule is
  strict and easy to audit — a name is accepted only if every path component
  is `Component::Normal`; absolute paths, `..` and Windows drive prefixes are
  **rejected with a 400 rather than sanitized**, since silently rewriting the
  path would write to a different session than the caller named (the same
  "explicit rejection over silent correction" rule as oversized bodies → 413).
  Applied at *every* handler that takes a `session` — `run`, `history`,
  `search`, `kql`, `graph` — through one shared pair of helpers, not just the
  one the escape was demonstrated on; `kql` and `graph` treat the parameter as
  optional and previously reached the filesystem with the raw string. Two
  regression tests, both confirmed to fail with the resolver reverted to a
  pass-through: one asserts 400 plus *no directory created* for an absolute
  path and for two traversals (including `a/../../escapee`, which only escapes
  after descending — the partial case a `starts_with("..")` guard would miss)
  while a plain relative name still persists under the root, the other sweeps
  all four remaining handlers.
  Side effect, deliberate: dropping `.` components normalizes the resolved
  path, so `a`, `./a` and `a/` now share one search-cache entry and one
  generation counter. Previously the raw request string was the cache key, so
  three spellings of one directory could invalidate each other's caches
  inconsistently.
  Out of scope and stated rather than silently assumed: a symlink *already
  planted inside the root* still leads where it points. No request can create
  one through this API, so that needs prior local write access to the
  operator's own session tree — a different threat model.
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
- **`knowledge_bus::Reindexer` duplicated documents on re-index**: its own doc
  comment promised that re-indexing an already-indexed graph node "replaces
  its document rather than duplicating it," but `Document::new` mints a fresh
  id on every call and nothing looked up the prior one — reproduced directly:
  reinforcing then re-indexing the same node (an ordinary, repeated
  occurrence) left two `graph_node` documents with identical `node_id`
  metadata in the store. Beyond unbounded growth, this silently inflated that
  node's own hybrid-search score, since `retrieval::fuse_rrf` merges
  same-titled hits *across* source lists but not duplicate-titled entries
  *within* one source's own list. Now reuses the existing document's id for
  that node, if one is stored, so re-indexing genuinely replaces it.
- **`Engine::run_workflow` reported a permanently-failed task as if the
  workflow had succeeded**: once a task's output kept failing *verification*
  across every retry, the dispatch loop's `Ok(result)` arm fell through to
  `scheduler.mark_completed` and pushed the `Failed` result exactly like a
  genuine success — unlike the parallel `Err(e)` arm (a runtime error
  exhausting retries), which already propagated an error correctly. This
  silently unblocked any dependent task gated on that task reaching a real
  `Completed` state, and still published `WorkflowCompleted` for a workflow
  that never actually finished — contradicting the function's own doc
  comment. Reproduced directly with an always-failing verifier check before
  fixing (both a single-task workflow and a two-step dependency chain, the
  latter proving a dependent task incorrectly dispatched). Now returns an
  error from that arm too, mirroring the runtime-error path exactly.
- **`ckos graph --session <dir>` silently destroyed the session's persisted
  graph instead of accumulating into it**: unlike `ckos run --session`
  (which explicitly loads any existing `graph.kg` before extracting, "so
  `ckos search` gains graph context"), `cmd_graph`'s session branch always
  started from an empty graph, sourced text only from the session's stored
  *documents*, and unconditionally overwrote `graph.kg`. Since `ckos run
  --session` extracts concepts from the intent text and step outputs — text
  that is never itself saved as a document — running `ckos graph --session`
  afterward (an ordinary, documented workflow) wiped out everything the
  prior runs had added, with no warning. Reproduced directly (two `ckos run
  --session` calls each contributing a distinct concept, then confirming
  `ckos graph --session` dropped one of them) before fixing; it now loads
  any existing graph first and extracts into it, matching `run`'s behavior.
- **`GraphStore::save` wrote node/edge kind tokens unsanitized**: every other
  free-form field it writes (id, date, provenance, label) is passed through
  `sanitize()` to strip tab/newline before being placed in the tab-separated
  file, but `NodeKind`/`EdgeKind`'s `Other(String)` variant — freely
  constructible by any caller, e.g. extraction code building a kind from free
  text — was written raw. A tab embedded in a custom kind shifted every later
  field on reload: confidence silently became 0, the real confidence was
  misread as the date, and the real provenance/label ran together. Reproduced
  directly before fixing (`NodeKind::Other("foo\tbar")` came back with
  `confidence: 0`, a bogus `date`, and a mangled label); the kind token is now
  sanitized like every other field.
- **Stemmed tokens bypassed the query-expansion stopword list** (§950), a
  self-inflicted regression from the S-stemmer landing in `tokens`: the list is
  written in natural spelling but candidates arrive stemmed, so every entry
  whose stem differs stopped matching (`this` → `thi`, `was` → `wa`, `has` →
  `ha`, `its` → `it`, `his` → `hi`). `this` is long enough to clear the
  length filter, so pseudo-relevance feedback injected the junk token `thi`
  into the query — as the *top* expansion term, since it was the most frequent
  word in the feedback set. Reproduced directly
  (`expand_query("cache", ["this system uses this cache and this queue"])` →
  `"cache thi queue system"`). The list is now stemmed at the point of use, and
  a test asserts no entry's stem can leak, so a future addition ending in `-s`
  cannot silently reintroduce this.
- **Synonym expansion silently did nothing for a plural query** (§949): the
  built-in [`SynonymTable`] is written in the singular and lookups used the raw
  query token, so `ckos search schedulers` got *no* synonym expansion while
  `scheduler` got the full set — an arbitrary split, and one that disagreed
  with the retrieval tokenizer now that it stems both documents and queries.
  Reproduced directly (`"scheduler"` → `scheduler dispatch dispatcher`,
  `"schedulers"` → unchanged, `"caches"` → unchanged). Table keys and lookups
  now share the retrieval tokenizer's stemmer, so the two agree; the
  already-present check dedups on the stem too, so a synonym present in another
  inflection is not appended twice.
- **PageRank silently destroyed rank mass on edges to missing nodes**
  (§948/§951/§952): `pagerank_core` counted *every* adjacency entry in a node's
  out-degree, but delivered a share only when the target existed
  (`next.get_mut(&edge.to)`), so the share allocated to an edge pointing at a
  node not in the graph was neither delivered nor redistributed — it vanished
  on every iteration. The already-correct dangling-node path only catches nodes
  with *no* out-edges, so a **partially** dangling node slipped past it.
  Neither `connect` nor `GraphStore::load` validates edge endpoints (load
  replays `E` rows without checking the `N` rows arrived), so a hand-edited or
  partially corrupt `graph.kg` reaches this state, and `ckos run --session` /
  `ckos graph --session` load one every time. Reproduced: a two-node graph with
  one live edge and one edge to a missing id summed to **0.46**, not the
  documented ~1.0. Out-degree is now counted over live targets only, which both
  conserves mass and makes a node whose every edge is dead fall through to the
  existing dangling path — keeping the transition matrix column-stochastic over
  the nodes that exist (Langville & Meyer, *Google's PageRank and Beyond*).
  Personalized PageRank shares the core and was equally affected.
- **Hybrid search hid a matching document when two shared a title** (§950):
  `fuse_rrf` keyed fusion on the display title, but nothing makes a title
  unique. Two distinct documents with the same title collapsed into one result
  — the second vanished from the output entirely, and the survivor's score was
  inflated by absorbing its reciprocal-rank contribution. RRF (Cormack et al.
  2009) fuses rankings *of the same item*, and a title is not an item identity.
  Reproduced: a store with three matching documents, two sharing a title,
  returned only two hits, the merged one scoring roughly double. `Hit` now
  carries an `id` (the document id; `None` for graph nodes, which this codebase
  identifies by label, as `graph::versioning` also does) and fusion keys on it.
  Cross-source corroboration is preserved by a second pass that folds a graph
  entry into a store entry with the same name — but only when exactly one store
  entry carries that name, since a node matching two different documents
  corroborates neither in particular.
- **`RuntimeRegistry` listed one runtime twice after re-registration** (§900):
  `register` pushed to the `order` index unconditionally while
  `runtimes.insert` *replaced* the map entry, so the two fell out of sync when
  an id was registered again — and `list()`, the §900 runtime table the CLI and
  dashboard render, reported a single runtime as two. Nothing requires a
  `Runtime` to return a freshly generated id, so a stable id plus a config
  reload reaches this state. Reproduced (`order.len() == 2`,
  `runtimes.len() == 1`, `list().len() == 2`). `register` now extends `order`
  only when the id is new, keeping the order/map key sets equal — the same
  invariant `retrieval::SearchCache` already maintains between its order queue
  and entry map — while a repeat registration still replaces the runtime.
- **`ckos serve`'s `/api/search` cache could resurrect data a concurrent
  `/api/run` had just invalidated**: the cache-miss check, the retrieval
  computation, and the cache write were three separate lock acquisitions. A
  `run` racing in between — invalidating the cache, then mutating the
  session — could finish entirely while a `search` that started earlier was
  still computing from the pre-run state; that search's now-stale result was
  then written into the cache *after* the invalidation, undetected (still
  reporting itself as freshly computed). Reproduced deterministically with a
  test-only stall hook (compiled out of the real binary) before fixing.
  `AppState` now tracks a mutation generation per session directory, bumped
  by every invalidation; a search only writes to the cache if the generation
  is unchanged from when it started, so a session mutated mid-computation
  simply isn't cached rather than being cached wrong.
- **KQL `RELATED` had two correctness bugs**, both in `execute()`'s neighbour
  loop: (1) results were deduplicated by the *rendered* `NodeMatch` value
  (label/kind/confidence/date/provenance), not by node identity, so two
  distinct nodes sharing all of those (e.g. two different "Config"/"Utils"
  concepts with no date or provenance set — a realistic knowledge-graph
  shape) silently collapsed into one, dropping a genuine result; (2)
  `RELATED <kind> VIA <edge>` and plain `RELATED <kind>` disagreed on whether
  a node's own self-loop counts as "related to itself" — the VIA path
  (`graph::neighbors_via`) included it, the non-VIA path (`graph::traverse`)
  excluded it only as an incidental side effect of how it seeds its BFS
  visited-set, not a deliberate choice — so toggling `VIA` alone changed the
  result set for a graph shape (`connect` allows `from == to`) the module
  never claims to treat specially. Both reproduced directly before fixing;
  dedup is now by node id, and a self-loop is now excluded explicitly and
  consistently regardless of which underlying primitive is used.
- **`ckos verify -h`/`--help` ran the verifier over the literal text "-h"**:
  every other subcommand checks `wants_help` first; `cmd_verify` was the one
  handler that didn't, so `-h`/`--help` were treated as ordinary input to
  verify (both trivially pass every check) instead of printing usage.
- **`FileStore` corrupted two classes of metadata key on reload**, both in the
  `key: value` header codec (`Document.metadata` is a public map, and the
  module documents round-trip safety for "every `meta.*` key"): (1) a key
  containing the `": "` delimiter (e.g. `"ratio: a:b"`) made `deserialize`'s
  `split_once(": ")` cut inside the key, truncating it and prepending its tail
  to the value — keys now escape `:` as `\c`, leaving no bare colon in the
  on-disk key; (2) a key that itself begins with `meta.` (stored on disk as
  `meta.meta.x`) was decoded with `trim_start_matches("meta.")`, which strips
  the prefix *repeatedly* and yielded `x` instead of `meta.x` — now
  `strip_prefix`, which removes exactly one. Both reproduced directly before
  fixing; value escaping (and human-readable colons in values) is unchanged.
- **Graph search matched query terms as bare substrings of node labels**
  (§951): `retrieval::graph_hits` scored a node whenever a query term appeared
  anywhere inside its label, so `"art"` matched an unrelated node `"Bart"` and
  short terms like `"in"`/`"os"` matched almost everything — false graph hits
  then fused into the results, inconsistent with `keyword_hits`, which matches
  whole tokens. Graph label matching is now token-exact (same `tokens`/`tf`
  helpers the keyword path uses), so `"art"` no longer matches `"Bart"` while
  every genuine whole-token match still lands. Reproduced directly before
  fixing; the now-unused substring helper was removed.
- **`ckos serve`'s request-size cap didn't bound the request line or headers**
  (§902): `http::parse` read the request line and each header with
  `read_line`, which buffers a whole line before returning — so a single line
  with no terminator allocated unbounded memory *before* any size check, even
  though the module documents the per-request cap as a memory-exhaustion
  guard covering "request line + headers + body". Each line read is now capped
  at the remaining budget (`take(budget)`), and a line that hits the cap
  without a terminator is rejected as `413`. Reproduced with a test that
  hangs to the read timeout under the old code (unbounded buffering) and is
  rejected immediately under the fix.
- **`ArithmeticCheck` rejected correct output containing sub-expressions**
  (§899): `match_equation` evaluated any `A op B = C` it found, even when the
  operand was really a term of a larger expression — so `-5 + 3 = -2` was
  misread as `5 + 3 = -2` (unary minus ignored), `1 + 2 * 3 = 7` was rejected
  via the fragment `2 * 3 = 7` (precedence), and `2 + 2 = 5 - 1` was rejected
  by reading the result as the bare `5`. All three are correct output. A digit
  preceded by an arithmetic operator is no longer treated as an equation
  start, and a result that continues into another operator+operand makes the
  equation non-evaluable — the same "skip what can't be judged whole rather
  than raise a false positive" rule as the grouped-number fix. A prose hyphen
  after the result (`= 5 - obviously wrong`) still evaluates, so detection of
  genuinely wrong equations is unchanged. Reproduced before fixing.
- **`web::json` serialized non-finite numbers as invalid JSON** (§902): a
  `Json::Number` holding `NaN`/`±∞` rendered via `f64`'s `Display` as the bare
  literals `NaN`/`inf`/`-inf`, none of which `JSON.parse` accepts — a single
  such value would make an entire API response unparseable, violating the
  serializer's documented RFC 8259 contract. Non-finite numbers now render as
  `null`; finite values are unchanged. (No current route produces a non-finite
  number — search scores are finite and a `NaN` cosine is filtered before it
  becomes a hit — but the serializer is a reusable primitive and must never
  emit invalid JSON.)

- **Message signing was forgeable without the key** (§930): `security::sign`
  computed `FNV(data).rotate_left(17) ^ K`, where `K` depended only on the
  key — so `K` cancelled in the XOR of any two signatures. An attacker who
  observed a **single** signed envelope could compute the valid signature for
  **any** message of their choosing, with no key recovery and no brute force
  (`forged = observed_sig ^ FNV(observed).rot17 ^ FNV(target).rot17`). The
  module documented the primitive as "not cryptographically strong", which
  understates a total break: it gave no protection against exactly the
  tampering adversary §930 names. Signing is now **HMAC-SHA256** over a new
  dependency-free `sdk::crypto` (SHA-256 per FIPS 180-4, HMAC per RFC 2104,
  both checked against the published FIPS/RFC 4231 test vectors), envelopes
  carry a full 32-byte tag, and verification compares in constant time so a
  wrong tag leaks nothing through timing. `Signer`/`ReplayGuard` also gained
  `with_key_bytes` for arbitrary-length key material, since a `u64` secret is
  only 64 bits of entropy however strong the MAC is. The forgery was
  reproduced directly before fixing and is now a regression test.

- **`gc` deleted a nondeterministic choice of duplicate** (§954):
  `maintenance::collect` consumed the backend's `search` order, but both
  in-tree backends iterate a `HashMap`, so *which* copy of a
  (type, title, body) group survived varied between runs. Documents sharing
  those three fields can still differ in id, confidence, metadata and
  embedding, so `ckos gc` could silently discard the higher-confidence copy —
  a different one each time. Reproduced directly (both confidences observed as
  survivors across 200 freshly built stores). `collect` now orders documents
  by confidence descending, with the document id as a stable tie-break, so the
  best copy is kept and the run is reproducible.

### Removed

- **The last two inert subsystems deleted**, closing the requirement question
  step 1 opened. Every dormant part was put to one test: *can a consumer use
  this as-is to accomplish something real?*
  - `MemoryTier` (§896) — a six-level hierarchy enum that nothing stored or
    read. `Document` has no tier field and no code path routes by tier, so
    using it accomplished nothing; identical in kind to the already-deleted
    `PluginKind`.
  - `ResourceProbe` / `NullProbe` / `ResourceSnapshot` (§904) — a sampling seam
    whose `sample()` is never called anywhere. Implementing the trait produced
    a snapshot no consumer reads.

  `GraphRepo` (§942/§943) and `ServiceMesh` (§912) **pass** that test and are
  kept: graph branch/merge and multi-agent routing each work standalone,
  in-process, for a library consumer today. They are SDK-level with no CLI
  surface by decision, now recorded in the handbook — `GraphRepo` in particular
  has no on-disk format, so a CLI would first need repo persistence rather than
  a command that silently loses branches between invocations.


- **Two redundant functions deleted**, each superseded by a better mechanism
  already in use — found by re-measuring reachability transitively rather than
  by direct grep:
  - `reflection::store_reflection` — zero production callers. `Session::
    record_reflections` already persists reflections *and* stamps them with the
    session key and the sequence number recency ranking needs, so this was a
    strictly weaker duplicate. Its test went with it; `record_reflections` is
    covered by both a unit test and the end-to-end test, so no real behaviour
    lost coverage.
  - `Retriever::search_synonyms` — zero production callers. The CLI composes
    `expand_query_with_synonyms` with `search` precisely so `--synonyms` stacks
    with `--expand` and `--diverse`; the all-in-one wrapper cannot compose, so
    it was the worse of the two designs.


- **Dead scaffolding deleted** (Musk-algorithm step 2 — delete before you
  optimise, because the most common error is refining a part that should not
  exist). Each removal was measured, not guessed: a workspace scan for public
  items with zero references outside their own definition and test module.
  - `Event::topic()` — zero callers anywhere, tests included. §933 is satisfied
    by a metrics sink, not by an unused label accessor.
  - `PluginKind` — a §901 category enum that categorised nothing: no field
    stored it, no function took or returned it. Add it back when a `Tool`
    actually needs classifying.
  - `Dag::roots()` — called only by its own assertions, i.e. a test justifying
    the existence of the thing it tests. The engine orders steps with
    `topological_order`.

  Kept deliberately: `crypto::to_hex`, which is test-only today but has a named
  near-term consumer — a 32-byte signature has to be rendered somehow to cross
  the §902 JSON API.

### Added

- **`scripts/check-status-doc.sh` — the §-by-§ coverage claim is now
  machine-checked**, and runs as part of `./scripts/check.sh`.
  `docs/implementation-status.md` maps every spec section to the code
  satisfying it: the product's central claim. Deleting the inert parts left
  **four rows citing symbols that no longer existed** (`MemoryTier`,
  `ResourceProbe`, `Event::topic()`, `search_synonyms`), plus a stale
  `store_reflection` reference in `docs/spec-v2.6.md` and two contradictory
  handbook entries — the table asserted coverage the code no longer provided.
  All are corrected, but fixing instances does not stop the class: the script
  extracts every backticked code symbol from the table and fails the gate if
  one no longer resolves anywhere in the sources or file tree. Verified both
  ways — passes on the corrected docs (86 symbols), and fails with exit 1 when
  a row is edited to cite something nonexistent. A deliberately historical
  mention (the §904 row records that the `ResourceProbe` seam *was removed*)
  goes in an `ALLOWED` map that demands a written reason.


- **`scripts/install-hooks.sh` + `.githooks/pre-commit` — the gate now runs
  itself** (Musk step 5, automate — deliberately last: the checks were
  questioned, the dead parts deleted, and four commands collapsed into one
  before anything was wired to run automatically, because automating the
  earlier messier version would have entrenched it). A manual step that must
  happen *every single time* is one that eventually does not. `core.hooksPath`
  points at the version-controlled `.githooks/`, so one command gives every
  clone the same hook. Verified by attempting a commit with a deliberately
  misformatted file: the commit was refused and `HEAD` did not move.
  `git commit --no-verify` remains the escape hatch for a genuine
  work-in-progress commit.


- **`scripts/check.sh` — one command for every gate** (Musk step 4, accelerate
  cycle time; step 5, automate, comes only after). The four gates were retyped
  as a long shell chain on every change: slow, easy to abbreviate under
  pressure, and impossible for CI and a contributor to share. The script runs
  format → clippy `-D warnings` → rustdoc `-D warnings` → tests, stops at the
  first failure and names it, and prints the passing-test total. `--fix`
  reformats first. Exit status verified 0 when green and 1 when a gate breaks.
  The staged CI (`docs/ci-workflow.yml`) now calls the same script instead of
  its own command list — it had **drifted weaker**, skipping the rustdoc gate
  entirely and running clippy without `-D warnings` — and its smoke test now
  also exercises the new `ckos index` path end to end.


- **`ckos index <dir> <file…>` — the §938 ingest path, now reachable.** Musk
  step 1 (question the requirement) applied to spec compliance surfaced the
  real gap: several subsystems had *no path from any product entry point*, so
  the §889–§962 claim was being met in name only.
  (**Correction:** the first measurement said "nine" by grepping `cli/` and
  `web/` for direct references only. That misses reachability *through* the
  SDK — `rank_memories` was already live via `Session::recall`, which
  `ckos history <dir> <query>` calls. Re-measured transitively, the accurate
  count of subsystems with no entry point was seven, three of which this
  command now connects.) `ckos run --session`
  could record what a run produced, but nothing could take an existing corpus
  *in*.

  One command turns five of those dormant pieces into user-visible behaviour:
  it chunks each file into retrievable passages (§939 recursive chunking with
  overlap), stores every passage embedded, feeds the text through
  `KnowledgeBus::ingest_text` so extracted concepts announce themselves (§923 →
  §941), and drains the `ReindexQueue` through `Reindexer` so each new node
  becomes an embedded `graph_node` document (§938). After indexing, `ckos
  search` reaches passages *and* concepts — verified end to end.

  Re-indexing a file **replaces** its passages rather than storing a second
  copy: `Document::new` mints a fresh id per call, so the first cut of this
  command duplicated every passage on a second run — the same defect class
  `Reindexer` was fixed for, caught here by testing idempotence before
  committing. The graph accumulates (nodes reinforced, never cloned or
  clobbered), matching `ckos run --session`.

  `KnowledgeBus::from_graph` was added because the command genuinely needs it:
  ingest must start from the graph already on disk, not an empty one.


- **Known-answer tests pinning both FNV-1a hashes to the published vectors**:
  `kernel::audit::content_hash` (64-bit) and `memory::embedding`'s bucket hash
  (32-bit) were covered only by same-build determinism checks, which cannot
  catch a changed constant. That matters most for the audit trail, whose §903
  claim is that a recorded hash proves what was processed without retaining the
  payload — a claim that requires stability *across* builds, not within one
  run. Both now assert the published Fowler–Noll–Vo reference values (verified
  to match before pinning), applying the same "check against the standard, not
  against ourselves" rule already used for SHA-256/HMAC in `sdk::crypto`.

- **Average precision / MAP in the eval harness** (§959): `sdk::eval` claimed
  the standard IR metric set but omitted average precision — the canonical
  TREC summary metric (Manning, Raghavan & Schütze, *IIR* §8.4). P@k and
  recall@k cannot distinguish a run that ranks the relevant items first from
  one that ranks them last, and reciprocal rank only ever looks at the first
  hit, so nothing in-tree scored *ordering across all* relevant items.
  `average_precision` follows the TREC convention (divide by the total
  relevant count, so items never retrieved contribute 0 instead of being
  silently excluded) and ignores repeated ids so a run cannot inflate its own
  score by listing a document twice; `mean_average_precision` averages it
  across queries. Reported by `ckos eval` as `MAP` and carried on
  `EvalScores`. Verified against hand-computed textbook values.

- **`sdk::crypto`** — dependency-free SHA-256 (FIPS 180-4), HMAC-SHA256
  (RFC 2104) and a constant-time comparison, verified against the published
  standard test vectors. Backs §930 message signing.

- **`docs/agent-handbook.md`**: a self-contained working handbook for an AI
  coding agent continuing development — the audit → reproduce → fix →
  regression-test → full-gates discipline and standing principles; the
  audited-clean module list; the fixed bug *classes* with commit refs;
  deliberately-parked gaps with the condition for lifting each; and
  priority-ordered improvement proposals (with measured anti-proposals).

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

286 tests passing (was 216); fmt, clippy `-D warnings`, and rustdoc
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
