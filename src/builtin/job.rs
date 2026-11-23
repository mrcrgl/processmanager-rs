//! One-shot jobs; see [`Job`].
use std::{borrow::Cow, future::Future, sync::Arc};

use crate::{
    ProcFuture, ProcessControlHandler, ProcessOperation, Runnable, RuntimeControlMessage,
    RuntimeError, RuntimeGuard,
};

type JobFn = dyn Fn() -> ProcFuture<'static> + Send + Sync;

/// A [`Runnable`] that runs an async closure once and then finishes.
///
/// Inside a [`ProcessManager`](crate::ProcessManager) a job that succeeds is
/// removed while its siblings keep running. A job that fails is *required* by default:
/// its error shuts down the whole manager, like any other failing child. Mark a
/// job [`optional`](Job::optional) when its failure should only be logged.
///
/// A shutdown request cancels a job that is still running; the job then
/// reports success so it does not mask the error that caused the shutdown.
///
/// # Example
/// ```no_run
/// use processmanager::{IdleProcess, Job, ProcessManagerBuilder};
///
/// let _mgr = ProcessManagerBuilder::default()
///     // Must succeed, otherwise the manager shuts down.
///     .pre_insert(Job::new("migrate", || async {
///         // run database migrations …
///         Ok(())
///     }))
///     // Failure is logged, siblings keep running.
///     .pre_insert(
///         Job::new("warm-cache", || async {
///             // pre-fill caches …
///             Ok(())
///         })
///         .optional(),
///     )
///     .pre_insert(IdleProcess::default())
///     .build();
/// ```
pub struct Job {
    name: Cow<'static, str>,
    run: Arc<JobFn>,
    optional: bool,
    runtime_guard: RuntimeGuard,
}

impl Job {
    /// Creates a required job named `name` that runs the future returned by
    /// `f`.
    ///
    /// `f` is called once per [`Runnable::process_start`], so a job wrapped in
    /// a [`RestartSupervisor`](crate::RestartSupervisor) is retried after a
    /// failure.
    pub fn new<F, Fut>(name: impl Into<Cow<'static, str>>, f: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), RuntimeError>> + Send + 'static,
    {
        Self {
            name: name.into(),
            run: Arc::new(move || Box::pin(f()) as ProcFuture<'static>),
            optional: false,
            runtime_guard: RuntimeGuard::default(),
        }
    }

    /// Marks the job as optional: a failure is logged and the job reports
    /// success, so sibling processes keep running.
    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }
}

impl Runnable for Job {
    fn process_start(&self) -> ProcFuture<'_> {
        Box::pin(async move {
            let ticker = self.runtime_guard.runtime_ticker().await;
            let mut work = (self.run)();

            let result = loop {
                match ticker.tick(&mut work).await {
                    ProcessOperation::Next(result) => break result,
                    // Cancel the job; report success so the shutdown's cause wins.
                    ProcessOperation::Control(RuntimeControlMessage::Shutdown) => return Ok(()),
                    ProcessOperation::Control(_) => continue,
                }
            };

            match result {
                Err(err) if self.optional => {
                    let name = &self.name;
                    #[cfg(feature = "tracing")]
                    ::tracing::warn!(name = %name, "Optional job failed: {err:?}");
                    #[cfg(all(not(feature = "tracing"), feature = "log"))]
                    ::log::warn!("Optional job {name} failed: {err:?}");
                    #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
                    eprintln!("Optional job {name} failed: {err:?}");
                    Ok(())
                }
                result => result,
            }
        })
    }

    fn process_handle(&self) -> Arc<dyn ProcessControlHandler> {
        self.runtime_guard.handle()
    }

    fn process_name(&self) -> Cow<'static, str> {
        self.name.clone()
    }
}
