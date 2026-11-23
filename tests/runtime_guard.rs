//! `RuntimeGuard` / `RuntimeTicker` contracts that don't need a full manager.

use std::sync::Arc;
use std::time::Duration;

use processmanager::{
    IdleProcess, Job, ProcFuture, ProcessControlHandler, ProcessManagerBuilder, ProcessOperation,
    RestartSupervisor, Runnable, RunnableWithContext, RuntimeContext, RuntimeGuard,
    with_runtime_context,
};
use tokio::time::timeout;

struct ContextNoop;

impl RunnableWithContext for ContextNoop {
    fn process_start_with_context(&self, _ctx: RuntimeContext) -> ProcFuture<'_> {
        Box::pin(async { Ok(()) })
    }
}

/// #65: building runnables and a manager must not require a Tokio runtime.
#[test]
fn constructors_work_outside_a_runtime() {
    let _guard = RuntimeGuard::new();
    let _guard = RuntimeGuard::default();
    let _idle = IdleProcess::new();
    let _job = Job::new("job", || async { Ok(()) });
    let _sup = RestartSupervisor::new(IdleProcess::new());
    let _ctx = with_runtime_context(ContextNoop);
    #[cfg(feature = "signal")]
    let _signals = processmanager::builtin::SignalReceiver::new();
}

/// #65: the common pattern — build in sync code, then run on a runtime.
#[test]
fn manager_built_before_runtime_runs_and_stops() {
    let manager = ProcessManagerBuilder::default()
        .pre_insert(IdleProcess::new())
        .pre_insert(Job::new("job", || async { Ok(()) }))
        .build();
    let handle = manager.process_handle();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async move {
        let run = tokio::spawn(async move { manager.process_start().await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        handle.shutdown().await;
        timeout(Duration::from_secs(2), run)
            .await
            .expect("manager must stop")
            .expect("manager task panicked")
            .expect("clean shutdown");
    });
}

/// #66: a ticker must not panic once its guard and all handles are gone.
#[tokio::test]
async fn tick_after_guard_dropped_does_not_panic() {
    let guard = RuntimeGuard::new();
    let ticker = guard.runtime_ticker().await;
    drop(guard);

    match ticker.tick(async { 7 }).await {
        ProcessOperation::Next(v) => assert_eq!(v, 7),
        ProcessOperation::Control(_) => panic!("no control message was sent"),
    }
    assert!(
        timeout(
            Duration::from_millis(50),
            ticker.tick(std::future::pending::<()>())
        )
        .await
        .is_err(),
        "with no work and no control messages, tick keeps waiting"
    );
}

/// A message sent before the ticker exists is delivered once it does.
#[tokio::test]
async fn control_sent_before_ticker_is_delivered() {
    let guard = RuntimeGuard::new();
    let handle = guard.handle();
    handle.shutdown().await;

    let ticker = guard.runtime_ticker().await;
    let op = timeout(
        Duration::from_secs(1),
        ticker.tick(std::future::pending::<()>()),
    )
    .await
    .expect("buffered message must be delivered");
    assert!(matches!(
        op,
        ProcessOperation::Control(processmanager::RuntimeControlMessage::Shutdown)
    ));
}

/// The single-ticker invariant holds under concurrent calls: two tickers are
/// never alive at the same time (a second live one panics instead).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn never_two_live_tickers() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    for _ in 0..50 {
        let guard = Arc::new(RuntimeGuard::new());
        let live = Arc::new(AtomicUsize::new(0));
        let max_live = Arc::new(AtomicUsize::new(0));

        let tasks: Vec<_> = (0..4)
            .map(|_| {
                let (guard, live, max_live) =
                    (Arc::clone(&guard), Arc::clone(&live), Arc::clone(&max_live));
                tokio::spawn(async move {
                    let ticker = guard.runtime_ticker().await;
                    let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                    max_live.fetch_max(now, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    live.fetch_sub(1, Ordering::SeqCst);
                    drop(ticker);
                })
            })
            .collect();

        let mut created = 0;
        for task in tasks {
            // A task panics when its ticker would have been a second live one.
            if task.await.is_ok() {
                created += 1;
            }
        }
        assert!(created >= 1, "at least one ticker must be created");
        assert_eq!(max_live.load(Ordering::SeqCst), 1, "two tickers were alive");
        assert!(!guard.is_running().await, "all tickers were dropped");
    }
}

/// A handle outliving its runtime does not hang.
#[tokio::test]
async fn handle_outliving_guard_does_not_hang() {
    let guard = RuntimeGuard::new();
    let handle: Arc<dyn ProcessControlHandler> = guard.handle();
    drop(guard);

    for _ in 0..5 {
        timeout(Duration::from_secs(1), handle.shutdown())
            .await
            .expect("send to a dropped runtime must not block");
    }
}
