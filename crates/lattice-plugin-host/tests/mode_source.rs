//! PH7.11a — the modes declaration seam, driven through a real guest.
//!
//! Instantiates the `modes-guest` fixture (a `wasm32-wasip2` `modes-plugin`
//! component) via [`PluginHost::spawn_mode_plugin`], which drives its
//! `register-modes` export against a native [`ModeRegistry`]. Proves the seam:
//!   - the guest's imported `register-mode` calls land minor modes in the SAME
//!     registry builtins use, carrying their activation policy + capabilities,
//!   - a mis-suffixed id is rejected by the registry's `-mode` gate (not in the
//!     returned ids, not in the registry).
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target — see build.rs).

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_mode::{ActivationPolicy, CapabilitySet, ModeId, ModeRegistry};
use lattice_plugin_host::{PluginBudget, PluginHost, PluginManifest, TrustTier};
use tempfile::TempDir;

const PLUGIN_ID: &str = "modes-fixture";

/// The fixture modes component path, or `None` when it wasn't built (skip).
fn guest_wasm() -> Option<&'static str> {
    let path = env!("MODES_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_declares_minor_modes_into_the_shared_registry_end_to_end() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: modes fixture guest not built (add the wasm32-wasip2 target)");
        return;
    };
    let dir = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data"))
        .expect("host builds");
    let component = host
        .compile(&std::fs::read(wasm).unwrap())
        .expect("compile modes fixture");
    let manifest = PluginManifest::new(PLUGIN_ID, Vec::new(), CapabilitySet::empty());

    // A fresh registry (no foundation modes) — the plugin modes are the only
    // entries, so assertions are hermetic.
    let mut registry = ModeRegistry::default();

    let ids = host
        .spawn_mode_plugin(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::event(),
            &mut registry,
        )
        .await
        .expect("spawn mode plugin");

    // Two well-formed minor modes registered; the mis-suffixed `not-suffixed` was
    // rejected by the registry's `-mode` gate (not returned, not registered).
    assert_eq!(ids.len(), 2, "two modes accepted: {ids:?}");
    assert!(ids.iter().any(|id| id.as_str() == "git-blame-mode"));
    assert!(ids.iter().any(|id| id.as_str() == "lsp-lens-mode"));
    assert!(registry.is_registered(ModeId::new("git-blame-mode")));
    assert!(
        !registry.is_registered(ModeId::new("not-suffixed")),
        "the `-mode` suffix gate rejected the mis-named declaration"
    );

    // The declared activation policy + capability requirements are carried onto
    // the registered mode.
    let lens = registry.get(ModeId::new("lsp-lens-mode")).expect("registered");
    assert!(matches!(lens.activation_policy(), ActivationPolicy::Universal));
    assert_eq!(
        lens.required_capabilities(),
        CapabilitySet::LSP | CapabilitySet::DIAGNOSTICS
    );
}
