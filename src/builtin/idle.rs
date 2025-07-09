//! A no-op [`Runnable`] that idles until it receives a shutdown request.
//!
//! `IdleProcess` is useful as a *tombstone* child for a [`ProcessManager`] that
//! would otherwise terminate immediately because it has no real children.
//! Registering an `IdleProcess` keeps the supervisor alive until an external
//! caller broadcasts [`ProcessControlHandler::shutdown`].
//!
//! # Example
//! ```no_run
//! use processmanager::{IdleProcess, ProcessManagerBuilder};
//!
//! // Supervisor without real children that should stay alive.
//! let _mgr = ProcessManagerBuilder::default()
//!     .pre_insert(IdleProcess::default())
//!     .build();
//! ```
use std::{sync::Arc, time::Duration};

use crate::{
    ProcFuture, ProcessControlHandler, ProcessOperation, Runnable, RuntimeControlMessage,
    RuntimeGuard,
};

/// A *tombstone* process that simply waits for a shutdown request.
///
/// It performs no work and exists solely to keep its parent
/// [`ProcessManager`] alive until an explicit shutdown is broadcast.
#[derive(Debug, Default)]
pub struct IdleProcess {
    runtime_guard: RuntimeGuard,
}

impl IdleProcess {
    /// Construct a fresh idle process. Equivalent to [`Default::default`].
    pub fn new() -> Self {
        Self {
            runtime_guard: RuntimeGuard::default(),
        }
    }
}
impl Runnable for IdleProcess {
    fn process_start(&self) -> ProcFuture<'_> {
        Box::pin(async {
            let ticker = self.runtime_guard.runtime_ticker().await;

            loop {
                // Sleep for a long time; the ticker wakes us early when a control message arrives.
                let sleep = tokio::time::sleep(Duration::from_secs(3600));

                match ticker.tick(sleep).await {
                    ProcessOperation::Next(_) => continue,
                    ProcessOperation::Control(RuntimeControlMessage::Shutdown) => break,
                    ProcessOperation::Control(_) => continue,
                }
            }

            Ok(())
        })
    }

    fn process_handle(&self) -> Arc<dyn ProcessControlHandler> {
        self.runtime_guard.handle()
    }

    fn process_name(&self) -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("IdleProcess")
    }
}
