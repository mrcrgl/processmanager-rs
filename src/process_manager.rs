use futures::FutureExt;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use super::{ProcessControlHandler, Runnable, RuntimeError};

static PID: OnceLock<AtomicUsize> = OnceLock::new();

pub struct ProcessManager {
    id: usize,
    processes: Vec<Arc<Box<dyn Runnable + Send + Sync + 'static>>>,
}

impl ProcessManager {
    pub fn new() -> Self {
        let pid = PID.get_or_init(|| AtomicUsize::new(0));
        Self {
            id: pid.fetch_add(1, Ordering::SeqCst),
            processes: vec![],
        }
    }

    pub fn insert(&mut self, process: impl Runnable + Send + Sync + 'static) {
        self.processes.push(Arc::new(Box::new(process)));
    }
}

#[async_trait::async_trait]
impl Runnable for ProcessManager {
    async fn process_start(&self) -> Result<(), RuntimeError> {
        async fn wrap_proc<F, Fut>(
            proc: Arc<Box<dyn Runnable + Send + Sync + 'static>>,
            init_shutdown: F,
        ) -> Result<(), RuntimeError>
        where
            Fut: Future<Output = ()>,
            F: FnOnce() -> Fut,
        {
            let proc_name = proc.process_name();

            #[cfg(feature = "log")]
            ::log::info!("Start process {proc_name}");
            #[cfg(feature = "tracing")]
            ::tracing::info!("Start process {proc_name}");
            #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
            eprintln!("Start process {proc_name}");

            let proc = proc.to_owned();
            tokio::spawn(async move { proc.process_start().await })
                .then(|prev| async {
                    let prev = prev.map_err(|err| RuntimeError::Internal {
                        message: format!("tokio spawn join error: {err:?}"),
                    })?;
                    if prev.is_err() {
                        #[cfg(feature = "log")]
                        ::log::error!(
                            "Process {proc_name} stopped unexpectedly: {:?}",
                            prev.as_ref().unwrap_err()
                        );
                        #[cfg(feature = "tracing")]
                        ::tracing::error!(
                            "Process {proc_name} stopped unexpectedly: {:?}",
                            prev.as_ref().unwrap_err()
                        );
                        #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
                        eprintln!(
                            "Process {proc_name} stopped unexpectedly: {:?}",
                            prev.as_ref().unwrap_err()
                        );
                        init_shutdown().await;
                    } else {
                        #[cfg(feature = "log")]
                        ::log::info!("Process {proc_name} stopped");
                        #[cfg(feature = "tracing")]
                        ::tracing::info!("Process {proc_name} stopped");
                        #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
                        eprintln!("Process {proc_name} stopped");
                    }
                    prev
                })
                .await
        }

        let handle = self.process_handle();
        let process_futures = self
            .processes
            .iter()
            .map(|proc| wrap_proc(proc.clone(), || async { handle.shutdown().await }))
            .collect::<Vec<_>>();

        #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
        eprintln!("Manager {} started", self.process_name());

        let result = futures::future::try_join_all(process_futures)
            .await
            .map(|_| ());

        #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
        eprintln!(
            "Manager {} end err={:?}",
            self.process_name(),
            result.is_err()
        );

        result
    }

    fn process_name(&self) -> String {
        format!("process-manager-{}", self.id)
    }

    fn process_handle(&self) -> Box<dyn ProcessControlHandler> {
        Box::new(ProcessHandle {
            runtime_handles: self
                .processes
                .iter()
                .map(|proc| (proc.process_name().clone(), proc.process_handle()))
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

#[async_trait::async_trait]
impl ProcessControlHandler for ProcessHandle {
    async fn shutdown(&self) {
        // TODO make shutdowns in parallel
        #[allow(unused_variables)]
        for (name, runtime_handle) in self.runtime_handles.iter() {
            #[cfg(feature = "log")]
            ::log::info!("Initiate shutdown on process {name}");
            #[cfg(feature = "tracing")]
            ::tracing::info!("Initiate shutdown on process {name}");
            #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
            eprintln!("Initiate shutdown on process {name}");

            runtime_handle.shutdown().await;
            #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
            eprintln!("Successfully shut down process {name}");
        }
    }

    async fn reload(&self) {
        // TODO make reloads in parallel
        #[allow(unused_variables)]
        for (name, runtime_handle) in self.runtime_handles.iter() {
            #[cfg(feature = "log")]
            ::log::info!("Initiate reload on process {name}");
            #[cfg(feature = "tracing")]
            ::tracing::info!("Initiate reload on process {name}");
            #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
            eprintln!("Initiate reload on process {name}");
            runtime_handle.reload().await;
        }
    }
}
