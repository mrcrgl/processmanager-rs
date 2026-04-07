# Release Process

This document defines the canonical release flow for `mrcrgl/processmanager-rs`.

## Prerequisites
- GitHub access with tag/release permissions
- crates.io publish token configured as `CARGO_REGISTRY_TOKEN` in repository secrets
- Clean working tree

## 1. Open a tracked release issue
Use the `Release` issue template and fill target version/date.

## 2. Prepare release branch
Follow branch policy from `AGENTS.md`:

```bash
git checkout main
git pull --ff-only origin main
git checkout -b chore/<issue-id>-release-vX-Y-Z
```

## 3. Prepare release PR
- Set `Cargo.toml` package version to the target `X.Y.Z`
- Move `CHANGELOG.md` entries from `## [Unreleased]` into:
  - `## [X.Y.Z] - YYYY-MM-DD`
- Open PR with issue link (`Closes #<issue-id>`)

## 4. Validate before merge
Run the required checks locally:

```bash
./scripts/release-check.sh
```

This script enforces:
- `cargo fmt --all -- --check`
- `cargo clippy --all-features -- -D warnings`
- `cargo test --all`
- `cargo test --no-default-features`
- all maintained examples with bounded runtimes

## 5. Merge and tag
After release PR is merged to `main`, create and push an annotated tag:

```bash
git checkout main
git pull --ff-only origin main
git tag -a vX.Y.Z -m "vX.Y.Z"
git push origin vX.Y.Z
```

## 6. Automated publish + GitHub release
Tag push triggers `.github/workflows/release.yml`.

The workflow:
1. re-runs release preflight checks
2. verifies tag `vX.Y.Z` matches crate version `X.Y.Z`
3. runs `cargo publish --dry-run`
4. publishes crate via `cargo publish`
5. creates GitHub Release notes from the matching `CHANGELOG.md` section

## 7. Post-release checks
- Verify crates.io package page/version
- Verify GitHub Release body content
- Confirm `## [Unreleased]` remains ready for next cycle
