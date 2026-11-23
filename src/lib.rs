//! A light-weight, Tokio-based supervisor for long-running async tasks.
//!
//! A [`ProcessManager`] runs a set of [`Runnable`]s (*processes*), each in its
//! own Tokio task, and coordinates them as a unit:
//!
//! * **Graceful shutdown** – [`ProcessControlHandler::shutdown`] reaches every
//!   child; children that ignore it are aborted after a grace period.
//! * **Error propagation** – the first child that fails shuts the others down
//!   and its error is returned from [`ProcessManager::process_start`](Runnable::process_start).
//! * **Reload** – [`ProcessControlHandler::reload`] is forwarded to every child.
//! * **Trees** – a `ProcessManager` is itself a `Runnable`, so managers nest.
//!
//! Built-in helpers cover common needs: [`IdleProcess`], [`Job`] (one-shot
//! tasks), [`RestartSupervisor`] (restart with backoff) and, with the `signal`
//! feature, `SignalReceiver` via `ProcessManagerBuilder::handle_signals()`.
//!
//! Runnables can be constructed anywhere; running them requires a Tokio
//! runtime.
//!
//! # Example
//!
//! A process reacts to control messages through a [`RuntimeGuard`]'s
//! [`RuntimeTicker`] (or implement [`RunnableWithContext`] to skip the guard):
//!
//! ```
//! use processmanager::*;
//! use std::sync::Arc;
//! use std::time::Duration;
//!
//! #[derive(Default)]
//! struct Worker {
//!     runtime_guard: RuntimeGuard,
//! }
//!
//! impl Runnable for Worker {
//!     fn process_start(&self) -> ProcFuture<'_> {
//!         Box::pin(async {
//!             let ticker = self.runtime_guard.runtime_ticker().await;
//!             let mut interval = tokio::time::interval(Duration::from_millis(100));
//!
//!             loop {
//!                 // Race one unit of work against control messages.
//!                 match ticker.tick(interval.tick()).await {
//!                     ProcessOperation::Next(_) => { /* do work */ }
//!                     ProcessOperation::Control(RuntimeControlMessage::Shutdown) => break,
//!                     ProcessOperation::Control(RuntimeControlMessage::Reload) => { /* reload */ }
//!                     ProcessOperation::Control(_) => {}
//!                 }
//!             }
//!             Ok(())
//!         })
//!     }
//!
//!     fn process_handle(&self) -> Arc<dyn ProcessControlHandler> {
//!         self.runtime_guard.handle()
//!     }
//! }
//!
//! #[tokio::main]
//! async fn main() -> Result<(), RuntimeError> {
//!     let manager = ProcessManagerBuilder::default()
//!         .pre_insert(Worker::default())
//!         .pre_insert(Job::new("migrate", || async { Ok(()) }))
//!         .build();
//!     let handle = manager.process_handle();
//!
//!     let run = tokio::spawn(async move { manager.process_start().await });
//!
//!     // Later: stop everything gracefully and wait for the result.
//!     handle.shutdown().await;
//!     run.await.expect("manager task panicked")
//! }
//! ```
//!
//! # Features
//!
//! * `signal` (default) – `SignalReceiver` and
//!   `ProcessManagerBuilder::handle_signals()` for Unix signals. Unix only:
//!   on other targets the feature compiles to nothing.
//! * `tracing` – log through the `tracing` crate.
//! * `log` – log through the `log` crate (`tracing` wins if both are enabled).
//!
//! Without `tracing` or `log`, lifecycle events are printed to stderr.
#![deny(rustdoc::broken_intra_doc_links)]
#![warn(missing_docs)]

/// Built-in [`Runnable`] helpers.
pub mod builtin;

// With both `tracing` and `log` enabled, `tracing` is used and `log` would
// otherwise be reported as an unused dependency.
#[cfg(all(feature = "tracing", feature = "log"))]
use log as _;
mod error;
mod process_manager;
mod process_manager_builder;
mod runtime_guard;
mod runtime_handle;
mod runtime_process;
mod runtime_ticker;
pub use builtin::*;
pub use error::*;
pub use process_manager::*;
pub use process_manager_builder::*;
pub use runtime_guard::*;
pub use runtime_handle::*;
pub use runtime_process::*;
pub use runtime_ticker::*;

/// Old location of [`builtin::SignalReceiver`].
#[cfg(all(unix, feature = "signal"))]
pub mod receiver {
    /// Deprecated alias of [`SignalReceiver`](crate::builtin::SignalReceiver).
    #[deprecated(note = "use `processmanager::builtin::SignalReceiver` instead")]
    pub type SignalReceiver = crate::builtin::SignalReceiver;
}

/// Compile-time checks that discarding a builder result is reported (#74).
/// Each `compile_fail` case has a compiling twin that keeps the result, so a
/// case cannot pass because of an unrelated error.
///
/// ```
/// #![deny(unused_must_use)]
/// # fn main() {
/// use processmanager::*;
/// use std::time::Duration;
/// let _b = ProcessManagerBuilder::new().auto_cleanup(false);
/// let _s = RestartSupervisor::new(IdleProcess::new()).reset_after(Duration::from_secs(1));
/// let _j = Job::new("job", || async { Ok(()) }).optional();
/// # }
/// ```
///
/// ```compile_fail
/// #![deny(unused_must_use)]
/// # fn main() {
/// processmanager::ProcessManagerBuilder::new().auto_cleanup(false);
/// # }
/// ```
///
/// ```compile_fail
/// #![deny(unused_must_use)]
/// # fn main() {
/// use processmanager::*;
/// RestartSupervisor::new(IdleProcess::new()).reset_after(std::time::Duration::from_secs(1));
/// # }
/// ```
///
/// ```compile_fail
/// #![deny(unused_must_use)]
/// # fn main() {
/// processmanager::Job::new("job", || async { Ok(()) }).optional();
/// # }
/// ```
#[cfg(doctest)]
pub struct MustUseChecks;
