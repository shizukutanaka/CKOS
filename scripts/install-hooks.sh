#!/usr/bin/env bash
# Automate the gate so it cannot be skipped by accident (Musk step 5 — and only
# after steps 1–4: the checks were questioned, the dead parts deleted, and the
# four commands collapsed into one before anything was wired to run by itself).
#
#   ./scripts/install-hooks.sh
#
# Points git at .githooks/, whose pre-commit hook runs ./scripts/check.sh. This
# uses `core.hooksPath` rather than copying into .git/hooks, so the hook is
# version-controlled and every clone gets the same one from a single command.
#
# Escape hatch, for the cases that genuinely warrant it (a work-in-progress
# commit on a scratch branch):
#
#   git commit --no-verify
set -euo pipefail
cd "$(dirname "$0")/.."

git config core.hooksPath .githooks
printf 'pre-commit hook installed: ./scripts/check.sh runs before every commit.\n'
printf 'Bypass a single commit with `git commit --no-verify`; undo with\n'
printf '`git config --unset core.hooksPath`.\n'
