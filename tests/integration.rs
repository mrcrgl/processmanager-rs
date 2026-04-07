use processmanager::{
    ProcFuture, ProcessControlHandler, ProcessManager, ProcessManagerBuilder, ProcessOperation,
    Runnable, RuntimeControlMessage, RuntimeError, RuntimeGuard,
};
use std::ops::Add;
use std::sync::Arc;
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

    manager_task.abort();
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
