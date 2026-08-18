#!/usr/bin/env bash
# Verify that every code symbol cited by docs/implementation-status.md still
# exists in the repository.
#
# That document is the product's central claim: a §-by-§ map from the spec to
# the code satisfying it. A hand-maintained map drifts silently — deleting an
# inert part left four separate entries citing symbols that no longer existed,
# so the table asserted coverage the code no longer provided. Fixing those
# instances is not enough; this removes the possibility of the class recurring
# by making the claim machine-checked.
#
# Run standalone, or via ./scripts/check.sh which includes it.
set -euo pipefail
cd "$(dirname "$0")/.."

exec python3 - "$@" <<'PY'
import re, pathlib, sys

DOC = pathlib.Path("docs/implementation-status.md")

# Symbols allowed to be absent, each with the reason. Keep this short: an entry
# here is a claim that the mention is deliberately historical, not a live one.
ALLOWED = {
    "ResourceProbe": "named only to record that the seam was removed (§904 row)",
}

repo = pathlib.Path(".")
def readable(p):
    try:
        return p.read_text(errors="ignore")
    except OSError:
        return ""

sources = "\n".join(
    readable(p)
    for p in repo.rglob("*")
    if p.is_file()
    and "target" not in p.parts
    and ".git" not in p.parts
    and p.suffix in {".rs", ".html", ".toml", ".yaml", ".yml"}
)
# File and directory names count too: the table cites Dockerfile, deploy/k8s/…
names = {p.name for p in repo.rglob("*") if "target" not in p.parts and ".git" not in p.parts}

doc = DOC.read_text()
cited = set()
for tok in re.findall(r"`([^`]+)`", doc):
    t = tok.strip()
    # Skip CLI invocations, HTTP routes, flags and spec references — prose, not symbols.
    if t.startswith(("ckos ", "GET ", "POST ", "PUT ", "DELETE ", "--", "/api", "§")):
        continue
    # A backticked path is a file reference, not a symbol. Without this, the
    # stem of a filename gets extracted and looked up as code: `CHANGELOG.md`
    # was reported as a missing symbol `CHANGELOG`. (`README.md` escaped only
    # because the bare word "README" happens to appear in a doc comment — so
    # this was a live false-positive class, not a one-off.)
    if t in names or pathlib.Path(t).name in names:
        continue
    for a, b, c, d in re.findall(
        r"\b([A-Za-z_][A-Za-z0-9_]*)::([A-Za-z_][A-Za-z0-9_]*)"
        r"|\b([A-Z][A-Za-z0-9_]{2,})\b"
        r"|\b([a-z_][a-z0-9_]{3,})\(\)",
        t,
    ):
        name = b or c or d
        if name:
            cited.add(name)

missing = sorted(
    n for n in cited
    if n not in ALLOWED
    and n not in names
    and not re.search(r"\b" + re.escape(n) + r"\b", sources)
)

if missing:
    print("docs/implementation-status.md cites symbols that no longer exist:", file=sys.stderr)
    for m in missing:
        print(f"  {m}", file=sys.stderr)
    print(
        "\nEither restore the symbol, correct the row to describe what actually\n"
        "satisfies that section, or — if the mention is deliberately historical —\n"
        "add it to ALLOWED in scripts/check-status-doc.sh with the reason.",
        file=sys.stderr,
    )
    sys.exit(1)

print(f"implementation-status.md: {len(cited)} cited symbols all present.")
PY
