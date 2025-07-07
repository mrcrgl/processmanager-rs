use super::RuntimeError;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Boxed future returned by [`Runnable::process_start`].
pub type ProcFuture<'a> = Pin<Box<dyn Future<Output = Result<(), RuntimeError>> + Send + 'a>>;

/// A long-running asynchronous component managed by the `ProcessManager`.
pub trait Runnable
where
    Self: Send + Sync + 'static,
{
    /// Start the component. The returned future resolves when the process ends
    /// (normally or in error).
    fn process_start(&self) -> ProcFuture<'_>;

    /// Human-readable name, used for logging only.
    fn process_name(&self) -> String {
        std::any::type_name::<Self>().to_string()
    }

    /// Obtain a handle for shutdown / reload signalling.
    fn process_handle(&self) -> Arc<dyn ProcessControlHandler>;
}

/// Boxed future returned by [`ProcessControlHandler`] control methods.
pub type CtrlFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// Handle that allows external code to control a running [`Runnable`].
pub trait ProcessControlHandler: Send + Sync {
    fn shutdown(&self) -> CtrlFuture<'_>;
    fn reload(&self) -> CtrlFuture<'_>;
}

pub enum ProcessOperation<T> {
    Next(T),
    Control(RuntimeControlMessage),
}

#[derive(Debug)]
pub enum RuntimeControlMessage {
    Reload,
    Shutdown,
    /// User-defined messages for future extensibility.
    Custom(Box<dyn std::any::Any + Send + Sync>),
}

impl Clone for RuntimeControlMessage {
    fn clone(&self) -> Self {
        match self {
            RuntimeControlMessage::Reload => RuntimeControlMessage::Reload,
            RuntimeControlMessage::Shutdown => RuntimeControlMessage::Shutdown,
            RuntimeControlMessage::Custom(_) => {
                panic!("Cloning `Custom` control messages is not supported")
            }
        }
    }
}
