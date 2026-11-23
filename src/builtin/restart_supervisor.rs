//! Restart supervisor for flaky child processes; see [`RestartSupervisor`].
use std::{
    borrow::Cow,
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use tokio::{
    task::{JoinError, JoinHandle},
    time::Instant,
};

use crate::{
    ProcFuture, ProcessControlHandler, ProcessOperation, Runnable, RuntimeControlMessage,
    RuntimeError, RuntimeGuard,
};

/// A [`JoinHandle`] that aborts its task when dropped.
///
/// Dropping a plain `JoinHandle` detaches the task; this wrapper makes the
/// task's lifetime follow the supervisor's future instead.
struct AbortOnDrop<T>(JoinHandle<T>);

impl<T> Future for AbortOnDrop<T> {
    type Output = Result<T, JoinError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(cx)
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        // No-op if the task has already finished.
        self.0.abort();
    }
}

/// Returned (as [`RuntimeError::Failed`]) when a [`RestartSupervisor`] gives
/// up because its child exceeded the
/// [`restart_limit`](RestartSupervisor::restart_limit).
///
/// The child's last error is available as [`source`](std::error::Error::source).
///
/// ```
/// # use processmanager::{RestartLimitExceeded, RuntimeError};
/// fn gave_up(err: &RuntimeError) -> Option<&RestartLimitExceeded> {
///     err.downcast_ref::<RestartLimitExceeded>()
/// }
/// ```
#[derive(Debug)]
pub struct RestartLimitExceeded {
    child: String,
    failures: usize,
    window: Duration,
    last_error: RuntimeError,
}

impl RestartLimitExceeded {
    /// Name of the supervised child.
    pub fn child(&self) -> &str {
        &self.child
    }

    /// Failures counted within [`window`](Self::window).
    pub fn failures(&self) -> usize {
        self.failures
    }

    /// The configured time window.
    pub fn window(&self) -> Duration {
        self.window
    }

    /// The child's last error.
    pub fn last_error(&self) -> &RuntimeError {
        &self.last_error
    }
}

impl std::fmt::Display for RestartLimitExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "restart supervisor gave up on `{}` after {} failures within {:?}",
            self.child, self.failures, self.window
        )
    }
}

impl std::error::Error for RestartLimitExceeded {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.last_error)
    }
}

/// Exponential backoff settings for [`RestartSupervisor`].
#[derive(Debug, Clone, Copy)]
pub struct RestartBackoff {
    /// Delay used after the first failure.
    pub initial: Duration,
    /// Maximum delay cap.
    pub max: Duration,
    /// Multiplication factor for subsequent failures. [`RestartBackoff::new`]
    /// clamps it to at least 1; set directly, `0` behaves like `1`.
    pub factor: u32,
}

impl RestartBackoff {
    /// Backoff starting at `initial`, multiplied by `factor` after each further
    /// failure and capped at `max`. A `factor` below 1 is treated as 1
    /// (constant delay).
    pub fn new(initial: Duration, max: Duration, factor: u32) -> Self {
        Self {
            initial,
            max,
            factor: factor.max(1),
        }
    }

