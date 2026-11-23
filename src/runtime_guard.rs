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
//! Dropping the ticker lets [`is_running`](crate::RuntimeGuard::is_running)
//! report `false`. Creating a second ticker while one is alive is prohibited
//! and will panic.
//!
//! Control messages are buffered in the guard's channel until a ticker reads
//! them, so a message sent before the runnable has started is not lost. The
//! guard spawns no task: it can be created outside a Tokio runtime, e.g. in
//! synchronous setup code before `Runtime::block_on`.
//!
//! # Concurrency & safety
//!
//! `RuntimeGuard` is `Send + Sync`; the control receiver is protected by a
//! `tokio::sync::Mutex` and the ticker invariant by an atomic flag.
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
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tokio::sync::{Mutex, mpsc};

use crate::{CtrlFuture, RuntimeControlMessage, RuntimeHandle, RuntimeTicker};

/// Number of control messages buffered while the runnable is busy or not yet
/// started. Further sends wait until the ticker has consumed one.
const CONTROL_CHANNEL_CAPACITY: usize = 2;

#[derive(Debug, Clone)]
pub struct RuntimeGuard {
    inner: Arc<GuardInner>,
}

/// State shared by a guard, its clones and its ticker.
#[derive(Debug)]
pub(crate) struct GuardInner {
    /// Cloned into every [`RuntimeHandle`]. Holding it here also keeps the
    /// channel open for as long as the guard or its ticker exists.
    control_tx: mpsc::Sender<RuntimeControlMessage>,
    /// Read by the (single) live ticker.
    pub(crate) control_rx: Mutex<mpsc::Receiver<RuntimeControlMessage>>,
    /// `true` while a ticker exists.
    pub(crate) ticker_alive: AtomicBool,
}

impl RuntimeGuard {
    /// Create a fresh guard.
    ///
    /// This spawns no task and does not require a Tokio runtime. You typically
    /// call [`runtime_ticker`](Self::runtime_ticker) from the runnable's
    /// `process_start`.
    pub fn new() -> Self {
        let (control_tx, control_rx) = mpsc::channel(CONTROL_CHANNEL_CAPACITY);
        Self {
            inner: Arc::new(GuardInner {
                control_tx,
                control_rx: Mutex::new(control_rx),
                ticker_alive: AtomicBool::new(false),
            }),
        }
    }

    /// Create the **single** [`RuntimeTicker`].
    ///
    /// Control messages sent before this call (e.g. a shutdown requested before
    /// the runnable started) are delivered to the new ticker.
    ///
    /// # Panics
    /// Panics if a ticker for this guard is still alive – i.e. the runtime is
    /// considered “running”.
    pub async fn runtime_ticker(&self) -> RuntimeTicker {
        let already_running = self.inner.ticker_alive.swap(true, Ordering::SeqCst);
        assert!(
            !already_running,
            "process already started – only one ticker allowed"
        );
        RuntimeTicker::new(Arc::clone(&self.inner))
    }

    /// Returns `true` while the ticker (and therefore the runtime) is alive.
    pub async fn is_running(&self) -> bool {
        self.inner.ticker_alive.load(Ordering::SeqCst)
    }

    /// Obtain a clonable [`RuntimeHandle`] that sends control messages to the
    /// ticker.
    pub fn handle(&self) -> Arc<RuntimeHandle> {
        Arc::new(RuntimeHandle::new(self.inner.control_tx.clone()))
    }

    /// Send an arbitrary runtime control message.
    pub fn control(&self, msg: RuntimeControlMessage) -> CtrlFuture<'_> {
        Box::pin(async move {
            // Cannot fail: the guard itself keeps the receiver alive.
            let _ = self.inner.control_tx.send(msg).await;
        })
    }

    /// Send a custom runtime control payload.
    pub fn custom<T>(&self, message: T) -> CtrlFuture<'_>
    where
        T: std::any::Any + Send + Sync + 'static,
    {
        self.control(RuntimeControlMessage::Custom(Arc::new(message)))
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
