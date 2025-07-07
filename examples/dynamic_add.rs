//! Dynamic `ProcessManager` demo.
//!
//! Showcases
//! ---------
//! 1. starting a manager with one child,
//! 2. adding new `Runnable`s *after* the manager is already running,
//! 3. shutting the whole tree down gracefully.
//!
//! Build & run
//! -----------
//! ```bash
//! cargo run --example dynamic_add
//! ```
mod simple;

use processmanager::*;
use std::{sync::Arc, time::Duration};
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
            let mut beat = interval(Duration::from_secs(1));

            loop {
                match ticker.tick(beat.tick()).await {
                    ProcessOperation::Next(_) => println!("worker-{id}: heartbeat"),
                    ProcessOperation::Control(RuntimeControlMessage::Reload) => {
                        println!("worker-{id}: received *reload*")
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
    // ------------------------------------------------------------------
    // 1. Manager with a single initial worker
    // ------------------------------------------------------------------
    let mgr = ProcessManagerBuilder::default()
        .pre_insert(Worker::new(0))
        .build();

    // We need to keep access to the manager after starting it, therefore
    // wrap it in an `Arc` and clone it for the spawning task.
    let mgr: Arc<ProcessManager> = Arc::new(mgr);
    let mgr_clone = Arc::clone(&mgr);

    tokio::spawn(async move {
        mgr_clone
            .process_start()
            .await
            .expect("manager encountered an error");
    });

    let handle = mgr.process_handle();

    // ------------------------------------------------------------------
    // 2. Dynamically add workers
    // ------------------------------------------------------------------
    println!("==> main: sleeping 3 s before adding worker-1");
    sleep(Duration::from_secs(3)).await;

    println!("==> main: adding worker-1");
    mgr.add(Worker::new(1));

    println!("==> main: sleeping 2 s");
    sleep(Duration::from_secs(2)).await;

    println!("==> main: adding worker-2");
    mgr.add(Worker::new(2));

    // ------------------------------------------------------------------
    // 3. Graceful shutdown after a short delay
    // ------------------------------------------------------------------
    println!("==> main: running 5 s before global shutdown");
    sleep(Duration::from_secs(5)).await;

    println!("==> main: initiating graceful shutdown");
    handle.shutdown().await;

    // Allow children to print their exit messages
    sleep(Duration::from_secs(1)).await;
}
