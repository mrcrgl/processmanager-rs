//! `SignalReceiver` / `ProcessManagerBuilder::handle_signals` tests.
//!
//! Signals are process-wide, so these tests live in their own binary and run
//! one at a time (see `SERIAL`). A backstop handler keeps a signal that
//! arrives before a receiver is registered from killing the test binary; the
//! tests re-raise until the receiver has reacted, so they don't depend on
//! timing.
#![cfg(all(unix, feature = "signal"))]

use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;

use processmanager::{
    IdleProcess, ProcFuture, ProcessControlHandler, ProcessManagerBuilder, ProcessOperation,
    Runnable, RuntimeControlMessage, RuntimeError, RuntimeGuard, builtin::SignalReceiver,
};
use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGQUIT, SIGTERM};
use tokio::{sync::Mutex, task::JoinHandle, time::timeout};

static SERIAL: Mutex<()> = Mutex::const_new(());

fn install_backstop() {
    static BACKSTOP: OnceLock<()> = OnceLock::new();
    BACKSTOP.get_or_init(|| {
        let flag = Arc::new(AtomicBool::new(false));
        for sig in [SIGHUP, SIGINT, SIGTERM, SIGQUIT] {
            signal_hook::flag::register(sig, Arc::clone(&flag)).expect("register backstop");
        }
    });
}

/// Raises `signal` until `task` finishes; returns its result.
async fn raise_until_finished<T>(signal: i32, mut task: JoinHandle<T>) -> T {
    for _ in 0..40 {
        signal_hook::low_level::raise(signal).expect("raise signal");
        if let Ok(result) = timeout(Duration::from_millis(50), &mut task).await {
            return result.expect("task panicked");
        }
    }
    panic!("signal {signal} did not stop the manager");
}

/// Counts reloads; stops on shutdown.
#[derive(Default)]
struct ReloadCounter {
    reloads: Arc<AtomicUsize>,
    guard: RuntimeGuard,
}

impl Runnable for ReloadCounter {
    fn process_start(&self) -> ProcFuture<'_> {
        Box::pin(async {
            let ticker = self.guard.runtime_ticker().await;
            loop {
                match ticker.tick(std::future::pending::<()>()).await {
                    ProcessOperation::Control(RuntimeControlMessage::Reload) => {
                        self.reloads.fetch_add(1, Ordering::SeqCst);
                    }
                    ProcessOperation::Control(RuntimeControlMessage::Shutdown) => return Ok(()),
                    _ => {}
                }
            }
        })
    }

    fn process_handle(&self) -> Arc<dyn ProcessControlHandler> {
        self.guard.handle()
    }
}

#[tokio::test]
async fn handle_signals_sigterm_shuts_down_gracefully() {
    let _serial = SERIAL.lock().await;
    install_backstop();

    let manager = ProcessManagerBuilder::default()
        .handle_signals()
        .pre_insert(IdleProcess::new())
        .build();
    let run = tokio::spawn(async move { manager.process_start().await });

    let result = raise_until_finished(SIGTERM, run).await;
    assert!(
        result.is_ok(),
        "SIGTERM must be a graceful stop, got {result:?}"
    );
}

#[tokio::test]
async fn handle_signals_sigint_shuts_down_gracefully() {
    let _serial = SERIAL.lock().await;
    install_backstop();

    let manager = ProcessManagerBuilder::default()
        .handle_signals()
        .pre_insert(IdleProcess::new())
        .build();
    let run = tokio::spawn(async move { manager.process_start().await });

    let result = raise_until_finished(SIGINT, run).await;
    assert!(
        result.is_ok(),
        "SIGINT must be a graceful stop, got {result:?}"
    );
}

#[tokio::test]
async fn handle_signals_sighup_reloads_children() {
    let _serial = SERIAL.lock().await;
    install_backstop();

    let counter = ReloadCounter::default();
    let reloads = Arc::clone(&counter.reloads);
    let manager = ProcessManagerBuilder::default()
        .handle_signals()
        .pre_insert(counter)
        .build();
    let mut run = tokio::spawn(async move { manager.process_start().await });

    let mut reloaded = false;
    for _ in 0..40 {
        signal_hook::low_level::raise(SIGHUP).expect("raise SIGHUP");
        tokio::time::sleep(Duration::from_millis(50)).await;
        if reloads.load(Ordering::SeqCst) > 0 {
            reloaded = true;
            break;
        }
    }
    assert!(reloaded, "SIGHUP must reload the manager's children");
    assert!(
        timeout(Duration::from_millis(100), &mut run).await.is_err(),
        "SIGHUP must not stop the manager"
    );

    let result = raise_until_finished(SIGTERM, run).await;
    assert!(result.is_ok(), "got {result:?}");
}

#[tokio::test]
async fn standalone_receiver_shuts_down_gracefully() {
    let _serial = SERIAL.lock().await;
    install_backstop();

    let manager = ProcessManagerBuilder::default()
        .pre_insert(SignalReceiver::new())
        .pre_insert(IdleProcess::new())
        .build();
    let run = tokio::spawn(async move { manager.process_start().await });

    let result = raise_until_finished(SIGQUIT, run).await;
    assert!(
        result.is_ok(),
        "a stand-alone receiver must also stop the manager gracefully, got {result:?}"
    );
}

#[tokio::test]
async fn standalone_receiver_returns_termination_signal() {
    let _serial = SERIAL.lock().await;
    install_backstop();

    let receiver = Arc::new(SignalReceiver::new());
    let run = tokio::spawn({
        let receiver = Arc::clone(&receiver);
        async move { receiver.process_start().await }
    });

    let result = raise_until_finished(SIGTERM, run).await;
    assert!(
        matches!(result, Err(RuntimeError::TerminationSignal)),
        "got {result:?}"
    );
}
