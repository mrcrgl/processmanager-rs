//! Fluent builder for constructing a [`ProcessManager`].
//!
//! Building a supervisor is a *setup-time* activity, whereas actually running
//! it is a *runtime* concern. Mixing the two often leads to the familiar
//! “wrong phase” panic when someone calls [`ProcessManager::insert`] **after**
//! the manager has already started.
//!
//! `ProcessManagerBuilder` makes the configuration phase explicit: every child,
//! option or tweak is registered **before** [`Self::build`] returns a
//! ready-to-run supervisor.
//!
//! Additional knobs (metrics, tracing, names, …) can be added here in the
//! future **without** touching the public API of [`ProcessManager`].
use crate::{ProcessManager, Runnable};
use std::borrow::Cow;

type BoxedInitializer = Box<dyn FnOnce(&mut ProcessManager) + Send>;

/// Build-time configuration for a [`ProcessManager`].
///
/// ```rust
/// # use processmanager::*;
/// # use std::sync::Arc;
/// # #[derive(Default)] struct MySvc;
/// # impl Runnable for MySvc {
/// #   fn process_start(&self) -> ProcFuture<'_> { Box::pin(async { Ok(()) }) }
/// #   fn process_handle(&self) -> Arc<dyn ProcessControlHandler> { unreachable!() }
/// # }
/// let mgr = ProcessManagerBuilder::default()
///     .auto_cleanup(true)
///     .pre_insert(MySvc)       // add before start
///     .build();
/// ```
#[derive(Default)]
pub struct ProcessManagerBuilder {
    /// Whether to clean up finished children automatically.
    auto_cleanup: bool,
    /// Optional custom name for the supervisor.
    custom_name: Option<Cow<'static, str>>,
    /// Deferred actions that will be executed against the manager right before
    /// it is returned to the caller.
    initializers: Vec<BoxedInitializer>,
}

impl ProcessManagerBuilder {
    /// Create a new builder with defaults (`auto_cleanup = true`, no children).
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable / disable automatic clean-up of finished children.
    pub fn auto_cleanup(mut self, enabled: bool) -> Self {
        self.auto_cleanup = enabled;
        self
    }

    /// Set a custom human-readable name for the supervisor.
    pub fn name<S: Into<Cow<'static, str>>>(mut self, name: S) -> Self {
        self.custom_name = Some(name.into());
        self
    }

    /// Register a child that should be present right from the start.
    ///
    /// This is the compile-time safe counterpart to `ProcessManager::insert`.
    pub fn pre_insert(mut self, process: impl Runnable) -> Self {
        self.initializers
            .push(Box::new(move |mgr: &mut ProcessManager| {
                // At this point the manager is *not* running yet so `insert`
                // is always the correct method.
                mgr.insert(process);
            }));
        self
    }

    /// Finalise the configuration and return a ready-to-use [`ProcessManager`].
    pub fn build(self) -> ProcessManager {
        // Construct base manager according to the chosen clean-up strategy.
        let mut mgr = ProcessManager::new();

        mgr.auto_cleanup = self.auto_cleanup;

        // Apply configuration knobs that require direct field access.
        mgr.custom_name = self.custom_name;

        // Run all queued initialisers (child registrations, …).
        for init in self.initializers {
            init(&mut mgr);
        }

        mgr
    }
}
