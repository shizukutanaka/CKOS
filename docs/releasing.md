# Releasing CKOS

Everything below is deliberately owner-only: automation in this repository
cannot push tags, create releases, touch `.github/workflows/`, or change
repository settings (verified — each channel is permission-denied). The
working branch prepares a release; a human publishes it. That split is the
approval gate: nothing becomes a release until you act.

The release itself is already prepared. `Cargo.toml` carries the version,
`CHANGELOG.md` has the dated section, and every commit on `main` passed the
gates (`./scripts/check.sh`: format, clippy `-D warnings`, rustdoc
`-D warnings`, status-doc symbols, deploy manifests, full test suite).

**Note what is already true, so nothing below reads as a blocker it isn't.**
The repository is public, its default branch is `main`, and `main` builds and
passes the documented quickstart from a clean clone. CKOS is distributed as
source, so *the software is already available to anyone*:

```sh
git clone https://github.com/shizukutanaka/CKOS.git
cd CKOS && cargo install --path cli   # verified: yields `ckos 2.8.0`
```

What the steps below add is a **named, citable version** — a tag, a release
page and its notes — plus CI. Those are discoverability and process, not the
delivery mechanism.

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

2. ~~Make `main` the default branch~~ — **already done.** Verified:
   `git ls-remote --symref origin HEAD` reports `refs/heads/main`, and the
   repository metadata reads `"default_branch": "main"`. Kept here, struck
   through rather than deleted, because earlier revisions of this file listed
   it as outstanding and someone following those notes would otherwise go
   looking for a setting that needs no change.

## Every release

1. Confirm the tree is green at the release commit, and that the documented
   user path actually works:

   ```sh
   ./scripts/check.sh              # the per-commit gate
   ./scripts/verify-quickstart.sh  # every README command + route, release build
   ```

   The second one is not redundant. Every gate in `check.sh` passed while
   `ckos index` silently recorded no provenance, because unit and integration
   tests each verified their own layer and nothing verified the documented
   path end to end. It is kept out of `check.sh` because it builds `--release`
   and starts a real server.

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
