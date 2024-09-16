use super::RuntimeError;
use std::future::Future;
use std::sync::Arc;
use tokio::select;
use tokio::sync::Mutex;

#[async_trait::async_trait]
pub trait Runnable {
    async fn process_start(&self) -> Result<(), RuntimeError>;

    fn process_name(&self) -> String {
        std::any::type_name::<Self>().to_string()
    }

    fn process_handle(&self) -> Box<dyn ProcessControlHandler>;
}

#[async_trait::async_trait]
pub trait ProcessControlHandler
where
    Self: Send + Sync,
{
    async fn shutdown(&self);

    async fn reload(&self);
}

pub enum ProcessOperation<T> {
    Next(T),
    Control(RuntimeControlMessage),
}

#[derive(Debug)]
pub enum RuntimeControlMessage {
    Reload,
    Shutdown,
}

pub struct RuntimeGuard {
    runtime_ticker_ch_sender: Arc<Mutex<Option<tokio::sync::mpsc::Sender<RuntimeControlMessage>>>>,
    control_ch_sender: tokio::sync::mpsc::Sender<RuntimeControlMessage>,
}

impl RuntimeGuard {
    pub fn new() -> Self {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);

        let ticker_ch_sender: Arc<Mutex<Option<tokio::sync::mpsc::Sender<RuntimeControlMessage>>>> =
            Arc::new(Mutex::new(None));
        let runtime_ticker_ch_sender = ticker_ch_sender.clone();
        tokio::task::spawn(async move {
            loop {
                if let Some(message) = receiver.recv().await {
                    let mut lock = runtime_ticker_ch_sender.lock().await;

                    if let Some(sender) = lock.as_mut() {
                        if sender.is_closed() {
                            // If the ticker sender is closed, we break the loop which
                            // brings `receiver` out of scope and closed the `RuntimeGuard`.
                            // Consecutive `shutdown` resolve the future immediately.
                            break;
                        }

                        sender.send(message).await.unwrap();
                    }
                }
            }
        });

        Self {
            runtime_ticker_ch_sender: ticker_ch_sender,
            control_ch_sender: sender,
        }
    }

    pub fn handle(&self) -> RuntimeHandle {
        RuntimeHandle {
            control_ch: Mutex::new(self.control_ch_sender.clone()),
        }
    }

    pub async fn runtime_ticker(&self) -> RuntimeTicker {
        assert!(!self.is_running().await, "process already started");

        let mut lock = self.runtime_ticker_ch_sender.lock().await;

        let (ticker, sender) = RuntimeTicker::new();

        lock.replace(sender);

        ticker
    }

    pub async fn is_running(&self) -> bool {
        let lock = self.runtime_ticker_ch_sender.lock().await;

        let ch_closed = lock.as_ref().map(|ch| ch.is_closed()).unwrap_or(true);

        !ch_closed
    }

    pub async fn block_until_shutdown(&self) {
        loop {
            if !self.is_running().await {
                break;
            }
        }
    }
}

impl Default for RuntimeGuard {
    fn default() -> Self {
        Self::new()
    }
}

pub struct RuntimeHandle {
    control_ch: Mutex<tokio::sync::mpsc::Sender<RuntimeControlMessage>>,
}

#[async_trait::async_trait]
impl ProcessControlHandler for RuntimeHandle {
    async fn shutdown(&self) {
        let ch = self.control_ch.lock().await;
        if !ch.is_closed() {
            ch.send(RuntimeControlMessage::Shutdown)
                .await
                .expect("send control message")
        }
    }

    async fn reload(&self) {
        let ch = self.control_ch.lock().await;
        if !ch.is_closed() {
            ch.send(RuntimeControlMessage::Reload)
                .await
                .expect("send control message")
        }
    }
}

pub struct RuntimeTicker {
    control_ch_receiver: Mutex<tokio::sync::mpsc::Receiver<RuntimeControlMessage>>,
}

impl RuntimeTicker {
    fn new() -> (Self, tokio::sync::mpsc::Sender<RuntimeControlMessage>) {
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        (
            Self {
                control_ch_receiver: Mutex::new(receiver),
            },
            sender,
        )
    }

    pub async fn tick<O, Fut>(&self, fut: Fut) -> ProcessOperation<O>
    where
        Fut: Future<Output = O>,
    {
        let mut lock = self.control_ch_receiver.lock().await;

        let response = select! {
            res = fut => ProcessOperation::Next(res),
            control = lock.recv() => match control {
                Some(message) => ProcessOperation::Control(message),
                None => unimplemented!()
            },
        };

        drop(lock);

        response
    }
}
