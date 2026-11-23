//! #74: every public type implements `Debug` with useful content.

use std::fmt::Debug;
use std::time::Duration;

use processmanager::*;

fn assert_debug<T: Debug>() {}

#[test]
fn public_types_implement_debug() {
    assert_debug::<ProcessManager>();
    assert_debug::<ProcessManagerBuilder>();
    assert_debug::<AddError>();
    assert_debug::<RuntimeError>();
    assert_debug::<RuntimeGuard>();
    assert_debug::<RuntimeHandle>();
    assert_debug::<RuntimeTicker>();
    assert_debug::<RuntimeContext>();
    assert_debug::<RuntimeContextRunnable<Ctx>>();
    assert_debug::<RuntimeControlMessage>();
    assert_debug::<ProcessOperation<u8>>();
    assert_debug::<IdleProcess>();
    assert_debug::<Job>();
    assert_debug::<RestartSupervisor>();
    assert_debug::<RestartBackoff>();
    assert_debug::<RestartLimitExceeded>();
    #[cfg(all(unix, feature = "signal"))]
    assert_debug::<SignalReceiver>();
}

/// A `RunnableWithContext` that does not implement `Debug` itself.
struct Ctx;

impl RunnableWithContext for Ctx {
    fn process_start_with_context(&self, _ctx: RuntimeContext) -> ProcFuture<'_> {
        Box::pin(async { Ok(()) })
    }

    fn process_name(&self) -> std::borrow::Cow<'static, str> {
        "ctx-worker".into()
    }
}

#[test]
fn debug_output_names_the_configuration() {
    let manager = ProcessManagerBuilder::default()
        .name("api")
        .pre_insert(IdleProcess::new())
        .build();
    let out = format!("{manager:?}");
    assert!(out.contains("\"api\""), "{out}");
    assert!(out.contains("NotStarted"), "{out}");

    let builder = ProcessManagerBuilder::default().name("api");
    assert!(format!("{builder:?}").contains("\"api\""));

    let job = Job::new("migrate", || async { Ok(()) }).optional();
    let out = format!("{job:?}");
    assert!(
        out.contains("\"migrate\"") && out.contains("optional: true"),
        "{out}"
    );

    let sup = RestartSupervisor::new(Job::new("flaky", || async { Ok(()) }))
        .restart_limit(3, Duration::from_secs(10));
    let out = format!("{sup:?}");
    assert!(
        out.contains("flaky") && out.contains("max_restarts: 3"),
        "{out}"
    );

    let ctx = with_runtime_context(Ctx);
    assert!(format!("{ctx:?}").contains("ctx-worker"));

    let op: ProcessOperation<u8> = ProcessOperation::Next(7);
    assert_eq!(format!("{op:?}"), "Next(7)");
}
