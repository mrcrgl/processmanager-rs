//! Fluent builder for constructing a [`ProcessManager`].
//!
//! The main goal of this helper is to get rid of the “wrong phase” panics
//! (`insert()` vs. `add()`) by making the set-up phase explicit.  All children
//! that are known at construction time are registered on the builder;
//! afterwards `build()` hands you a fully configured manager that can be
//! started immediately.
//!
//! Further configuration knobs (metrics, names, tracing options…) can be added
//! here without changing the `ProcessManager` API again.
///
use crate::{ProcessManager, Runnable};

/// Build-time configuration for a [`ProcessManager`].
///
/// ```rust
/// # use processmanager::*;
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
    /// Deferred actions that will be executed against the manager right before
    /// it is returned to the caller.
    initialisers: Vec<Box<dyn FnOnce(&mut ProcessManager) + Send>>,
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

    /// Register a child that should be present right from the start.
    ///
    /// This is the compile-time safe counterpart to `ProcessManager::insert`.
    pub fn pre_insert(mut self, process: impl Runnable) -> Self {
        self.initialisers
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
        let mut mgr = if self.auto_cleanup {
            ProcessManager::new()
        } else {
            ProcessManager::manual_cleanup()
        };

        // Run all queued initialisers (child registrations, …).
        for init in self.initialisers {
            init(&mut mgr);
        }

        mgr
    }
}
