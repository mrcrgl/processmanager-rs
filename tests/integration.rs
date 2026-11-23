use processmanager::{
    ProcFuture, ProcessControlHandler, ProcessManager, ProcessManagerBuilder, ProcessOperation,
    RestartBackoff, RestartSupervisor, Runnable, RunnableWithContext, RuntimeContext,
    RuntimeControlMessage, RuntimeError, RuntimeGuard, with_runtime_context,
};
use std::ops::Add;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::oneshot::channel;
use tokio::time::timeout;

#[derive(Default)]
struct ExampleController {
    id: usize,
    die_after: Option<Duration>,
    exit_after: Option<Duration>,
    runtime_guard: RuntimeGuard,
}

impl Runnable for ExampleController {
    fn process_start(&self) -> ProcFuture<'_> {
        Box::pin(async {
            let ticker = self.runtime_guard.runtime_ticker().await;
            // This can be any type of future like an async streams
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(1));
            let started = tokio::time::Instant::now();

            loop {
                match ticker.tick(interval.tick()).await {
                    ProcessOperation::Next(_) => {
                        let elapsed =
                            |after: Duration| started.add(after).lt(&tokio::time::Instant::now());
                        if let Some(die_after) = self.die_after.filter(|d| elapsed(*d)) {
                            return Err(RuntimeError::Internal {
                                message: format!("died after {:?}", die_after),
                            });
                        }
                        if self.exit_after.is_some_and(elapsed) {
                            return Ok(());
                        }
                        println!("work {}", self.id)
                    }
                    ProcessOperation::Control(RuntimeControlMessage::Shutdown) => {
                        println!("shutdown {}", self.id);
                        break;
                    }
                    ProcessOperation::Control(RuntimeControlMessage::Reload) => {
                        println!("trigger reload {}", self.id)
                    }
                    // absorb any future control messages we don't explicitly handle
                    ProcessOperation::Control(_) => continue,
                }
            }

            Ok(())
        })
    }

    fn process_handle(&self) -> Arc<dyn ProcessControlHandler> {
        self.runtime_guard.handle()
    }
}

impl ExampleController {
    pub fn new(id: usize, die_after: Option<Duration>, exit_after: Option<Duration>) -> Self {
        Self {
            id,
            die_after,
            exit_after,
            runtime_guard: RuntimeGuard::default(),
        }
    }
}

struct SlowShutdownController {
    shutdown_delay: Duration,
    runtime_guard: RuntimeGuard,
}

struct SlowReloadController {
    reload_delay: Duration,
    runtime_guard: RuntimeGuard,
}

impl SlowReloadController {
    fn new(reload_delay: Duration) -> Self {
        Self {
            reload_delay,
            runtime_guard: RuntimeGuard::default(),
        }
    }
}

struct SlowReloadHandle {
    inner: Arc<dyn ProcessControlHandler>,
    reload_delay: Duration,
}

impl ProcessControlHandler for SlowReloadHandle {
    fn shutdown(&self) -> processmanager::CtrlFuture<'_> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            inner.shutdown().await;
        })
    }

    fn reload(&self) -> processmanager::CtrlFuture<'_> {
        let inner = Arc::clone(&self.inner);
        let delay = self.reload_delay;
        Box::pin(async move {
            tokio::time::sleep(delay).await;
            inner.reload().await;
        })
    }
}

impl SlowShutdownController {
    fn new(shutdown_delay: Duration) -> Self {
        Self {
            shutdown_delay,
            runtime_guard: RuntimeGuard::default(),
        }
    }
}

impl Runnable for SlowReloadController {
    fn process_start(&self) -> ProcFuture<'_> {
        Box::pin(async {
            let ticker = self.runtime_guard.runtime_ticker().await;

            loop {
                match ticker
                    .tick(tokio::time::sleep(Duration::from_secs(30)))
                    .await
                {
                    ProcessOperation::Next(_) => continue,
                    ProcessOperation::Control(RuntimeControlMessage::Shutdown) => break,
                    ProcessOperation::Control(_) => continue,
                }
            }

            Ok(())
        })
    }

    fn process_handle(&self) -> Arc<dyn ProcessControlHandler> {
        Arc::new(SlowReloadHandle {
            inner: self.runtime_guard.handle(),
            reload_delay: self.reload_delay,
        })
    }
}

#[derive(Default)]
struct IgnoreShutdownController {
    runtime_guard: RuntimeGuard,
}

struct FlakyController {
    fail_until_attempt: usize,
    attempts: Arc<AtomicUsize>,
}

impl FlakyController {
    fn new(failures: usize, attempts: Arc<AtomicUsize>) -> Self {
        Self {
            fail_until_attempt: failures,
            attempts,
        }
    }
}

struct ContextController {
    reloads: Arc<AtomicUsize>,
}

impl ContextController {
    fn new(reloads: Arc<AtomicUsize>) -> Self {
        Self { reloads }
    }
}

impl RunnableWithContext for ContextController {
    fn process_start_with_context(&self, ctx: RuntimeContext) -> ProcFuture<'_> {
        let reloads = Arc::clone(&self.reloads);
        Box::pin(async move {
            loop {
                match ctx.tick(tokio::time::sleep(Duration::from_secs(30))).await {
                    ProcessOperation::Next(_) => continue,
                    ProcessOperation::Control(RuntimeControlMessage::Shutdown) => break,
                    ProcessOperation::Control(RuntimeControlMessage::Reload) => {
                        reloads.fetch_add(1, Ordering::SeqCst);
                    }
                    ProcessOperation::Control(_) => continue,
                }
            }

            Ok(())
        })
    }
}

impl Runnable for FlakyController {
    fn process_start(&self) -> ProcFuture<'_> {
        Box::pin(async {
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
            if attempt <= self.fail_until_attempt {
                return Err(RuntimeError::Internal {
                    message: format!("intentional failure on attempt {attempt}"),
                });
            }
            Ok(())
        })
    }

    fn process_handle(&self) -> Arc<dyn ProcessControlHandler> {
        Arc::new(StubControlHandle)
    }
}

struct AlwaysFailController {
    attempts: Arc<AtomicUsize>,
}

impl AlwaysFailController {
    fn new(attempts: Arc<AtomicUsize>) -> Self {
        Self { attempts }
    }
}

impl Runnable for AlwaysFailController {
    fn process_start(&self) -> ProcFuture<'_> {
        Box::pin(async {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            Err(RuntimeError::Internal {
                message: "intentional permanent failure".to_string(),
            })
        })
    }

    fn process_handle(&self) -> Arc<dyn ProcessControlHandler> {
        Arc::new(StubControlHandle)
    }
}

struct StubControlHandle;

impl ProcessControlHandler for StubControlHandle {
    fn shutdown(&self) -> processmanager::CtrlFuture<'_> {
        Box::pin(async {})
    }

    fn reload(&self) -> processmanager::CtrlFuture<'_> {
        Box::pin(async {})
    }
}

