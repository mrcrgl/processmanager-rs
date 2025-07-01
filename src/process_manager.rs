use futures::FutureExt;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use super::{CtrlFuture, ProcFuture, ProcessControlHandler, Runnable, RuntimeError};

static PID: OnceLock<AtomicUsize> = OnceLock::new();

/// Groups several [`Runnable`] instances and starts / stops them as a unit.
///
/// Every child process runs in its own Tokio task; if one exits with an error
/// the manager propagates a shutdown to the remaining ones.
pub struct ProcessManager {
    id: usize,
    processes: Vec<Arc<Box<dyn Runnable>>>,
}

impl ProcessManager {
    pub fn new() -> Self {
        let pid = PID.get_or_init(|| AtomicUsize::new(0));
        Self {
            id: pid.fetch_add(1, Ordering::SeqCst),
            processes: Vec::new(),
        }
    }

    /// Register a child process. Call this **before** the manager is started.
    pub fn insert(&mut self, process: impl Runnable) {
        self.processes.push(Arc::new(Box::new(process)));
    }
}

impl Runnable for ProcessManager {
    fn process_start(&self) -> ProcFuture<'_> {
        Box::pin(async move {
            /// Helper that spawns a process and handles its completion.
            async fn wrap_proc<F, Fut>(
                proc: Arc<Box<dyn Runnable>>,
                init_shutdown: F,
            ) -> Result<(), RuntimeError>
            where
                Fut: Future<Output = ()>,
                F: FnOnce() -> Fut,
            {
                let name = proc.process_name();

                #[cfg(feature = "tracing")]
                ::tracing::info!("Start process {name}");
                #[cfg(all(not(feature = "tracing"), feature = "log"))]
                ::log::info!("Start process {name}");
                #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
                eprintln!("Start process {name}");

                let fut = async move { proc.process_start().await };

                tokio::spawn(fut)
                    .then(|join| async {
                        let result = join.map_err(|err| RuntimeError::Internal {
                            message: format!("tokio join error: {err:?}"),
                        })?;

                        match result {
                            Ok(_) => {
                                #[cfg(feature = "tracing")]
                                ::tracing::info!("Process {name} stopped");
                                #[cfg(all(not(feature = "tracing"), feature = "log"))]
                                ::log::info!("Process {name} stopped");
                                #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
                                eprintln!("Process {name} stopped");
                            }
                            Err(ref err) => {
                                #[cfg(feature = "tracing")]
                                ::tracing::error!("Process {name} failed: {err:?}");
                                #[cfg(all(not(feature = "tracing"), feature = "log"))]
                                ::log::error!("Process {name} failed: {err:?}");
                                #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
                                eprintln!("Process {name} failed: {err:?}");

                                // propagate shutdown to the rest of the manager
                                init_shutdown().await;
                            }
                        }
                        result
                    })
                    .await
            }

            let handle = self.process_handle();

            let tasks = self
                .processes
                .iter()
                .map(|p| wrap_proc(p.clone(), || async { handle.shutdown().await }))
                .collect::<Vec<_>>();

            #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
            eprintln!("Manager {} started", self.process_name());

            let result = futures::future::try_join_all(tasks).await.map(|_| ());

            #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
            eprintln!(
                "Manager {} exited (error = {})",
                self.process_name(),
                result.is_err()
            );

            result
        })
    }

    fn process_name(&self) -> String {
        format!("process-manager-{}", self.id)
    }

    fn process_handle(&self) -> Box<dyn ProcessControlHandler> {
        Box::new(ProcessHandle {
            runtime_handles: self
                .processes
                .iter()
                .map(|p| (p.process_name(), p.process_handle()))
                .collect(),
        })
    }
}

impl Default for ProcessManager {
    fn default() -> Self {
        Self::new()
    }
}

struct ProcessHandle {
    runtime_handles: Vec<(String, Box<dyn ProcessControlHandler>)>,
}

impl ProcessControlHandler for ProcessHandle {
    fn shutdown(&self) -> CtrlFuture<'_> {
        Box::pin(async move {
            for (name, handle) in self.runtime_handles.iter() {
                #[cfg(feature = "tracing")]
                ::tracing::info!("Initiating shutdown of process {name}");
                #[cfg(all(not(feature = "tracing"), feature = "log"))]
                ::log::info!("Initiating shutdown of process {name}");
                #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
                eprintln!("Initiating shutdown of process {name}");

                handle.shutdown().await;
            }
        })
    }

    fn reload(&self) -> CtrlFuture<'_> {
        Box::pin(async move {
            for (name, handle) in self.runtime_handles.iter() {
                #[cfg(feature = "tracing")]
                ::tracing::info!("Initiating reload of process {name}");
                #[cfg(all(not(feature = "tracing"), feature = "log"))]
                ::log::info!("Initiating reload of process {name}");
                #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
                eprintln!("Initiating reload of process {name}");

                handle.reload().await;
            }
        })
    }
}
