/// Runtime-level traits and helper types.
///
/// This module defines:
/// • [`Runnable`] – abstraction for long-running async components supervised by
///   a `ProcessManager`.
/// • [`ProcessControlHandler`] – fire-and-forget interface for broadcasting
///   control messages.
/// • [`RuntimeControlMessage`] – set of built-in control messages understood by
///   the runtime helpers (`RuntimeGuard`, `RuntimeTicker`, …).
use super::RuntimeError;
use std::borrow::Cow;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Boxed future returned by [`Runnable::process_start`].
pub type ProcFuture<'a> = Pin<Box<dyn Future<Output = Result<(), RuntimeError>> + Send + 'a>>;

/// Trait implemented by every long-running asynchronous component that should
/// be supervised by a [`ProcessManager`].
///
/// The trait is **object-safe** and requires `Send + Sync + 'static`, allowing
/// implementors to be moved across tasks and shared between threads.
///
/// The lifetime parameter on [`process_start`](Runnable::process_start) lets an
/// implementor return a future that borrows from `self` if desired.
pub trait Runnable
where
    Self: Send + Sync + 'static,
{
    /// Start the component. The returned future resolves when the process ends
    /// (normally or in error).
    fn process_start(&self) -> ProcFuture<'_>;

    /// Human-readable name, used for logging only.
    fn process_name(&self) -> Cow<'static, str> {
        Cow::Borrowed(std::any::type_name::<Self>())
    }

    /// Obtain a handle for shutdown / reload signalling.
    fn process_handle(&self) -> Arc<dyn ProcessControlHandler>;
}

/// Boxed future returned by [`ProcessControlHandler`] control methods.
pub type CtrlFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// Minimal handle that external code can use to *control* a running
/// [`Runnable`].
///
/// All methods are **fire-and-forget**: they enqueue the requested control
/// instruction and return once the message has been sent.
pub trait ProcessControlHandler: Send + Sync {
    fn shutdown(&self) -> CtrlFuture<'_>;
    fn reload(&self) -> CtrlFuture<'_>;
}

pub enum ProcessOperation<T> {
    Next(T),
    Control(RuntimeControlMessage),
}

/// Built-in control messages understood by runtime helpers such as
/// [`RuntimeTicker`] and [`RuntimeGuard`].
/// Built-in control messages understood by runtime helpers such as
/// [`RuntimeGuard`](crate::RuntimeGuard) and [`RuntimeTicker`](crate::RuntimeTicker).
///
/// The enum is marked `#[non_exhaustive]`, requiring downstream crates to add a
/// wildcard arm (`_`) when pattern-matching so that new variants introduced in
/// future releases do not break compilation.
#[non_exhaustive]
#[derive(Debug)]
pub enum RuntimeControlMessage {
    /// Trigger a *hot reload*.
    Reload,
    /// Request a *graceful shutdown*.
    Shutdown,
    /// User-defined message for custom extensions.
    Custom(Box<dyn std::any::Any + Send + Sync>),
}

impl Clone for RuntimeControlMessage {
    /// Manual implementation is required because the enum is
    /// `#[non_exhaustive]`; remember to update this when adding new variants.
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

impl<R> Runnable for Arc<R>
where
    R: Runnable + ?Sized,
{
    fn process_start(&self) -> ProcFuture<'_> {
        R::process_start(self)
    }

    fn process_handle(&self) -> Arc<dyn ProcessControlHandler> {
        R::process_handle(self)
    }

    fn process_name(&self) -> Cow<'static, str> {
        R::process_name(self)
    }
}