impl Runnable for IgnoreShutdownController {
    fn process_start(&self) -> ProcFuture<'_> {
        Box::pin(async {
            let ticker = self.runtime_guard.runtime_ticker().await;

            loop {
                match ticker
                    .tick(tokio::time::sleep(Duration::from_secs(30)))
                    .await
                {
                    ProcessOperation::Next(_) => continue,
                    ProcessOperation::Control(RuntimeControlMessage::Shutdown) => continue,
                    ProcessOperation::Control(_) => continue,
                }
            }
        })
    }

    fn process_handle(&self) -> Arc<dyn ProcessControlHandler> {
        self.runtime_guard.handle()
    }
}

impl Runnable for SlowShutdownController {
    fn process_start(&self) -> ProcFuture<'_> {
        Box::pin(async {
            let ticker = self.runtime_guard.runtime_ticker().await;

            loop {
                match ticker
                    .tick(tokio::time::sleep(Duration::from_secs(30)))
                    .await
                {
                    ProcessOperation::Next(_) => continue,
                    ProcessOperation::Control(RuntimeControlMessage::Shutdown) => {
                        tokio::time::sleep(self.shutdown_delay).await;
                        break;
                    }
                    ProcessOperation::Control(_) => continue,
                }
            }

            Ok(())
        })
    }

    fn process_handle(&self) -> Arc<dyn ProcessControlHandler> {
        self.runtime_guard.handle()
    }
}

#[tokio::test]
async fn test_runnable() {
    let controller = ExampleController::default();

    let (tx, rx) = channel::<bool>();

    let handle = controller.process_handle();
    tokio::task::spawn(async move {
        controller.process_start().await.unwrap();
        tx.send(true).unwrap();
    });

    tokio::time::sleep(Duration::from_secs(1)).await;

    handle.shutdown().await;

    assert!(
        timeout(Duration::from_secs(5), rx).await.is_ok(),
        "timed out"
    );
}

#[tokio::test]
async fn test_add_before_start_is_queued() {
    let manager = ProcessManager::new();
    let attempts = Arc::new(AtomicUsize::new(0));

    manager
        .add(FlakyController::new(0, Arc::clone(&attempts)))
        .expect("add() before start must queue the child");
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        0,
        "queued, not started yet"
    );

    timeout(Duration::from_secs(1), manager.process_start())
        .await
        .expect("manager must finish after its only child")
        .expect("child succeeded");
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1,
        "queued child ran on start"
    );
}

