//! PH7.4c.1b — the per-plugin actor bridge, driven through a real guest.
//!
//! These instantiate the `picker-guest` fixture (a `wasm32-wasip2`
//! `picker-source-plugin` component) via [`PluginHost::spawn_picker_source`],
//! spawn its [`PickerActor`] on a multi-thread runtime, and exercise the
//! `Send + Sync` [`PickerClient`]:
//!   - `spec` / `init` / `accept` round-trip (happy path + the guest's own WIT
//!     `err`, distinct from a host trap),
//!   - inputs (`args` + the `PickerContext` projection) provably cross the
//!     channel (the fixture echoes them back),
//!   - the graceful "plugin gone" surface when the actor task has ended.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target — see
//! build.rs), the same pattern as the trampoline bench.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_mode::CapabilitySet;
use lattice_plugin_host::picker_task::{
    ActiveBufferSnapshot, PickerAcceptOutcome, PickerContext, Position, RoutingPayload,
};
use lattice_plugin_host::{PluginBudget, PluginHost, PluginManifest, TrustTier};
use tempfile::TempDir;

/// The fixture picker component path, or `None` when it wasn't built (skip).
fn guest_wasm() -> Option<&'static str> {
    let path = env!("PICKER_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

/// A host with hermetic cache + data-dir base under `dir`.
fn host_in(dir: &TempDir) -> PluginHost {
    PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data"))
        .expect("host builds with tempdirs")
}

/// A minimal manifest — no OS/editor capabilities. The fixture walks nothing;
/// the grant just has to instantiate the sandboxed `Store`.
fn manifest() -> PluginManifest {
    PluginManifest::new("picker-fixture", Vec::new(), CapabilitySet::empty())
}

/// A minimal owned `PickerContext` with a known `workspace_root` the fixture
/// echoes back (everything else empty — the fixture reads only the root).
fn ctx(workspace_root: &str) -> PickerContext {
    PickerContext {
        active_buffer: ActiveBufferSnapshot {
            buffer_id: 0,
            path: None,
            language: None,
            cursor: Position { line: 0, byte: 0 },
            selection: None,
            syntax_symbols: Vec::new(),
        },
        workspace_root: workspace_root.to_string(),
        recent_files: Vec::new(),
        position_history: Vec::new(),
        buffers: Vec::new(),
        marks: Vec::new(),
        registers: Vec::new(),
    }
}

/// Spawn the fixture picker actor on the current runtime and return its client.
async fn spawn(host: &PluginHost) -> lattice_plugin_host::PickerClient {
    let component = host
        .compile(&std::fs::read(guest_wasm().unwrap()).unwrap())
        .unwrap();
    let (client, actor) = host
        .spawn_picker_source(
            &component,
            &manifest(),
            TrustTier::Bundled,
            PluginBudget::default(),
            &std::sync::Arc::new(lattice_runtime::EventBus::new()),
        )
        .await
        .expect("picker source instantiates + bridges");
    tokio::spawn(actor.run());
    client
}

/// PO.2: with a tracer attached, each guest export the actor calls emits a
/// boundary `PluginTraceRecord`. The client awaits each reply, so by the time a
/// call returns the actor has already emitted — no polling needed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn picker_calls_emit_boundary_trace_records() {
    let Some(_) = guest_wasm() else {
        eprintln!("SKIP: picker_actor trace — fixture guest not built");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let host = host_in(&tmp);

    // Default `Debug` so per-call (Debug-level) records are kept.
    let tracer: lattice_plugin_host::PluginTracerHandle = std::sync::Arc::new(
        lattice_plugin_host::PluginTracer::new(lattice_plugin_host::TraceLevel::Debug, 64),
    );
    let component = host
        .compile(&std::fs::read(guest_wasm().unwrap()).unwrap())
        .unwrap();
    let (client, actor) = host
        .spawn_picker_source(
            &component,
            &manifest(),
            TrustTier::Bundled,
            PluginBudget::default(),
            &std::sync::Arc::new(lattice_runtime::EventBus::new()),
        )
        .await
        .expect("picker source instantiates");
    tokio::spawn(actor.with_tracer(Some(tracer.clone())).run());

    let _ = client.spec().await.expect("spec reaches the guest");
    let _ = client
        .init(ctx("/ws"), vec!["hello".into()])
        .await
        .expect("init reaches the guest");

    let recs = tracer.snapshot_global();
    assert!(
        recs.iter()
            .any(|r| r.call == "spec"
                && matches!(r.seam, lattice_plugin_host::PluginSeam::PickerSource)),
        "the `spec` guest call emitted a PickerSource boundary trace"
    );
    assert!(
        recs.iter().any(|r| r.call == "init"),
        "the `init` guest call emitted a trace"
    );
    assert!(
        recs.iter()
            .all(|r| matches!(r.outcome, lattice_plugin_host::TraceOutcome::Ok { .. })),
        "happy-path calls record an Ok outcome (no traps)"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spec_init_accept_round_trip_through_the_channel() {
    let Some(_) = guest_wasm() else {
        eprintln!("SKIP: picker_actor — fixture guest not built (add wasm32-wasip2)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let host = host_in(&tmp);
    let client = spawn(&host).await;

    // `spec()` — the no-`result` reply path.
    let spec = client.spec().await.expect("spec call reaches the guest");
    assert_eq!(spec.id, "fixture");
    assert!(!spec.live);

    // `init()` happy path — the fixture echoes `args` and `workspace_root`, so a
    // match here proves both inputs crossed the channel into the guest.
    let pairs = client
        .init(ctx("/ws/root"), vec!["hello".into(), "world".into()])
        .await
        .expect("init call reaches the guest")
        .expect("guest produced candidates");
    assert_eq!(pairs.len(), 2);
    assert_eq!(pairs[0].candidate.text, "hello,world");
    assert_eq!(pairs[0].candidate.source.as_deref(), Some("fixture"));
    assert_eq!(pairs[1].candidate.text, "/ws/root");
    // The routing token the fixture emitted per candidate.
    assert!(matches!(&pairs[0].routing, RoutingPayload::OpenFile(p) if p == "/args/hello/world"));
    assert!(matches!(pairs[1].routing, RoutingPayload::Buffer(0)));

    // `accept()` maps a routing token to an outcome (happy path).
    let outcome = client
        .accept(
            ctx("/ws/root"),
            RoutingPayload::OpenFile("/some/file".into()),
        )
        .await
        .expect("accept call reaches the guest")
        .expect("guest resolved the routing");
    assert!(matches!(outcome, PickerAcceptOutcome::OpenFile(p) if p == "/some/file"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn guest_typed_errors_surface_as_inner_err_not_a_host_trap() {
    let Some(_) = guest_wasm() else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let host = host_in(&tmp);
    let client = spawn(&host).await;

    // `init(["fail"])` returns the guest's WIT `err` — the OUTER result is `Ok`
    // (no host trap), the INNER is `Err(message)`.
    let init = client
        .init(ctx("/ws"), vec!["fail".into()])
        .await
        .expect("call reached the guest (no host-side trap)");
    // Compare only the err arm (the generated `CandidatePair` need not derive
    // `PartialEq`, so don't `assert_eq!` the whole `Result`).
    assert_eq!(init.err().as_deref(), Some("fixture asked to fail"));

    // `accept` of a token the fixture doesn't recognise is likewise an inner err.
    let accept = client
        .accept(ctx("/ws"), RoutingPayload::LspCompletion(7))
        .await
        .expect("call reached the guest");
    assert!(accept.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn calls_after_the_actor_ends_are_a_typed_plugin_gone_error() {
    use lattice_plugin_host::PluginHostError;
    let Some(_) = guest_wasm() else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let host = host_in(&tmp);
    let component = host
        .compile(&std::fs::read(guest_wasm().unwrap()).unwrap())
        .unwrap();
    let (client, actor) = host
        .spawn_picker_source(
            &component,
            &manifest(),
            TrustTier::Bundled,
            PluginBudget::default(),
            &std::sync::Arc::new(lattice_runtime::EventBus::new()),
        )
        .await
        .unwrap();
    // Drive the actor, then abort it: its receiver drops, so the next send fails.
    let handle = tokio::spawn(actor.run());
    handle.abort();
    let _ = handle.await;

    match client.spec().await {
        Err(PluginHostError::PluginGone { func }) => assert_eq!(func, "spec"),
        other => panic!("expected PluginGone, got {other:?}"),
    }
}
