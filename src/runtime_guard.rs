//! Central runtime control hub.
//!
//! `RuntimeGuard` owns two *async* channels: a **global control** channel that
//! forwards messages from any number of [`RuntimeHandle`]s and the
//! **ticker channel** consumed by exactly one [`RuntimeTicker`].
//!
//! The guard exposes three high-level operations:
//!
//! 1. [`runtime_ticker`](RuntimeGuard::runtime_ticker) – creates the sole ticker
//!    and connects it to the control fan-out.
//! 2. [`handle`](RuntimeGuard::handle) – returns a cheap, clonable
//!    [`ProcessControlHandler`] that broadcasts control messages.
//! 3. [`is_running`](RuntimeGuard::is_running) /
//!    [`block_until_shutdown`](RuntimeGuard::block_until_shutdown) – helpers
//!    for observing runtime state in tests and demos.
//!
//! Dropping the ticker closes the channel which in turn lets [`is_running`]
//! report `false`.  Constructing a second ticker is prohibited and will panic.
//!
//! Internally the guard spawns a “fan-out” task that waits for control messages
//! and forwards them to the ticker once it exists.
//!
//! # Concurrency & safety
//!
//! All interior mutability is protected by `tokio::sync::Mutex`; therefore
//! `RuntimeGuard` is `Send + Sync`.
//!
//! ---
//!
//! ```no_run
//! # use processmanager::*;
//! # async fn demo() {
//! let guard  = RuntimeGuard::new();
//! let ticker = guard.runtime_ticker().await;
//! let handle = guard.handle();
//!
//! handle.reload().await;   // broadcast control instruction
//! /* ... */
//! # }
//! ```
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::{ProcessControlHandler, RuntimeControlMessage, RuntimeHandle, RuntimeTicker};

#[derive(Debug, Clone)]
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
    /// Create a fresh guard and spawn the internal *fan-out* task.
    ///
    /// The returned instance is ready for immediate use; you typically call
    /// [`runtime_ticker`](Self::runtime_ticker) right after construction.
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

    /// Create the **single** [`RuntimeTicker`] and connect it to the fan-out.
    ///
    /// # Panics
    /// Panics if a ticker has already been created – i.e. the runtime is
    /// considered “running”.
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

    /// Returns `true` while the ticker (and therefore the runtime) is alive.
    pub async fn is_running(&self) -> bool {
        let lock = self.inner.runtime_ticker_ch_sender.lock().await;
        let closed = lock.as_ref().map(|s| s.is_closed()).unwrap_or(true);
        !closed
    }

    /// Obtain a clonable [`ProcessControlHandler`] that broadcasts control
    /// messages to the ticker.
    pub fn handle(&self) -> Arc<dyn ProcessControlHandler> {
        Arc::new(RuntimeHandle::new(Arc::clone(
            &self.inner.control_ch_sender,
        )))
    }

    /// **Busy-wait** helper for tests and demos.
    ///
    /// Polls [`is_running`](Self::is_running) once every 10 ms until it
    /// returns `false`.
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
