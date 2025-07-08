//! Integration-style tests for the public `ProcessManagerBuilder` API.
//
//! The goal is to ensure that compile-time plumbing between the builder, the
//! resulting `ProcessManager` instance and its runtime metadata works as
//! expected.

use std::sync::Arc;

use processmanager::{
    CtrlFuture, ProcFuture, ProcessControlHandler, ProcessManagerBuilder, Runnable, RuntimeError,
};

/// A no-op service that terminates immediately and successfully.
///
/// We mostly need this to satisfy the `pre_insert` type parameter; the process
/// never actually runs inside the test.
#[derive(Default)]
struct NoopSvc;

impl Runnable for NoopSvc {
    fn process_start(&self) -> ProcFuture<'_> {
        Box::pin(async { Ok::<(), RuntimeError>(()) })
    }

    fn process_handle(&self) -> Arc<dyn ProcessControlHandler> {
        // The test never calls control methods, so a stub handle is sufficient.
        Arc::new(StubHandle)
    }
}

/// A minimal `ProcessControlHandler` implementation that does nothing.
struct StubHandle;

impl ProcessControlHandler for StubHandle {
    fn shutdown(&self) -> CtrlFuture<'_> {
        Box::pin(async {})
    }

    fn reload(&self) -> CtrlFuture<'_> {
        Box::pin(async {})
    }
}

#[test]
fn builder_sets_custom_name() {
    let mgr = ProcessManagerBuilder::default()
        .name("my-supervisor")
        .pre_insert(NoopSvc)
        .build();

    assert_eq!(mgr.process_name(), "my-supervisor");
}
