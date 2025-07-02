use std::sync::Arc;

use tokio::{select, sync::Mutex};

use crate::{ProcessOperation, RuntimeControlMessage};

pub struct RuntimeTicker {
    control_ch_receiver: Arc<Mutex<tokio::sync::mpsc::Receiver<RuntimeControlMessage>>>,
}

unsafe impl Send for RuntimeTicker {}
unsafe impl Sync for RuntimeTicker {}

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

    /// Await `fut` and the control channel concurrently, returning whichever
    /// completes first.
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
