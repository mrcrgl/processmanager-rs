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
//!    [`RuntimeHandle`] that broadcasts control messages.
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

use tokio::sync::{Mutex, Notify};

use crate::{CtrlFuture, RuntimeControlMessage, RuntimeHandle, RuntimeTicker};

#[derive(Debug, Clone)]
pub struct RuntimeGuard {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    runtime_ticker_ch_sender: Arc<Mutex<Option<tokio::sync::mpsc::Sender<RuntimeControlMessage>>>>,
    control_ch_sender: Arc<Mutex<tokio::sync::mpsc::Sender<RuntimeControlMessage>>>,
    ticker_ready: Arc<Notify>,
}

async fn wait_for_ticker_sender(
    sender_slot: Arc<Mutex<Option<tokio::sync::mpsc::Sender<RuntimeControlMessage>>>>,
    ticker_ready: Arc<Notify>,
) -> tokio::sync::mpsc::Sender<RuntimeControlMessage> {
    loop {
        // Register interest before checking state to avoid missing notifications.
        let notified = ticker_ready.notified();

        if let Some(sender) = sender_slot.lock().await.clone() {
            return sender;
        }

        notified.await;
    }
}

impl RuntimeGuard {
    /// Create a fresh guard and spawn the internal *fan-out* task.
    ///
    /// The returned instance is ready for immediate use; you typically call
    /// [`runtime_ticker`](Self::runtime_ticker) right after construction.
    pub fn new() -> Self {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);

        let ticker_sender: Arc<Mutex<Option<tokio::sync::mpsc::Sender<RuntimeControlMessage>>>> =
            Arc::new(Mutex::new(None));
        let ticker_ready = Arc::new(Notify::new());

        let fanout_sender = Arc::clone(&ticker_sender);
        let fanout_ready = Arc::clone(&ticker_ready);

        // Fan-out task: forward messages from the central control channel to
        // the (single) ticker once it has been created.
        tokio::spawn(async move {
            while let Some(msg) = receiver.recv().await {
                let ticker =
                    wait_for_ticker_sender(Arc::clone(&fanout_sender), Arc::clone(&fanout_ready))
                        .await;

                if ticker.send(msg).await.is_err() {
                    break; // ticker dropped
                }
            }
        });

        Self {
            inner: Arc::new(Inner {
                runtime_ticker_ch_sender: ticker_sender,
                control_ch_sender: Arc::new(Mutex::new(sender)),
                ticker_ready,
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
        self.inner.ticker_ready.notify_waiters();
        ticker
    }

    /// Returns `true` while the ticker (and therefore the runtime) is alive.
    pub async fn is_running(&self) -> bool {
        let lock = self.inner.runtime_ticker_ch_sender.lock().await;
        let closed = lock.as_ref().map(|s| s.is_closed()).unwrap_or(true);
        !closed
    }

    /// Obtain a clonable [`RuntimeHandle`] that broadcasts control messages to
    /// the ticker.
    pub fn handle(&self) -> Arc<RuntimeHandle> {
        Arc::new(RuntimeHandle::new(Arc::clone(
            &self.inner.control_ch_sender,
        )))
    }

    /// Send an arbitrary runtime control message.
    pub fn control(&self, msg: RuntimeControlMessage) -> CtrlFuture<'_> {
        Box::pin(async move {
            let ch = self.inner.control_ch_sender.lock().await;
            let _ = ch.send(msg).await;
        })
    }

    /// Send a custom runtime control payload.
    pub fn custom<T>(&self, message: T) -> CtrlFuture<'_>
    where
        T: std::any::Any + Send + Sync + 'static,
    {
        self.control(RuntimeControlMessage::Custom(Box::new(message)))
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
