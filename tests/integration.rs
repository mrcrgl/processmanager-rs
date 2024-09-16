use std::ops::Add;
use processmanager::{
    ProcessControlHandler, ProcessManager, ProcessOperation, Runnable, RuntimeControlMessage,
    RuntimeError, RuntimeGuard,
};
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

#[async_trait::async_trait]
impl Runnable for ExampleController {
    async fn process_start(&self) -> Result<(), RuntimeError> {
        let ticker = self.runtime_guard.runtime_ticker().await;
        // This can be any type of future like an async streams
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(1));
        let started = tokio::time::Instant::now();

        loop {
            match ticker.tick(interval.tick()).await {
                ProcessOperation::Next(_) => {
                    if let Some(die_after) = self.die_after {
                        if started.add(die_after).lt(&tokio::time::Instant::now()) {
                            return Err(RuntimeError::Internal { message: format!("died after {:?}", die_after) })
                        }
                    }
                    if let Some(exit_after) = self.exit_after {
                        if started.add(exit_after).lt(&tokio::time::Instant::now()) {
                            return Ok(())
                        }
                    }
                    println!("work {}", self.id)
                },
                ProcessOperation::Control(RuntimeControlMessage::Shutdown) => {
                    println!("shutdown {}", self.id);
                    break;
                }
                ProcessOperation::Control(RuntimeControlMessage::Reload) => {
                    println!("trigger reload {}", self.id)
                }
            }
        }

        Ok(())
    }

    fn process_handle(&self) -> Box<dyn ProcessControlHandler> {
        Box::new(self.runtime_guard.handle())
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
    let controller1 = ExampleController::new(1, None, None );
    let controller2 = ExampleController::new(2, Some(Duration::from_secs(2)), None );

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
    let controller1 = ExampleController::new(1, None, None );
    let controller2 = ExampleController::new(2, None, Some(Duration::from_secs(2)) );

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
        "expectd time out"
    );
}

#[tokio::test]
async fn test_nested_process_runnable_multiple_one_exits_2() {
    let controller1 = ExampleController::new(1, None, None );
    let controller2 = ExampleController::new(2, None, Some(Duration::from_secs(2)) );

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
