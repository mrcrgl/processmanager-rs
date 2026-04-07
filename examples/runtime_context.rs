//! Runtime context demo.
//!
//! Shows how to implement `RunnableWithContext` so a process can receive
//! runtime control messages without storing its own `RuntimeGuard`.
//!
//! Build & run:
//! ```bash
//! cargo run --example runtime_context
//! ```

use processmanager::*;
use std::{borrow::Cow, time::Duration};
use tokio::time::{interval, sleep};

struct Worker {
    id: usize,
}

impl Worker {
    fn new(id: usize) -> Self {
        Self { id }
    }
}

impl RunnableWithContext for Worker {
    fn process_start_with_context(&self, ctx: RuntimeContext) -> ProcFuture<'_> {
        let id = self.id;

        Box::pin(async move {
            let mut beat = interval(Duration::from_secs(1));

            loop {
                match ctx.tick(beat.tick()).await {
                    ProcessOperation::Next(_) => println!("worker-{id}: heartbeat"),
                    ProcessOperation::Control(RuntimeControlMessage::Reload) => {
                        println!("worker-{id}: reload");
                    }
                    ProcessOperation::Control(RuntimeControlMessage::Shutdown) => {
                        println!("worker-{id}: shutdown");
                        break;
                    }
                    ProcessOperation::Control(_) => continue,
                }
            }

            Ok(())
        })
    }

    fn process_name(&self) -> Cow<'static, str> {
        Cow::Borrowed("ContextWorker")
    }
}

#[tokio::main]
async fn main() {
    let manager = ProcessManagerBuilder::default()
        .pre_insert(with_runtime_context(Worker::new(0)))
        .build();

    let handle = manager.process_handle();

    tokio::spawn(async move {
        manager
            .process_start()
            .await
            .expect("manager encountered an error");
    });

    sleep(Duration::from_secs(2)).await;
    handle.reload().await;
    sleep(Duration::from_secs(2)).await;
    handle.shutdown().await;
    sleep(Duration::from_millis(300)).await;
}
