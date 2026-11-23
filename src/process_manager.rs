//! Dynamic supervisor for asynchronous [`Runnable`] implementations; see
//! [`ProcessManager`].
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
        if let Ok(mut registry) = self.0.registry.lock() {
            // Also covers an aborted supervisor: `add` must not spawn into a
            // manager that is no longer running.
            registry.phase = Phase::Stopped;
            for child in registry.children.iter() {
                child.join_handle.abort();
            }
        }
    }
}

/// Lifecycle of a [`ProcessManager`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// `process_start` has not run yet.
    NotStarted,
    /// `process_start` is running.
    Running,
    /// `process_start` has returned (it may be started again, e.g. by a
    /// `RestartSupervisor`).
    Stopped,
}

/// Lifecycle state and children, guarded by a single lock so that `add`,
/// start-up and the supervisor's exit decision cannot interleave.
struct Registry {
    phase: Phase,
    /// Children of the current run.
    children: Vec<Child>,
    /// Children spawned by every `process_start` (from `insert`, and from
    /// `add` before the first start).
    template: Vec<Arc<dyn Runnable>>,
}

/// Why [`ProcessManager::add`] did not accept a child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AddError {
    /// The manager has stopped: its `process_start` has returned (or was
    /// aborted).
    Stopped,
    /// The manager is shutting down.
    ShuttingDown,
}

impl std::fmt::Display for AddError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AddError::Stopped => write!(f, "process manager has stopped"),
            AddError::ShuttingDown => write!(f, "process manager is shutting down"),
        }
    }
}

impl std::error::Error for AddError {}

type ProcessCompletionChannel =
    tokio::sync::Mutex<mpsc::UnboundedReceiver<(usize, Result<(), RuntimeError>)>>;

/// Shared state between the handle you pass around, the supervisor task and all
/// children.
struct Inner {
    registry: Mutex<Registry>,
    /// Latched by [`Handle::shutdown`] so that pre-start children spawned
    /// afterwards are shut down too, and `add` rejects new children. Reset
    /// when `process_start` returns.
    shutdown_requested: AtomicBool,
    next_id: AtomicUsize,
    active: AtomicUsize,
    // supervisor RECEIVES from here, children (spawn_child) only send
    completion_tx: mpsc::UnboundedSender<(usize, Result<(), RuntimeError>)>,
    completion_rx: OnceCell<ProcessCompletionChannel>,
    shutdown_grace_period: Duration,
}

