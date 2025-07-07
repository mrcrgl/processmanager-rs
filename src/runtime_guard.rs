use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tokio::sync::{Mutex, Notify, mpsc};

use crate::{CtrlFuture, ProcessControlHandler, RuntimeControlMessage, RuntimeTicker};

/// Cheap run-state guard for long-running processes.
///
/// A `RuntimeGuard`
/// * acts as the fan-out hub for *reload* / *shutdown* control messages,
/// * provides the optional [`RuntimeTicker`] helper,
/// * offers cheap, lock-free `is_running()` checks, and
/// * allows external code to wait for graceful shutdown without busy-loops.
pub struct RuntimeGuard {
    inner: Arc<Inner>,
}

struct Inner {
    /// Central control channel every [`ProcessControlHandler`] writes to.
    control_tx: Mutex<mpsc::Sender<RuntimeControlMessage>>,

    /// Filled lazily once a ticker is requested.
    ticker_tx: Mutex<Option<mpsc::Sender<RuntimeControlMessage>>>,

    /// `true` while the process **should** keep running.
    shutdown: AtomicBool,

    /// Notifies waiters when `shutdown` flips to `false`.
    notify: Notify,
}

// SAFETY: interior mutability is protected by async primitives.
unsafe impl Send for RuntimeGuard {}
unsafe impl Sync for RuntimeGuard {}

impl RuntimeGuard {
    /// Construct a new guard in the *running* state.
    pub fn new() -> Self {
        // central fan-in channel: ProcessControlHandler → guard task
        let (control_tx, mut control_rx) = mpsc::channel::<RuntimeControlMessage>(1);

        let inner = Arc::new(Inner {
            control_tx: Mutex::new(control_tx),
            ticker_tx: Mutex::new(None),
            shutdown: AtomicBool::new(true),
            notify: Notify::new(),
        });

        // Fan-out task: forward control messages to the (single) ticker,
        // exit automatically once `shutdown` becomes false.
        let fanout_inner = Arc::clone(&inner);
        tokio::spawn(async move {
            while let Some(msg) = control_rx.recv().await {
                let shutdown_requested = matches!(msg, RuntimeControlMessage::Shutdown);

                // Forward to ticker if one exists
                if let Some(ref tx) = *fanout_inner.ticker_tx.lock().await {
                    let _ = tx.send(msg).await;
                }

                if shutdown_requested {
                    fanout_inner.shutdown.store(false, Ordering::Release);
                    fanout_inner.notify.notify_waiters();
                    break; // nothing more to route
                }
            }
        });

        Self { inner }
    }

    /// Create a `RuntimeTicker` for the caller and connect it to the fan-out.
    ///
    /// Panics if invoked more than once.
    pub async fn runtime_ticker(&self) -> RuntimeTicker {
        assert!(
            self.is_running(),
            "process already shut down – ticker no longer available"
        );

        let mut lock = self.inner.ticker_tx.lock().await;
        assert!(lock.is_none(), "only one ticker allowed");

        let ticker = RuntimeTicker::new();
        *lock = Some(ticker.sender());
        ticker
    }

    /// Returns `false` once a graceful shutdown has been requested.
    #[inline]
    pub fn is_running(&self) -> bool {
        self.inner.shutdown.load(Ordering::Acquire)
    }

    /// Non-blocking handle creation (cheap `Arc` clone).
    pub fn handle(&self) -> Arc<dyn ProcessControlHandler> {
        Arc::new(Handle {
            inner: Arc::clone(&self.inner),
        })
    }

    /// Wait until `shutdown` is observed.
    ///
    /// Useful in demos & tests; production code typically awaits the main
    /// process future instead.
    pub async fn block_until_shutdown(&self) {
        if self.is_running() {
            self.inner.notify.notified().await;
        }
    }
}

impl Default for RuntimeGuard {
    fn default() -> Self {
        Self::new()
    }
}

/* ========================================================================= */
/* ProcessControlHandler implementation                                      */
/* ========================================================================= */

struct Handle {
    inner: Arc<Inner>,
}

impl ProcessControlHandler for Handle {
    fn shutdown(&self) -> CtrlFuture<'_> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            {
                let tx = inner.control_tx.lock().await;
                // ignore errors – receiver might have gone already
                let _ = tx.send(RuntimeControlMessage::Shutdown).await;
            }
            // ensure flag flips even if fan-out task is gone
            inner.shutdown.store(false, Ordering::Release);
            inner.notify.notify_waiters();
        })
    }

    fn reload(&self) -> CtrlFuture<'_> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            let tx = inner.control_tx.lock().await;
            let _ = tx.send(RuntimeControlMessage::Reload).await;
        })
    }
}
