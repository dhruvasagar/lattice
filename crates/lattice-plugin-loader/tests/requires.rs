//! A manifest's `requires` orders loading and unloading.
//!
//! Design: [`plugin-extension.md`](../../../docs/dev/architecture/plugin-extension.md) §2.
//!
//! The fixture is the canonical no-op lifecycle component, so every load here
//! genuinely compiles and activates; what varies is only the manifests. The
//! unload tests load from directories on disk, because a dependent that is
//! unloaded is re-read from where it came from to be parked.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_mode::CapabilitySet;
use lattice_plugin_host::{PluginHost, PluginManifest, TrustTier};
use lattice_plugin_loader::{DiscoveredPlugin, PluginLoader, PluginLoaderError};

const NOOP_WAT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../lattice-plugin-host/tests/fixtures/noop.wat"
));

fn noop_bytes() -> Vec<u8> {
    wat::parse_str(NOOP_WAT).expect("no-op fixture WAT assembles to component bytes")
}

fn host(base: &std::path::Path) -> Arc<PluginHost> {
    Arc::new(PluginHost::with_dirs(base.join("cache"), base.join("data")).expect("host builds"))
}

fn plugin(id: &str, requires: &[&str]) -> DiscoveredPlugin {
    let mut manifest = PluginManifest::new(id, Vec::new(), CapabilitySet::empty());
    manifest.requires = requires.iter().map(|r| r.to_string()).collect();
    DiscoveredPlugin {
        source: lattice_plugin_loader::SourceRecord::Unknown,
        manifest,
        component_bytes: noop_bytes(),
        dir: std::path::PathBuf::from("<in-memory>"),
    }
}

/// A plugin directory on disk: a `plugin.toml` and the no-op component.
fn on_disk(root: &std::path::Path, id: &str, requires: &[&str]) -> std::path::PathBuf {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let list = requires
        .iter()
        .map(|r| format!("\"{r}\""))
        .collect::<Vec<_>>()
        .join(", ");
    std::fs::write(
        dir.join("plugin.toml"),
        format!("id = \"{id}\"\nrequires = [{list}]\n"),
    )
    .unwrap();
    std::fs::write(dir.join(format!("{id}.wasm")), noop_bytes()).unwrap();
    dir
}

fn unmet(result: Result<lattice_plugin_host::PluginId, PluginLoaderError>) -> String {
    match result {
        Err(PluginLoaderError::RequirementsUnmet(reason)) => reason,
        other => panic!("expected the plugin to be parked, got {other:?}"),
    }
}

/// The order a user declares plugins in must not matter: a dependent found
/// first waits, and loads the moment its requirement does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dependent_loaded_first_waits_then_loads_with_its_requirement() {
    let base = tempfile::tempdir().unwrap();
    let loader = PluginLoader::new(host(base.path()));

    let reason = unmet(
        loader
            .load_discovered(&plugin("org-clients", &["org"]), TrustTier::UserInstalled)
            .await,
    );
    assert_eq!(reason, "waiting for `org`, which is not loaded");
    assert!(
        !loader.is_loaded("org-clients"),
        "a parked plugin is not loaded"
    );

    loader
        .load_discovered(&plugin("org", &[]), TrustTier::UserInstalled)
        .await
        .expect("org loads");
    assert!(loader.is_loaded("org"));
    assert!(
        loader.is_loaded("org-clients"),
        "the parked dependent loads as soon as its requirement does"
    );
}

/// A chain settles in one load: `c` needs `b` needs `a`, found in reverse.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chain_found_in_reverse_settles_when_its_root_loads() {
    let base = tempfile::tempdir().unwrap();
    let loader = PluginLoader::new(host(base.path()));
    unmet(
        loader
            .load_discovered(&plugin("c", &["b"]), TrustTier::UserInstalled)
            .await,
    );
    unmet(
        loader
            .load_discovered(&plugin("b", &["a"]), TrustTier::UserInstalled)
            .await,
    );
    loader
        .load_discovered(&plugin("a", &[]), TrustTier::UserInstalled)
        .await
        .unwrap();
    for id in ["a", "b", "c"] {
        assert!(loader.is_loaded(id), "`{id}` should be loaded");
    }
}

