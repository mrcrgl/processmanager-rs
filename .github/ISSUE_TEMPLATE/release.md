---
name: Release
about: Track a versioned release from prep to publish
title: "Release vX.Y.Z"
labels: documentation
assignees: ""
---

## Target version
- [ ] Version: `vX.Y.Z`
- [ ] Release date confirmed

## Pre-release prep
- [ ] Create branch: `chore/<issue-id>-release-vX-Y-Z`
- [ ] Ensure `Cargo.toml` version is `X.Y.Z`
- [ ] Move `Unreleased` entries into `## [X.Y.Z] - YYYY-MM-DD` in `CHANGELOG.md`
- [ ] Include issue/PR references in changelog entries where possible
- [ ] Open release PR linked to this issue

## Validation
- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --all-features -- -D warnings`
- [ ] `cargo test --all`
- [ ] `cargo test --no-default-features`
- [ ] `./scripts/release-check.sh`

## Publish
- [ ] Merge release PR to `main`
- [ ] Create and push annotated tag: `vX.Y.Z`
- [ ] Confirm GitHub `Release` workflow passes on tag
- [ ] Confirm crate published to crates.io
- [ ] Confirm GitHub Release notes match changelog section

## Post-release
- [ ] Restore/keep `## [Unreleased]` section in changelog
- [ ] Announce release notes