#[tokio::test]
async fn test_runnable_with_context_works_without_runtime_guard_field() {
    let reloads = Arc::new(AtomicUsize::new(0));
    let mut manager = ProcessManager::new();
    manager.insert(with_runtime_context(ContextController::new(Arc::clone(
        &reloads,
    ))));

    let (tx, rx) = channel::<bool>();
    let handle = manager.process_handle();
    tokio::spawn(async move {
        manager.process_start().await.unwrap();
        tx.send(true).unwrap();
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    handle.reload().await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    handle.shutdown().await;

    assert!(
        timeout(Duration::from_secs(2), rx).await.is_ok(),
        "manager did not terminate after shutdown"
    );
    assert_eq!(reloads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_shutdown_waits_for_child_termination() {
    let mut manager = ProcessManager::new();
    manager.insert(SlowShutdownController::new(Duration::from_millis(300)));

    let (tx, rx) = channel::<bool>();
    let handle = manager.process_handle();
    tokio::spawn(async move {
        manager.process_start().await.unwrap();
        tx.send(true).unwrap();
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let started = tokio::time::Instant::now();
    handle.shutdown().await;
    let elapsed = started.elapsed();

    assert!(
        elapsed >= Duration::from_millis(250),
        "shutdown returned too early: elapsed={elapsed:?}"
    );
    assert!(
        timeout(Duration::from_secs(2), rx).await.is_ok(),
        "manager did not terminate after shutdown"
    );
}

#[tokio::test]
async fn test_runtime_guard_shutdown_sent_before_ticker_is_not_lost() {
    let guard = RuntimeGuard::default();
    let handle = guard.handle();

    // Send shutdown before the ticker is created.
    handle.shutdown().await;

    let ticker = guard.runtime_ticker().await;

    let op = timeout(
        Duration::from_secs(1),
        ticker.tick(tokio::time::sleep(Duration::from_secs(5))),
    )
    .await
    .expect("timed out waiting for queued shutdown message");

    assert!(
        matches!(
            op,
            ProcessOperation::Control(RuntimeControlMessage::Shutdown)
        ),
        "expected shutdown control message, got different operation"
    );
}

#[tokio::test]
async fn test_runtime_guard_supports_restart_after_ticker_drop() {
    let guard = RuntimeGuard::default();
    let handle = guard.handle();

    // First start/stop cycle.
    let first = guard.runtime_ticker().await;
    handle.shutdown().await;
    let first_op = timeout(
        Duration::from_secs(1),
        first.tick(tokio::time::sleep(Duration::from_secs(5))),
    )
    .await
    .expect("timed out waiting for first shutdown");
    assert!(matches!(
        first_op,
        ProcessOperation::Control(RuntimeControlMessage::Shutdown)
    ));
    drop(first);

    // Send control while no ticker is alive. This must not kill fanout and
    // must be delivered when the next ticker starts.
    handle.shutdown().await;

    // Second start/stop cycle using the same RuntimeGuard.
    let second = guard.runtime_ticker().await;
    let second_op = timeout(
        Duration::from_secs(1),
        second.tick(tokio::time::sleep(Duration::from_secs(5))),
    )
    .await
    .expect("timed out waiting for second shutdown");
    assert!(matches!(
        second_op,
        ProcessOperation::Control(RuntimeControlMessage::Shutdown)
    ));
}

#[tokio::test]
async fn test_runtime_handle_custom_control_message_is_delivered() {
    let guard = RuntimeGuard::default();
    let handle = guard.handle();

    // Send custom control message before ticker creation to verify both custom
    // payload support and startup buffering behavior.
    handle.custom(42_u32).await;

    let ticker = guard.runtime_ticker().await;

    let op = timeout(
        Duration::from_secs(1),
        ticker.tick(tokio::time::sleep(Duration::from_secs(5))),
    )
    .await
    .expect("timed out waiting for queued custom message");

    match op {
        ProcessOperation::Control(RuntimeControlMessage::Custom(payload)) => {
            let value = payload
                .downcast::<u32>()
                .expect("expected u32 custom payload");
            assert_eq!(*value, 42_u32);
        }
        _ => panic!("expected custom control message"),
    }
}

#[tokio::test]
async fn test_runnable_can_restart_start_shutdown_start() {
    let controller = Arc::new(ExampleController::default());
    let handle = controller.process_handle();

    for _ in 0..2 {
        let runnable = Arc::clone(&controller);
        let join = tokio::spawn(async move {
            runnable.process_start().await.unwrap();
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        handle.shutdown().await;

        timeout(Duration::from_secs(2), join)
            .await
            .expect("timed out waiting for runnable shutdown")
            .expect("runnable task failed");
    }
}

#[test]
fn test_runtime_control_message_custom_clone_is_safe() {
    let msg = RuntimeControlMessage::Custom(Arc::new(7_u8));
    let cloned = msg.clone();

    match cloned {
        RuntimeControlMessage::Custom(payload) => {
            let value = payload
                .downcast::<u8>()
                .expect("expected u8 custom payload");
            assert_eq!(*value, 7_u8);
        }
        _ => panic!("expected custom control message"),
    }
}

#[tokio::test]
async fn test_reload_dispatch_is_parallel() {
    let mut manager = ProcessManager::new();
    manager.insert(SlowReloadController::new(Duration::from_millis(700)));
    manager.insert(SlowReloadController::new(Duration::from_millis(700)));

    let (tx, rx) = channel::<bool>();

    let handle = manager.process_handle();
    tokio::task::spawn(async move {
        manager.process_start().await.unwrap();
        tx.send(true).unwrap();
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let started = tokio::time::Instant::now();
    handle.reload().await;
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_millis(1200),
        "reload was slower than expected for parallel dispatch: elapsed={elapsed:?}"
    );

    handle.shutdown().await;
    assert!(
        timeout(Duration::from_secs(2), rx).await.is_ok(),
        "manager did not terminate after shutdown"
    );
}

#[tokio::test]
async fn test_restart_supervisor_restarts_failed_child_with_backoff() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let wrapper = RestartSupervisor::new(FlakyController::new(2, Arc::clone(&attempts))).backoff(
        RestartBackoff::new(Duration::from_millis(60), Duration::from_millis(200), 2),
    );

    let started = tokio::time::Instant::now();
    wrapper.process_start().await.unwrap();
    let elapsed = started.elapsed();

    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    assert!(
        elapsed >= Duration::from_millis(150),
        "expected visible backoff delay before successful restart, elapsed={elapsed:?}"
    );
}

#[tokio::test]
async fn test_restart_supervisor_shutdown_interrupts_backoff() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let wrapper = Arc::new(
        RestartSupervisor::new(AlwaysFailController::new(Arc::clone(&attempts))).backoff(
            RestartBackoff::new(Duration::from_secs(5), Duration::from_secs(5), 2),
        ),
    );
    let handle = wrapper.process_handle();

    let run = tokio::spawn({
        let wrapper = Arc::clone(&wrapper);
        async move { wrapper.process_start().await.unwrap() }
    });

    // Wait for at least one failed attempt so the wrapper is in backoff.
    for _ in 0..20 {
        if attempts.load(Ordering::SeqCst) > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(attempts.load(Ordering::SeqCst) > 0);

    let started = tokio::time::Instant::now();
    handle.shutdown().await;
    timeout(Duration::from_secs(1), run)
        .await
        .expect("restart wrapper did not stop promptly after shutdown")
        .expect("restart wrapper task failed");
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(1),
        "shutdown should interrupt backoff promptly, elapsed={elapsed:?}"
    );
}

/// Runs for `run_for`, then fails; records the start time of every attempt.
struct RunThenFailController {
    run_for: Duration,
    starts: Arc<std::sync::Mutex<Vec<tokio::time::Instant>>>,
}

impl Runnable for RunThenFailController {
    fn process_start(&self) -> ProcFuture<'_> {
        Box::pin(async {
            self.starts
                .lock()
                .unwrap()
                .push(tokio::time::Instant::now());
            tokio::time::sleep(self.run_for).await;
            Err(RuntimeError::Internal {
                message: "failed after running".to_string(),
            })
        })
    }

    fn process_handle(&self) -> Arc<dyn ProcessControlHandler> {
        Arc::new(StubControlHandle)
    }
}

#[tokio::test]
async fn test_restart_supervisor_gives_up_after_restart_limit() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let wrapper = RestartSupervisor::new(AlwaysFailController::new(Arc::clone(&attempts)))
        .backoff(RestartBackoff::new(
            Duration::from_millis(10),
            Duration::from_millis(10),
            1,
        ))
        .restart_limit(2, Duration::from_secs(10));

    let result = timeout(Duration::from_secs(2), wrapper.process_start())
        .await
        .expect("restart supervisor must give up instead of retrying forever");

    let err = result.expect_err("supervisor must give up");
    let gave_up = err
        .downcast_ref::<processmanager::RestartLimitExceeded>()
        .unwrap_or_else(|| panic!("expected RestartLimitExceeded, got {err:?}"));
    assert_eq!(gave_up.failures(), 3);
    assert!(gave_up.child().contains("AlwaysFailController"));
    assert!(
        matches!(gave_up.last_error(), RuntimeError::Internal { message } if message == "intentional permanent failure"),
        "the child's last error must be kept, got {:?}",
        gave_up.last_error()
    );
    // The chain: give-up -> last error.
    let source = std::error::Error::source(gave_up).expect("last error as source");
    assert!(source.to_string().contains("intentional permanent failure"));
    assert!(
        err.to_string().contains("gave up"),
        "transparent Display: {err}"
    );
    // initial run + 2 restarts, then the third failure exceeds the limit
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn test_restart_supervisor_limit_shuts_down_siblings_in_manager() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let manager = ProcessManagerBuilder::default()
        .pre_insert(processmanager::IdleProcess::default())
        .pre_insert(
            RestartSupervisor::new(AlwaysFailController::new(Arc::clone(&attempts)))
                .backoff(RestartBackoff::new(
                    Duration::from_millis(10),
                    Duration::from_millis(10),
                    1,
                ))
                .restart_limit(1, Duration::from_secs(10)),
        )
        .build();

    let result = timeout(Duration::from_secs(2), manager.process_start())
        .await
        .expect("manager must stop once the restart limit is exceeded");

    let err = result.expect_err("manager must return the give-up error");
    assert!(
        err.downcast_ref::<processmanager::RestartLimitExceeded>()
            .is_some(),
        "manager must return the give-up error, got {err:?}"
    );
}

#[tokio::test]
async fn test_restart_supervisor_resets_backoff_after_healthy_run() {
    let starts = Arc::new(std::sync::Mutex::new(Vec::new()));
    // Without a reset the delays would grow 20ms -> 200ms -> 2s.
    let wrapper = Arc::new(
        RestartSupervisor::new(RunThenFailController {
            run_for: Duration::from_millis(80),
            starts: Arc::clone(&starts),
        })
        .backoff(RestartBackoff::new(
            Duration::from_millis(20),
            Duration::from_secs(2),
            10,
        ))
        .reset_after(Duration::from_millis(50)),
    );
    let handle = wrapper.process_handle();
    let run = tokio::spawn({
        let wrapper = Arc::clone(&wrapper);
        async move { wrapper.process_start().await }
    });

    timeout(Duration::from_secs(2), async {
        while starts.lock().unwrap().len() < 4 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("child must be restarted quickly when the backoff resets");

    handle.shutdown().await;
    let _ = timeout(Duration::from_secs(1), run).await;

    let starts = starts.lock().unwrap();
    for pair in starts.windows(2) {
        let gap = pair[1].duration_since(pair[0]);
        assert!(
            gap < Duration::from_millis(80 + 150),
            "every restart should use the initial delay, got gap={gap:?}"
        );
    }
}

#[tokio::test]
async fn test_restart_supervisor_name_includes_child() {
    let wrapper = RestartSupervisor::new(FlakyController::new(0, Arc::new(AtomicUsize::new(0))));
    let name = wrapper.process_name();
    assert!(
        name.starts_with("RestartSupervisor(") && name.contains("FlakyController"),
        "unexpected name: {name}"
    );
}

#[tokio::test]
async fn test_shutdown_grace_period_is_configurable() {
    let manager = ProcessManagerBuilder::default()
        .shutdown_grace_period(Duration::from_millis(100))
        .pre_insert(IgnoreShutdownController::default())
        .build();

    let handle = manager.process_handle();
    let manager_task = tokio::task::spawn(async move {
        let _ = manager.process_start().await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let started = tokio::time::Instant::now();
    handle.shutdown().await;
    let elapsed = started.elapsed();

    assert!(
        elapsed >= Duration::from_millis(50) && elapsed < Duration::from_millis(600),
        "expected shutdown to honor short configured grace period, got elapsed={elapsed:?}"
    );

    timeout(Duration::from_secs(1), manager_task)
        .await
        .expect("manager must return after its child was aborted")
        .expect("manager task panicked");
}

struct DelayedFailController {
    delay: Duration,
}

impl Runnable for DelayedFailController {
    fn process_start(&self) -> ProcFuture<'_> {
        Box::pin(async {
            tokio::time::sleep(self.delay).await;
            Err(RuntimeError::Internal {
                message: "intentional failure".to_string(),
            })
        })
    }

    fn process_handle(&self) -> Arc<dyn ProcessControlHandler> {
        Arc::new(StubControlHandle)
    }
}

#[tokio::test]
async fn test_process_start_returns_first_error_when_sibling_is_aborted() {
    let manager = ProcessManagerBuilder::default()
        .shutdown_grace_period(Duration::from_millis(200))
        .pre_insert(IgnoreShutdownController::default())
        .pre_insert(DelayedFailController {
            delay: Duration::from_millis(50),
        })
        .build();

    let result = timeout(Duration::from_secs(2), manager.process_start())
        .await
        .expect("process_start must return once the straggler is aborted");

    match result {
        Err(RuntimeError::Internal { message }) => assert_eq!(
            message, "intentional failure",
            "the first error must win over the abort error"
        ),
        other => panic!("expected the original failure, got {other:?}"),
    }
}

#[tokio::test]
async fn test_process_start_returns_after_external_shutdown_aborts_child() {
    let manager = ProcessManagerBuilder::default()
        .shutdown_grace_period(Duration::from_millis(100))
        .pre_insert(IgnoreShutdownController::default())
        .build();

    let handle = manager.process_handle();
    let manager_task = tokio::task::spawn(async move { manager.process_start().await });

    tokio::time::sleep(Duration::from_millis(50)).await;
    handle.shutdown().await;

    let result = timeout(Duration::from_secs(1), manager_task)
        .await
        .expect("process_start must return after the child was aborted")
        .expect("manager task panicked");

    match result {
        Err(RuntimeError::Internal { message }) => {
            assert!(
                message.contains("IgnoreShutdownController") && message.contains("aborted"),
                "abort error must name the aborted child, got: {message}"
            );
        }
        other => panic!("expected an abort error, got {other:?}"),
    }
}

#[tokio::test]
async fn test_process_runnable() {
    let controller = ExampleController::default();
    let mut manager = ProcessManager::new();
    manager.insert(controller);

    let (tx, rx) = channel::<bool>();

    let handle = manager.process_handle();
    tokio::task::spawn(async move {
        manager.process_start().await.unwrap();
        tx.send(true).unwrap();
    });

    tokio::time::sleep(Duration::from_secs(1)).await;

    handle.shutdown().await;

    assert!(
        timeout(Duration::from_secs(5), rx).await.is_ok(),
        "timed out"
    );
}

#[tokio::test]
async fn test_process_runnable_multiple() {
    let controller1 = ExampleController::new(1, None, None);
    let controller2 = ExampleController::new(2, None, None);
    let mut manager = ProcessManager::new();
    manager.insert(controller1);
    manager.insert(controller2);

    let (tx, rx) = channel::<bool>();

    let handle = manager.process_handle();
    tokio::task::spawn(async move {
        manager.process_start().await.unwrap();
        tx.send(true).unwrap();
    });

    tokio::time::sleep(Duration::from_secs(1)).await;

    handle.shutdown().await;

    assert!(
        timeout(Duration::from_secs(5), rx).await.is_ok(),
        "timed out"
    );
}

#[tokio::test]
async fn test_nested_process_runnable_multiple() {
    let controller1 = ExampleController::new(1, None, None);
    let controller2 = ExampleController::new(2, None, None);

    let mut manager1 = ProcessManager::new();
    manager1.insert(controller1);

    let mut manager2 = ProcessManager::new();
    manager2.insert(controller2);

    let mut manager = ProcessManager::new();
    manager.insert(manager1);
    manager.insert(manager2);

    let (tx, rx) = channel::<bool>();

    let handle = manager.process_handle();
    tokio::task::spawn(async move {
        manager.process_start().await.unwrap();
        tx.send(true).unwrap();
    });

    tokio::time::sleep(Duration::from_secs(1)).await;

    handle.shutdown().await;

    assert!(
        timeout(Duration::from_secs(5), rx).await.is_ok(),
        "timed out"
    );
}

#[tokio::test]
async fn test_process_runnable_multiple_one_dies() {
    let controller1 = ExampleController::new(1, None, None);
    let controller2 = ExampleController::new(2, Some(Duration::from_secs(2)), None);
    let mut manager = ProcessManager::new();
    manager.insert(controller1);
    manager.insert(controller2);

    let (tx, rx) = channel::<bool>();

    let _handle = manager.process_handle();
    tokio::task::spawn(async move {
        let result = manager.process_start().await;
        assert!(result.is_err());
        tx.send(true).unwrap();
    });

    assert!(
        timeout(Duration::from_secs(7), rx).await.is_ok(),
        "timed out"
    );
}

#[tokio::test]
async fn test_process_runnable_multiple_one_exits() {
    let controller1 = ExampleController::new(1, None, None);
    let controller2 = ExampleController::new(2, None, Some(Duration::from_secs(2)));
    let mut manager = ProcessManager::new();
    manager.insert(controller1);
    manager.insert(controller2);

    let (tx, rx) = channel::<bool>();

    let _handle = manager.process_handle();
    tokio::task::spawn(async move {
        manager.process_start().await.unwrap();
        tx.send(true).unwrap();
    });

    assert!(
        timeout(Duration::from_secs(5), rx).await.is_err(),
        "expected time out"
    );
}

#[tokio::test]
async fn test_nested_process_runnable_multiple_one_dies() {
    let controller1 = ExampleController::new(1, None, None);
    let controller2 = ExampleController::new(2, Some(Duration::from_secs(2)), None);

    let mut manager = ProcessManager::new();

    let mut manager1 = ProcessManager::new();
    manager1.insert(controller1);

    let mut manager2 = ProcessManager::new();
    manager2.insert(controller2);

    manager.insert(manager1);
    manager.insert(manager2);

    let (tx, rx) = channel::<bool>();

    let _handle = manager.process_handle();
    tokio::task::spawn(async move {
        let result = manager.process_start().await;
        assert!(result.is_err(), "expect process to exit with error");
        tx.send(true).unwrap();
    });

    assert!(
        timeout(Duration::from_secs(10), rx).await.is_ok(),
        "timed out"
    );
}

#[tokio::test]
async fn test_nested_process_runnable_multiple_one_exits_1() {
    let controller1 = ExampleController::new(1, None, None);
    let controller2 = ExampleController::new(2, None, Some(Duration::from_secs(2)));

    let mut manager1 = ProcessManager::new();
    manager1.insert(controller1);

    let mut manager2 = ProcessManager::new();
    manager2.insert(controller2);

    let mut manager = ProcessManager::new();
    manager.insert(manager1);
    manager.insert(manager2);

    let (tx, rx) = channel::<bool>();

    let _handle = manager.process_handle();
    tokio::task::spawn(async move {
        let _ = manager.process_start().await;
        tx.send(true).unwrap();
    });

    assert!(
        timeout(Duration::from_secs(5), rx).await.is_err(),
        "expected time out"
    );
}

#[tokio::test]
async fn test_nested_process_runnable_multiple_one_exits_2() {
    let controller1 = ExampleController::new(1, None, None);
    let controller2 = ExampleController::new(2, None, Some(Duration::from_secs(2)));

    let mut manager1 = ProcessManager::new();
    manager1.insert(controller1);

    let mut manager = ProcessManager::new();
    manager.insert(controller2);
    manager.insert(manager1);

    let (tx, rx) = channel::<bool>();

    let _handle = manager.process_handle();
    tokio::task::spawn(async move {
        let _ = manager.process_start().await;
        tx.send(true).unwrap();
    });

    assert!(
        timeout(Duration::from_secs(5), rx).await.is_err(),
        "expected time out"
    );
}

mod job {
    use super::*;
    use processmanager::{IdleProcess, Job};

    #[tokio::test]
    async fn required_job_runs_once_and_manager_finishes() {
        let runs = Arc::new(AtomicUsize::new(0));
        let manager = ProcessManagerBuilder::default()
            .pre_insert(Job::new("count", {
                let runs = Arc::clone(&runs);
                move || {
                    let runs = Arc::clone(&runs);
                    async move {
                        runs.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                }
            }))
            .build();

        let result = timeout(Duration::from_secs(1), manager.process_start())
            .await
            .expect("manager must finish after its only job");
        assert!(result.is_ok(), "got {result:?}");
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn successful_job_leaves_siblings_running() {
        let manager = ProcessManagerBuilder::default()
            .pre_insert(Job::new("ok", || async { Ok(()) }))
            .pre_insert(IdleProcess::default())
            .build();
        let handle = manager.process_handle();
        let run = tokio::spawn(async move { manager.process_start().await });

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!run.is_finished(), "sibling must keep the manager running");

        handle.shutdown().await;
        let result = timeout(Duration::from_secs(1), run)
            .await
            .expect("manager must stop after shutdown")
            .expect("manager task panicked");
        assert!(result.is_ok(), "got {result:?}");
    }

    #[tokio::test]
    async fn failed_required_job_shuts_down_manager() {
        let manager = ProcessManagerBuilder::default()
            .pre_insert(Job::new("migrate", || async {
                Err(RuntimeError::Internal {
                    message: "migration failed".to_string(),
                })
            }))
            .pre_insert(IdleProcess::default())
            .build();

        let result = timeout(Duration::from_secs(1), manager.process_start())
            .await
            .expect("a failed required job must stop the manager");
        assert!(
            matches!(&result, Err(RuntimeError::Internal { message }) if message == "migration failed"),
            "got {result:?}"
        );
    }

    #[tokio::test]
    async fn failed_optional_job_leaves_siblings_running() {
        let manager = ProcessManagerBuilder::default()
            .pre_insert(
                Job::new("warm-cache", || async {
                    Err(RuntimeError::Internal {
                        message: "cache unavailable".to_string(),
                    })
                })
                .optional(),
            )
            .pre_insert(IdleProcess::default())
            .build();
        let handle = manager.process_handle();
        let run = tokio::spawn(async move { manager.process_start().await });

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !run.is_finished(),
            "optional failure must not stop the manager"
        );

        handle.shutdown().await;
        let result = timeout(Duration::from_secs(1), run)
            .await
            .expect("manager must stop after shutdown")
            .expect("manager task panicked");
        assert!(result.is_ok(), "got {result:?}");
    }

    #[tokio::test]
    async fn shutdown_cancels_running_job() {
        let manager = ProcessManagerBuilder::default()
            .shutdown_grace_period(Duration::from_secs(5))
            .pre_insert(Job::new("forever", std::future::pending))
            .build();
        let handle = manager.process_handle();
        let run = tokio::spawn(async move { manager.process_start().await });

        tokio::time::sleep(Duration::from_millis(50)).await;
        let started = tokio::time::Instant::now();
        handle.shutdown().await;
        let result = timeout(Duration::from_secs(1), run)
            .await
            .expect("job must be cancelled well before the grace period")
            .expect("manager task panicked");
        assert!(result.is_ok(), "got {result:?}");
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn job_in_restart_supervisor_is_retried() {
        let runs = Arc::new(AtomicUsize::new(0));
        let job = Job::new("flaky", {
            let runs = Arc::clone(&runs);
            move || {
                let runs = Arc::clone(&runs);
                async move {
                    if runs.fetch_add(1, Ordering::SeqCst) < 2 {
                        Err(RuntimeError::Internal {
                            message: "not yet".to_string(),
                        })
                    } else {
                        Ok(())
                    }
                }
            }
        });
        let wrapper = RestartSupervisor::new(job).backoff(RestartBackoff::new(
            Duration::from_millis(10),
            Duration::from_millis(10),
            1,
        ));

        let result = timeout(Duration::from_secs(1), wrapper.process_start())
            .await
            .expect("retried job must eventually succeed");
        assert!(result.is_ok(), "got {result:?}");
        assert_eq!(runs.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn job_name_is_process_name() {
        let job = Job::new("migrate", || async { Ok(()) });
        assert_eq!(job.process_name(), "migrate");
    }
}

mod finished_children {
    use super::*;
    use processmanager::{IdleProcess, Job};

    async fn reload_repeatedly_with_finished_job(auto_cleanup: bool) {
        let manager = ProcessManagerBuilder::default()
            .auto_cleanup(auto_cleanup)
            .pre_insert(IdleProcess::new())
            .pre_insert(Job::new("done", || async { Ok(()) }))
            .build();
        let handle = manager.process_handle();
        let run = tokio::spawn(async move { manager.process_start().await });

        // Let the job finish while the idle sibling keeps the manager alive.
        tokio::time::sleep(Duration::from_millis(100)).await;

        for i in 1..=5 {
            timeout(Duration::from_secs(1), handle.reload())
                .await
                .unwrap_or_else(|_| panic!("reload {i} hung on a finished child"));
        }

        handle.shutdown().await;
        timeout(Duration::from_secs(1), run)
            .await
            .expect("manager must stop after shutdown")
            .expect("manager task panicked")
            .expect("manager must stop cleanly");
    }

    #[tokio::test]
    async fn reload_does_not_hang_on_finished_child_with_default_builder() {
        reload_repeatedly_with_finished_job(true).await;
    }

    #[tokio::test]
    async fn reload_does_not_hang_on_finished_child_without_auto_cleanup() {
        reload_repeatedly_with_finished_job(false).await;
    }

    #[tokio::test]
    async fn shutdown_skips_finished_children() {
        let manager = ProcessManagerBuilder::default()
            .auto_cleanup(false)
            .shutdown_grace_period(Duration::from_secs(5))
            .pre_insert(Job::new("done", || async { Ok(()) }))
            .build();
        let handle = manager.process_handle();

        timeout(Duration::from_secs(1), manager.process_start())
            .await
            .expect("manager must finish after its only job")
            .expect("job succeeded");

        // The finished job is still registered (auto_cleanup is off). Repeated
        // broadcasts must neither block on its dead control channel nor wait
        // for the grace period.
        for i in 1..=5 {
            timeout(Duration::from_secs(1), handle.shutdown())
                .await
                .unwrap_or_else(|_| panic!("shutdown {i} blocked on a finished child"));
        }
    }
}

mod early_shutdown {
    use super::*;
    use processmanager::IdleProcess;

    #[tokio::test]
    async fn shutdown_before_start_is_honoured() {
        let manager = ProcessManagerBuilder::default()
            .pre_insert(IdleProcess::new())
            .build();
        let handle = manager.process_handle();

        timeout(Duration::from_secs(1), handle.shutdown())
            .await
            .expect("shutdown with no running children returns at once");

        let result = timeout(Duration::from_secs(2), manager.process_start())
            .await
            .expect("process_start must honour a shutdown requested before start");
        assert!(result.is_ok(), "got {result:?}");
    }

    #[tokio::test]
    async fn shutdown_racing_start_is_honoured() {
        for _ in 0..50 {
            let manager = ProcessManagerBuilder::default()
                .pre_insert(IdleProcess::new())
                .build();
            let handle = manager.process_handle();
            let run = tokio::spawn(async move { manager.process_start().await });

            // No sleep: race the first poll of process_start.
            handle.shutdown().await;

            let result = timeout(Duration::from_secs(2), run)
                .await
                .expect("shutdown racing start must not be lost")
                .expect("manager task panicked");
            assert!(result.is_ok(), "got {result:?}");
        }
    }

    #[tokio::test]
    async fn nested_manager_shut_down_before_start_stops() {
        let inner = ProcessManagerBuilder::default()
            .pre_insert(IdleProcess::new())
            .build();
        inner.process_handle().shutdown().await;

        let outer = ProcessManagerBuilder::default().pre_insert(inner).build();
        let result = timeout(Duration::from_secs(2), outer.process_start())
            .await
            .expect("nested manager must honour its early shutdown");
        assert!(result.is_ok(), "got {result:?}");
    }

    #[tokio::test]
    async fn add_during_shutdown_is_rejected() {
        let manager = Arc::new(
            ProcessManagerBuilder::default()
                .shutdown_grace_period(Duration::from_secs(5))
                // Stops cleanly, but slowly: keeps the shutdown in progress
                // without producing an error that would trigger a second
                // shutdown broadcast.
                .pre_insert(SlowShutdownController::new(Duration::from_millis(300)))
                .build(),
        );
        let handle = manager.process_handle();
        let run = tokio::spawn({
            let manager = Arc::clone(&manager);
            async move { manager.process_start().await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        let shutdown = tokio::spawn({
            let handle = Arc::clone(&handle);
            async move { handle.shutdown().await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(
            manager.add(IdleProcess::new()),
            Err(processmanager::AddError::ShuttingDown),
            "a shutting-down manager must reject new children"
        );

        let result = timeout(Duration::from_secs(2), run)
            .await
            .expect("manager must finish its shutdown")
            .expect("manager task panicked");
        assert!(result.is_ok(), "got {result:?}");
        shutdown.await.expect("shutdown task panicked");
    }

    /// Delegates to an inner handle and counts `shutdown` calls.
    struct CountingHandle {
        inner: Arc<dyn ProcessControlHandler>,
        shutdowns: Arc<AtomicUsize>,
    }

    impl ProcessControlHandler for CountingHandle {
        fn shutdown(&self) -> processmanager::CtrlFuture<'_> {
            self.shutdowns.fetch_add(1, Ordering::SeqCst);
            self.inner.shutdown()
        }

        fn reload(&self) -> processmanager::CtrlFuture<'_> {
            self.inner.reload()
        }
    }

    struct CountedIdle {
        idle: IdleProcess,
        shutdowns: Arc<AtomicUsize>,
    }

    impl Runnable for CountedIdle {
        fn process_start(&self) -> ProcFuture<'_> {
            self.idle.process_start()
        }

        fn process_handle(&self) -> Arc<dyn ProcessControlHandler> {
            Arc::new(CountingHandle {
                inner: self.idle.process_handle(),
                shutdowns: Arc::clone(&self.shutdowns),
            })
        }
    }

    #[tokio::test]
    async fn concurrent_shutdowns_signal_each_child_once() {
        let shutdowns = Arc::new(AtomicUsize::new(0));
        let manager = ProcessManagerBuilder::default()
            .pre_insert(CountedIdle {
                idle: IdleProcess::new(),
                shutdowns: Arc::clone(&shutdowns),
            })
            .build();
        let handle = manager.process_handle();
        let run = tokio::spawn(async move { manager.process_start().await });
        tokio::time::sleep(Duration::from_millis(50)).await;

        tokio::join!(handle.shutdown(), handle.shutdown());

        timeout(Duration::from_secs(2), run)
            .await
            .expect("manager must stop")
            .expect("manager task panicked")
            .expect("clean shutdown");
        assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
    }
}

mod abort_propagation {
    use super::*;

    /// Ignores shutdown and counts how often it was still alive.
    struct Stubborn {
        ticks: Arc<AtomicUsize>,
    }

    impl Runnable for Stubborn {
        fn process_start(&self) -> ProcFuture<'_> {
            Box::pin(async {
                loop {
                    self.ticks.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
        }

        fn process_handle(&self) -> Arc<dyn ProcessControlHandler> {
            Arc::new(StubControlHandle)
        }
    }

    /// Asserts that `ticks` no longer advances, i.e. the stubborn child is gone.
    async fn assert_stopped(ticks: &AtomicUsize) {
        // Give an abort a moment to take effect.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let before = ticks.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(150)).await;
        let after = ticks.load(Ordering::SeqCst);
        assert_eq!(
            before, after,
            "child is still running after its parent ended"
        );
    }

    #[tokio::test]
    async fn aborted_restart_supervisor_aborts_its_child() {
        let ticks = Arc::new(AtomicUsize::new(0));
        let manager = ProcessManagerBuilder::default()
            .shutdown_grace_period(Duration::from_millis(200))
            .pre_insert(RestartSupervisor::new(Stubborn {
                ticks: Arc::clone(&ticks),
            }))
            .build();
        let handle = manager.process_handle();
        let run = tokio::spawn(async move { manager.process_start().await });
        tokio::time::sleep(Duration::from_millis(50)).await;

        handle.shutdown().await;
        timeout(Duration::from_secs(2), run)
            .await
            .expect("manager must return after aborting the supervisor")
            .expect("manager task panicked")
            .expect_err("supervisor was aborted");

        assert!(ticks.load(Ordering::SeqCst) > 0, "child never ran");
        assert_stopped(&ticks).await;
    }

    #[tokio::test]
    async fn aborted_nested_manager_aborts_its_children() {
        let ticks = Arc::new(AtomicUsize::new(0));
        let nested = ProcessManagerBuilder::default()
            // Longer than the outer grace period, so the outer manager aborts
            // the nested one while it is still waiting for its child.
            .shutdown_grace_period(Duration::from_secs(5))
            .pre_insert(Stubborn {
                ticks: Arc::clone(&ticks),
            })
            .build();
        let outer = ProcessManagerBuilder::default()
            .shutdown_grace_period(Duration::from_millis(200))
            .pre_insert(nested)
            .build();
        let handle = outer.process_handle();
        let run = tokio::spawn(async move { outer.process_start().await });
        tokio::time::sleep(Duration::from_millis(50)).await;

        handle.shutdown().await;
        timeout(Duration::from_secs(2), run)
            .await
            .expect("outer manager must return after aborting the nested one")
            .expect("manager task panicked")
            .expect_err("nested manager was aborted");

        assert!(ticks.load(Ordering::SeqCst) > 0, "child never ran");
        assert_stopped(&ticks).await;
    }

    #[tokio::test]
    async fn dropping_manager_future_aborts_children() {
        let ticks = Arc::new(AtomicUsize::new(0));
        let manager = ProcessManagerBuilder::default()
            .pre_insert(Stubborn {
                ticks: Arc::clone(&ticks),
            })
            .build();

        // The timeout drops the process_start future while the child runs.
        let _ = timeout(Duration::from_millis(50), manager.process_start()).await;

        assert!(ticks.load(Ordering::SeqCst) > 0, "child never ran");
        assert_stopped(&ticks).await;
    }

    #[tokio::test]
    async fn dropping_restart_supervisor_future_aborts_child() {
        let ticks = Arc::new(AtomicUsize::new(0));
        let supervisor = RestartSupervisor::new(Stubborn {
            ticks: Arc::clone(&ticks),
        });

        let _ = timeout(Duration::from_millis(50), supervisor.process_start()).await;

        assert!(ticks.load(Ordering::SeqCst) > 0, "child never ran");
        assert_stopped(&ticks).await;
    }
}

mod termination_request {
    use super::*;
    use processmanager::IdleProcess;

    /// Asks its manager to stop, like a `SignalReceiver` after SIGTERM.
    struct RequestsTermination;

    impl Runnable for RequestsTermination {
        fn process_start(&self) -> ProcFuture<'_> {
            Box::pin(async {
                tokio::time::sleep(Duration::from_millis(20)).await;
                Err(RuntimeError::TerminationSignal)
            })
        }

        fn process_handle(&self) -> Arc<dyn ProcessControlHandler> {
            Arc::new(StubControlHandle)
        }
    }

    /// Fails while shutting down.
    #[derive(Default)]
    struct FailsOnShutdown {
        guard: RuntimeGuard,
    }

    impl Runnable for FailsOnShutdown {
        fn process_start(&self) -> ProcFuture<'_> {
            Box::pin(async {
                let ticker = self.guard.runtime_ticker().await;
                loop {
                    if let ProcessOperation::Control(RuntimeControlMessage::Shutdown) =
                        ticker.tick(std::future::pending::<()>()).await
                    {
                        return Err(RuntimeError::Internal {
                            message: "failed while stopping".to_string(),
                        });
                    }
                }
            })
        }

        fn process_handle(&self) -> Arc<dyn ProcessControlHandler> {
            self.guard.handle()
        }
    }

    #[tokio::test]
    async fn termination_request_is_a_graceful_stop() {
        let manager = ProcessManagerBuilder::default()
            .pre_insert(RequestsTermination)
            .pre_insert(IdleProcess::new())
            .build();

        let result = timeout(Duration::from_secs(2), manager.process_start())
            .await
            .expect("termination request must stop the manager");
        assert!(result.is_ok(), "got {result:?}");
    }

    #[tokio::test]
    async fn failure_during_termination_is_reported() {
        let manager = ProcessManagerBuilder::default()
            .pre_insert(RequestsTermination)
            .pre_insert(FailsOnShutdown::default())
            .build();

        let result = timeout(Duration::from_secs(2), manager.process_start())
            .await
            .expect("termination request must stop the manager");
        assert!(
            matches!(&result, Err(RuntimeError::Internal { message }) if message == "failed while stopping"),
            "a real failure must win over the termination request, got {result:?}"
        );
    }
}

mod child_panics {
    use super::*;
    use processmanager::IdleProcess;

    /// Panics while *creating* its future, before returning it.
    struct PanicsBeforeFuture;

    impl Runnable for PanicsBeforeFuture {
        #[allow(unreachable_code)]
        fn process_start(&self) -> ProcFuture<'_> {
            panic!("boom before future");
            Box::pin(async { Ok(()) })
        }

        fn process_handle(&self) -> Arc<dyn ProcessControlHandler> {
            Arc::new(StubControlHandle)
        }
    }

    /// Panics while its future is being polled.
    struct PanicsInFuture;

    impl Runnable for PanicsInFuture {
        fn process_start(&self) -> ProcFuture<'_> {
            Box::pin(async { panic!("boom in future") })
        }

        fn process_handle(&self) -> Arc<dyn ProcessControlHandler> {
            Arc::new(StubControlHandle)
        }
    }

    async fn run_with_idle_sibling(child: impl Runnable) -> Result<(), RuntimeError> {
        let manager = ProcessManagerBuilder::default()
            .pre_insert(child)
            .pre_insert(IdleProcess::new())
            .build();
        timeout(Duration::from_secs(2), manager.process_start())
            .await
            .expect("a panicking child must stop the manager")
    }

    #[tokio::test]
    async fn panic_before_future_is_reported_as_panic() {
        let result = run_with_idle_sibling(PanicsBeforeFuture).await;
        match result {
            Err(RuntimeError::Internal { message }) => assert_eq!(
                message, "process panicked: boom before future",
                "must not be reported as an abort"
            ),
            other => panic!("expected a panic error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn panic_in_future_is_reported_as_panic() {
        let result = run_with_idle_sibling(PanicsInFuture).await;
        match result {
            Err(RuntimeError::Internal { message }) => {
                assert_eq!(message, "process panicked: boom in future")
            }
            other => panic!("expected a panic error, got {other:?}"),
        }
    }
}

mod add_contract {
    use super::*;
    use processmanager::{AddError, IdleProcess, Job};

    #[tokio::test]
    async fn add_right_after_spawning_start_never_fails() {
        for _ in 0..50 {
            let manager = Arc::new(ProcessManagerBuilder::default().build());
            let run = tokio::spawn({
                let manager = Arc::clone(&manager);
                async move { manager.process_start().await }
            });

            // Races the first poll of process_start: either queued or spawned.
            manager
                .add(IdleProcess::new())
                .expect("add must succeed until the manager has stopped");

            manager.process_handle().shutdown().await;
            timeout(Duration::from_secs(2), run)
                .await
                .expect("manager must stop")
                .expect("manager task panicked")
                .expect("clean shutdown");
        }
    }

    #[tokio::test]
    async fn add_after_stop_is_rejected() {
        let manager = ProcessManagerBuilder::default()
            .pre_insert(Job::new("done", || async { Ok(()) }))
            .build();
        timeout(Duration::from_secs(1), manager.process_start())
            .await
            .expect("manager must finish")
            .expect("job succeeded");

        assert_eq!(manager.add(IdleProcess::new()), Err(AddError::Stopped));
    }

    #[tokio::test]
    async fn add_after_aborted_start_is_rejected() {
        let manager = ProcessManagerBuilder::default()
            .pre_insert(IdleProcess::new())
            .build();
        // Dropping the future aborts the manager.
        let _ = timeout(Duration::from_millis(50), manager.process_start()).await;

        assert_eq!(manager.add(IdleProcess::new()), Err(AddError::Stopped));
    }

    /// An `add` racing the supervisor's exit either keeps the manager
    /// running (the new child is supervised) or is rejected; the child is
    /// never orphaned.
    static ACCEPTED: AtomicUsize = AtomicUsize::new(0);

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn add_racing_exit_is_supervised_or_rejected() {
        for i in 0..100 {
            let manager = Arc::new(
                ProcessManagerBuilder::default()
                    .pre_insert(Job::new("short", move || async move {
                        tokio::time::sleep(Duration::from_micros(i * 20)).await;
                        Ok(())
                    }))
                    .build(),
            );
            let mut run = tokio::spawn({
                let manager = Arc::clone(&manager);
                async move { manager.process_start().await }
            });

            // Aim the add at the moment the job finishes and the manager exits.
            tokio::time::sleep(Duration::from_micros(i * 20)).await;
            let accepted = match manager.add(IdleProcess::new()) {
                Ok(()) => true,
                Err(AddError::Stopped) => false,
                Err(other) => panic!("unexpected {other:?}"),
            };

            if accepted {
                ACCEPTED.fetch_add(1, Ordering::SeqCst);
                // The idle child keeps the manager alive until shut down.
                assert!(
                    timeout(Duration::from_millis(20), &mut run).await.is_err(),
                    "round {i}: manager exited although it accepted a child"
                );
                manager.process_handle().shutdown().await;
            }
            timeout(Duration::from_secs(2), run)
                .await
                .expect("manager must stop")
                .expect("manager task panicked")
                .expect("clean shutdown");
        }
        let accepted = ACCEPTED.load(Ordering::SeqCst);
        eprintln!("add racing exit: {accepted}/100 accepted, the rest rejected");
    }
}

mod runtime_error {
    use super::*;
    use std::error::Error as _;

    #[derive(Debug)]
    struct DbError {
        code: u32,
    }

    impl std::fmt::Display for DbError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "database error {}", self.code)
        }
    }

    impl std::error::Error for DbError {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            None
        }
    }

    /// Fails with a typed error through the manager.
    struct FailsTyped;

    impl Runnable for FailsTyped {
        fn process_start(&self) -> ProcFuture<'_> {
            Box::pin(async { Err(RuntimeError::failed(DbError { code: 42 })) })
        }

        fn process_handle(&self) -> Arc<dyn ProcessControlHandler> {
            Arc::new(StubControlHandle)
        }
    }

    #[tokio::test]
    async fn typed_child_error_survives_the_manager() {
        let manager = ProcessManagerBuilder::default()
            .pre_insert(FailsTyped)
            .build();
        let err = timeout(Duration::from_secs(1), manager.process_start())
            .await
            .expect("manager must stop")
            .expect_err("child failed");

        let db = err
            .downcast_ref::<DbError>()
            .unwrap_or_else(|| panic!("typed error lost: {err:?}"));
        assert_eq!(db.code, 42);
        assert_eq!(err.to_string(), "database error 42");
        assert!(err.source().is_none(), "transparent: no duplicate link");
    }

    #[test]
    fn question_mark_converts_io_errors() {
        fn body() -> Result<(), RuntimeError> {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "config.toml",
            ))?;
            Ok(())
        }
        let err = body().unwrap_err();
        assert_eq!(
            err.downcast_ref::<std::io::Error>().map(|e| e.kind()),
            Some(std::io::ErrorKind::NotFound)
        );
    }

    #[test]
    fn source_chain_is_not_duplicated() {
        #[derive(Debug)]
        struct Outer(std::io::Error);
        impl std::fmt::Display for Outer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "loading config failed")
            }
        }
        impl std::error::Error for Outer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }

        let err = RuntimeError::failed(Outer(std::io::Error::other("permission denied")));
        let mut chain = vec![err.to_string()];
        let mut next = err.source();
        while let Some(e) = next {
            chain.push(e.to_string());
            next = e.source();
        }
        assert_eq!(chain, ["loading config failed", "permission denied"]);
    }

    #[test]
    fn other_variants_have_no_source_or_downcast() {
        let internal = RuntimeError::Internal {
            message: "x".into(),
        };
        assert!(internal.source().is_none());
        assert!(internal.downcast_ref::<std::io::Error>().is_none());
        assert!(RuntimeError::TerminationSignal.source().is_none());
    }
}
