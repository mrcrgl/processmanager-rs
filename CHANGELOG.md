# Changelog

All notable changes to this project will be documented in this file.
This project adheres to [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
and its version numbers follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- `Debug` for `ProcessManager`, `ProcessManagerBuilder`, `RuntimeTicker`,
  `RuntimeContext`, `RuntimeContextRunnable`, `ProcessOperation`, `Job`,
  `RestartSupervisor` and `SignalReceiver`. Types holding closures or trait
  objects print names and configuration (#74).
- `#[must_use]` on the by-value builder methods of `ProcessManagerBuilder`,
  `RestartSupervisor` and `Job`, so a discarded result is reported (#74).
- `rust-version = "1.85"` in `Cargo.toml`, checked by a new MSRV CI job (#70).
- `RuntimeError::Failed(BoxError)` carries a child's own typed error
  (transparent `Display`/`source`), created with `RuntimeError::failed(err)`
  and read back with `RuntimeError::downcast_ref::<E>()`; `?` converts
  `std::io::Error` (`#68`).
- `RestartLimitExceeded`, returned (as `RuntimeError::Failed`) when a
  `RestartSupervisor` gives up; it exposes the child, failure count, window
  and the child's last error as `source` (`#68`).
- `AddError` (`Stopped`, `ShuttingDown`), returned by `ProcessManager::add`
  (`#67`).
- `ProcessManagerBuilder::handle_signals()` installs a `SignalReceiver` wired
  to its manager: `SIGTERM`/`SIGINT`/`SIGQUIT` shut it down gracefully and
  `SIGHUP` reloads all children (`#64`).
- `Job` built-in for one-shot tasks such as migrations: runs an async closure
  once, shuts the manager down on failure unless marked `.optional()`, and is
  cancelled by shutdown (`#3`).
- `RestartSupervisor::restart_limit(max_restarts, window)`: give up and return
  an error once a child fails too often within a time window, which shuts
  down sibling processes in a `ProcessManager` (`#4`).
- `RestartSupervisor::reset_after(duration)`: reset the backoff after the
  child ran healthily for `duration` (default: 60s) (`#4`).

### Changed
- **Breaking:** the empty `manager` feature is removed; drop it from
  `features = [...]` (#75).
- tokio is requested with only `macros`, `rt`, `sync` and `time`, so the
  crate no longer forces `rt-multi-thread` on users; `once_cell` is no longer
  a dependency. The published crate only contains sources, README, changelog
  and license (#75).
- The `signal` feature compiles to nothing on non-Unix targets, so a default
  build works on Windows (#75).
- CI lints tests and examples (`--all-targets`), checks and tests the `log`
  feature, uses `dtolnay/rust-toolchain`, and publishes with `--locked`;
  `Cargo.lock` is now committed (#70).
- **Breaking:** `RuntimeError` is `#[non_exhaustive]`; `match` it with a
  wildcard arm (`#68`).
- `RestartSupervisor` no longer flattens errors into strings: giving up
  returns `RestartLimitExceeded` and a child task join failure keeps its
  `JoinError`, both as `RuntimeError::Failed` (`#68`).
- **Breaking:** `ProcessManager::add` now returns `Result<(), AddError>`
  instead of panicking. Before the manager has started it queues the child
  (like `insert`); while running it spawns it; it returns
  `AddError::ShuttingDown` during a shutdown and `AddError::Stopped` once the
  manager has stopped (`#67`).
- A child ending with `RuntimeError::TerminationSignal` (e.g. a
  `SignalReceiver` after Ctrl-C) is now treated as a graceful shutdown
  request: the manager stops its other children and returns `Ok(())` instead
  of `Err(TerminationSignal)`. A real failure of another child still wins
  (`#64`).
- `SignalReceiver` registers its signal handlers when it starts and removes
  them when it stops; registration errors are returned from `process_start`
  instead of panicking in `SignalReceiver::new` (`#64`).
- `RestartSupervisor` backoff now resets after 60 seconds of healthy runtime
  instead of growing for the lifetime of the supervisor (`#4`).
- `RestartSupervisor::process_name` now includes the child's name,
  e.g. `RestartSupervisor(my_crate::Worker)` (`#4`).

### Fixed
- A control message that a run of a `RuntimeGuard`-based runnable never read
  is discarded when the run ends, instead of reaching the next run (e.g. a
  restarted child under `RestartSupervisor` stopping at once). Messages sent
  before the first start or between two runs are still delivered (#72).
- `ProcessManager::process_start` called while the manager is already
  running returns an error instead of re-spawning its children next to the
  running ones (which brought the running manager down). Restarting a stopped
  manager is no longer disturbed by children of an earlier run whose future
  was dropped (#75).
- Documentation matches the code again: the crate overview is on the crate
  page (it was attached to a private module), every public item is
  documented (`missing_docs` is now enforced), `ProcessManager`,
  `RuntimeGuard`, `IdleProcess` and `RestartSupervisor` carry their docs on
  the public types, the `ProcessControlHandler` docs describe what its
  futures wait for, and the README states the actual license (MIT), runtime
  requirement and logging behaviour. The deprecated
  `processmanager::receiver::SignalReceiver` path now actually warns
  (`#69`).
- `ProcessManager::add` no longer races the manager's start (an `add` right
  after spawning `process_start` panicked) or its exit (a child added as the
  last one finished could be orphaned) (`#67`).
- A child whose `process_start` panics before returning its future is now
  reported as `process panicked: …` instead of being misreported as aborted
  after the shutdown grace period (`#71`).
- `RuntimeGuard` no longer spawns a background task, so `RuntimeGuard::new`
  and the built-ins using it (`IdleProcess`, `Job`, `RestartSupervisor`,
  `SignalReceiver`, `with_runtime_context`) can be constructed outside a
  Tokio runtime, e.g. while building a manager before `Runtime::block_on`
  (`#65`).
- `RuntimeTicker::tick` no longer panics with `unreachable!` after its
  `RuntimeGuard` and handles are dropped (`#66`).
- Two concurrent `runtime_ticker()` calls can no longer both succeed, and a
  `RuntimeHandle` that outlives its runtime no longer hangs (`#72`).
- `SignalReceiver` no longer silently ignores `SIGHUP` (it reloads when
  installed via `handle_signals()` and logs a warning otherwise), no longer
  spins when its signal stream ends, and its docs match its behaviour
  (`#64`).
- Aborting or dropping a `RestartSupervisor` or a nested `ProcessManager`
  (e.g. after a parent's shutdown grace period) now aborts its children
  instead of leaving them running detached (`#63`).
- A `ProcessManager` shutdown requested before or while `process_start` runs
  is no longer lost: it is remembered and delivered to children spawned
  afterwards, including children passed to `add()` during a shutdown. Each
  child receives the shutdown message at most once (`#62`).
- `ProcessManagerBuilder` now defaults to `auto_cleanup = true`, as
  documented; it previously defaulted to `false`, so finished children were
  never removed (`#61`).
- `reload()` and `shutdown()` on a `ProcessManager` handle skip children that
  have already finished, so repeated broadcasts no longer hang (reload) or
  wait for the grace period (shutdown) on a dead child (`#61`).
- Broken intra-doc links in private module docs (`IdleProcess`,
  `SignalReceiver`, `ProcessManagerBuilder`, `RuntimeGuard`, `RuntimeTicker`)
  and a duplicated doc sentence on `RuntimeControlMessage` (`#57`).
- `cargo doc` builds again: broken and private intra-doc links in the
  `ProcessManager`, `Runnable` and `IdleProcess` docs now resolve, and CI
  builds the docs with warnings denied (`#55`).
- `ProcessManager::process_start` no longer hangs when a child is force-aborted
  after `shutdown_grace_period`; the aborted child is reported as a
  `RuntimeError::Internal` naming it, and the first error is still returned
  (`#53`).

## [0.6.0] – 2026-04-07

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
- Release process documentation and release issue template for standardized
  versioned publish workflows (`#47`).

### Changed
- `process_handle()` now returns `Arc<dyn ProcessControlHandler>` (cheap cloning,
  no double boxing).
- Default `process_name()` no longer allocates; returns `Cow<'static, str>`.
- `ProcessManagerBuilder` is the recommended way to construct a
  `ProcessManager`. (This entry previously claimed that `ProcessManager`
  constructors were deprecated; they were not.)
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
- Added tag-driven `Release` workflow to validate, publish to crates.io, and
  create GitHub releases from changelog sections (`#47`).
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
