//! Dynamic supervisor for asynchronous [`Runnable`] implementations.
//
//! A `ProcessManager` can
//!
//! • be filled with children *before* it is started via [`insert`]
//! • accept additional children *after* it has started via [`add`]
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
}

type ProcessCompletionChannel =
    tokio::sync::Mutex<mpsc::UnboundedReceiver<(usize, Result<(), RuntimeError>)>>;

/// Shared state between the handle you pass around, the supervisor task and all
/// children.
struct Inner {
    processes: Mutex<Vec<Child>>,
    /// Cached list of `ProcessControlHandler`s for fast broadcast without
    /// temporary allocations.
    handles: Mutex<Vec<Arc<dyn ProcessControlHandler>>>,
    running: AtomicBool,
    next_id: AtomicUsize,
    active: AtomicUsize,
    // supervisor RECEIVES from here, children (spawn_child) only send
    completion_tx: mpsc::UnboundedSender<(usize, Result<(), RuntimeError>)>,
    completion_rx: OnceCell<ProcessCompletionChannel>,
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

/* ========================================================================== */
/*  Construction / configuration                                              */
/* ========================================================================== */

impl ProcessManager {
    /// Creates a fresh supervisor.
    ///
    /// * A unique *process-manager id* is assigned automatically.
    /// * [`auto_cleanup`](ProcessManager::auto_cleanup) is **enabled** by
    ///   default so that finished children are removed from the internal
    ///   bookkeeping lists.
    ///
    /// The manager may be configured further with [`insert`] **before** it is
    /// started or with [`add`] **after** it is running.
    pub fn new() -> Self {
        let id = PID.fetch_add(1, Ordering::SeqCst);

        let (tx, rx) = mpsc::unbounded_channel();

        Self {
            id,
            pre_start: Vec::new(),
            inner: Arc::new(Inner {
                processes: Mutex::new(Vec::new()),
                handles: Mutex::new(Vec::new()),
                running: AtomicBool::new(false),
                next_id: AtomicUsize::new(0),
                active: AtomicUsize::new(0),
                completion_tx: tx,
                completion_rx: {
                    let cell = OnceCell::new();
                    let _ = cell.set(tokio::sync::Mutex::new(rx));
                    cell
                },
            }),
            custom_name: None,
            auto_cleanup: true,
        }
    }

    /// Registers a child **before** the supervisor itself is started.
    ///
    /// # Panics
    /// Panics if the manager is already running.  Use [`add`] in that case.
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
    /// Calling this method **before** start-up is equivalent to [`insert`].
    pub fn add(&self, process: impl Runnable) {
        let proc: Arc<dyn Runnable> = Arc::from(Box::new(process) as Box<dyn Runnable>);

        assert!(
            self.inner.running.load(Ordering::SeqCst),
            "cannot call add() before manager has started – use insert() instead"
        );

        // Running → register & spawn immediately.
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let handle = proc.process_handle();
        // cache handle immediately
        self.inner.handles.lock().unwrap().push(Arc::clone(&handle));

        {
            let mut guard = self.inner.processes.lock().unwrap();
            guard.push(Child {
                id,
                proc: Arc::clone(&proc),
                handle,
                join_handle: Arc::new(spawn_child(id, proc, Arc::clone(&self.inner))),
            });
        }
    }
}

/* ========================================================================== */
/*  Runnable implementation                                                   */
/* ========================================================================== */

impl Runnable for ProcessManager {
    fn process_start(&self) -> ProcFuture<'_> {
        let inner = Arc::clone(&self.inner);
        let auto_cleanup = self.auto_cleanup;
        let initial = self.pre_start.clone();

        let manager_handle = self.process_handle();

        Box::pin(async move {
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
                let handle = proc.process_handle();

                {
                    let mut g = inner.processes.lock().unwrap();
                    g.push(Child {
                        id,
                        proc: Arc::clone(&proc),
                        handle: Arc::clone(&handle),
                        join_handle: Arc::new(spawn_child(id, proc, Arc::clone(&inner))),
                    });
                    inner.handles.lock().unwrap().push(handle);
                }
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
                        ::tracing::info!(
                            "Process {}: running={:?}",
                            child.proc.process_name(),
                            !child.join_handle.is_finished()
                        );
                    }
                }