    /// `initial * factor^(failures - 1)`, capped at `max`; O(1) for any
    /// number of failures.
    fn delay_for_failures(self, failures: u32) -> Duration {
        if failures == 0 || self.initial.is_zero() {
            return Duration::ZERO;
        }
        // In u128 nanoseconds: any overflow is far beyond every `max`.
        let factor = u128::from(self.factor.max(1));
        factor
            .checked_pow(failures - 1)
            .and_then(|multiplier| self.initial.as_nanos().checked_mul(multiplier))
            .filter(|&nanos| nanos < self.max.as_nanos())
            .map_or(self.max, |nanos| {
                // Below `max`, so the seconds fit in a u64.
                Duration::new(
                    (nanos / 1_000_000_000) as u64,
                    (nanos % 1_000_000_000) as u32,
                )
            })
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

/// Limit on how often a child may fail within a sliding time window.
#[derive(Debug, Clone, Copy)]
struct RestartLimit {
    max_restarts: u32,
    window: Duration,
}

/// Default for [`RestartSupervisor::reset_after`].
const DEFAULT_RESET_AFTER: Duration = Duration::from_secs(60);

/// Wraps one child [`Runnable`] and restarts it after failures with backoff.
///
/// `RestartSupervisor` supervises one child [`Runnable`]. When the child exits
/// with an error (or panics), the supervisor restarts it after an exponential
/// backoff delay.
///
/// This is useful for services that should auto-recover from transient
/// failures.
///
/// Two knobs turn this into a crash policy:
///
/// * [`RestartSupervisor::restart_limit`] gives up once the child fails too
///   often within a time window. The supervisor then returns an error, which
///   makes a parent [`ProcessManager`](crate::ProcessManager) shut down all
///   sibling processes.
/// * [`RestartSupervisor::reset_after`] resets the backoff once the child has
///   run long enough, so a crash after a long healthy run is restarted quickly
///   again.
///
/// The child runs in its own task. If the supervisor is dropped or aborted
/// (e.g. by a parent manager after its shutdown grace period), that task is
/// aborted too, so no child outlives its supervisor.
///
/// On shutdown the supervisor forwards the request and waits for the child to
/// stop, without a time limit of its own. Inside a
/// [`ProcessManager`](crate::ProcessManager) that wait is bounded by the
/// manager's shutdown grace period; when used on its own, wrap the
/// supervisor's future in a timeout.
pub struct RestartSupervisor {
    child: Arc<dyn Runnable>,
    child_handle: Arc<dyn ProcessControlHandler>,
    runtime_guard: RuntimeGuard,
    backoff: RestartBackoff,
    restart_limit: Option<RestartLimit>,
    reset_after: Duration,
}

impl std::fmt::Debug for RestartSupervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RestartSupervisor")
            .field("child", &self.child.process_name())
            .field("backoff", &self.backoff)
            .field("restart_limit", &self.restart_limit)
            .field("reset_after", &self.reset_after)
            .finish_non_exhaustive()
    }
}

impl RestartSupervisor {
    /// Wraps `child`, restarting it after every failure with the default
    /// [`RestartBackoff`], no restart limit and a backoff reset after
    /// 60 seconds of healthy runtime.
    pub fn new(child: impl Runnable) -> Self {
        let child: Arc<dyn Runnable> = Arc::from(Box::new(child) as Box<dyn Runnable>);
        let child_handle = child.process_handle();
        Self {
            child,
            child_handle,
            runtime_guard: RuntimeGuard::default(),
            backoff: RestartBackoff::default(),
            restart_limit: None,
            reset_after: DEFAULT_RESET_AFTER,
        }
    }

    /// Sets the backoff applied between restarts.
    #[must_use = "builder methods return the modified supervisor"]
    pub fn backoff(mut self, backoff: RestartBackoff) -> Self {
        self.backoff = backoff;
        self
    }

    /// Gives up once the child has failed more than `max_restarts` times
    /// within the sliding `window`.
    ///
    /// When the limit is exceeded the supervisor stops restarting and returns
    /// a [`RuntimeError::Failed`] wrapping [`RestartLimitExceeded`], which
    /// names the child and keeps its last error as `source`. Inside a
    /// [`ProcessManager`](crate::ProcessManager) this shuts down all sibling
    /// processes.
    ///
    /// `max_restarts == 0` means the first failure is final. Without this
    /// setting the child is restarted indefinitely.
    #[must_use = "builder methods return the modified supervisor"]
    pub fn restart_limit(mut self, max_restarts: u32, window: Duration) -> Self {
        self.restart_limit = Some(RestartLimit {
            max_restarts,
            window,
        });
        self
    }

