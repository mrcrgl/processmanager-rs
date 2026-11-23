//! Cooperative work / control multiplexer.
//!
//! A `RuntimeTicker` receives **control messages** from the runtime and
//! concurrently drives a caller-supplied *work future*. Call
//! [`tick`](crate::RuntimeTicker::tick) with the future that represents *one unit of user
//! work*; the method races it against incoming
//! [`RuntimeControlMessage`](crate::RuntimeControlMessage)s and
//! returns a [`ProcessOperation`] that tells the caller what happened first.
//!
//! The ticker is created internally by
//! [`RuntimeGuard::runtime_ticker`](crate::RuntimeGuard::runtime_ticker). Only
//! one ticker may exist at any time.
use std::{
    future::Future,
    sync::{Arc, atomic::Ordering},
};

use tokio::select;

use crate::{ProcessOperation, runtime_guard::GuardInner};

/// Receiving end of a [`RuntimeGuard`](crate::RuntimeGuard): races the
/// runnable's work against incoming control messages via
/// [`tick`](Self::tick).
///
/// Created by [`RuntimeGuard::runtime_ticker`](crate::RuntimeGuard::runtime_ticker);
/// only one may exist per guard at a time.
#[derive(Debug)]
pub struct RuntimeTicker {
    /// Shared with the guard. Holding it keeps the control channel open (the
    /// guard owns a sender), so `tick` never sees a closed channel.
    guard: Arc<GuardInner>,
}

impl RuntimeTicker {
    pub(crate) fn new(guard: Arc<GuardInner>) -> Self {
        Self { guard }
    }

    /// Race `fut` against the next control message and return the winner.
    ///
    /// Only one mutex lock is taken per call to inspect the control channel.
    /// The returned [`ProcessOperation`] describes what completed first:
    /// * `ProcessOperation::Next(res)` – the work future finished and yielded `res`.
    /// * `ProcessOperation::Control(msg)` – a control instruction (`Reload`, `Shutdown`, …) was received.
    pub async fn tick<O, Fut>(&self, fut: Fut) -> ProcessOperation<O>
    where
        Fut: Future<Output = O>,
    {
        let mut control_rx = self.guard.control_rx.lock().await;

        select! {
            res = fut => ProcessOperation::Next(res),
            // The channel cannot close while this ticker exists (see `guard`);
            // should it ever, the branch is disabled and only `fut` is awaited.
            Some(msg) = control_rx.recv() => ProcessOperation::Control(msg),
        }
    }
}

impl Drop for RuntimeTicker {
    fn drop(&mut self) {
        // Messages this run never read were meant for it, not for the next
        // run (#72). Drained before the next ticker can be created. The lock
        // is free: `tick` borrows the ticker, so none can be in progress.
        if let Ok(mut control_rx) = self.guard.control_rx.try_lock() {
            while control_rx.try_recv().is_ok() {}
        }
        self.guard.ticker_alive.store(false, Ordering::SeqCst);
    }
}
