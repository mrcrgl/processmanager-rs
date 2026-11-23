//! Dynamic supervisor for asynchronous [`Runnable`] implementations.
//
//! A `ProcessManager` can
//!
//! • be filled with children *before* it is started via
//!   [`insert`](ProcessManager::insert)
//! • accept additional children *after* it has started via
//!   [`add`](ProcessManager::add)
//!
//! Each child runs in its own Tokio task.  The first child that returns an
//! `Err(_)` causes the supervisor to propagate `shutdown()` to all remaining
//! children and to return that same error.  Children that finish successfully
//! are (optionally) removed from the internal list so that long-running systems
//! do not leak memory.
//
//! The type itself implements [`Runnable`], which means you can build an
//! arbitrary process tree by nesting managers.
//
//! ```ignore
//! # use processmanager::*;
//! # #[derive(Default)] struct MyService;
//! # impl Runnable for MyService {
//! #     fn process_start(&self) -> ProcFuture<'_> { Box::pin(async { Ok(()) }) }
//! #     fn process_handle(&self) -> Arc<dyn ProcessControlHandler> {
//! #         unreachable!()
//! #     }
//! # }
//! let root = ProcessManagerBuilder::default()
//!     .pre_insert(MyService)                // add before start
//!     .build();
//!
//! let handle = root.process_handle();
//! tokio::spawn(async move { root.process_start().await.unwrap(); });
//!
//! handle.reload().await;                    // control a running manager
//! ```
use std::{
    borrow::Cow,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures::FutureExt as _;
use once_cell::sync::OnceCell;
use std::panic::AssertUnwindSafe;
use tokio::{
    sync::mpsc,
    task::{JoinHandle, JoinSet},
    time::Instant,
};

#[cfg(feature = "tracing")]
use tracing::Instrument;

use crate::{CtrlFuture, ProcFuture, ProcessControlHandler, Runnable, RuntimeError};

/// Global monotonically increasing identifier for every `ProcessManager`.
static PID: AtomicUsize = AtomicUsize::new(0);

/// Metadata kept for each child.
struct Child {
    id: usize,
    #[allow(dead_code)]
    proc: Arc<dyn Runnable>,
    handle: Arc<dyn ProcessControlHandler>,
    join_handle: Arc<JoinHandle<()>>,
    /// Set once a shutdown has been sent to this child, so concurrent or
    /// repeated broadcasts don't send it again.
    shutdown_sent: Arc<AtomicBool>,
}

impl Child {
    fn new(id: usize, proc: Arc<dyn Runnable>, inner: &Arc<Inner>) -> Self {
        let handle = proc.process_handle();
        Self {
            id,
            proc: Arc::clone(&proc),
            handle,
            join_handle: Arc::new(spawn_child(id, proc, Arc::clone(inner))),
            shutdown_sent: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// Aborts every child task when dropped.
///
/// Held by the `process_start` future: if that future is dropped before it
/// completes (e.g. a parent manager aborts this nested manager after its
/// shutdown grace period), the children are aborted instead of being left
/// running detached. After a normal return all children have finished, so
/// the abort is a no-op.
struct AbortChildrenOnDrop(Arc<Inner>);

impl Drop for AbortChildrenOnDrop {
    fn drop(&mut self) {
        if let Ok(children) = self.0.processes.lock() {
            for child in children.iter() {
                child.join_handle.abort();
            }
        }
    }
}

type ProcessCompletionChannel =
    tokio::sync::Mutex<mpsc::UnboundedReceiver<(usize, Result<(), RuntimeError>)>>;

/// Shared state between the handle you pass around, the supervisor task and all
/// children.
struct Inner {
    processes: Mutex<Vec<Child>>,
    running: AtomicBool,
    /// Latched by [`Handle::shutdown`] so that children spawned afterwards
    /// (pre-start children in `process_start`, or via `add`) are shut down
    /// too. Reset when `process_start` returns.
    shutdown_requested: AtomicBool,
    next_id: AtomicUsize,
    active: AtomicUsize,
    // supervisor RECEIVES from here, children (spawn_child) only send
    completion_tx: mpsc::UnboundedSender<(usize, Result<(), RuntimeError>)>,
    completion_rx: OnceCell<ProcessCompletionChannel>,
    shutdown_grace_period: Duration,
}

/// Groups several [`Runnable`] instances and starts / stops them as a unit.
pub struct ProcessManager {
    id: usize,
    pre_start: Vec<Arc<dyn Runnable>>,
    inner: Arc<Inner>,
    /// Optional human-readable name overriding the default
    /// `"process-manager-<id>"`.
    ///
    /// If `None`, [`ProcessManager::process_name`] falls back to the automatic
    /// naming scheme.
    pub(crate) custom_name: Option<Cow<'static, str>>,
    /// When `true`, children that finish *successfully* are removed from the
    /// internal lists so that long-running supervisors do not leak memory.
    pub(crate) auto_cleanup: bool,
}

impl ProcessManager {
    /// Creates a fresh supervisor.
    ///
    /// * A unique *process-manager id* is assigned automatically.
    /// * [`auto_cleanup`](crate::ProcessManagerBuilder::auto_cleanup) is **enabled** by
    ///   default so that finished children are removed from the internal
    ///   bookkeeping lists.
    ///
    /// The manager may be configured further with [`insert`](Self::insert)
    /// **before** it is started or with [`add`](Self::add) **after** it is
    /// running.
    pub fn new() -> Self {
        let id = PID.fetch_add(1, Ordering::SeqCst);

        let (tx, rx) = mpsc::unbounded_channel();

        Self {
            id,
            pre_start: Vec::new(),
            inner: Arc::new(Inner {
                processes: Mutex::new(Vec::new()),
                running: AtomicBool::new(false),
                shutdown_requested: AtomicBool::new(false),
                next_id: AtomicUsize::new(0),
                active: AtomicUsize::new(0),
                completion_tx: tx,
                completion_rx: {
                    let cell = OnceCell::new();
                    let _ = cell.set(tokio::sync::Mutex::new(rx));
                    cell
                },
                shutdown_grace_period: Duration::from_secs(30),
            }),
            custom_name: None,
            auto_cleanup: true,
        }
    }

    /// Return the grace period used before force-aborting children during
    /// shutdown.
    pub fn shutdown_grace_period(&self) -> Duration {
        self.inner.shutdown_grace_period
    }

    pub(crate) fn set_shutdown_grace_period(&mut self, duration: Duration) {
        let inner = Arc::get_mut(&mut self.inner)
            .expect("inner must be uniquely owned during manager setup");
        inner.shutdown_grace_period = duration;
    }

    /// Registers a child **before** the supervisor itself is started.
    ///
    /// # Panics
    /// Panics if the manager is already running.  Use [`add`](Self::add) in that case.
    pub fn insert(&mut self, process: impl Runnable) {
        assert!(
            !self.inner.running.load(Ordering::SeqCst),
            "cannot call insert() after manager has started – use add() instead"
        );
        self.pre_start
            .push(Arc::from(Box::new(process) as Box<dyn Runnable>));
    }

    /// Adds a child **while the manager is already running**.
    ///
    /// The new `Runnable` is spawned immediately in its own Tokio task.
    ///
    /// # Panics
    /// Panics if called before start-up. Use [`insert`](Self::insert) during setup.
    pub fn add(&self, process: impl Runnable) {
        let proc: Arc<dyn Runnable> = Arc::from(Box::new(process) as Box<dyn Runnable>);

        assert!(
            self.inner.running.load(Ordering::SeqCst),
            "cannot call add() before manager has started – use insert() instead"
        );

        // Running → register & spawn immediately.
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        self.inner
            .processes
            .lock()
            .unwrap()
            .push(Child::new(id, proc, &self.inner));

        // A shutdown may have taken its snapshot before this child was
        // registered; make sure the child is shut down as well.
        if self.inner.shutdown_requested.load(Ordering::SeqCst) {
            let handle = Handle {
                inner: Arc::clone(&self.inner),
            };
            tokio::spawn(async move { handle.shutdown().await });
        }
    }
}

impl Runnable for ProcessManager {
    fn process_start(&self) -> ProcFuture<'_> {
        let inner = Arc::clone(&self.inner);
        let auto_cleanup = self.auto_cleanup;
        let initial = self.pre_start.clone();

        let manager_handle = self.process_handle();

        Box::pin(async move {
            let _abort_children = AbortChildrenOnDrop(Arc::clone(&inner));
            inner.running.store(true, Ordering::SeqCst);

            let name = self.process_name();

            #[cfg(feature = "tracing")]
            ::tracing::info!("Start process manager {name}");
            #[cfg(all(not(feature = "tracing"), feature = "log"))]
            ::log::info!("Start process manager {name}");
            #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
            eprintln!("Start process manager {name}");

            /* -- spawn every child registered before start() ---------------- */
            for proc in initial {
                let id = inner.next_id.fetch_add(1, Ordering::SeqCst);
                inner
                    .processes
                    .lock()
                    .unwrap()
                    .push(Child::new(id, proc, &inner));
            }

            // A shutdown requested before (or while) the children were
            // registered must not be lost: deliver it to every child that
            // has not received it yet.
            if inner.shutdown_requested.load(Ordering::SeqCst) {
                manager_handle.shutdown().await;
            }

            /* -- supervisor event-loop -------------------------------------- */
            let completion_rx = inner
                .completion_rx
                .get()
                .expect("process_start called twice");
            let mut completion_rx = completion_rx.lock().await;

            let mut first_error: Option<RuntimeError> = None;

            loop {
                #[cfg(feature = "tracing")]
                {
                    for child in self.inner.processes.lock().unwrap().iter() {
                        ::tracing::debug!(
                            "Process {}: running={:?}",
                            child.proc.process_name(),
                            !child.join_handle.is_finished()
                        );
                    }
                }

                // exit criterion: no active children left
                if inner.active.load(Ordering::SeqCst) == 0 {
                    inner.running.store(false, Ordering::SeqCst);
                    inner.shutdown_requested.store(false, Ordering::SeqCst);
                    break;
                }

                match completion_rx.recv().await {
                    Some((cid, res)) => {
                        match res {
                            Ok(()) => {
                                if auto_cleanup {
                                    inner.processes.lock().unwrap().retain(|c| c.id != cid);
                                }
                            }
                            Err(err) => {
                                if first_error.is_none() {
                                    first_error = Some(err);
                                    manager_handle.shutdown().await;
                                }
                            }
                        }
                        inner.active.fetch_sub(1, Ordering::SeqCst);
                    }
                    None => {
                        // Sender dropped – supervisor should stop.
                        inner.running.store(false, Ordering::SeqCst);
                        inner.shutdown_requested.store(false, Ordering::SeqCst);
                        return Err(RuntimeError::Internal {
                            message: "completion channel closed unexpectedly".into(),
                        });
                    }
                }
            }

            match first_error {
                Some(error) => {
                    #[cfg(feature = "tracing")]
                    ::tracing::warn!("Shutdown process manager {name} with error: {error:?}");
                    #[cfg(all(not(feature = "tracing"), feature = "log"))]
                    ::log::warn!("Shutdown process manager {name} with error: {error:?}");
                    #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
                    eprintln!("Shutdown process manager {name} with error: {error:?}");
                    Err(error)
                }
                None => {
                    #[cfg(feature = "tracing")]
                    ::tracing::info!("Shutdown process manager {name}");
                    #[cfg(all(not(feature = "tracing"), feature = "log"))]
                    ::log::info!("Shutdown process manager {name}");
                    #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
                    eprintln!("Shutdown process manager {name}");
                    Ok(())
                }
            }
        })
    }

    /// Returns the supervisor’s public name.
    ///
    /// If a custom name was set via
    /// [`ProcessManagerBuilder::name`](crate::ProcessManagerBuilder::name), that
    /// value is returned verbatim; otherwise the default pattern
    /// `"process-manager-<id>"` is used.
    fn process_name(&self) -> Cow<'static, str> {
        if let Some(ref name) = self.custom_name {
            name.clone()
        } else {
            format!("process-manager-{}", self.id).into()
        }
    }

    /// Returns a handle that can control *all* currently running children of
    /// this manager.
    ///
    /// The handle can be cloned freely and used from any async context.
    fn process_handle(&self) -> Arc<dyn ProcessControlHandler> {
        Arc::new(Handle {
            inner: Arc::clone(&self.inner),
        })
    }
}

impl Default for ProcessManager {
    fn default() -> Self {
        Self::new()
    }
}

struct Handle {
    inner: Arc<Inner>,
}

impl ProcessControlHandler for Handle {
    /// Broadcasts [`ProcessControlHandler::shutdown`] to every currently active
    /// child and waits for them to complete.
    ///
    /// Children whose task has already finished are skipped: nothing drains
    /// their control channel any more, so a send could block until the grace
    /// period expires.
    ///
    /// The request is remembered: children spawned afterwards (pre-start
    /// children when `process_start` runs, or children passed to `add`) are
    /// shut down as well. Each child receives the shutdown message at most
    /// once; a repeated call still waits for the children to finish.
    fn shutdown(&self) -> CtrlFuture<'_> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            // Latch *before* taking the snapshot: a child registered after the
            // snapshot then sees the flag (see `process_start` / `add`).
            inner.shutdown_requested.store(true, Ordering::SeqCst);

            let mut set = JoinSet::new();

            let handles = {
                let guard = inner.processes.lock().unwrap();
                guard
                    .iter()
                    .filter(|child| !child.join_handle.is_finished())
                    .map(|child| {
                        (
                            child.proc.process_name(),
                            child.handle.clone(),
                            child.join_handle.clone(),
                            !child.shutdown_sent.swap(true, Ordering::SeqCst),
                        )
                    })
                    .collect::<Vec<_>>()
            };
            let shutdown_grace_period = inner.shutdown_grace_period;

            for (name, h, jh, send) in handles {
                set.spawn(async move {
                    #[cfg(feature = "tracing")]
                    ::tracing::info!(name = %name, "Initiate shutdown");
                    #[cfg(all(not(feature = "tracing"), feature = "log"))]
                    ::log::info!("Initiate shutdown {name}");
                    #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
                    eprintln!("Initiate shutdown {name}");

                    let dur = shutdown_grace_period;
                    let now = Instant::now();
                    let watched = Arc::clone(&jh);

                    let timeout = tokio::time::timeout(dur, async move {
                        // Ask the child to shut down first, unless an earlier
                        // broadcast already did.
                        if send {
                            h.shutdown().await;
                        }

                        // Then wait until the underlying task has actually finished.
                        while !watched.is_finished() {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    })
                    .await;
                    let _elapsed = now.elapsed();

                    match timeout {
                        Ok(_) => {
                            #[cfg(feature = "tracing")]
                            ::tracing::info!(name = %name, elapsed = ?_elapsed, "Shutdown completed");
                            #[cfg(all(not(feature = "tracing"), feature = "log"))]
                            ::log::info!("Process {name}: shutdown completed");
                            #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
                            eprintln!("Process {name}: shutdown completed");
                        }
                        Err(_) => {
                            jh.abort();
                            #[cfg(feature = "tracing")]
                            ::tracing::info!(name = %name, elapsed = ?_elapsed, "Shutdown timed out");
                            #[cfg(all(not(feature = "tracing"), feature = "log"))]
                            ::log::info!("Process {name}: Shutdown timed out after {dur:?}");
                            #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
                            eprintln!("Process {name}: Shutdown timed out after {dur:?}");
                        }
                    }
                });
            }
            let _ = set.join_all().await;
        })
    }

    /// Broadcasts [`ProcessControlHandler::reload`] to every active child.
    /// The reload operations are executed in parallel and awaited before the
    /// future completes.
    ///
    /// Children whose task has already finished are skipped: nothing drains
    /// their control channel any more, so repeated sends would block forever.
    fn reload(&self) -> CtrlFuture<'_> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            let mut set = JoinSet::new();

            let handles = {
                let guard = inner.processes.lock().unwrap();
                guard
                    .iter()
                    .filter(|child| !child.join_handle.is_finished())
                    .map(|child| Arc::clone(&child.handle))
                    .collect::<Vec<_>>()
            };

            for h in handles {
                set.spawn(async move {
                    h.reload().await;
                });
            }
            let _ = set.join_all().await;
        })
    }
}

