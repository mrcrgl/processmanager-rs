//! A minimal, self-contained demo of `ProcessManager`.
//!
//! • Builds a manager.
//! • Registers two workers **before** start-up.
//! • Runs for three seconds.
//! • Initiates a graceful shutdown.
//!
//! Build & run:
//! ```bash
//! cargo run --example simple
//! ```

use processmanager::*;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::{interval, sleep};

/// A lightweight long-running component that emits a heartbeat.
struct Worker {
    id: usize,
    guard: Arc<RuntimeGuard>,
}

impl Worker {
    fn new(id: usize) -> Self {
        Self {
            id,
            guard: Arc::new(RuntimeGuard::default()),
        }
    }
}

impl Runnable for Worker {
    fn process_start(&self) -> ProcFuture<'_> {
        let id = self.id;
        let guard = self.guard.clone();

        Box::pin(async move {
            let ticker = guard.runtime_ticker().await;
            let mut tic = interval(Duration::from_secs(1));

            loop {
                match ticker.tick(tic.tick()).await {
                    ProcessOperation::Next(_) => {
                        println!("worker-{id}: heartbeat");
                    }
                    ProcessOperation::Control(RuntimeControlMessage::Reload) => {
                        println!("worker-{id}: received *reload*");
                    }
                    ProcessOperation::Control(RuntimeControlMessage::Shutdown) => {
                        println!("worker-{id}: shutting down");
                        break;
                    }
                    // absorb any future control messages we don't explicitly handle
                    ProcessOperation::Control(_) => continue,
                }
            }
            Ok(())
        })
    }

    fn process_handle(&self) -> Arc<dyn ProcessControlHandler> {
        self.guard.handle()
    }
}

#[tokio::main]
async fn main() {
    // -----------------------------------------------------------
    // 1. Build a manager and register two workers
    // -----------------------------------------------------------
    let manager = ProcessManagerBuilder::default()
        .pre_insert(Worker::new(0))
        .pre_insert(Worker::new(1))
        .build();

    let handle = manager.process_handle();

    // Spawn the supervisor; it will oversee both workers.
    tokio::spawn(async move {
        manager
            .process_start()
            .await
            .expect("manager encountered an error");
    });

    // -----------------------------------------------------------
    // 2. Let the system run for a short while
    // -----------------------------------------------------------
    println!("==> main: sleeping 3 s");
    sleep(Duration::from_secs(3)).await;

    // -----------------------------------------------------------
    // 3. Graceful shutdown
    // -----------------------------------------------------------
    println!("==> main: initiating graceful shutdown");
    handle.shutdown().await;

    // Give children time to print their exit messages
    sleep(Duration::from_secs(1)).await;
}
