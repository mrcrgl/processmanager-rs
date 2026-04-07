# AGENTS

Contribution and review policy for `mrcrgl/processmanager-rs`.

## Source of truth
- Issues are managed in GitHub: https://github.com/mrcrgl/processmanager-rs/issues
- Work must start from a tracked issue.

## Issue and PR policy (mandatory)
- No direct merges to `main`/`master`.
- Every change must go through a Pull Request.
- Every PR must be linked to at least one issue.
- PR descriptions must include explicit issue references (for example: `Closes #123`, `Fixes #123`, or `Refs #123`).
- If a PR addresses multiple issues, list all related issue numbers.

## Branching scheme (mandatory)
- Use this branch format for all work: `<type>/<issue-id>-<short-kebab-description>`.
- Common `type` values: `feat`, `fix`, `docs`, `refactor`, `test`, `ci`, `chore`.
- Branches must include the primary GitHub issue id for traceability.
- One branch should target one primary issue; if additional issues are touched, reference them in the PR body.
- Examples:
  - `feat/42-runtime-restart-support`
  - `fix/24-runtimeguard-startup-message-drop`
  - `docs/29-agents-branching-policy`

## Changelog policy (mandatory)
- `CHANGELOG.md` must be updated for every issue resolved by a PR.
- Add entries under `## [Unreleased]` using Keep a Changelog categories:
  - `Added`
  - `Changed`
  - `Fixed`
  - `Removed`
- Changelog entries should be user-facing, concise, and reference issue/PR IDs when possible.

## Rust engineering standards
- Keep code idiomatic, explicit, and maintainable.
- Prefer safe Rust; avoid `unsafe` unless strictly necessary and documented.
- Handle errors with typed errors and meaningful context.
- Avoid panics in library/runtime paths unless panic is the explicit contract.
- Keep public API docs and examples up to date when behavior changes.
- Add or update tests for behavior changes and bug fixes.

## Required checks before merge
All checks must pass on all supported targets/toolchains defined by CI.

Minimum required commands:
- `cargo fmt --all -- --check`
- `cargo clippy --all-features -- -D warnings`
- `cargo test --all`
- `cargo test --no-default-features`

If the CI matrix expands, these requirements expand with it.
