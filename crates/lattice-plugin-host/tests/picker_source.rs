//! PH7.4c.2 — the `WasmPickerSource` adapter, driven through a real guest.
//!
//! Instantiates the `picker-guest` fixture via [`PluginHost::spawn_picker_source`],
//! wraps its [`PickerClient`] as a `WasmPickerSource` (an
//! `Arc<dyn PickerSourceGenerator>`), registers it into a `PickerRegistry`, and
//! exercises the native trait end-to-end:
//!   - `spec()` is the converted native spec (cached at `connect`),
//!   - `init()` returns a `PickerInitResult::Future` that resolves to the
//!     native candidate batch (inputs echoed by the fixture → proof they crossed),
//!   - `accept_async()` resolves the routing token to a native outcome,
//!   - a guest WIT `err` surfaces as the future's `Err`.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target — see build.rs).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_core::Buffer;
use lattice_mode::CapabilitySet;
use lattice_picker::context::{ActiveBufferSnapshot, PickerContext};
use lattice_picker::outcome::PickerAcceptOutcome;
use lattice_picker::{PickerInitResult, PickerRegistry, PickerSourceGenerator, RoutingPayload};
use lattice_plugin_host::{PluginBudget, PluginHost, PluginManifest, TrustTier, WasmPickerSource};
use lattice_protocol::Position;
use tempfile::TempDir;

fn guest_wasm() -> Option<&'static str> {
    let path = env!("PICKER_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

fn host_in(dir: &TempDir) -> PluginHost {
    PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data"))
        .expect("host builds with tempdirs")
}

fn manifest() -> PluginManifest {
    PluginManifest::new("picker-fixture", Vec::new(), CapabilitySet::empty())
}

/// Connect a `WasmPickerSource` over a freshly-spawned fixture actor.
async fn connect(host: &PluginHost) -> WasmPickerSource {
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
    tokio::spawn(actor.run());
    WasmPickerSource::connect(client)
        .await
        .expect("connect fetches + converts the spec")
}

/// A minimal native `PickerContext` with a known `workspace_root` the fixture
/// echoes. The rope is never crossed (§4.2), so an empty buffer suffices.
fn with_ctx<R>(workspace_root: &str, f: impl FnOnce(&PickerContext<'_>) -> R) -> R {
    let buffer = Buffer::empty();
    let ctx = PickerContext {
        active_buffer: ActiveBufferSnapshot {
            buffer_id: 0,
            path: None,
            language: None,
            cursor: Position::new(0, 0),
            selection: None,
            buffer: &buffer,
            syntax_symbols: Vec::new(),
            syntax_highlights: Vec::new(),
        },
        workspace_root: workspace_root.into(),
        recent_files: &[],
        position_history: Vec::new(),
        buffers: Vec::new(),
        marks: Vec::new(),
        registers: Vec::new(),
        yank_ring: Vec::new(),
        active_modes: Vec::new(),
        command_history: Vec::new(),
        search_history: Vec::new(),
        pane_buffer_history: Vec::new(),
    };
    f(&ctx)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adapter_spec_registers_and_init_accept_round_trip() {
    let Some(_) = guest_wasm() else {
        eprintln!("SKIP: picker_source — fixture guest not built (add wasm32-wasip2)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let host = host_in(&tmp);
    let source = connect(&host).await;

    // spec() is the converted native spec, cached at connect.
    assert_eq!(source.spec().id, "fixture");
    assert!(!source.spec().live);

    // Registers into the registry indistinguishably from a first-party source.
    let mut reg = PickerRegistry::new();
    reg.register_generator(Arc::new(source));
    let generator = reg
        .generator("fixture")
        .expect("plugin source is registered under its spec id")
        .clone();

    // init() → Future → native batch. The fixture echoes args + workspace_root,
    // so matching here proves both inputs crossed into the guest and back.
    let init = with_ctx("/ws/root", |ctx| {
        generator.init(ctx, &["hello".to_string(), "world".to_string()])
    })
    .expect("init returns a result");
    let batch = match init {
        PickerInitResult::Future(fut) => fut.await.expect("guest produced candidates"),
        other => panic!("expected Future, got {other:?}"),
    };
    assert_eq!(batch.len(), 2);
    assert_eq!(batch[0].0.text, "hello,world");
    assert!(batch[0].0.source.is_some());
    assert_eq!(batch[1].0.text, "/ws/root");
    assert!(
        matches!(&batch[0].1, RoutingPayload::OpenFile { path } if path.to_str() == Some("/args/hello/world"))
    );
    assert!(matches!(batch[1].1, RoutingPayload::Buffer { id: 0 }));

    // accept_async() → Future → native outcome.
    let fut = with_ctx("/ws/root", |ctx| {
        generator.accept_async(
            ctx,
            &RoutingPayload::OpenFile {
                path: "/some/file".into(),
            },
        )
    })
    .expect("WASM source returns Some from accept_async");
    let outcome = fut.await.expect("guest resolved the routing");
    assert!(
        matches!(outcome, PickerAcceptOutcome::OpenFile { path } if path.to_str() == Some("/some/file"))
    );

    // The sync accept is the defensive tripwire — never used in the wired path.
    let sync = with_ctx("/ws", |ctx| {
        generator.accept(ctx, &RoutingPayload::OpenFile { path: "/x".into() })
    });
    assert!(sync.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn guest_init_error_surfaces_through_the_future() {
    let Some(_) = guest_wasm() else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let host = host_in(&tmp);
    let source = connect(&host).await;

    // The fixture returns a WIT `err` for args containing "fail"; it must reach
    // the caller as the init future's `Err` (the picker echoes it, stays closed).
    let init = with_ctx("/ws", |ctx| source.init(ctx, &["fail".to_string()])).expect("init result");
    let err = match init {
        PickerInitResult::Future(fut) => fut.await.expect_err("fixture asked to fail"),
        other => panic!("expected Future, got {other:?}"),
    };
    assert_eq!(err, "fixture asked to fail");
}