type CompletionSender = mpsc::UnboundedSender<(usize, Result<(), RuntimeError>)>;

/// Reports a child's completion to the supervisor exactly once.
///
/// If the child task is dropped before calling [`CompletionReporter::complete`]
/// (e.g. because it was aborted after the shutdown grace period), the `Drop`
/// implementation reports an [`RuntimeError::Internal`] instead, so the
/// supervisor's accounting can never be skipped.
struct CompletionReporter {
    id: usize,
    name: Cow<'static, str>,
    tx: Option<CompletionSender>,
}

impl CompletionReporter {
    fn new(id: usize, name: Cow<'static, str>, tx: CompletionSender) -> Self {
        Self {
            id,
            name,
            tx: Some(tx),
        }
    }

    fn complete(mut self, res: Result<(), RuntimeError>) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send((self.id, res)); // ignore error if supervisor already gone
        }
    }
}

impl Drop for CompletionReporter {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send((
                self.id,
                Err(RuntimeError::Internal {
                    message: format!(
                        "process {} was aborted before it completed (shutdown grace period exceeded)",
                        self.name
                    ),
                }),
            ));
        }
    }
}

/// Spawns one child task, converts panics into `RuntimeError`s and notifies the
/// supervisor through the *completion channel*.
///
/// Completion is reported exactly once per child, including when the task is
/// cancelled (aborted) before it finishes; see [`CompletionReporter`].
///
/// Accounting with [`Inner::active`] is done **before** the task is actually
/// spawned so the supervisor has an accurate count even if the spawn fails.
fn spawn_child(id: usize, proc: Arc<dyn Runnable>, inner: Arc<Inner>) -> JoinHandle<()> {
    // increment *before* spawning the task – guarantees the counter is in sync
    inner.active.fetch_add(1, Ordering::SeqCst);
    let name = proc.process_name();
    // Created before spawning so that even a task aborted before its first
    // poll still reports completion when its future is dropped.
    let reporter = CompletionReporter::new(id, name.clone(), inner.completion_tx.clone());

    tokio::spawn(async move {
        // Task already accounted for in the caller.
        #[cfg(feature = "tracing")]
        ::tracing::info!(name = %name, "Start process");
        #[cfg(all(not(feature = "tracing"), feature = "log"))]
        ::log::info!("Start process {name}");
        #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
        eprintln!("Start process {name}");

        // run the child and convert a panic into an `Err` so the supervisor
        // can react instead of hanging forever.
        let catch_fut = AssertUnwindSafe(proc.process_start()).catch_unwind();

        #[cfg(feature = "tracing")]
        let catch_result = {
            let span = ::tracing::info_span!("process", name = %name);
            catch_fut.instrument(span).await
        };
        #[cfg(not(feature = "tracing"))]
        let catch_result = { catch_fut.await };

        let res = catch_result.unwrap_or_else(|panic| {
            let msg = if let Some(s) = panic.downcast_ref::<&str>() {
                (*s).to_string()
            } else if let Some(s) = panic.downcast_ref::<String>() {
                s.clone()
            } else {
                "unknown panic".to_string()
            };
            Err(RuntimeError::Internal {
                message: format!("process panicked: {msg}"),
            })
        });

        match &res {
            Ok(_) => {
                #[cfg(feature = "tracing")]
                ::tracing::info!(name = %name, "Process stopped");
                #[cfg(all(not(feature = "tracing"), feature = "log"))]
                ::log::info!("Process {name}: stopped");
                #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
                eprintln!("Process {name}: stopped");
            }
            Err(err) => {
                #[cfg(feature = "tracing")]
                ::tracing::error!(name = %name, "Process failed: {err:?}");
                #[cfg(all(not(feature = "tracing"), feature = "log"))]
                ::log::error!("Process {name}: failed {err:?}");
                #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
                eprintln!("Process {name}: failed {err:?}");
            }
        }

        reporter.complete(res);
    })
}
