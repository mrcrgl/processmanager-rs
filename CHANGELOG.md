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

### Changed
- `process_handle()` now returns `Arc<dyn ProcessControlHandler>` (cheap cloning,
  no double boxing).
- Default `process_name()` no longer allocates; returns `Cow<'static, str>`.
- `ProcessManager` constructors clarified: `new()` (auto-cleanup) and
  `manual_cleanup()`.
- Busy-wait loops in `RuntimeGuard` replaced with `Notify`-based signalling.
- Child panic handling now caught with `catch_unwind`, ensuring supervisor
  never hangs.
- Internal channels refactored to remove extra locks (use of `OnceCell`).

### Fixed
- Active-child counter accuracy under edge conditions (spawn panics).
- Numerous doc examples updated for new APIs.

### Removed
- Unused aliases and imports producing compiler warnings.

---

## [0.4.1] – 2024-04-19
Removed dependency to `async_trait`.
