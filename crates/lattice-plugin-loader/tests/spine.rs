//! PL8.A spine proof: the loader drives one component through the full
//! `compile → instantiate_plugin → activate` lifecycle and records it as loaded.
//!
//! This is the end-to-end evidence that the Phase-7 runtime is reachable from a
//! loader subsystem — the boot side (the service resolves after `Editor::boot`)
//! is pinned in `lattice-host/tests/boot_regression_pins.rs`. The fixture is the
//! canonical no-op `plugin`-world component the runtime crate's own lifecycle
//! tests assemble from WAT, referenced in place (single source of truth — no
//! duplicated fixture).

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_mode::CapabilitySet;
use lattice_plugin_host::{PluginHost, PluginManifest, TrustTier};
use lattice_plugin_loader::{DiscoveredPlugin, PluginLoader, PluginLoaderError};
use std::sync::Arc;

/// The canonical no-op lifecycle fixture (`activate`/`deactivate` do nothing),
/// assembled from the runtime crate's WAT so both crates load the same bytes.
const NOOP_WAT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../lattice-plugin-host/tests/fixtures/noop.wat"
));

fn noop_bytes() -> Vec<u8> {
    wat::parse_str(NOOP_WAT).expect("no-op fixture WAT assembles to component bytes")
}

fn manifest(id: &str) -> PluginManifest {
    // No requested OS capabilities + no editor capabilities + empty `provides`:
    // the minimal honest manifest for a no-op *lifecycle* plugin.
    PluginManifest::new(id, Vec::new(), CapabilitySet::empty())
}

/// Wrap raw bytes + a manifest as a discovered plugin (bypassing the on-disk
/// scan) so the spine proof drives the load path directly.
fn discovered(bytes: Vec<u8>, manifest: PluginManifest) -> DiscoveredPlugin {
    DiscoveredPlugin {
        source: lattice_plugin_loader::SourceRecord::Unknown,
        manifest,
        component_bytes: bytes,
        dir: std::path::PathBuf::from("<in-memory>"),
    }
}

/// A hermetic host that writes its cache + per-plugin data dirs under a temp
/// base, so the spine proof never touches the user's real plugin dirs.
fn host() -> Arc<PluginHost> {
    let base = std::env::temp_dir().join(format!(
        "lattice-plugin-loader-spine-{}",
        std::process::id()
    ));
    let cache = base.join("cache");
    let data = base.join("data");
    Arc::new(PluginHost::with_dirs(cache, data).expect("host builds with temp dirs"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loads_a_component_through_the_lifecycle_spine() {
    let loader = PluginLoader::new(host());
    assert_eq!(loader.loaded_count(), 0, "loader starts empty");

    let id = loader
        .load_discovered(
            &discovered(noop_bytes(), manifest("noop-plugin")),
            TrustTier::Bundled,
        )
        .await
        .expect("the no-op component compiles, instantiates, and activates");

    assert_eq!(
        loader.loaded_count(),
        1,
        "the loaded set records the plugin"
    );
    assert!(
        loader.is_loaded("noop-plugin"),
        "the plugin is reachable by its manifest id (the `:plugin-unload <name>` key)"
    );
    // The host issues a real, non-forgeable id (the base for `SourceLayer::Plugin`).
    let _ = id;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_load_leaves_the_loader_live_and_unchanged() {
    let loader = PluginLoader::new(host());

    // Garbage bytes: `compile` rejects them. The load returns a typed error
    // (never a panic), and the loaded set is untouched — the graceful-
    // degradation contract PL8.B's discovery relies on to skip a bad plugin.
    let err = loader
        .load_discovered(
            &discovered(b"not a wasm component".to_vec(), manifest("broken")),
            TrustTier::Bundled,
        )
        .await
        .expect_err("garbage bytes must not load");
    assert!(
        matches!(err, PluginLoaderError::Host(_)),
        "compile failure is a host error"
    );

    assert_eq!(
        loader.loaded_count(),
        0,
        "a failed load stores no partial record"
    );
    assert!(
        !loader.is_loaded("broken"),
        "a failed load is not reported as loaded"
    );
}
