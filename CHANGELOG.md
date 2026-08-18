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

### Fixed

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

282 tests passing (was 216); fmt, clippy `-D warnings`, and rustdoc
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
