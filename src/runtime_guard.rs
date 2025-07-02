use std::sync::Arc;

use tokio::sync::Mutex;

use crate::{RuntimeControlMessage, RuntimeHandle, RuntimeTicker};

pub struct RuntimeGuard {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    runtime_ticker_ch_sender: Arc<Mutex<Option<tokio::sync::mpsc::Sender<RuntimeControlMessage>>>>,
    control_ch_sender: Arc<Mutex<tokio::sync::mpsc::Sender<RuntimeControlMessage>>>,
}

// SAFETY: All interior mutability is protected by `tokio::sync::Mutex`, so
// `&RuntimeGuard` can be safely shared between threads.
unsafe impl Send for RuntimeGuard {}
unsafe impl Sync for RuntimeGuard {}

impl RuntimeGuard {
    pub fn new() -> Self {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);

        let ticker_sender: Arc<Mutex<Option<tokio::sync::mpsc::Sender<RuntimeControlMessage>>>> =
            Arc::new(Mutex::new(None));

        let fanout_sender = Arc::clone(&ticker_sender);

        // Fan-out task: forward messages from the central control channel to
        // the (single) ticker once it has been created.
        tokio::spawn(async move {
            while let Some(msg) = receiver.recv().await {
                let lock = fanout_sender.lock().await;
                if let Some(ref s) = *lock {
                    if s.send(msg).await.is_err() {
                        break; // ticker dropped
                    }
                } else {
                    ::tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            }
        });

        Self {
            inner: Arc::new(Inner {
                runtime_ticker_ch_sender: ticker_sender,
                control_ch_sender: Arc::new(Mutex::new(sender)),
            }),
        }
    }

    /// Create a ticker for the caller and connect it to the control fan-out.
    pub async fn runtime_ticker(&self) -> RuntimeTicker {
        assert!(
            !self.is_running().await,
            "process already started – only one ticker allowed"
        );

        let mut lock = self.inner.runtime_ticker_ch_sender.lock().await;
        let (ticker, sender) = RuntimeTicker::new();
        lock.replace(sender);
        ticker
    }

    pub async fn is_running(&self) -> bool {
        let lock = self.inner.runtime_ticker_ch_sender.lock().await;
        let closed = lock.as_ref().map(|s| s.is_closed()).unwrap_or(true);
        !closed
    }

    pub fn handle(&self) -> RuntimeHandle {
        RuntimeHandle::new(Arc::clone(&self.inner.control_ch_sender))
    }

    /// Busy-wait helper for tests / demos.
    pub async fn block_until_shutdown(&self) {
        while self.is_running().await {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
}

impl Default for RuntimeGuard {
    fn default() -> Self {
        Self::new()
    }
}
