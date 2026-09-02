#!/usr/bin/env bash
# Release-time gate: refuse to tag a version whose notes would be wrong.
#
# `docs/releasing.md` builds the GitHub release body by extracting the
# `## [<version>]` section from CHANGELOG.md. That silently produces the *wrong*
# notes whenever work has landed since the last dated section: the tag would
# carry the new code while the release page described the previous version.
# Caught in exactly that state — 26 entries sat under `## [Unreleased]` while
# `Cargo.toml` still read 2.8.0, so the documented command would have published
# a release describing none of them.
#
# Deliberately NOT part of `scripts/check.sh`: `## [Unreleased]` is *supposed*
# to have entries during development, so this would fail every ordinary commit.
# It runs once, at release time, from docs/releasing.md.
set -euo pipefail
cd "$(dirname "$0")/.."

bold() { printf '\033[1m%s\033[0m\n' "$1"; }
fail() { printf '\033[31mFAILED: %s\033[0m\n' "$1"; exit 1; }

version=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
[ -n "$version" ] || fail "could not read the version from Cargo.toml"
bold "==> release readiness for v$version"

# 1. Unreleased work must have been folded into a dated section first.
unreleased=$(awk '/^## \[Unreleased\]/{f=1;next} /^## \[/{f=0} f' CHANGELOG.md | grep -c '^- ' || true)
if [ "$unreleased" -gt 0 ]; then
  fail "CHANGELOG.md still has $unreleased entr(ies) under [Unreleased].
       Rename that heading to '## [$version] — $(date +%F)' (bumping the version
       in Cargo.toml first if this release warrants it), then re-run.
       Tagging now would publish release notes that omit those entries."
fi

# 2. The version being tagged must actually have notes.
notes=$(awk "/^## \\[$version\\]/{f=1;next} /^## \\[/{f=0} f" CHANGELOG.md | grep -c '^- ' || true)
[ "$notes" -gt 0 ] || fail "CHANGELOG.md has no entries under '## [$version]' — the release body would be empty"

# 3. That section must be dated, since the release page shows it verbatim.
grep -qE "^## \[$version\] — [0-9]{4}-[0-9]{2}-[0-9]{2}" CHANGELOG.md \
  || fail "the '## [$version]' heading carries no ISO date"

# 4. The tag must not already exist.
if git rev-parse -q --verify "refs/tags/v$version" >/dev/null; then
  fail "tag v$version already exists locally"
fi

printf '\033[32mReady: v%s has %s dated changelog entr(ies) and nothing stranded under [Unreleased].\033[0m\n' "$version" "$notes"
