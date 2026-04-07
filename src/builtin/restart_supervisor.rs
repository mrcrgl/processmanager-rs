//! Restart supervisor for flaky child processes.
//!
//! `RestartSupervisor` supervises one child [`Runnable`]. When the child exits
//! with an error (or panics), the supervisor restarts it after an exponential
//! backoff delay.
//!
//! This is useful for services that should auto-recover from transient
//! failures.
use std::{borrow::Cow, sync::Arc, time::Duration};

use crate::{
    ProcFuture, ProcessControlHandler, ProcessOperation, Runnable, RuntimeControlMessage,
    RuntimeError, RuntimeGuard,
};

/// Exponential backoff settings for [`RestartSupervisor`].
#[derive(Debug, Clone, Copy)]
pub struct RestartBackoff {
    /// Delay used after the first failure.
    pub initial: Duration,
    /// Maximum delay cap.
    pub max: Duration,
    /// Multiplication factor for subsequent failures.
    pub factor: u32,
}

impl RestartBackoff {
    pub fn new(initial: Duration, max: Duration, factor: u32) -> Self {
        Self {
            initial,
            max,
            factor: factor.max(1),
        }
    }

    fn delay_for_failures(self, failures: u32) -> Duration {
        if failures == 0 {
            return Duration::ZERO;
        }

        let mut delay = self.initial;
        for _ in 1..failures {
            delay = delay.saturating_mul(self.factor);
            if delay >= self.max {
                return self.max;
            }
        }
        delay.min(self.max)
    }
}

impl Default for RestartBackoff {
    fn default() -> Self {
        Self {
            initial: Duration::from_millis(200),
            max: Duration::from_secs(30),
            factor: 2,
        }
    }
}

/// Wraps one child [`Runnable`] and restarts it after failures with backoff.
pub struct RestartSupervisor {
    child: Arc<dyn Runnable>,
    child_handle: Arc<dyn ProcessControlHandler>,
    runtime_guard: RuntimeGuard,
    backoff: RestartBackoff,
    control_poll: Duration,
}

impl RestartSupervisor {
    pub fn new(child: impl Runnable) -> Self {
        let child: Arc<dyn Runnable> = Arc::from(Box::new(child) as Box<dyn Runnable>);
        let child_handle = child.process_handle();
        Self {
            child,
            child_handle,
            runtime_guard: RuntimeGuard::default(),
            backoff: RestartBackoff::default(),
            control_poll: Duration::from_millis(50),
        }
    }

    pub fn backoff(mut self, backoff: RestartBackoff) -> Self {
        self.backoff = backoff;
        self
    }
}

impl Runnable for RestartSupervisor {
    fn process_start(&self) -> ProcFuture<'_> {
        let child = Arc::clone(&self.child);
        let child_handle = Arc::clone(&self.child_handle);
        let backoff = self.backoff;
        let control_poll = self.control_poll;
        let guard = self.runtime_guard.clone();
        let child_name = child.process_name().to_string();

        Box::pin(async move {
            let ticker = guard.runtime_ticker().await;
            let mut failures = 0_u32;

            loop {
                let mut child_task = tokio::spawn({
                    let child = Arc::clone(&child);
                    async move { child.process_start().await }
                });

                // Child run-loop. React to control messages while the child is alive.
                let child_result = loop {
                    match ticker
                        .tick(tokio::time::timeout(control_poll, &mut child_task))
                        .await
                    {
                        ProcessOperation::Next(Ok(join_result)) => break join_result,
                        ProcessOperation::Next(Err(_timeout)) => continue,
                        ProcessOperation::Control(RuntimeControlMessage::Shutdown) => {
                            child_handle.shutdown().await;
                            let _ = child_task.await;
                            return Ok(());
                        }
                        ProcessOperation::Control(RuntimeControlMessage::Reload) => {
                            child_handle.reload().await;
                        }
                        ProcessOperation::Control(_) => {}
                    }
                };

                let failure = match child_result {
                    Ok(Ok(())) => {
                        // Child stopped successfully; supervisor also exits successfully.
                        return Ok(());
                    }
                    Ok(Err(err)) => err,
                    Err(join_err) => RuntimeError::Internal {
                        message: format!(
                            "restart supervisor child `{child_name}` join failure: {join_err}"
                        ),
                    },
                };

                failures = failures.saturating_add(1);
                let delay = backoff.delay_for_failures(failures);

                #[cfg(feature = "tracing")]
                ::tracing::warn!(
                    child = %child_name,
                    failures = failures,
                    backoff = ?delay,
                    "Child failed; restarting after backoff: {failure:?}"
                );
                #[cfg(all(not(feature = "tracing"), feature = "log"))]
                ::log::warn!(
                    "Child {child_name} failed ({failure:?}); restarting in {delay:?} (attempt {failures})"
                );
                #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
                eprintln!(
                    "Child {child_name} failed ({failure:?}); restarting in {delay:?} (attempt {failures})"
                );

                // Backoff wait remains responsive to shutdown/reload.
                match ticker.tick(tokio::time::sleep(delay)).await {
                    ProcessOperation::Control(RuntimeControlMessage::Shutdown) => {
                        return Ok(());
                    }
                    ProcessOperation::Control(RuntimeControlMessage::Reload) => {
                        child_handle.reload().await;
                    }
                    ProcessOperation::Control(_) | ProcessOperation::Next(_) => {}
                }
            }
        })
    }

    fn process_handle(&self) -> Arc<dyn ProcessControlHandler> {
        self.runtime_guard.handle()
    }

    fn process_name(&self) -> Cow<'static, str> {
        Cow::Borrowed("RestartSupervisor")
    }
}

#[deprecated(note = "use `RestartSupervisor` instead")]
pub type RestartWrapper = RestartSupervisor;
