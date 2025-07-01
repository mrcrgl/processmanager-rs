# ProcessManager

Manage multiple running services. A ProcessManager collects impl of `Runnable`
and takes over the runtime management like starting, stopping (graceful or in
failure) of services.

If one service fails, the manager initiates  a graceful shutdown for all other.

# Examples

```rust
use processmanager::*;

#[tokio::main]
async fn main() {

    #[derive(Default)]
    struct ExampleController {
        runtime_guard: RuntimeGuard,
    }

    impl Runnable for ExampleController {
        fn process_start(&self) -> ProcFuture<'_> {
            Box::pin(async {
                // This can be any type of future like an async streams
                let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(1));

                loop {
                    match self.runtime_guard.tick(interval.tick()).await {
                        ProcessOperation::Next(_) => println!("work"),
                        ProcessOperation::Control(RuntimeControlMessage::Shutdown) => {
                            println!("shutdown");
                            break
                        },
                        ProcessOperation::Control(RuntimeControlMessage::Reload) => println!("trigger relead"),
                    }
                }

                Ok(())
            })
        }

        fn process_handle(&self) -> Box<dyn ProcessControlHandler> {
            Box::new(self.runtime_guard.handle())
         }
    }

    let mut manager = ProcessManager::new();
    manager.insert(ExampleController::default());

    let handle = manager.process_handle();

    // start all processes
    let _ = tokio::spawn(async move {
        manager.process_start().await.expect("service start failed");
    });

    // Shutdown waits for all services to shutdown gracefully.
    handle.shutdown().await;
}

```
