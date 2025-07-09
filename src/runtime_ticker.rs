//! Cooperative work / control multiplexer.
//!
//! A `RuntimeTicker` receives **control messages** from the runtime and
//! concurrently drives a caller-supplied *work future*. Call
//! [`tick`](Self::tick) with the future that represents *one unit of user
//! work*; the method races it against incoming [`RuntimeControlMessage`]s and
//! returns a [`ProcessOperation`] that tells the caller what happened first.
//!
//! The ticker is created internally by [`RuntimeGuard::runtime_ticker`]. Only
//! one ticker may exist at any time.
use std::future::Future;
use std::sync::Arc;

use tokio::{select, sync::Mutex};

use crate::{ProcessOperation, RuntimeControlMessage};

pub struct RuntimeTicker {
    control_ch_receiver: Arc<Mutex<tokio::sync::mpsc::Receiver<RuntimeControlMessage>>>,
}

impl RuntimeTicker {
    pub(crate) fn new() -> (Self, tokio::sync::mpsc::Sender<RuntimeControlMessage>) {
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        (
            Self {
                control_ch_receiver: Arc::new(Mutex::new(receiver)),
            },
            sender,
        )
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
        let mut lock = self.control_ch_receiver.lock().await;

        select! {
            res = fut                => ProcessOperation::Next(res),
            msg = lock.recv() => match msg {
                Some(c) => ProcessOperation::Control(c),
                None    => unreachable!("control channel closed unexpectedly"),
            },
        }
    }
}
