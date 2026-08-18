#!/usr/bin/env bash
# Run every gate CKOS requires green before a commit, in one command.
#
# The gates were previously retyped as a long shell one-liner on every
# change, which is slow, easy to abbreviate under time pressure, and impossible
# for CI and a contributor to share. One script means the local loop, the
# staged CI workflow (docs/ci-workflow.yml) and a new contributor all run
# exactly the same checks.
#
#   ./scripts/check.sh          # all gates
#   ./scripts/check.sh --fix    # format in place first, then all gates
#
# Exits non-zero on the first gate that fails, naming it.
set -euo pipefail

cd "$(dirname "$0")/.."

FIX=0
for arg in "$@"; do
  case "$arg" in
    --fix) FIX=1 ;;
    -h|--help) sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $arg (try --help)" >&2; exit 2 ;;
  esac
done

step() { printf '\n\033[1m==> %s\033[0m\n' "$1"; }
fail() { printf '\n\033[31mFAILED: %s\033[0m\n' "$1" >&2; exit 1; }

if [ "$FIX" -eq 1 ]; then
  step "cargo fmt --all (writing)"
  cargo fmt --all
fi

step "format"
cargo fmt --all -- --check || fail "formatting (run ./scripts/check.sh --fix)"

step "clippy (-D warnings)"
RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets || fail "clippy"

step "rustdoc (-D warnings)"
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps || fail "rustdoc"

step "status-doc symbols"
./scripts/check-status-doc.sh || fail "docs/implementation-status.md cites a symbol that no longer exists"

step "tests"
# Capture so the total can be reported; stream it too, so a hang is visible.
out=$(mktemp)
trap 'rm -f "$out"' EXIT
cargo test --workspace 2>&1 | tee "$out" || fail "tests"
# Explicit `if`: `grep -q ... && fail` leaves the script's exit status
# riding on grep's, which is 1 in the *good* case — confusing under `set -e`.
if grep -q "FAILED" "$out"; then fail "tests"; fi

total=$(grep -E '^test result: ok' "$out" \
  | awk -F'[.] ' '{print $2}' | awk '{s+=$1} END {print s+0}')

printf '\n\033[32mAll gates green — %s tests passing.\033[0m\n' "$total"
