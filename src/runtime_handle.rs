use std::sync::Arc;

use tokio::sync::Mutex;

use crate::{CtrlFuture, ProcessControlHandler, RuntimeControlMessage};

pub struct RuntimeHandle {
    control_ch: Arc<Mutex<tokio::sync::mpsc::Sender<RuntimeControlMessage>>>,
}

impl RuntimeHandle {
    pub(crate) fn new(control_ch: tokio::sync::mpsc::Sender<RuntimeControlMessage>) -> Self {
        Self {
            control_ch: Arc::new(Mutex::new(control_ch)),
        }
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
