use super::{
    ProcessControlHandler, ProcessOperation, Runnable, RuntimeControlMessage, RuntimeError,
    RuntimeGuard,
};
use async_trait::async_trait;
use futures::stream::StreamExt as _;
use signal_hook::consts::signal::*;
use signal_hook::iterator::Handle;
use signal_hook_tokio::{Signals, SignalsInfo};
use tokio::sync::Mutex;

pub struct SignalReceiver {
    signals: Mutex<SignalsInfo>,
    signal_handle: Handle,
    runtime_guard: RuntimeGuard,
}

impl SignalReceiver {
    pub fn new() -> Self {
        let signals = Signals::new([SIGHUP, SIGINT, SIGTERM, SIGQUIT])
            .map_err(|err| RuntimeError::Internal {
                message: format!("register process signals: {err:?}"),
            })
            .expect("signals to register");

        let handle = signals.handle();

        Self {
            signals: Mutex::new(signals),
            signal_handle: handle,
            runtime_guard: RuntimeGuard::default(),
        }
    }
}

impl Default for SignalReceiver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Runnable for SignalReceiver {
    async fn process_start(&self) -> Result<(), RuntimeError> {
        let mut signals = self.signals.lock().await;

        loop {
            let signal = match self.runtime_guard.tick(signals.next()).await {
                ProcessOperation::Next(None) => continue,
                ProcessOperation::Next(Some(signal)) => signal,
                ProcessOperation::Control(RuntimeControlMessage::Shutdown) => break,
                ProcessOperation::Control(RuntimeControlMessage::Reload) => continue,
            };

            //      tracing::warn!("Received process signal: {signal:?}");

            match signal {
                SIGHUP => {
                    // Reload configuration
                    // Reopen the log file
                    //             tracing::warn!("signal handler {signal:?} not implemented");
                }
                SIGTERM | SIGINT | SIGQUIT => {
                    // Shutdown the system;
                    self.signal_handle.close();

                    return Err(RuntimeError::TerminationSignal);
                }
                _ => unreachable!(),
            }
        }

        Ok(())
    }

    fn process_handle(&self) -> Box<dyn ProcessControlHandler> {
        Box::new(self.runtime_guard.handle())
    }
}
