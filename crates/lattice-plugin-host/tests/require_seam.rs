//! PM.7 — the `require` seam, driven through a real guest.
//!
//! Instantiates the `plugin-manager-guest` fixture (a `wasm32-wasip2`
//! `plugin-manager-plugin` component) via
//! [`PluginHost::spawn_plugin_manager_plugin`], which drives its
//! `register-plugins` export. Proves the seam end to end:
//!
//! - each of the three source kinds crosses the boundary intact,
//! - `enable-mode` and `pinned` survive the round trip,
//! - a path-traversal name is refused **without trapping**, so one bad entry
//!   in a user's `init.rs` costs that entry and nothing else,
//! - the specs are *declarations*: nothing was resolved, cloned, built or
//!   downloaded by the call itself.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target — see
//! build.rs).

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_mode::CapabilitySet;
use lattice_plugin_host::plugin_manager_host::RequiredSource;
use lattice_plugin_host::{PluginBudget, PluginHost, PluginManifest, TrustTier};
use tempfile::TempDir;

const PLUGIN_ID: &str = "require-fixture";

/// The fixture component path, or `None` when it wasn't built (skip).
fn guest_wasm() -> Option<&'static str> {
    let path = env!("PLUGIN_MANAGER_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_guest_declares_plugins_across_all_three_source_kinds() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: plugin-manager fixture guest not built (add the wasm32-wasip2 target)");
        return;
    };
    let dir = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data"))
        .expect("host builds");
    let component = host
        .compile(&std::fs::read(wasm).unwrap())
        .expect("compile require fixture");
    let manifest = PluginManifest::new(PLUGIN_ID, Vec::new(), CapabilitySet::empty());

    let specs = host
        .spawn_plugin_manager_plugin(
            &component,
            &manifest,
            PluginBudget::event(),
            TrustTier::Bundled,
        )
        .await
        .expect("spawn plugin-manager guest");

    // Three accepted; the traversal name was refused inside the guest call
    // (the guest itself asserts `require` returned false for it).
    assert_eq!(
        specs.len(),
        3,
        "three well-formed specs recorded: {specs:?}"
    );

    let local = specs.iter().find(|s| s.name == "local-demo").unwrap();
    assert_eq!(
        local.source,
        RequiredSource::Local("/tmp/lattice-demo".into())
    );
    assert_eq!(
        local.enable_mode.as_deref(),
        Some("demo-mode"),
        "use-package's enable-mode sugar must survive the boundary"
    );
    assert!(!local.pinned);

    let git = specs.iter().find(|s| s.name == "git_demo").unwrap();
    assert_eq!(
        git.source,
        RequiredSource::Git {
            url: "https://example.invalid/demo.git".into(),
            rev: Some("abc123".into()),
        }
    );
    assert!(git.pinned, "pinned must survive the boundary");

    let prebuilt = specs.iter().find(|s| s.name == "prebuilt-demo").unwrap();
    assert_eq!(
        prebuilt.source,
        RequiredSource::Prebuilt {
            url: "https://example.invalid/d.wasm".into(),
        }
    );

    assert!(
        !specs.iter().any(|s| s.name.contains("escape")),
        "a path-traversal name must never reach the host's spec list: {specs:?}"
    );
}

/// `require` records; it does not act.
///
/// The fixture names a local source at `/tmp/lattice-demo` that does not
/// exist, a git URL at `example.invalid` that cannot resolve, and a prebuilt
/// URL likewise. If the seam resolved inline, this test would fail — slowly,
/// on a DNS timeout. That it returns promptly with three specs and an
/// untouched cache directory is the assertion.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn require_records_without_resolving_building_or_downloading() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: plugin-manager fixture guest not built");
        return;
    };
    let dir = TempDir::new().unwrap();
    let cache = dir.path().join("cache");
    let host = PluginHost::with_dirs(cache.clone(), dir.path().join("data")).expect("host builds");
    let component = host
        .compile(&std::fs::read(wasm).unwrap())
        .expect("compile require fixture");
    let manifest = PluginManifest::new(PLUGIN_ID, Vec::new(), CapabilitySet::empty());

    let started = std::time::Instant::now();
    let specs = host
        .spawn_plugin_manager_plugin(
            &component,
            &manifest,
            PluginBudget::event(),
            TrustTier::Bundled,
        )
        .await
        .expect("spawn plugin-manager guest");
    let elapsed = started.elapsed();

    assert_eq!(specs.len(), 3);
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "require must not perform network or build work inline (took {elapsed:?})"
    );
    // Nothing was cloned: the source cache the resolver would have written to
    // holds no checkout for the declared git plugin.
    assert!(
        !cache.join("git_demo").exists(),
        "no clone may happen inside the guest call"
    );
}
