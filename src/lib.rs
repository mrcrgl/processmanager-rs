/// Manage multiple running services. A ProcessManager collects impl of `Runnable`
/// and takes over the runtime management like starting, stopping (graceful or in
/// failure) of services.
///
/// ```rust
/// use processmanager::*;
/// use std::sync::Arc;
///
/// #[tokio::main]
/// async fn main() {
///
///     #[derive(Default)]
///     struct ExampleController {
///         runtime_guard: RuntimeGuard,
///     }
///
///     impl Runnable for ExampleController {
///         fn process_start(&self) -> ProcFuture<'_> {
///             Box::pin(async{
///                 let ticker = self.runtime_guard.runtime_ticker().await;
///
///                 // This can be any type of future like an async streams
///                 let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(1));
///
///                 loop {
///                     match ticker.tick(interval.tick()).await {
///                         ProcessOperation::Next(_) => println!("work"),
///                         ProcessOperation::Control(RuntimeControlMessage::Shutdown) => {
///                             println!("shutdown");
///                             break
///                         },
///                         ProcessOperation::Control(RuntimeControlMessage::Reload) => println!("trigger relead"),
///                         ProcessOperation::Control(RuntimeControlMessage::Custom(_)) => println!("trigger custom action"),
///                     }
///                 }
///
///                 Ok(())
///             })
///         }
///
///         fn process_handle(&self) -> Arc<dyn ProcessControlHandler> {
///             self.runtime_guard.handle()
///          }
///     }
///
///     let manager = ProcessManagerBuilder::default()
///         .pre_insert(ExampleController::default())
///         .build();
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
mod process_manager_builder;
#[cfg(feature = "signal")]
pub mod receiver;
mod runtime_guard;
mod runtime_handle;
mod runtime_process;
mod runtime_ticker;
pub use error::*;
pub use process_manager::*;
pub use process_manager_builder::*;
pub use runtime_guard::*;
pub use runtime_handle::*;
pub use runtime_process::*;
pub use runtime_ticker::*;