/// Groups several [`Runnable`] instances and starts / stops them as a unit.
///
/// * Register children with [`insert`](Self::insert) /
///   [`ProcessManagerBuilder::pre_insert`](crate::ProcessManagerBuilder::pre_insert)
///   before start, or [`add`](Self::add) at any time until the manager stops.
/// * [`process_start`](Runnable::process_start) runs each child in its own
///   Tokio task and returns once all of them have ended.
/// * The first child that fails makes the manager shut the others down and
///   return that error. A child ending with
///   [`RuntimeError::TerminationSignal`] (e.g. after Ctrl-C) triggers the
///   same shutdown, but the manager returns `Ok(())`.
/// * [`process_handle`](Runnable::process_handle) returns a handle whose
///   `shutdown` / `reload` reach every child. Children still running after
///   the [shutdown grace period](crate::ProcessManagerBuilder::shutdown_grace_period)
///   are aborted.
/// * Children that finish successfully are removed (on by default, see
///   [`auto_cleanup`](crate::ProcessManagerBuilder::auto_cleanup)).
///
/// A `ProcessManager` is itself a [`Runnable`], so managers can be nested
/// into trees. Prefer [`ProcessManagerBuilder`](crate::ProcessManagerBuilder)
/// to construct one.
///
/// ```
/// # use processmanager::*;
/// # #[tokio::main] async fn main() {
/// let root = ProcessManagerBuilder::default()
///     .pre_insert(IdleProcess::new()) // registered before start
///     .build();
/// let handle = root.process_handle();
///
/// let run = tokio::spawn(async move { root.process_start().await });
///
/// handle.reload().await;   // control a running manager
/// handle.shutdown().await; // stop it gracefully
/// run.await.unwrap().unwrap();
/// # }
/// ```
pub struct ProcessManager {
    id: usize,
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
    /// Children are registered with [`insert`](Self::insert) or
    /// [`add`](Self::add).
    pub fn new() -> Self {
        let id = PID.fetch_add(1, Ordering::SeqCst);

        let (tx, rx) = mpsc::unbounded_channel();

        Self {
            id,
            inner: Arc::new(Inner {
                registry: Mutex::new(Registry {
                    phase: Phase::NotStarted,
                    children: Vec::new(),
                    template: Vec::new(),
                }),
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

    /// Registers a child that is spawned every time the manager starts.
    ///
    /// # Panics
    /// Panics if the manager is running. Use [`add`](Self::add) in that case.
    pub fn insert(&mut self, process: impl Runnable) {
        let mut registry = self.inner.registry.lock().unwrap();
        assert!(
            registry.phase != Phase::Running,
            "cannot call insert() while the manager is running – use add() instead"
        );
        registry
            .template
            .push(Arc::from(Box::new(process) as Box<dyn Runnable>));
    }

    /// Adds a child.
    ///
    /// * **Before the manager has started**, the child is queued and spawned
    ///   when [`process_start`](Runnable::process_start) runs, exactly like
    ///   [`insert`](Self::insert). This includes the moment right after
    ///   `tokio::spawn(manager.process_start())`, before that future first
    ///   runs.
    /// * **While the manager is running**, the child is spawned immediately in
    ///   its own Tokio task. It belongs to the current run only and is not
    ///   spawned again if the manager is restarted.
    ///
    /// # Errors
    /// * [`AddError::ShuttingDown`] while the manager is shutting down.
    /// * [`AddError::Stopped`] once the manager's `process_start` has
    ///   returned.
    pub fn add(&self, process: impl Runnable) -> Result<(), AddError> {
        let proc: Arc<dyn Runnable> = Arc::from(Box::new(process) as Box<dyn Runnable>);

        // Decided under the registry lock: the supervisor only stops while
        // holding it, and only if no child is active. Registering a child
        // here makes it active, so it cannot be orphaned by a concurrent exit.
        let mut registry = self.inner.registry.lock().unwrap();
        match registry.phase {
            Phase::NotStarted => {
                registry.template.push(proc);
                Ok(())
            }
            Phase::Stopped => Err(AddError::Stopped),
            Phase::Running => {
                // `Handle::shutdown` sets the flag before taking its snapshot
                // under this lock, so a child either lands in that snapshot or
                // is rejected here.
                if self.inner.shutdown_requested.load(Ordering::SeqCst) {
                    return Err(AddError::ShuttingDown);
                }
                let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
                registry.children.push(Child::new(id, proc, &self.inner));
                Ok(())
            }
        }
    }
}

impl Runnable for ProcessManager {
    fn process_start(&self) -> ProcFuture<'_> {
        let inner = Arc::clone(&self.inner);
        let auto_cleanup = self.auto_cleanup;

        let manager_handle = self.process_handle();

        Box::pin(async move {
            let _abort_children = AbortChildrenOnDrop(Arc::clone(&inner));

            let name = self.process_name();

            #[cfg(feature = "tracing")]
            ::tracing::info!("Start process manager {name}");
            #[cfg(all(not(feature = "tracing"), feature = "log"))]
            ::log::info!("Start process manager {name}");
            #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
            eprintln!("Start process manager {name}");

            /* -- spawn every child registered before start() ---------------- */
            {
                let mut registry = inner.registry.lock().unwrap();
                registry.phase = Phase::Running;
                let template = registry.template.clone();
                for proc in template {
                    let id = inner.next_id.fetch_add(1, Ordering::SeqCst);
                    registry.children.push(Child::new(id, proc, &inner));
                }
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
            // Set when a child asked for a graceful stop by returning
            // `RuntimeError::TerminationSignal` (e.g. `SignalReceiver`).
            let mut termination_requested = false;

            loop {
                #[cfg(feature = "tracing")]
                {
                    for child in self.inner.registry.lock().unwrap().children.iter() {
                        ::tracing::debug!(
                            "Process {}: running={:?}",
                            child.proc.process_name(),
                            !child.join_handle.is_finished()
                        );
                    }
                }

                // exit criterion: no active children left. Checked under the
                // registry lock so a concurrent `add` either registers its
                // child first (keeping the manager running) or sees `Stopped`.
                {
                    let mut registry = inner.registry.lock().unwrap();
                    if inner.active.load(Ordering::SeqCst) == 0 {
                        registry.phase = Phase::Stopped;
                        inner.shutdown_requested.store(false, Ordering::SeqCst);
                        break;
                    }
                }

                match completion_rx.recv().await {
                    Some((cid, res)) => {
                        match res {
                            Ok(()) => {
                                if auto_cleanup {
                                    inner
                                        .registry
                                        .lock()
                                        .unwrap()
                                        .children
                                        .retain(|c| c.id != cid);
                                }
                            }
                            // A termination request is a graceful stop, not a
                            // failure: shut the siblings down, but don't
                            // report it as the manager's error.
                            Err(RuntimeError::TerminationSignal) => {
                                if first_error.is_none() && !termination_requested {
                                    termination_requested = true;
                                    manager_handle.shutdown().await;
                                }
                            }
                            Err(err) => {
                                if first_error.is_none() {
                                    let already_stopping = termination_requested;
                                    first_error = Some(err);
                                    if !already_stopping {
                                        manager_handle.shutdown().await;
                                    }
                                }
                            }
                        }
                        inner.active.fetch_sub(1, Ordering::SeqCst);
                    }
                    None => {
                        // Sender dropped – supervisor should stop.
                        inner.registry.lock().unwrap().phase = Phase::Stopped;
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

#[cfg(feature = "signal")]
impl ProcessManager {
    /// A handle that does not keep the manager alive.
    ///
    /// Used for children that need to control their own manager (e.g. the
    /// `SignalReceiver` installed by `ProcessManagerBuilder::handle_signals`):
    /// a strong handle stored inside one of the manager's own children would
    /// form a reference cycle. Once the manager is gone, the handle's methods
    /// do nothing.
    #[cfg(feature = "signal")]
    pub(crate) fn weak_handle(&self) -> Arc<dyn ProcessControlHandler> {
        Arc::new(WeakHandle {
            inner: Arc::downgrade(&self.inner),
        })
    }
}

#[cfg(feature = "signal")]
struct WeakHandle {
    inner: std::sync::Weak<Inner>,
}

#[cfg(feature = "signal")]
impl ProcessControlHandler for WeakHandle {
    fn shutdown(&self) -> CtrlFuture<'_> {
        let inner = self.inner.upgrade();
        Box::pin(async move {
            if let Some(inner) = inner {
                Handle { inner }.shutdown().await;
            }
        })
    }

    fn reload(&self) -> CtrlFuture<'_> {
        let inner = self.inner.upgrade();
        Box::pin(async move {
            if let Some(inner) = inner {
                Handle { inner }.reload().await;
            }
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
                let registry = inner.registry.lock().unwrap();
                registry
                    .children
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
                let registry = inner.registry.lock().unwrap();
                registry
                    .children
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

        // Run the child and convert a panic into an `Err` so the supervisor
        // can react instead of hanging forever. `process_start()` is called
        // *inside* the unwind boundary, so a panic while creating the future
        // is caught just like a panic while polling it.
        let catch_fut = AssertUnwindSafe(async { proc.process_start().await }).catch_unwind();

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
            Err(RuntimeError::TerminationSignal) => {
                #[cfg(feature = "tracing")]
                ::tracing::info!(name = %name, "Process requested termination");
                #[cfg(all(not(feature = "tracing"), feature = "log"))]
                ::log::info!("Process {name}: requested termination");
                #[cfg(all(not(feature = "tracing"), not(feature = "log")))]
                eprintln!("Process {name}: requested termination");
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
