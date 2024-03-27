use std::future::Future;
use std::sync::Arc;

use futures::FutureExt;

use super::{ProcessControlHandler, Runnable, RuntimeError};

pub struct ProcessManager {
    processes: Vec<Arc<Box<dyn Runnable + Send + Sync + 'static>>>,
}

impl ProcessManager {
    pub fn new() -> Self {
        Self { processes: vec![] }
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
            let proc_name = proc.process_name().to_string();
            //tracing::info!("Start process {proc_name}");

            let proc = proc.to_owned();
            tokio::spawn(async move { proc.process_start().await })
                .then(|prev| async {
                    let prev = prev.map_err(|err| RuntimeError::Internal {
                        message: format!("tokio spawn join error: {err:?}"),
                    })?;
                    if prev.is_err() {
                        /*          tracing::error!(
                            "Process {proc_name} stopped unexpectedly: {:?}",
                            prev.as_ref().unwrap_err()
                        );*/
                        init_shutdown().await;
                    } else {
                        //tracing::info!("Process {proc_name} stopped");
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
        let _bars: Vec<_> = futures::future::join_all(process_futures).await;
        Ok(())
    }

    fn process_handle(&self) -> Box<dyn ProcessControlHandler> {
        Box::new(ProcessHandle {
            runtime_handles: self
                .processes
                .iter()
                .map(|proc| (proc.process_name(), proc.process_handle()))
                .collect(),
        })
    }
}

impl Default for ProcessManager {
    fn default() -> Self {
        Self::new()
    }
}

struct ProcessHandle<'a> {
    runtime_handles: Vec<(&'a str, Box<dyn ProcessControlHandler>)>,
}

#[async_trait::async_trait]
impl ProcessControlHandler for ProcessHandle<'_> {
    async fn shutdown(&self) {
        // TODO make shutdowns in parallel
        for (name, runtime_handle) in self.runtime_handles.iter() {
            //tracing::info!("Initiate shutdown on process {name}");
            runtime_handle.shutdown().await;
        }
    }

    async fn reload(&self) {
        // TODO make reloads in parallel
        for (name, runtime_handle) in self.runtime_handles.iter() {
            //tracing::info!("Initiate reload on process {name}");
            runtime_handle.reload().await;
        }
    }
}

impl Clone for ProcessHandle<'_> {
    fn clone(&self) -> Self {
        todo!()
    }
}
