/// Manage multiple running services. A ProcessManager collects impl of `Runnable`
/// and takes over the runtime management like starting, stopping (graceful or in
/// failure) of services.
///
/// ```rust
/// use processmanager::*;
///
/// #[tokio::main]
/// async fn main() {
///
///     #[derive(Default)]
///     struct ExampleController {
///         runtime_guard: RuntimeGuard,
///     }
///
///     #[async_trait::async_trait]
///     impl Runnable for ExampleController {
///         async fn process_start(&self) -> Result<(), RuntimeError> {
///             let ticker = self.runtime_guard.runtime_ticker().await;
///
///             // This can be any type of future like an async streams
///             let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(1));
///
///             loop {
///                 match ticker.tick(interval.tick()).await {
///                     ProcessOperation::Next(_) => println!("work"),
///                     ProcessOperation::Control(RuntimeControlMessage::Shutdown) => {
///                         println!("shutdown");
///                         break
///                     },
///                     ProcessOperation::Control(RuntimeControlMessage::Reload) => println!("trigger relead"),
///                 }
///             }
///
///             Ok(())
///         }
///
///         fn process_handle(&self) -> Box<dyn ProcessControlHandler> {
///             Box::new(self.runtime_guard.handle())
///          }
///     }
///
///     let mut manager = ProcessManager::new();
///     manager.insert(ExampleController::default());
///
///     let handle = manager.process_handle();
///
///     // start all processes
///     let _ = tokio::spawn(async move {
///         manager.process_start().await.expect("service start failed");
///     });
///
///     // Shutdown waits for all services to shut down gracefully.
///     handle.shutdown().await;
/// }
/// ```
///
mod error;
mod process_manager;
#[cfg(feature = "signal")]
pub mod receiver;
mod runtime_process;

pub use error::*;
pub use process_manager::*;
pub use runtime_process::*;
