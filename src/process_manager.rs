//! Dynamic supervisor for asynchronous `Runnable`s.
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
//! let mut root = ProcessManager::new();
//! root.insert(MyService);                   // add before start
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
};

use futures::FutureExt as _;
use once_cell::sync::OnceCell;
use std::panic::AssertUnwindSafe;
use tokio::sync::mpsc;

use crate::{CtrlFuture, ProcFuture, ProcessControlHandler, Runnable, RuntimeError};

/// Global monotonically increasing identifier for every `ProcessManager`.
static PID: AtomicUsize = AtomicUsize::new(0);

/// Metadata kept for each child.
struct Child {
    id: usize,
    #[allow(dead_code)]
    proc: Arc<dyn Runnable>,
    handle: Arc<dyn ProcessControlHandler>,
}

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
    completion_rx:
        OnceCell<tokio::sync::Mutex<mpsc::UnboundedReceiver<(usize, Result<(), RuntimeError>)>>>,
}

/// Groups several [`Runnable`] instances and starts / stops them as a unit.
pub struct ProcessManager {
    id: usize,
    pre_start: Vec<Arc<dyn Runnable>>,
    inner: Arc<Inner>,
    auto_cleanup: bool,
}

/* ========================================================================== */
/*  Construction / configuration                                              */
/* ========================================================================== */

impl ProcessManager {
    /// New manager with auto-cleanup of finished children enabled.
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
            auto_cleanup: true,
        }
    }

    /// Create a manager that keeps finished children (no automatic cleanup).
    ///
    /// This is the counterpart to the default [`new`] constructor which
    /// _removes_ children automatically once they exit successfully.
    pub fn manual_cleanup() -> Self {
        let mut mgr = Self::new();
        mgr.auto_cleanup = false;
        mgr
    }

    /// Create a manager with automatic cleanup of finished children (alias for [`new`]).
    pub fn auto_cleanup() -> Self {
        Self::new()
    }

    /// Register a child **before** the supervisor is started.
    ///
    /// Panics when called after [`process_start`](Runnable::process_start).
    pub fn insert(&mut self, process: impl Runnable) {
        assert!(
            !self.inner.running.load(Ordering::SeqCst),
            "cannot call insert() after manager has started – use add() instead"
        );
        self.pre_start
            .push(Arc::from(Box::new(process) as Box<dyn Runnable>));
    }

    /// Add a child *while* the manager is already running. The child is spawned
    /// immediately.  Before start-up this behaves the same as [`insert`].
    pub fn add(&self, process: impl Runnable) {
        let proc: Arc<dyn Runnable> = Arc::from(Box::new(process) as Box<dyn Runnable>);

        // Not running yet? → queue for start-up.
        if !self.inner.running.load(Ordering::SeqCst) {
            let mut guard = self.inner.processes.lock().unwrap();
            let handle = proc.process_handle();
            guard.push(Child {
                id: self.inner.next_id.fetch_add(1, Ordering::SeqCst),
                handle: Arc::clone(&handle),
                proc,
            });
            // cache for broadcasts
            self.inner.handles.lock().unwrap().push(handle);
            return;
        }

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
            });
        }

        spawn_child(id, proc, Arc::clone(&self.inner));
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
                    });
                    inner.handles.lock().unwrap().push(handle);
                }
                spawn_child(id, proc, Arc::clone(&inner));
            }

            /* -- supervisor event-loop -------------------------------------- */
            let completion_rx = inner
                .completion_rx
                .get()
                .expect("process_start called twice");
            let mut completion_rx = completion_rx.lock().await;

            let mut first_error: Option<RuntimeError> = None;

            loop {
                // exit criterion: no active children left
                if inner.active.load(Ordering::SeqCst) == 0 {
                    inner.running.store(false, Ordering::SeqCst);
                    return match first_error {
                        Some(e) => Err(e),
                        None => Ok(()),
                    };
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
        })
    }

    fn process_name(&self) -> Cow<'static, str> {
        format!("process-manager-{}", self.id).into()
    }

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
    fn shutdown(&self) -> CtrlFuture<'_> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            let handles = {
                let guard = inner.handles.lock().unwrap();
                guard.clone()
            };

            for h in handles {
                h.shutdown().await;
            }
        })
    }

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

fn spawn_child(id: usize, proc: Arc<dyn Runnable>, inner: Arc<Inner>) {
    // increment *before* spawning the task – guarantees the counter is in sync
    inner.active.fetch_add(1, Ordering::SeqCst);
    let tx = inner.completion_tx.clone();

    tokio::spawn(async move {
        // Task already accounted for in the caller.
        let name = proc.process_name();
        #[cfg(feature = "tracing")]
        let _span_enter = ::tracing::info_span!("process", name = %name).entered();

        #[cfg(feature = "tracing")]
        ::tracing::info!("Start process {name}");
        #[cfg(all(not(feature = "tracing"), feature = "log"))]
        ::log::info!("Start process {name}");
        #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
        eprintln!("Start process {name}");

        // run the child and convert a panic into an `Err` so the supervisor
        // can react instead of hanging forever.
        let res = AssertUnwindSafe(proc.process_start())
            .catch_unwind()
            .await
            .unwrap_or_else(|panic| {
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
                ::tracing::info!("Process {name} stopped");
                #[cfg(all(not(feature = "tracing"), feature = "log"))]
                ::log::info!("Process {name} stopped");
                #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
                eprintln!("Process {name} stopped");
            }
            Err(err) => {
                #[cfg(feature = "tracing")]
                ::tracing::error!("Process {name} failed: {err:?}");
                #[cfg(all(not(feature = "tracing"), feature = "log"))]
                ::log::error!("Process {name} failed: {err:?}");
                #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
                eprintln!("Process {name} failed: {err:?}");
            }
        }

        let _ = tx.send((id, res)); // ignore error if supervisor already gone
    });
}
