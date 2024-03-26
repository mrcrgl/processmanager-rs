use super::RuntimeError;
use std::future::Future;
use tokio::select;
use tokio::sync::Mutex;

#[async_trait::async_trait]
pub trait Runnable {
    async fn process_start(&self) -> Result<(), RuntimeError>;

    fn process_name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    fn process_handle(&self) -> Box<dyn ProcessControlHandler>;
}

#[async_trait::async_trait]
pub trait ProcessControlHandler where Self: Send + Sync {
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
    control_ch_receiver: Mutex<tokio::sync::mpsc::Receiver<RuntimeControlMessage>>,
    control_ch_sender: tokio::sync::mpsc::Sender<RuntimeControlMessage>,
}

impl RuntimeGuard {
    pub fn new() -> Self {
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        Self {
            control_ch_receiver: Mutex::new(receiver),
            control_ch_sender: sender,
        }
    }

    pub fn handle(&self) -> RuntimeHandle {
        RuntimeHandle {
            control_ch: Mutex::new(self.control_ch_sender.clone()),
        }
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

    pub async fn block_until_shutdown(&self) -> RuntimeControlMessage {
        loop {
            let mut lock = self.control_ch_receiver.lock().await;
            if let Some(message) = lock.recv().await {
                return message;
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
        self.control_ch
            .lock()
            .await
            .send(RuntimeControlMessage::Shutdown)
            .await
            .expect("send control message");
    }

    async fn reload(&self) {
        self.control_ch
            .lock()
            .await
            .send(RuntimeControlMessage::Reload)
            .await
            .expect("send control message");
    }
}
