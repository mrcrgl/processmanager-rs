/// Runtime-level traits and helper types.
///
/// This module defines:
/// • [`Runnable`] – abstraction for long-running async components supervised by
///   a `ProcessManager`.
/// • [`RunnableWithContext`] – convenience trait that receives a runtime
///   context and does not require implementors to manage `RuntimeGuard`.
/// • [`ProcessControlHandler`] – fire-and-forget interface for broadcasting
///   control messages.
/// • [`RuntimeControlMessage`] – set of built-in control messages understood by
///   the runtime helpers (`RuntimeGuard`, `RuntimeTicker`, …).
use super::RuntimeError;
use std::borrow::Cow;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::{RuntimeGuard, RuntimeTicker};

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

/// Runtime context passed to [`RunnableWithContext`].
///
/// This wrapper exposes the runtime ticker and a control handle without
/// requiring each runnable to carry its own [`RuntimeGuard`].
pub struct RuntimeContext {
    ticker: RuntimeTicker,
    handle: Arc<dyn ProcessControlHandler>,
}

impl RuntimeContext {
    fn new(ticker: RuntimeTicker, handle: Arc<dyn ProcessControlHandler>) -> Self {
        Self { ticker, handle }
    }

    /// Race one unit of work against runtime control messages.
    pub async fn tick<O, Fut>(&self, fut: Fut) -> ProcessOperation<O>
    where
        Fut: Future<Output = O>,
    {
        self.ticker.tick(fut).await
    }

    /// Return the control handle associated with this runtime.
    pub fn handle(&self) -> Arc<dyn ProcessControlHandler> {
        Arc::clone(&self.handle)
    }
}

/// Convenience variant of [`Runnable`] that receives a [`RuntimeContext`].
///
/// Unlike [`Runnable`], implementors do not need to store a `RuntimeGuard` or
/// provide their own control handle. Wrap it with [`RuntimeContextRunnable`]
/// (or [`with_runtime_context`]) to use it in a `ProcessManager`.
pub trait RunnableWithContext
where
    Self: Send + Sync + 'static,
{
    fn process_start_with_context(&self, ctx: RuntimeContext) -> ProcFuture<'_>;

    fn process_name(&self) -> Cow<'static, str> {
        Cow::Borrowed(std::any::type_name::<Self>())
    }
}

/// Adapter that turns a [`RunnableWithContext`] into a regular [`Runnable`].
pub struct RuntimeContextRunnable<R>
where
    R: RunnableWithContext,
{
    inner: R,
    runtime_guard: RuntimeGuard,
}

impl<R> RuntimeContextRunnable<R>
where
    R: RunnableWithContext,
{
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            runtime_guard: RuntimeGuard::default(),
        }
    }
}

/// Wrap a [`RunnableWithContext`] for use anywhere a [`Runnable`] is expected.
pub fn with_runtime_context<R>(runnable: R) -> RuntimeContextRunnable<R>
where
    R: RunnableWithContext,
{
    RuntimeContextRunnable::new(runnable)
}

impl<R> Runnable for RuntimeContextRunnable<R>
where
    R: RunnableWithContext,
{
    fn process_start(&self) -> ProcFuture<'_> {
        Box::pin(async move {
            let ticker = self.runtime_guard.runtime_ticker().await;
            let ctx = RuntimeContext::new(ticker, self.runtime_guard.handle());
            self.inner.process_start_with_context(ctx).await
        })
    }

    fn process_name(&self) -> Cow<'static, str> {
        self.inner.process_name()
    }

    fn process_handle(&self) -> Arc<dyn ProcessControlHandler> {
        self.runtime_guard.handle()
    }
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
#[derive(Debug, Clone)]
pub enum RuntimeControlMessage {
    /// Trigger a *hot reload*.
    Reload,
    /// Request a *graceful shutdown*.
    Shutdown,
    /// User-defined message for custom extensions.
    Custom(Arc<dyn std::any::Any + Send + Sync>),
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
