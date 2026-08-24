#!/usr/bin/env bash
# Run the exact command sequence README.md tells a new user to run, against a
# release build, and assert each one produces what the README says it does.
#
# Why this exists: every gate in ./scripts/check.sh passed while `ckos index`
# recorded no provenance, so the README's own quickstart —
#   ckos index ./sess paper.md
#   ckos kql --session ./sess 'FIND Concept "…" RETURN Graph + Sources'
# — answered `src=<unknown>` for everything it had just loaded. Unit and
# integration tests each verified their own layer; nothing verified the
# documented user path end to end, which is where the gap lived. That bug was
# found by hand from a clean clone. This script is that check, automated, so
# the class cannot come back silently.
#
# Deliberately separate from check.sh: this builds --release and runs a real
# server, which is far slower than the per-commit gate should be. Run it
# before a release (docs/releasing.md) or after touching the CLI, the web
# routes, or the README's documented commands.
#
#   ./scripts/verify-quickstart.sh
set -euo pipefail
cd "$(dirname "$0")/.."

bold() { printf '\n\033[1m==> %s\033[0m\n' "$1"; }
fail() { printf '\033[31mFAILED: %s\033[0m\n' "$1" >&2; exit 1; }

# `expect <haystack> <needle> <what>` — assert and say what was actually got.
expect() { case "$1" in *"$2"*) ;; *) printf 'got: %s\n' "$1" >&2; fail "$3";; esac; }
reject() { case "$1" in *"$2"*) printf 'got: %s\n' "$1" >&2; fail "$3";; esac; }

bold "build (release)"
cargo build --release -q
CKOS="$PWD/target/release/ckos"

WORK="$(mktemp -d)"
SERVER_PID=""
cleanup() {
  [ -n "$SERVER_PID" ] && kill "$SERVER_PID" 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

cat > "$WORK/paper.md" <<'DOC'
# Attention Is All You Need

The Transformer is a model architecture relying entirely on an attention
mechanism. Vaswani introduced the Transformer in 2017, built at Google Brain.
It dispenses with recurrence and convolutions entirely.

Self-attention relates different positions of a single sequence to compute a
representation of that sequence. The Transformer depends on multi-head
attention.
DOC

cd "$WORK"
SESS="$WORK/sess"

bold "CLI quickstart (README 'Try it')"

out=$("$CKOS" index "$SESS" paper.md)
expect "$out" "passage(s)" "ckos index should report passages"
expect "$out" "new concept(s)" "ckos index should report concepts"

out=$("$CKOS" run --session "$SESS" "research the Transformer paper by Vaswani")
expect "$out" "session saved" "ckos run --session should persist the session"
expect "$out" "graph updated" "ckos run --session should grow the graph"

out=$("$CKOS" search "$SESS" "Transformer")
expect "$out" "hit(s)" "ckos search should return hits"
expect "$out" "paper.md#" "search must reach an indexed passage, not just concepts"

# The documented provenance query. This is the assertion the shipped bug broke:
# every indexed concept came back unsourced.
out=$("$CKOS" kql --session "$SESS" 'FIND Concept "Transformer" RETURN Graph + Sources')
expect "$out" "Transformer" "kql should find the indexed concept"
reject "$out" "<unknown>" "an indexed concept must carry its source file"

out=$("$CKOS" history "$SESS" Transformer)
expect "$out" "recalled" "ckos history <dir> <query> should recall, not dump"

out=$("$CKOS" plan --dot "research X")
expect "$out" "digraph workflow {" "ckos plan --dot should emit Graphviz"

out=$("$CKOS" gc "$SESS")
expect "$out" "garbage-collected" "ckos gc should report what it collected"

# gc must not break the session it just swept.
out=$("$CKOS" search "$SESS" "Transformer")
expect "$out" "hit(s)" "search must still work after gc"

bold "web gateway (README 'Browser dashboard')"

PORT=$(( 18000 + RANDOM % 2000 ))
mkdir -p "$WORK/root"
"$CKOS" serve --port "$PORT" --session-root "$WORK/root" > "$WORK/serve.log" 2>&1 &
SERVER_PID=$!

for _ in $(seq 1 50); do
  curl -sf -o /dev/null "http://127.0.0.1:$PORT/" && break
  sleep 0.1
done
curl -sf -o /dev/null "http://127.0.0.1:$PORT/" || { cat "$WORK/serve.log"; fail "server did not come up"; }

B="http://127.0.0.1:$PORT"
expect "$(curl -s "$B/")" "CKOS" "dashboard should render"
expect "$(curl -s "$B/api/capabilities")" '"capabilities":[' "/api/capabilities"
expect "$(curl -s "$B/api/runtimes")" '"runtimes":[' "/api/runtimes"
expect "$(curl -s "$B/api/status")" "audit_records" "/api/status"
expect "$(curl -s "$B/api/plan?intent=research%20X")" '"steps":[' "/api/plan"
expect "$(curl -s -X POST --data 'intent=study+the+Transformer&session=s' "$B/api/run")" \
  '"results":[' "POST /api/run"
expect "$(curl -s "$B/api/search?session=s&q=Transformer")" '"hits":[' "/api/search"
expect "$(curl -s "$B/api/history?session=s")" '"items":[' "/api/history"
expect "$(curl -s "$B/api/graph?session=s")" '"nodes":[' "/api/graph"
expect "$(curl -s "$B/api/verify?text=hello")" '"checks":[' "/api/verify"
# --data-urlencode, not --data: a raw `+` in a form body decodes to a space,
# which would send `RETURN Graph   Sources` and fail as a parse error that
# looks like a product bug but is the caller's encoding mistake.
expect "$(curl -s -X POST --data-urlencode 'query=FIND Concept "Transformer" RETURN Graph + Sources' \
  --data 'session=s' "$B/api/kql")" '"primary":[' "POST /api/kql"

# The session-confinement boundary, from outside: a request must not be able
# to name a path outside the server's session root.
code=$(curl -s -o /dev/null -w '%{http_code}' -X POST --data "intent=x&session=$WORK/escapee" "$B/api/run")
[ "$code" = "400" ] || fail "an absolute session path must be rejected (got HTTP $code)"
[ ! -e "$WORK/escapee" ] || fail "a rejected session must not create anything outside the root"

printf '\n\033[32mQuickstart verified: every documented command and route behaves as README.md says.\033[0m\n'