                // exit criterion: no active children left
                if inner.active.load(Ordering::SeqCst) == 0 {
                    inner.running.store(false, Ordering::SeqCst);
                    break;
                }

                match completion_rx.recv().await {
                    Some((cid, res)) => {
                        match res {
                            Ok(()) => {
                                if auto_cleanup {
                                    let mut g = inner.processes.lock().unwrap();
                                    g.retain(|c| c.id != cid);
                                    // also remove cached handle
                                    inner
                                        .handles
                                        .lock()
                                        .unwrap()
                                        .retain(|h| g.iter().any(|c| Arc::ptr_eq(&c.handle, h)));
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
                        return Err(RuntimeError::Internal {
                            message: "completion channel closed unexpectedly".into(),
                        });
                    }
                }
            }

            match first_error {
                Some(error) => {
                    #[cfg(feature = "tracing")]
                    ::tracing::info!("Shutdown process manager {name} with error: {error:?}");
                    #[cfg(all(not(feature = "tracing"), feature = "log"))]
                    ::log::info!("Shutdown process manager {name} with error: {error:?}");
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
    /// If [`custom_name`](ProcessManager::custom_name) is `Some`, that value is
    /// returned verbatim; otherwise the default pattern
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

/* ========================================================================== */
/*  Control Handle                                                            */
/* ========================================================================== */

struct Handle {
    inner: Arc<Inner>,
}

impl ProcessControlHandler for Handle {
    /// Broadcasts [`ProcessControlHandler::shutdown`] to every currently active
    /// child and waits for them to complete.
    fn shutdown(&self) -> CtrlFuture<'_> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            let mut set = JoinSet::new();

            let handles = {
                let guard = inner.processes.lock().unwrap();
                guard
                    .iter()
                    .map(|child| {
                        (
                            child.proc.process_name(),
                            child.handle.clone(),
                            child.join_handle.clone(),
                        )
                    })
                    .collect::<Vec<_>>()
            };

            for (name, h, jh) in handles {
                set.spawn(async move {
                    #[cfg(feature = "tracing")]
                    ::tracing::info!(name = %name, "Initiate shutdown");
                    #[cfg(all(not(feature = "tracing"), feature = "log"))]
                    ::log::info!("Initiate shutdown {name}");
                    #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
                    eprintln!("Initiate shutdown {name}");

                    let dur = Duration::from_secs(30);
                    let now = Instant::now();
                    let timeout = tokio::time::timeout(dur, h.shutdown()).await;
                    let _elapsed = now.elapsed();

                    match timeout {
                        Ok(_) => {
                            // Shutdown ok
                            #[cfg(feature = "tracing")]
                            ::tracing::info!(name = %name, elapsed = ?_elapsed, "Shutdown completed");
                            #[cfg(all(not(feature = "tracing"), feature = "log"))]
                            ::log::info!("Process {name}: shutdown completed");
                            #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
                            eprintln!("Process {name}: shutdown completed");
                        }
                        Err(_) => {
                            jh.abort();
                            // Timed out.
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
    fn reload(&self) -> CtrlFuture<'_> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            let handles = {
                let guard = inner.handles.lock().unwrap();
                guard.clone()
            };

            for h in handles {
                h.reload().await;
            }
        })
    }
}

/* ========================================================================== */
/*  Helper – spawn a single child                                             */
/* ========================================================================== */
/// Spawns one child task, converts panics into `RuntimeError`s and notifies the
/// supervisor through the *completion channel*.
///
/// Accounting with [`Inner::active`] is done **before** the task is actually
/// spawned so the supervisor has an accurate count even if the spawn fails.
fn spawn_child(id: usize, proc: Arc<dyn Runnable>, inner: Arc<Inner>) -> JoinHandle<()> {
    // increment *before* spawning the task – guarantees the counter is in sync
    inner.active.fetch_add(1, Ordering::SeqCst);
    let tx = inner.completion_tx.clone();

    tokio::spawn(async move {
        // Task already accounted for in the caller.
        let name = proc.process_name();
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

        let _ = tx.send((id, res)); // ignore error if supervisor already gone
    })
}
