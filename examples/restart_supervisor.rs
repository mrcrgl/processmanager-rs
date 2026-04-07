//! Restart supervisor demo.
//!
//! Shows how `RestartSupervisor` can wrap a flaky `Runnable` and restart it
//! with exponential backoff until it succeeds.
//!
//! Build & run:
//! ```bash
//! cargo run --example restart_supervisor
//! ```

use processmanager::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::time::{interval, sleep};

struct FlakyWorker {
    fail_until_attempt: usize,
    attempts: Arc<AtomicUsize>,
    guard: Arc<RuntimeGuard>,
}

impl FlakyWorker {
    fn new(fail_until_attempt: usize, attempts: Arc<AtomicUsize>) -> Self {
        Self {
            fail_until_attempt,
            attempts,
            guard: Arc::new(RuntimeGuard::default()),
        }
    }
}

impl Runnable for FlakyWorker {
    fn process_start(&self) -> ProcFuture<'_> {
        let attempts = Arc::clone(&self.attempts);
        let guard = Arc::clone(&self.guard);
        let fail_until_attempt = self.fail_until_attempt;

        Box::pin(async move {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
            println!("flaky-worker: start attempt {attempt}");

            if attempt <= fail_until_attempt {
                return Err(RuntimeError::Internal {
                    message: format!("simulated failure at attempt {attempt}"),
                });
            }

            let ticker = guard.runtime_ticker().await;
            let mut beat = interval(Duration::from_millis(500));

            loop {
                match ticker.tick(beat.tick()).await {
                    ProcessOperation::Next(_) => println!("flaky-worker: heartbeat"),
                    ProcessOperation::Control(RuntimeControlMessage::Shutdown) => {
                        println!("flaky-worker: shutdown");
                        break;
                    }
                    ProcessOperation::Control(RuntimeControlMessage::Reload) => {
                        println!("flaky-worker: reload");
                    }
                    ProcessOperation::Control(_) => continue,
                }
            }

            Ok(())
        })
    }

    fn process_handle(&self) -> Arc<dyn ProcessControlHandler> {
        self.guard.handle()
    }

    fn process_name(&self) -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("FlakyWorker")
    }
}

#[tokio::main]
async fn main() {
    let attempts = Arc::new(AtomicUsize::new(0));

    let wrapped = RestartSupervisor::new(FlakyWorker::new(2, Arc::clone(&attempts))).backoff(
        RestartBackoff::new(Duration::from_millis(200), Duration::from_secs(1), 2),
    );

    let manager = ProcessManagerBuilder::default().pre_insert(wrapped).build();
    let handle = manager.process_handle();

    tokio::spawn(async move {
        manager
            .process_start()
            .await
            .expect("manager encountered an error");
    });

    // Let the wrapped worker fail twice, restart, and then run.
    sleep(Duration::from_secs(3)).await;
    println!(
        "main: observed {} total worker start attempts",
        attempts.load(Ordering::SeqCst)
    );

    handle.shutdown().await;
    sleep(Duration::from_millis(300)).await;
}
