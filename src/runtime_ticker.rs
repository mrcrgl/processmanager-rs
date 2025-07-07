use std::future::Future;

use std::sync::Arc;

use tokio::{
    select,
    sync::{Mutex, mpsc},
};

use crate::{ProcessOperation, RuntimeControlMessage};

/// Helper that multiplexes *work* futures with control messages (`Reload`,
/// `Shutdown`, …).  A `RuntimeTicker` is **optional** – components that handle
/// shutdown in their own way don’t need to create one.
pub struct RuntimeTicker {
    control_tx: mpsc::Sender<RuntimeControlMessage>,
    control_rx: Mutex<mpsc::Receiver<RuntimeControlMessage>>,
}

impl RuntimeTicker {
    /// Create a new ticker.  Callers keep the returned instance and may clone
    /// the embedded sender via [`Self::sender`] to hook it into their own
    /// control infrastructure.
    pub(crate) fn new() -> Self {
        let (tx, rx) = mpsc::channel(1);

        Self {
            control_tx: tx,
            control_rx: Mutex::new(rx),
        }
    }

    /// Obtain a clone of the underlying `mpsc::Sender`.
    pub(crate) fn sender(&self) -> mpsc::Sender<RuntimeControlMessage> {
        self.control_tx.clone()
    }

    /// Await `fut` and the control channel concurrently, returning whichever
    /// completes first.
    pub async fn tick<O, Fut>(&self, fut: Fut) -> ProcessOperation<O>
    where
        Fut: Future<Output = O>,
    {
        let mut lock = self.control_rx.lock().await;

        select! {
            res = fut                => ProcessOperation::Next(res),
            msg = lock.recv() => match msg {
                Some(c) => ProcessOperation::Control(c),
                None    => unreachable!("control channel closed unexpectedly"),
            },
        }
    }
}

/// Safety – all interior mutability is protected by async primitives.
unsafe impl Send for RuntimeTicker {}
unsafe impl Sync for RuntimeTicker {}