/// Two plugins that require each other are one packaging bug, and the reason
/// says so rather than reporting each as a missing install.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cycle_is_named_as_a_cycle() {
    let base = tempfile::tempdir().unwrap();
    let loader = PluginLoader::new(host(base.path()));
    unmet(
        loader
            .load_discovered(&plugin("a", &["b"]), TrustTier::UserInstalled)
            .await,
    );
    let reason = unmet(
        loader
            .load_discovered(&plugin("b", &["a"]), TrustTier::UserInstalled)
            .await,
    );
    assert!(
        reason.contains("cycle") && reason.contains("`a`") && reason.contains("`b`"),
        "the reason names the cycle and both plugins: {reason}"
    );
    assert!(!loader.is_loaded("a") && !loader.is_loaded("b"));
}

/// A requirement that is loaded already is no reason to wait.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_met_requirement_loads_at_once() {
    let base = tempfile::tempdir().unwrap();
    let loader = PluginLoader::new(host(base.path()));
    loader
        .load_discovered(&plugin("org", &[]), TrustTier::UserInstalled)
        .await
        .unwrap();
    loader
        .load_discovered(&plugin("org-clients", &["org"]), TrustTier::UserInstalled)
        .await
        .expect("loads straight away");
}

/// The on-disk path records a parked plugin in `:plugins` with the reason,
/// and clears it when the plugin loads.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_parked_plugin_is_visible_until_it_loads() {
    let base = tempfile::tempdir().unwrap();
    let loader = PluginLoader::new(host(base.path()));
    let clients = on_disk(base.path(), "org-clients", &["org"]);
    let org = on_disk(base.path(), "org", &[]);

    unmet(loader.load_path(&clients, TrustTier::UserInstalled).await);
    let failed = loader.failed_loads();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].name, "org-clients");
    assert_eq!(failed[0].error, "waiting for `org`, which is not loaded");

    loader
        .load_path(&org, TrustTier::UserInstalled)
        .await
        .unwrap();
    assert!(loader.is_loaded("org-clients"));
    assert!(
        loader.failed_loads().is_empty(),
        "loading clears the waiting record: {:?}",
        loader.failed_loads()
    );
}

/// Unloading a plugin unloads what requires it first, and keeps it waiting
/// rather than dropping it, so loading the plugin again brings both back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unloading_a_requirement_parks_its_dependents_and_a_reload_restores_them() {
    let base = tempfile::tempdir().unwrap();
    let loader = PluginLoader::new(host(base.path()));
    let org = on_disk(base.path(), "org", &[]);
    let clients = on_disk(base.path(), "org-clients", &["org"]);
    loader
        .load_path(&org, TrustTier::UserInstalled)
        .await
        .unwrap();
    loader
        .load_path(&clients, TrustTier::UserInstalled)
        .await
        .unwrap();

    loader.unload("org").expect("org was loaded");
    assert!(!loader.is_loaded("org"));
    assert!(!loader.is_loaded("org-clients"), "the dependent went first");
    let failed = loader.failed_loads();
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!(failed[0].error, "waiting for `org`, which is not loaded");

    loader
        .load_path(&org, TrustTier::UserInstalled)
        .await
        .unwrap();
    assert!(
        loader.is_loaded("org-clients"),
        "the dependent came back with it"
    );
}

/// A reload is an unload and a load under one lock; the dependent must come
/// back through it too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reloading_a_requirement_reloads_its_dependents() {
    let base = tempfile::tempdir().unwrap();
    let loader = PluginLoader::new(host(base.path()));
    let org = on_disk(base.path(), "org", &[]);
    let clients = on_disk(base.path(), "org-clients", &["org"]);
    loader
        .load_path(&org, TrustTier::UserInstalled)
        .await
        .unwrap();
    loader
        .load_path(&clients, TrustTier::UserInstalled)
        .await
        .unwrap();

    loader
        .reload("org", TrustTier::UserInstalled)
        .await
        .unwrap();
    assert!(loader.is_loaded("org") && loader.is_loaded("org-clients"));
    assert!(
        loader.failed_loads().is_empty(),
        "{:?}",
        loader.failed_loads()
    );
}

/// Unloading a parked plugin withdraws it: it must not load later when its
/// requirement does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unloading_a_parked_plugin_stops_it_loading_later() {
    let base = tempfile::tempdir().unwrap();
    let loader = PluginLoader::new(host(base.path()));
    unmet(
        loader
            .load_discovered(&plugin("org-clients", &["org"]), TrustTier::UserInstalled)
            .await,
    );
    assert!(
        loader.unload("org-clients").is_none(),
        "it was never loaded"
    );
    loader
        .load_discovered(&plugin("org", &[]), TrustTier::UserInstalled)
        .await
        .unwrap();
    assert!(!loader.is_loaded("org-clients"));
}
