//! Central runtime control hub; see [`RuntimeGuard`].
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tokio::sync::{Mutex, mpsc};

use crate::{CtrlFuture, RuntimeControlMessage, RuntimeHandle, RuntimeTicker};

/// Number of control messages buffered while the runnable is busy or not yet
/// started. Further sends wait until the ticker has consumed one.
const CONTROL_CHANNEL_CAPACITY: usize = 2;

/// Control hub for one [`Runnable`](crate::Runnable): hands out
/// [`RuntimeHandle`]s that send control messages, and the single
/// [`RuntimeTicker`] that receives them.
///
/// * [`runtime_ticker`](Self::runtime_ticker) – creates the ticker, typically
///   at the start of `process_start`. Only one ticker may be alive at a time;
///   a second one panics. Once it is dropped, a new one can be created (the
///   runnable can be started again).
/// * [`handle`](Self::handle) – a cheap, clonable [`RuntimeHandle`], usually
///   returned from `process_handle`.
/// * [`is_running`](Self::is_running) /
///   [`block_until_shutdown`](Self::block_until_shutdown) – observe whether a
///   ticker is alive, mostly for tests and demos.
///
/// Control messages are buffered until a ticker reads them, so a message
/// sent before the runnable has started (or between two runs) reaches the next
/// run. Messages a run did not read are discarded when its ticker is dropped,
/// so they don't leak into a restart. The guard spawns no task and can be
/// created outside a Tokio runtime. Clones share the same state.
///
/// ```
/// # use processmanager::*;
/// # #[tokio::main] async fn main() {
/// let guard = RuntimeGuard::new();
/// let handle = guard.handle();
/// handle.reload().await; // buffered: no ticker yet
///
/// let ticker = guard.runtime_ticker().await;
/// match ticker.tick(std::future::pending::<()>()).await {
///     ProcessOperation::Control(RuntimeControlMessage::Reload) => {}
///     _ => unreachable!(),
/// }
/// # }
/// ```
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
    /// the runnable started) are delivered to the new ticker. Messages the
    /// previous ticker left unread were discarded when it was dropped.
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