    /// Resets the backoff to [`RestartBackoff::initial`] when the child ran
    /// for at least `duration` before it failed.
    ///
    /// Defaults to 60 seconds.
    #[must_use = "builder methods return the modified supervisor"]
    pub fn reset_after(mut self, duration: Duration) -> Self {
        self.reset_after = duration;
        self
    }
}

impl Runnable for RestartSupervisor {
    fn process_start(&self) -> ProcFuture<'_> {
        let child = Arc::clone(&self.child);
        let child_handle = Arc::clone(&self.child_handle);
        let backoff = self.backoff;
        let restart_limit = self.restart_limit;
        let reset_after = self.reset_after;
        let guard = self.runtime_guard.clone();
        let child_name = child.process_name().to_string();

        Box::pin(async move {
            let ticker = guard.runtime_ticker().await;
            let mut failures = 0_u32;
            let mut failure_times: VecDeque<Instant> = VecDeque::new();

            loop {
                let started = Instant::now();
                let mut child_task = AbortOnDrop(tokio::spawn({
                    let child = Arc::clone(&child);
                    async move { child.process_start().await }
                }));

                // Child run-loop. React to control messages while the child is alive.
                let child_result = loop {
                    // `&mut JoinHandle` is cancel-safe: a control message
                    // drops only the borrow, not the child task.
                    match ticker.tick(&mut child_task).await {
                        ProcessOperation::Next(join_result) => break join_result,
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
                    Err(join_err) => RuntimeError::failed(join_err),
                };

                let now = Instant::now();
                if now.duration_since(started) >= reset_after {
                    failures = 0;
                }

                if let Some(limit) = restart_limit {
                    failure_times.push_back(now);
                    while failure_times
                        .front()
                        .is_some_and(|t| now.duration_since(*t) > limit.window)
                    {
                        failure_times.pop_front();
                    }
                    if failure_times.len() > limit.max_restarts as usize {
                        return Err(RuntimeError::failed(RestartLimitExceeded {
                            child: child_name,
                            failures: failure_times.len(),
                            window: limit.window,
                            last_error: failure,
                        }));
                    }
                }

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
        Cow::Owned(format!("RestartSupervisor({})", self.child.process_name()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The loop this formula replaced, kept as the reference.
    fn reference_delay(b: RestartBackoff, failures: u32) -> Duration {
        if failures == 0 {
            return Duration::ZERO;
        }
        let mut delay = b.initial;
        for _ in 1..failures {
            delay = delay.saturating_mul(b.factor.max(1));
            if delay >= b.max {
                return b.max;
            }
        }
        delay.min(b.max)
    }

    #[test]
    fn closed_form_matches_the_iterative_backoff() {
        let configs = [
            RestartBackoff::default(),
            RestartBackoff::new(Duration::from_millis(1), Duration::from_secs(3600), 3),
            RestartBackoff::new(Duration::from_millis(10), Duration::from_secs(1), 1),
            RestartBackoff::new(Duration::ZERO, Duration::from_secs(1), 2),
            RestartBackoff::new(Duration::from_secs(1), Duration::from_millis(500), 2),
            RestartBackoff {
                initial: Duration::from_millis(7),
                max: Duration::from_secs(60),
                factor: 0,
            },
            RestartBackoff::new(Duration::from_nanos(1), Duration::MAX, u32::MAX),
        ];
        for b in configs {
            for failures in 0..200 {
                assert_eq!(
                    b.delay_for_failures(failures),
                    reference_delay(b, failures),
                    "{b:?}, failures={failures}"
                );
            }
        }
    }

    #[test]
    fn huge_failure_counts_are_capped() {
        let b = RestartBackoff::default();
        assert_eq!(b.delay_for_failures(u32::MAX), b.max);
        let constant = RestartBackoff::new(Duration::from_millis(5), Duration::from_secs(1), 1);
        assert_eq!(
            constant.delay_for_failures(u32::MAX),
            Duration::from_millis(5)
        );
    }
}
