//! Unix signal integration.
//!
//! `SignalReceiver` listens for `SIGHUP`, `SIGINT`, `SIGTERM`, and `SIGQUIT` and
//! converts them into runtime-level control events so a [`ProcessManager`] can
//! perform a graceful shutdown or reload.
//!
//! This module is compiled only when the crate’s **`signal`** feature is
//! enabled.
//!
//! ```no_run
//! use processmanager::{builtin::SignalReceiver, ProcessManagerBuilder};
//!
//! let mgr = ProcessManagerBuilder::default()
//!     .pre_insert(SignalReceiver::default())
//!     .build();
//! ```
//!
use crate::{
    ProcFuture, ProcessControlHandler, ProcessOperation, Runnable, RuntimeControlMessage,
    RuntimeError, RuntimeGuard,
};
use futures::stream::StreamExt as _;
use signal_hook::consts::signal::*;
use signal_hook::iterator::Handle;
use signal_hook_tokio::{Signals, SignalsInfo};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Built-in [`Runnable`] that converts POSIX signals into shutdown / reload
/// requests and propagates them through the process-manager runtime.
pub struct SignalReceiver {
    signals: Mutex<SignalsInfo>,
    signal_handle: Handle,
    runtime_guard: RuntimeGuard,
}

impl SignalReceiver {
    pub fn new() -> Self {
        let signals = Signals::new([SIGHUP, SIGINT, SIGTERM, SIGQUIT])
            .map_err(|err| RuntimeError::Internal {
                message: format!("register signal handler: {err:?}"),
            })
            .expect("signals to register");

        let handle = signals.handle();

        Self {
            signals: Mutex::new(signals),
            signal_handle: handle,
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
            let mut signals = self.signals.lock().await;
            let ticker = self.runtime_guard.runtime_ticker().await;

            loop {
                let signal = match ticker.tick(signals.next()).await {
                    ProcessOperation::Next(None) => continue,
                    ProcessOperation::Next(Some(signal)) => signal,
                    ProcessOperation::Control(RuntimeControlMessage::Shutdown) => break,
                    ProcessOperation::Control(_) => continue,
                };

                #[cfg(feature = "tracing")]
                ::tracing::warn!(signal = ?signal, "Received process signal");

                match signal {
                    SIGHUP => {
                        // Reload configuration
                        // Reopen the log file
                        //             tracing::warn!("signal handler {signal:?} not implemented");
                    }
                    SIGTERM | SIGINT | SIGQUIT => {
                        // Shutdown the system;
                        self.signal_handle.close();

                        return Err(RuntimeError::TerminationSignal);
                    }
                    _ => unreachable!(),
                }
            }

            Ok(())
        })
    }

    fn process_handle(&self) -> Arc<dyn ProcessControlHandler> {
        self.runtime_guard.handle()
    }

    fn process_name(&self) -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("SignalReceiver")
    }
}
