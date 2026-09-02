# Releasing CKOS

Everything below is deliberately owner-only: automation in this repository
cannot push tags, create releases, touch `.github/workflows/`, or change
repository settings (verified — each channel is permission-denied). The
working branch prepares a release; a human publishes it. That split is the
approval gate: nothing becomes a release until you act.

Every commit on `main` passed the gates (`./scripts/check.sh`: format, clippy
`-D warnings`, rustdoc `-D warnings`, status-doc symbols, deploy manifests,
full test suite).

**A release is not automatically ready to tag.** Work lands under
`## [Unreleased]` in `CHANGELOG.md`, and step 3 below builds the release body by
extracting the `## [<version>]` section — so tagging while entries are still
stranded under `[Unreleased]` publishes notes that *omit exactly the work being
released*. Step 1 closes that gap and `./scripts/check-release-ready.sh`
enforces it.

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

1. **Settle the version and close the changelog.** Decide what this release is
   (patch/minor/major) from what accumulated under `## [Unreleased]`, set that
   version in the workspace `Cargo.toml` if it differs, then rename the
   `## [Unreleased]` heading to `## [X.Y.Z] — YYYY-MM-DD`. Commit it.

   ```sh
   ./scripts/check-release-ready.sh
   ```

   This refuses to proceed unless the version in `Cargo.toml` has a **dated**
   changelog section with entries, nothing is left under `[Unreleased]`, and the
   tag does not already exist. It is not in `check.sh`, because `[Unreleased]`
   is supposed to have entries during ordinary development — it is a release-time
   check only.

2. Confirm the tree is green at the release commit, and that the documented
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

3. Tag the version that `Cargo.toml` declares, and push the tag:

   ```sh
   V=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
   git tag -a "v$V" -m "CKOS v$V"
   git push origin "v$V"
   ```

4. Create the GitHub release from that tag, using the matching `CHANGELOG.md`
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
`CHANGELOG.md`, since step 1 consumed the previous one.
