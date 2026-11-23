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
                        if let Some(die_after) = self.die_after {
                            if started.add(die_after).lt(&tokio::time::Instant::now()) {
                                return Err(RuntimeError::Internal {
                                    message: format!("died after {:?}", die_after),
                                });
                            }
                        }
                        if let Some(exit_after) = self.exit_after {
                            if started.add(exit_after).lt(&tokio::time::Instant::now()) {
                                return Ok(());
                            }
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

#[test]
fn test_add_before_start_panics_with_contract_message() {
    let manager = ProcessManager::new();
    let attempts = Arc::new(AtomicUsize::new(0));

    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        manager.add(FlakyController::new(0, Arc::clone(&attempts)));
    }))
    .expect_err("add() before startup must panic");

    let message = if let Some(s) = panic.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_owned()
    };

    assert!(
        message.contains("cannot call add() before manager has started"),
        "unexpected panic message: {message}"
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

    match result {
        Err(RuntimeError::Internal { message }) => {
            assert!(
                message.contains("gave up") && message.contains("intentional permanent failure"),
                "error must explain the give-up and carry the last failure, got: {message}"
            );
        }
        other => panic!("expected give-up error, got {other:?}"),
    }
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

    assert!(
        matches!(&result, Err(RuntimeError::Internal { message }) if message.contains("gave up")),
        "manager must return the give-up error, got {result:?}"
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
