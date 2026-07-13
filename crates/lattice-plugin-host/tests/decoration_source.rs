//! PH7.9c — the decoration producer seam, driven through a real guest.
//!
//! Instantiates the `decorations-guest` fixture (a `wasm32-wasip2`
//! `decorations-plugin` component) via [`PluginHost::spawn_decoration_source`],
//! drives its `gutter-decorations` producer through the [`WasmDecorationSource`]
//! adapter + `DecorationActor` bridge, and asserts the native result — proving
//! the whole seam end to end OFF the render path:
//!   - the owned `decoration-context` projection (buffer id / path / line count)
//!     crosses in — the last-line decoration is keyed off `line_count`,
//!   - the `list<gutter-decoration>` crosses back and converts to native
//!     `GutterDecoration`s (Diff / Severity, per-line),
//!   - an empty buffer degrades gracefully to a guest `err` the adapter surfaces
//!     (the caller keeps the prior cached snapshot — no flicker, §8).
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target — see build.rs).

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_mode::{CapabilitySet, GutterDecoration, GutterDiffKind, GutterSeverityLevel};
use lattice_plugin_host::{
    PluginBudget, PluginHost, PluginManifest, TrustTier, WasmDecorationSource,
};
use tempfile::TempDir;

/// The fixture decorations component path, or `None` when it wasn't built (skip).
fn guest_wasm() -> Option<&'static str> {
    let path = env!("DECORATIONS_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

/// Instantiate the fixture + spawn its actor; returns the host-facing producer.
async fn source(host: &PluginHost) -> WasmDecorationSource {
    let component = host
        .compile(&std::fs::read(guest_wasm().unwrap()).unwrap())
        .expect("compile decorations fixture");
    let manifest = PluginManifest::new("decorations-fixture", Vec::new(), CapabilitySet::empty());
    let (client, actor) = host
        .spawn_decoration_source(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::decoration(),
        )
        .await
        .expect("spawn decoration source");
    tokio::spawn(actor.run());
    WasmDecorationSource::new(client)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn producer_crosses_context_and_returns_gutter_decorations() {
    let Some(_) = guest_wasm() else {
        eprintln!("SKIP: decorations fixture guest not built (add the wasm32-wasip2 target)");
        return;
    };
    let dir = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data")).unwrap();
    let src = source(&host).await;

    let decos = src
        .gutter_decorations(7, Some(std::path::Path::new("src/lib.rs")), 5)
        .await
        .expect("producer returns decorations");
    assert_eq!(decos.len(), 3);
    assert!(matches!(
        decos[0],
        GutterDecoration::Diff {
            line: 0,
            kind: GutterDiffKind::Change
        }
    ));
    assert!(matches!(
        decos[1],
        GutterDecoration::Severity {
            line: 1,
            level: GutterSeverityLevel::Error
        }
    ));
    // The last-line decoration is `line_count - 1` = 4 → proves the projected
    // context (line_count = 5) crossed in and drove the guest.
    assert!(matches!(
        decos[2],
        GutterDecoration::Diff {
            line: 4,
            kind: GutterDiffKind::Add
        }
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_buffer_degrades_gracefully_to_a_guest_err() {
    let Some(_) = guest_wasm() else {
        eprintln!("SKIP: decorations fixture guest not built");
        return;
    };
    let dir = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data")).unwrap();
    let src = source(&host).await;

    // line_count == 0 → the guest returns a WIT `err` (not a trap); the adapter
    // surfaces it as `Err`, and a boot-wired host keeps the buffer's prior cached
    // snapshot rather than clearing it (no flicker, §8).
    let err = src
        .gutter_decorations(7, None, 0)
        .await
        .expect_err("empty buffer yields a graceful guest err");
    assert!(
        err.contains("empty buffer"),
        "graceful guest err surfaced, got: {err}"
    );
}
