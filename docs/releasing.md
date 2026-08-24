# Releasing CKOS

Everything below is deliberately owner-only: automation in this repository
cannot push tags, create releases, touch `.github/workflows/`, or change
repository settings (verified — each channel is permission-denied). The
working branch prepares a release; a human publishes it. That split is the
approval gate: nothing becomes a release until you act.

The release itself is already prepared. `Cargo.toml` carries the version,
`CHANGELOG.md` has the dated section, and every commit on `main` passed the
five gates (`./scripts/check.sh`: format, clippy `-D warnings`, rustdoc
`-D warnings`, status-doc symbol check, full test suite).

## One-time setup (first release only)

1. **Activate CI.** Copy the staged workflow into place — automation cannot:

   ```sh
   git checkout main && git pull
   mkdir -p .github/workflows
   cp docs/ci-workflow.yml .github/workflows/ci.yml
   git add .github/workflows/ci.yml
   git commit -m "Activate CI from the staged workflow"
   git push
   ```

2. **Make `main` the default branch** (Settings → General → Default branch),
   so the repository landing page shows the released state.

## Every release

1. Confirm the tree is green at the release commit:

   ```sh
   ./scripts/check.sh
   ```

2. Tag the version that `Cargo.toml` declares, and push the tag:

   ```sh
   V=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
   git tag -a "v$V" -m "CKOS v$V"
   git push origin "v$V"
   ```

3. Create the GitHub release from that tag, using the matching `CHANGELOG.md`
   section as the body:

   ```sh
   gh release create "v$V" --title "CKOS v$V" --notes-file <(
     awk "/^## \\[$V\\]/{f=1;next} /^## \\[/{f=0} f" CHANGELOG.md
   )
   ```

No binaries are attached: CKOS is `std`-only and builds from source anywhere
with a Rust toolchain (`cargo build --release`), which is the distribution
story — there is no artifact whose provenance would need attesting.

## After releasing

Open the next cycle by adding a fresh `## [Unreleased]` section at the top of
`CHANGELOG.md` if the release consumed it.
