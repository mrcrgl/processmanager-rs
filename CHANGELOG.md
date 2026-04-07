# Changelog

All notable changes to this project will be documented in this file.
This project adheres to [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
and its version numbers follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- GitHub Actions workflow for CI (`build`, `clippy`, `fmt`, full test-matrix,
  feature power-set).
- `CHANGELOG.md` with Keep-a-Changelog layout.
- Optional `tracing` span around every child process (requires `tracing` feature).
- Cached vector of child `ProcessControlHandler`s for allocation-free broadcast.
- Architecture diagram (Mermaid) in `README.md`.
- `Custom(Box<dyn Any>)` variant to `RuntimeControlMessage` for future
  extensibility.
- **Fluent `ProcessManagerBuilder`** allowing compile-time safe setup and
  configuration.
- `.name("…")` builder method and internal plumbing for custom supervisor
  names.
- `AGENTS.md` contribution policy with mandatory issue-linked branch naming
  scheme (`<type>/<issue-id>-<short-kebab-description>`).
- `AGENTS.md` PR title convention (no coding-agent/tool tags) and mandatory
  `git pull --ff-only origin main` before branch creation (`#32`).
- Regression test coverage for runtime coordination contracts now explicitly
  includes the `add()` pre-start panic behavior (`#28`).
- `RunnableWithContext`, `RuntimeContext`, and
  `with_runtime_context(...)` so runnables can consume runtime
  control/ticker context without carrying a `RuntimeGuard` field (`#21`).
- New `axum` example showing how to run a web server under `ProcessManager`
  with graceful shutdown and reload handling (`#11`).

### Changed
- `process_handle()` now returns `Arc<dyn ProcessControlHandler>` (cheap cloning,
  no double boxing).
- Default `process_name()` no longer allocates; returns `Cow<'static, str>`.
- `ProcessManager` constructors **deprecated** in favour of the new builder;
  `new()`, `manual_cleanup()` and `auto_cleanup()` now issue warnings.
- Busy-wait loops in `RuntimeGuard` replaced with `Notify`-based signalling.
- Child panic handling now caught with `catch_unwind`, ensuring supervisor
  never hangs.
- All examples, doctests and integration tests migrated to the builder API
  (no more deprecation warnings in user-facing code).
- Internal channels refactored to remove extra locks (use of `OnceCell`).
- `ProcessManagerBuilder::shutdown_grace_period(...)` now allows configuring
  shutdown grace timeout instead of relying on a fixed 30s value (`#17`).
- `ProcessManager::add` docs now match runtime behavior: calling `add` before
  startup panics and `insert` must be used during setup (`#26`).
- CI now executes `simple`, `dynamic_add`, and `restart_supervisor` examples
  with bounded runtimes to catch regressions in sample programs (`#37`).
- CI now also executes the `runtime_context` example to keep the
  context-based runnable API covered (`#21`).
- CI now executes the `axum` example to keep web-framework integration sample
  code validated (`#11`).
- Added `RestartSupervisor` with configurable exponential backoff to
  automatically restart failed child runnables (`#19`).

### Fixed
- Active-child counter accuracy under edge conditions (spawn panics).
- Numerous doc examples updated for new APIs.
- Runtime control messages sent before ticker initialization are now retained
  and delivered once the ticker is created (`#24`).
- `ProcessManager` shutdown now waits for child task termination before
  returning; children are aborted after grace timeout (`#23`).
- `ProcessManager` reload fanout now dispatches in parallel, matching documented
  behavior (`#25`).
- `RuntimeHandle` now supports forwarding arbitrary control messages, including
  `RuntimeControlMessage::Custom(...)`, via `control(...)` and `custom(...)`
  helpers (`#20`).
- `RuntimeControlMessage::Custom(...)` is now clone-safe and no longer panics
  when cloned (`#27`).
- `RuntimeGuard` now supports reliable restart cycles (`start, shutdown, start`)
  by retrying control-message delivery across ticker generations (`#18`).

### Removed
- Unused aliases and imports producing compiler warnings.

---

## [0.4.1] – 2024-04-19
Removed dependency to `async_trait`.
