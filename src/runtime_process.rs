use super::RuntimeError;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::select;
use tokio::sync::Mutex;

/// Boxed future returned by [`Runnable::process_start`].
pub type ProcFuture<'a> = Pin<Box<dyn Future<Output = Result<(), RuntimeError>> + Send + 'a>>;

/// A long-running asynchronous component managed by the `ProcessManager`.
pub trait Runnable: Send + Sync + 'static {
    /// Start the component. The returned future resolves when the process ends
    /// (normally or in error).
    fn process_start(&self) -> ProcFuture<'_>;

    /// Human-readable name, used for logging only.
    fn process_name(&self) -> String {
        std::any::type_name::<Self>().to_string()
    }

    /// Obtain a handle for shutdown / reload signalling.
    fn process_handle(&self) -> Box<dyn ProcessControlHandler>;
}

/// Boxed future returned by [`ProcessControlHandler`] control methods.
pub type CtrlFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// Handle that allows external code to control a running [`Runnable`].
pub trait ProcessControlHandler: Send + Sync {
    fn shutdown(&self) -> CtrlFuture<'_>;
    fn reload(&self) -> CtrlFuture<'_>;
}

/* ------------------------------------------------------------------------
Runtime messaging
-------------------------------------------------------------------- */

pub enum ProcessOperation<T> {
    Next(T),
    Control(RuntimeControlMessage),
}

#[derive(Debug)]
pub enum RuntimeControlMessage {
    Reload,
    Shutdown,
}

/* ------------------------------------------------------------------------
RuntimeGuard – owns the control channels
-------------------------------------------------------------------- */

pub struct RuntimeGuard {
    runtime_ticker_ch_sender: Arc<Mutex<Option<tokio::sync::mpsc::Sender<RuntimeControlMessage>>>>,
    control_ch_sender: tokio::sync::mpsc::Sender<RuntimeControlMessage>,
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
                }
            }
        });

        Self {
            runtime_ticker_ch_sender: ticker_sender,
            control_ch_sender: sender,
        }
    }

    /// Create a ticker for the caller and connect it to the control fan-out.
    pub async fn runtime_ticker(&self) -> RuntimeTicker {
        assert!(
            !self.is_running().await,
            "process already started – only one ticker allowed"
        );

        let mut lock = self.runtime_ticker_ch_sender.lock().await;
        let (ticker, sender) = RuntimeTicker::new();
        lock.replace(sender);
        ticker
    }

    pub async fn is_running(&self) -> bool {
        let lock = self.runtime_ticker_ch_sender.lock().await;
        let closed = lock.as_ref().map(|s| s.is_closed()).unwrap_or(true);
        !closed
    }

    pub fn handle(&self) -> RuntimeHandle {
        RuntimeHandle {
            control_ch: Mutex::new(self.control_ch_sender.clone()),
        }
    }

    /// Busy-wait helper for tests / demos.
    pub async fn block_until_shutdown(&self) {
        while self.is_running().await {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
}

impl Default for RuntimeGuard {
    fn default() -> Self {
        Self::new()
    }
}

/* ------------------------------------------------------------------------
RuntimeHandle – `ProcessControlHandler` impl
-------------------------------------------------------------------- */

pub struct RuntimeHandle {
    control_ch: Mutex<tokio::sync::mpsc::Sender<RuntimeControlMessage>>,
}

impl ProcessControlHandler for RuntimeHandle {
    fn shutdown(&self) -> CtrlFuture<'_> {
        Box::pin(async move {
            let ch = self.control_ch.lock().await;
            let _ = ch.send(RuntimeControlMessage::Shutdown).await;
        })
    }

    fn reload(&self) -> CtrlFuture<'_> {
        Box::pin(async move {
            let ch = self.control_ch.lock().await;
            let _ = ch.send(RuntimeControlMessage::Reload).await;
        })
    }
}

/* ------------------------------------------------------------------------
RuntimeTicker – combine user future with control channel
-------------------------------------------------------------------- */

pub struct RuntimeTicker {
    control_ch_receiver: Arc<Mutex<tokio::sync::mpsc::Receiver<RuntimeControlMessage>>>,
}

impl RuntimeTicker {
    fn new() -> (Self, tokio::sync::mpsc::Sender<RuntimeControlMessage>) {
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
