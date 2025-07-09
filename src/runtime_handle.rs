//! [`ProcessControlHandler`] implementation that forwards runtime‐level control
//! messages (`Shutdown`, `Reload`, …) to a shared `tokio::mpsc::Sender`.
//!
//! Cloning a `RuntimeHandle` is cheap – all clones share the same underlying
//! channel.  The async [`shutdown`](ProcessControlHandler::shutdown) and
//! [`reload`](ProcessControlHandler::reload) methods enqueue the requested
//! operation and return immediately without waiting for it to be executed.
//
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::{CtrlFuture, ProcessControlHandler, RuntimeControlMessage};

/// Handle produced by [`RuntimeGuard::handle`](crate::RuntimeGuard::handle).
///
/// Acts as a concrete [`ProcessControlHandler`]: every instruction is simply
/// forwarded to the runtime’s central control channel.  The handle can be
/// cloned and sent across tasks at will.
#[derive(Debug, Clone)]
pub struct RuntimeHandle {
    control_ch: Arc<Mutex<tokio::sync::mpsc::Sender<RuntimeControlMessage>>>,
}

impl RuntimeHandle {
    pub(crate) fn new(
        control_ch: Arc<Mutex<tokio::sync::mpsc::Sender<RuntimeControlMessage>>>,
    ) -> Self {
        Self { control_ch }
    }
}

impl ProcessControlHandler for RuntimeHandle {
    fn shutdown(&self) -> CtrlFuture<'_> {
        Box::pin(async move {
            let ch = self.control_ch.lock().await;
            let _ = ch.send(RuntimeControlMessage::Shutdown).await;
        })
    }

    fn reload(&self) -> CtrlFuture<'_> {
        Box::pin(async move {
            let ch = self.control_ch.lock().await;
            let _ = ch.send(RuntimeControlMessage::Reload).await;
        })
    }
}
