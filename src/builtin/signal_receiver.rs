//! Unix signal integration.
//!
//! `SignalReceiver` listens for `SIGHUP`, `SIGINT`, `SIGTERM` and `SIGQUIT`
//! and turns them into a graceful shutdown or a reload.
//!
//! This module is compiled only when the crate’s **`signal`** feature is
//! enabled.
use crate::{
    ProcFuture, ProcessControlHandler, ProcessOperation, Runnable, RuntimeControlMessage,
    RuntimeError, RuntimeGuard,
};
use futures::stream::StreamExt as _;
use signal_hook::consts::signal::*;
use signal_hook_tokio::Signals;
use std::sync::Arc;

/// Built-in [`Runnable`] that turns POSIX signals into a graceful shutdown or
/// a reload of its [`ProcessManager`](crate::ProcessManager).
///
/// | Signal                          | Effect                                        |
/// | ------------------------------- | --------------------------------------------- |
/// | `SIGTERM`, `SIGINT`, `SIGQUIT`  | Graceful shutdown of the manager.             |
/// | `SIGHUP`                        | `reload()` of the manager (see below).        |
///
/// The easiest way to use it is
/// [`ProcessManagerBuilder::handle_signals`](crate::ProcessManagerBuilder::handle_signals),
/// which wires the receiver to its manager:
///
/// ```no_run
/// use processmanager::{IdleProcess, ProcessManagerBuilder, Runnable};
///
/// # #[tokio::main] async fn main() {
/// let manager = ProcessManagerBuilder::default()
///     .handle_signals()
///     .pre_insert(IdleProcess::new())
///     .build();
///
/// // Returns Ok(()) after SIGTERM / Ctrl-C once all children have stopped.
/// manager.process_start().await.unwrap();
/// # }
/// ```
///
/// A receiver created with [`SignalReceiver::new`] and inserted like any other
/// child also shuts its manager down gracefully: it ends with
/// [`RuntimeError::TerminationSignal`], which a `ProcessManager` treats as a
/// shutdown request rather than a failure. It cannot reach its manager,
/// though, so it ignores `SIGHUP`.
///
/// Signal handlers are installed when the receiver starts and removed when it
/// stops. Registration errors are returned from
/// [`process_start`](Runnable::process_start).
pub struct SignalReceiver {
    /// Manager to shut down / reload; `None` for a stand-alone receiver.
    target: Option<Arc<dyn ProcessControlHandler>>,
    runtime_guard: RuntimeGuard,
}

impl std::fmt::Debug for SignalReceiver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignalReceiver")
            .field("wired_to_manager", &self.target.is_some())
            .field("runtime_guard", &self.runtime_guard)
            .finish()
    }
}

impl SignalReceiver {
    /// Creates a stand-alone receiver.
    ///
    /// Termination signals stop its manager gracefully; `SIGHUP` is ignored.
    /// Prefer
    /// [`ProcessManagerBuilder::handle_signals`](crate::ProcessManagerBuilder::handle_signals),
    /// which also supports reload.
    pub fn new() -> Self {
        Self {
            target: None,
            runtime_guard: RuntimeGuard::default(),
        }
    }

    /// Creates a receiver that controls `target` (its own manager).
    ///
    /// `target` must not keep the manager alive (use a weak handle), as the
    /// receiver is stored inside that manager.
    pub(crate) fn for_manager(target: Arc<dyn ProcessControlHandler>) -> Self {
        Self {
            target: Some(target),
            runtime_guard: RuntimeGuard::default(),
        }
    }
}

impl Default for SignalReceiver {
    fn default() -> Self {
        Self::new()
    }
}

impl Runnable for SignalReceiver {
    fn process_start(&self) -> ProcFuture<'_> {
        Box::pin(async {
            // Dropped (and the handlers removed) when this future ends.
            let mut signals = Signals::new([SIGHUP, SIGINT, SIGTERM, SIGQUIT]).map_err(|err| {
                RuntimeError::Internal {
                    message: format!("register signal handlers: {err}"),
                }
            })?;
            let ticker = self.runtime_guard.runtime_ticker().await;

            loop {
                let signal = match ticker.tick(signals.next()).await {
                    ProcessOperation::Next(Some(signal)) => signal,
                    // The signal stream has ended; nothing more to receive.
                    ProcessOperation::Next(None) => return Ok(()),
                    ProcessOperation::Control(RuntimeControlMessage::Shutdown) => return Ok(()),
                    ProcessOperation::Control(_) => continue,
                };

                #[cfg(feature = "tracing")]
                ::tracing::info!(signal = signal, "Received process signal");
                #[cfg(all(not(feature = "tracing"), feature = "log"))]
                ::log::info!("Received process signal {signal}");
                #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
                eprintln!("Received process signal {signal}");

                match (signal, &self.target) {
                    (SIGHUP, Some(target)) => {
                        // Spawned: the reload also reaches this receiver,
                        // which must keep ticking to accept it.
                        let target = Arc::clone(target);
                        tokio::spawn(async move { target.reload().await });
                    }
                    (SIGHUP, None) => {
                        #[cfg(feature = "tracing")]
                        ::tracing::warn!(
                            "SIGHUP ignored: SignalReceiver is not attached to a manager; \
                             use ProcessManagerBuilder::handle_signals()"
                        );
                        #[cfg(all(not(feature = "tracing"), feature = "log"))]
                        ::log::warn!(
                            "SIGHUP ignored: SignalReceiver is not attached to a manager; \
                             use ProcessManagerBuilder::handle_signals()"
                        );
                        #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
                        eprintln!(
                            "SIGHUP ignored: SignalReceiver is not attached to a manager; \
                             use ProcessManagerBuilder::handle_signals()"
                        );
                    }
                    (_, Some(target)) => {
                        // Spawned: the shutdown waits for this receiver to
                        // stop, which happens when it receives `Shutdown`.
                        let target = Arc::clone(target);
                        tokio::spawn(async move { target.shutdown().await });
                    }
                    (_, None) => return Err(RuntimeError::TerminationSignal),
                }
            }
        })
    }

    fn process_handle(&self) -> Arc<dyn ProcessControlHandler> {
        self.runtime_guard.handle()
    }

    fn process_name(&self) -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("SignalReceiver")
    }
}
