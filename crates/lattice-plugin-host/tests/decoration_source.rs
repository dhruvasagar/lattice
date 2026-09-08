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

use lattice_mode::{CapabilitySet, GutterDecoration};
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
///
/// `signs` is the registry the SG.3b name→id resolution runs against. `None`
/// is the unwired-harness shape: sign placements are skipped and everything
/// else still crosses.
async fn source_with_signs(
    host: &PluginHost,
    signs: Option<lattice_mode::SignRegistryHandle>,
) -> WasmDecorationSource {
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
            &std::sync::Arc::new(lattice_runtime::EventBus::new()),
        )
        .await
        .expect("spawn decoration source");
    tokio::spawn(actor.run());
    WasmDecorationSource::new(client, signs)
}

async fn source(host: &PluginHost) -> WasmDecorationSource {
    source_with_signs(host, None).await
}

/// A registry with the built-ins (so the fixture's `diff` / `severity` sugar
/// arms resolve) plus the one sign its defined placement names.
fn fixture_signs() -> lattice_mode::SignRegistryHandle {
    let mut r = lattice_mode::SignRegistry::new();
    lattice_mode::register_builtin_signs(&mut r, lattice_mode::DiagnosticGlyphs::default());
    r.define(lattice_mode::SignDefinition {
        name: "fixture.mark".into(),
        text: "\u{f111}".into(),
        fallback: "●".into(),
        theme_element: "fixture.mark".into(),
        priority: 5,
        column: lattice_mode::SIGN_COLUMN_MARK.into(),
    });
    std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(r))
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
    // SG.4b: with no sign registry wired, NOTHING crosses — not even the
    // `diff` / `severity` arms, because those are sugar for a built-in sign's
    // name and a name needs the registry to resolve. In production the
    // registry is a boot service and always present; a harness without one
    // gets the same inert degradation every other unwired seam here takes,
    // and inert is the honest answer rather than marks resolving to whatever
    // sits at id 0.
    assert!(
        decos.is_empty(),
        "an unwired sign registry contributes no marks: {decos:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_wire_sugar_arms_resolve_to_builtin_signs_end_to_end() {
    // SG.4b: the guest still says "line 0 is a change" and "line 1 is an
    // error" in the wire's own vocabulary — it does not have to know the host
    // spells those `diff.change` and `diagnostic.error`. That mapping happens
    // at the boundary, which is why the arms were kept as sugar rather than
    // deleted along with the native variants.
    let Some(_) = guest_wasm() else {
        eprintln!("SKIP: decorations fixture guest not built (add the wasm32-wasip2 target)");
        return;
    };
    let dir = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data")).unwrap();
    let signs = fixture_signs();
    let ids = {
        let snapshot = signs.load();
        (
            snapshot.id_of("diff.change").unwrap(),
            snapshot.id_of("diagnostic.error").unwrap(),
            snapshot.id_of("diff.add").unwrap(),
        )
    };
    let src = source_with_signs(&host, Some(signs)).await;

    let decos = src
        .gutter_decorations(7, Some(std::path::Path::new("src/lib.rs")), 5)
        .await
        .expect("producer returns decorations");

    assert_eq!(
        decos[0],
        GutterDecoration::Sign {
            line: 0,
            sign: ids.0
        }
    );
    assert_eq!(
        decos[1],
        GutterDecoration::Sign {
            line: 1,
            sign: ids.1
        }
    );
    // The last-line decoration is `line_count - 1` = 4 → proves the projected
    // context (line_count = 5) crossed in and drove the guest.
    assert_eq!(
        decos[2],
        GutterDecoration::Sign {
            line: 4,
            sign: ids.2
        }
    );
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_guest_places_a_sign_by_name_and_the_host_interns_it() {
    // SG.3b end-to-end. The guest has no `SignId` to carry — it names the sign
    // and the host resolves it HERE, at the boundary and off the render path,
    // which is what lets the native placement stay `Copy` with no per-line
    // `String`.
    let Some(_) = guest_wasm() else {
        eprintln!("SKIP: decorations fixture guest not built (add the wasm32-wasip2 target)");
        return;
    };
    let dir = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data")).unwrap();
    let signs = fixture_signs();
    let expected = signs.load().id_of("fixture.mark").unwrap();
    let src = source_with_signs(&host, Some(signs)).await;

    let decos = src
        .gutter_decorations(7, Some(std::path::Path::new("src/lib.rs")), 5)
        .await
        .expect("producer returns decorations");

    // FOUR: the three non-sign marks plus the one sign whose name resolves.
    // The guest's second placement names a sign nothing defined and is
    // skipped — and crucially the three around it still crossed, which is what
    // would be lost if an unknown name failed the batch.
    assert_eq!(decos.len(), 4, "got {decos:?}");
    assert_eq!(
        decos[3],
        GutterDecoration::Sign {
            line: 2,
            sign: expected
        },
        "the name interned to the id the registry had already issued"
    );
    assert!(
        !decos
            .iter()
            .any(|d| matches!(d, GutterDecoration::Sign { line: 3, .. })),
        "the undefined name is skipped, not invented: {decos:?}"
    );
}
