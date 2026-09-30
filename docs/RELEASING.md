# Releasing

This project uses **Semantic Versioning** driven by **Conventional Commits**. The version
lives in [`crates/sampa-native/Cargo.toml`](../crates/sampa-native/Cargo.toml); the changelog
is [`CHANGELOG.md`](../CHANGELOG.md), where [git-cliff](https://git-cliff.org/) (configured by
[`cliff.toml`](../cliff.toml)) drafts each release's commit list and we **hand-curate** it —
adding a prose lead and tidying the entries — before committing.

## Why git-cliff, not release-plz

Historically `sampa-native` consumed the sibling `sampa_graphics_terminal` crates via **path
deps** (per ADR 0002, for local N0 development), so a cargo-based release tool like `release-plz`
— which runs `cargo` in CI where those paths don't exist — couldn't resolve the workspace.
**git-cliff reads only git history** — no cargo — so it worked regardless. The core crates are
now **all git-pinned** (the ADR 0002 path→git migration), so cargo *does* resolve in CI and
release-plz is technically an option; we keep git-cliff as the established, dependency-light
choice (it stays correct even while `publish = false`).

The crate is also marked `publish = false`: git dependencies can't be published to crates.io.

## Cut a release

1. **Pick the version.** From the unreleased commits since the last tag:
   ```bash
   git-cliff --bumped-version        # prints the next semver (feat → minor, fix → patch)
   ```
   `feat:` → minor, `fix:`/`perf:` → patch, a `!`/`BREAKING CHANGE` → major (pre-1.0: minor).

2. **Bump `Cargo.toml`** to that version (`crates/sampa-native/Cargo.toml`).

3. **Draft and curate the new changelog entry.** Generate **only** the unreleased section and
   prepend it above the latest entry — do **not** run `git-cliff -o CHANGELOG.md`, which rewrites
   the *whole* file and clobbers the hand-curated prose leads of past releases (and re-flows the
   header):
   ```bash
   git-cliff --unreleased --tag vX.Y.Z --strip header   # prints just the new ## [X.Y.Z] section
   ```
   Paste that section under the intro, then curate it in house style (see any prior entry):

   - open with a short **prose lead** naming the headline features;
   - keep a `### Features` list (and `### Bug Fixes` / `### Testing` / etc. when they apply),
     trimming noise and merging closely-related commits into one readable line.

   Then bump the lockfile and commit both on a branch → PR → merge to `main`:
   ```bash
   cargo build -p sampa-native   # refreshes Cargo.lock with the new version
   git add CHANGELOG.md Cargo.lock crates/sampa-native/Cargo.toml
   git commit -m "chore(release): X.Y.Z"
   ```
   (The pushed tag's **GitHub Release notes** are still generated fresh by git-cliff in CI, so the
   curation is only for the committed `CHANGELOG.md`.)

4. **Tag the merge commit and push the tag:**
   ```bash
   git tag -a vX.Y.Z -m "vX.Y.Z"
   git push origin vX.Y.Z
   ```

Pushing the `vX.Y.Z` tag triggers [`.github/workflows/release.yml`](../.github/workflows/release.yml),
which regenerates that tag's notes with git-cliff and publishes a **GitHub Release**.

## Install git-cliff

```bash
cargo install git-cliff        # or: cargo binstall git-cliff
```
