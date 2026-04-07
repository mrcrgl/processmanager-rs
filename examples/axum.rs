//! Axum integration example.
//!
//! Demonstrates running an `axum` HTTP server under `ProcessManager` using
//! `RunnableWithContext` so no `RuntimeGuard` field is needed in the server
//! runnable.
//!
//! Build & run:
//! ```bash
//! cargo run --example axum
//! ```

use axum::{Router, routing::get};
use processmanager::*;
use std::{borrow::Cow, time::Duration};
use tokio::sync::oneshot;
use tokio::time::sleep;

struct AxumServer;

impl RunnableWithContext for AxumServer {
    fn process_start_with_context(&self, ctx: RuntimeContext) -> ProcFuture<'_> {
        Box::pin(async move {
            let app = Router::new()
                .route("/", get(root))
                .route("/healthz", get(healthz));

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .map_err(|err| RuntimeError::Internal {
                    message: format!("failed to bind axum listener: {err}"),
                })?;

            let local_addr = listener
                .local_addr()
                .map_err(|err| RuntimeError::Internal {
                    message: format!("failed to read local address: {err}"),
                })?;

            println!("axum server listening on http://{local_addr}");

            let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
            let mut shutdown_tx = Some(shutdown_tx);

            let mut server_task = tokio::spawn(async move {
                axum::serve(listener, app)
                    .with_graceful_shutdown(async move {
                        let _ = shutdown_rx.await;
                    })
                    .await
            });

            loop {
                match ctx
                    .tick(tokio::time::timeout(
                        Duration::from_millis(200),
                        &mut server_task,
                    ))
                    .await
                {
                    ProcessOperation::Next(Ok(join_result)) => match join_result {
                        Ok(Ok(())) => return Ok(()),
                        Ok(Err(err)) => {
                            return Err(RuntimeError::Internal {
                                message: format!("axum server stopped with error: {err}"),
                            });
                        }
                        Err(join_err) => {
                            return Err(RuntimeError::Internal {
                                message: format!("axum server task join error: {join_err}"),
                            });
                        }
                    },
                    ProcessOperation::Next(Err(_timeout)) => continue,
                    ProcessOperation::Control(RuntimeControlMessage::Shutdown) => {
                        if let Some(tx) = shutdown_tx.take() {
                            let _ = tx.send(());
                        }

                        match server_task.await {
                            Ok(Ok(())) => return Ok(()),
                            Ok(Err(err)) => {
                                return Err(RuntimeError::Internal {
                                    message: format!("axum shutdown failed: {err}"),
                                });
                            }
                            Err(join_err) => {
                                return Err(RuntimeError::Internal {
                                    message: format!("axum shutdown join error: {join_err}"),
                                });
                            }
                        }
                    }
                    ProcessOperation::Control(RuntimeControlMessage::Reload) => {
                        println!("axum server: reload signal received");
                    }
                    ProcessOperation::Control(_) => {}
                }
            }
        })
    }

    fn process_name(&self) -> Cow<'static, str> {
        Cow::Borrowed("AxumServer")
    }
}

async fn root() -> &'static str {
    "hello from processmanager + axum"
}

async fn healthz() -> &'static str {
    "ok"
}

#[tokio::main]
async fn main() {
    let manager = ProcessManagerBuilder::default()
        .name("axum-demo")
        .pre_insert(with_runtime_context(AxumServer))
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
