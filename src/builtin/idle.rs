//! A no-op [`Runnable`] that just idles until it receives a shutdown request.
//!
//! `IdleProcess` is handy as a “tombstone” child for a [`ProcessManager`] that
//! would otherwise stop immediately because it starts without any real
//! children.  By registering an `IdleProcess`, the manager keeps running until
//! an external caller invokes [`ProcessControlHandler::shutdown`].
//!
//! # Example
//! ```rust
//! use processmanager::*;
//!
//! // Manager without real children that should stay alive.
//! let mgr = ProcessManagerBuilder::default()
//!     .pre_insert(IdleProcess::default())
//!     .build();
//! ```
use std::sync::Arc;

use crate::{
    ProcFuture, ProcessControlHandler, ProcessOperation, Runnable, RuntimeControlMessage,
    RuntimeGuard,
};

/// A no-op process that simply waits for a shutdown request.
#[derive(Debug, Default)]
pub struct IdleProcess {
    runtime_guard: RuntimeGuard,
}

impl IdleProcess {
    /// Create a new idle process.
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
                // Sleep for a long time; the ticker wakes us early on control messages.
                let sleep = tokio::time::sleep(std::time::Duration::from_secs(3600));

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
