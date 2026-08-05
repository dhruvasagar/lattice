//! PO.5 — the `logging` (Layer 2) seam, driven through a real guest.
//!
//! Instantiates the `logging-guest` fixture (a `wasm32-wasip2` base `plugin`-world
//! component) with a [`PluginTracer`] wired into the host, calls its `activate`
//! export, and asserts the guest's `logging.log` calls landed in the tracer as
//! `seam = logging`, `direction = host-import` records — proving the guest's own
//! narrative reaches the same ring as the boundary trace (design §8), and that
//! the per-plugin verbosity gate applies to Layer 2 exactly as to Layer 1.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target — see build.rs).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_plugin_host::{
    Direction, PluginHost, PluginSeam, PluginTracer, PluginTracerHandle, TraceLevel,
};

/// The fixture logging component path, or `None` when it wasn't built (skip).
fn guest_wasm() -> Option<&'static str> {
    let path = env!("LOGGING_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

/// Build a host with `tracer` wired, instantiate + activate the logging fixture,
/// and return the plugin id so the caller can inspect its ring. `default_level`
/// seeds the tracer's gate before `activate` fires the guest's logs.
async fn activate_logging_guest(default_level: TraceLevel) -> (PluginTracerHandle, u32) {
    let path = guest_wasm().unwrap();
    let dirs = tempfile::tempdir().unwrap();
    let host = PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();
    let tracer: PluginTracerHandle = Arc::new(PluginTracer::new(default_level, 100));
    host.set_tracer(tracer.clone());

    let component = host.compile(&std::fs::read(path).unwrap()).unwrap();
    let mut plugin = host.instantiate(&component).await.unwrap();
    let id = plugin.id().0;
    plugin.activate().await.unwrap();
    (tracer, id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn guest_log_lines_land_in_the_tracer_as_logging_records() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: logging fixture guest not built");
        return;
    }
    // The default `Info` gate keeps info/warn/error (+ critical, which folds to
    // error); the guest's `debug` and `trace` lines are dropped — the gate applies
    // to Layer 2 exactly as to Layer 1.
    let (tracer, id) = activate_logging_guest(TraceLevel::Info).await;

    let recs = tracer.snapshot_plugin(id);
    assert_eq!(
        recs.len(),
        4,
        "info + warn + error + critical(→error) kept at Info; debug + trace dropped — got {recs:#?}"
    );
    for r in &recs {
        assert_eq!(r.seam, PluginSeam::Logging, "tagged as the logging seam");
        assert!(
            matches!(r.direction, Direction::HostImport),
            "a guest→host import, not a host→guest export"
        );
    }
    // The first line: an `info` in context `boot`, the message in `detail`.
    let first = &recs[0];
    assert_eq!(first.level, TraceLevel::Info);
    assert_eq!(first.call, "boot", "the guest's context rides in `call`");
    assert_eq!(
        first.detail.as_deref(),
        Some("logging guest activated"),
        "the guest's message rides in `detail`"
    );
    // The error line has no context (empty `call`) and maps to Error level.
    let err = recs.iter().find(|r| r.call.is_empty()).unwrap();
    assert_eq!(err.level, TraceLevel::Error);
    assert_eq!(err.detail.as_deref(), Some("a context-less error line"));
    // `critical` folds into Error (map_log_level) and is kept at the Info gate.
    let critical = recs.iter().find(|r| r.call == "fatal").unwrap();
    assert_eq!(critical.level, TraceLevel::Error, "critical maps to Error");
    assert_eq!(critical.detail.as_deref(), Some("critical folds to error"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raising_the_gate_captures_the_guest_debug_line() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: logging fixture guest not built");
        return;
    }
    // At `Trace` all six activate lines are kept — the guest's `debug` and
    // `trace` narrative appears once the plugin is raised.
    let (tracer, id) = activate_logging_guest(TraceLevel::Trace).await;
    let recs = tracer.snapshot_plugin(id);
    assert_eq!(
        recs.len(),
        6,
        "every activate line kept at Trace — got {recs:#?}"
    );
    let debug = recs.iter().find(|r| r.level == TraceLevel::Debug).unwrap();
    assert_eq!(debug.call, "detail");
    assert_eq!(debug.detail.as_deref(), Some("walked 40 files in 3ms"));
    // The `trace`-level line maps to Trace (map_log_level's most-verbose arm).
    let trace = recs.iter().find(|r| r.call == "verbose").unwrap();
    assert_eq!(trace.level, TraceLevel::Trace);
    assert_eq!(trace.detail.as_deref(), Some("trace-level narration"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_guest_log_without_a_tracer_wired_is_a_graceful_drop() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: logging fixture guest not built");
        return;
    }
    // PO.5 degradation contract: with NO tracer wired on the host, `log_ctx` is
    // None, so each guest `log` debug-drops. The guest must still activate cleanly
    // — never a panic (the `event_emit`-None precedent).
    let path = guest_wasm().unwrap();
    let dirs = tempfile::tempdir().unwrap();
    let host = PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();
    // Deliberately do NOT call host.set_tracer.
    let component = host.compile(&std::fs::read(path).unwrap()).unwrap();
    let mut plugin = host.instantiate(&component).await.unwrap();
    plugin
        .activate()
        .await
        .expect("activate succeeds even though every log() drops (no tracer wired)");
}
