//! PH7.2 — capability & security model, host-layer proofs.
//!
//! These assert the *model* at the host layer: the grant/deny matrix through a
//! real [`PluginHost`], the per-plugin data-dir mount, provenance issuance +
//! non-forgeability, and graceful degradation of a denied/missing capability.
//! The end-to-end "a guest attempts a write and WASI denies it" proof is
//! deferred to PH7.4 (it needs the real `wasm32-wasip2` `fuzzy-finder` guest,
//! the toolchain PH7.0 deferred to that slice); the WASI-layer OS enforcement
//! itself rests on wasmtime's tested no-ambient-authority guarantee, which
//! these exercise via the grant→preopen mapping.
//!
//! Instantiation + lifecycle calls are async (the canonical ABI), so these use
//! a tokio runtime. Cache + data-dir base point at per-test tempdirs
//! (`PluginHost::with_dirs`) so nothing touches the real user dirs.
#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::PathBuf;

use lattice_grammar::SourceLayer;
use lattice_mode::CapabilitySet;
use lattice_plugin_host::{Capability, PluginBudget, PluginHost, PluginManifest, TrustTier};
use tempfile::TempDir;

const NOOP_WAT: &str = include_str!("fixtures/noop.wat");

fn noop_bytes() -> Vec<u8> {
    wat::parse_str(NOOP_WAT).expect("no-op component WAT assembles")
}

/// A host with hermetic cache + data-dir base under `dir`.
fn host_in(dir: &TempDir) -> PluginHost {
    PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data"))
        .expect("host builds with tempdirs")
}

#[tokio::test]
async fn plugin_instantiates_under_a_grant_and_gets_a_data_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let host = host_in(&tmp);
    let component = host.compile(&noop_bytes()).expect("compiles");

    // A readable + writable grant on real (existing) prefixes.
    let ro = tmp.path().join("readable");
    let rw = tmp.path().join("writable");
    std::fs::create_dir_all(&ro).unwrap();
    std::fs::create_dir_all(&rw).unwrap();
    let manifest = PluginManifest::new(
        "grantee",
        vec![
            Capability::FsRead(ro.clone()),
            Capability::FsWrite(rw.clone()),
        ],
        CapabilitySet::empty(),
    );

    let mut plugin = host
        .instantiate_plugin(&component, &manifest, TrustTier::Bundled, PluginBudget::default())
        .await
        .expect("instantiates under grant");

    // The private data dir was created and is recorded.
    let data_dir = plugin
        .data_dir()
        .expect("plugin has a data dir")
        .to_path_buf();
    assert!(data_dir.is_dir(), "data dir exists on disk: {data_dir:?}");
    assert!(
        data_dir.ends_with(PathBuf::from("grantee").join("data")),
        "data dir keyed by the manifest string id: {data_dir:?}"
    );

    // The grant reflects exactly the request; nothing denied.
    assert_eq!(plugin.grant().fs.len(), 2);
    assert!(plugin.denied_capabilities().is_empty());

    // The lifecycle round-trip still runs.
    plugin.activate().await.expect("activate runs under grant");
    plugin.deactivate().await.expect("deactivate runs");
}

#[tokio::test]
async fn no_grant_load_reaches_no_filesystem_and_has_no_data_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let host = host_in(&tmp);
    let component = host.compile(&noop_bytes()).expect("compiles");

    let plugin = host.instantiate(&component).await.expect("instantiates");

    // The degenerate load has no grant and no data dir — the guest reaches no
    // filesystem at all.
    assert!(plugin.data_dir().is_none());
    assert!(plugin.grant().fs.is_empty());
    assert!(plugin.grant().net_http.is_empty());
    assert!(!plugin.grant().proc_spawn);
}

#[tokio::test]
async fn proc_spawn_is_bundled_only_and_denial_is_surfaced_not_fatal() {
    let tmp = tempfile::tempdir().unwrap();
    let host = host_in(&tmp);
    let component = host.compile(&noop_bytes()).expect("compiles");
    let manifest =
        PluginManifest::new("spawner", vec![Capability::ProcSpawn], CapabilitySet::empty());

    // Bundled: granted.
    let bundled = host
        .instantiate_plugin(&component, &manifest, TrustTier::Bundled, PluginBudget::default())
        .await
        .expect("bundled plugin instantiates");
    assert!(bundled.grant().proc_spawn);
    assert!(bundled.denied_capabilities().is_empty());

    // User-installed: withheld, surfaced on `denied`, but the plugin still
    // loads (graceful degradation — never fails the load).
    let mut user = host
        .instantiate_plugin(
            &component,
            &manifest,
            TrustTier::UserInstalled,
            PluginBudget::default(),
        )
        .await
        .expect("user plugin still instantiates, degraded");
    assert!(!user.grant().proc_spawn);
    assert_eq!(user.denied_capabilities(), &[Capability::ProcSpawn]);
    user.activate().await.expect("degraded plugin still activates");
}

#[tokio::test]
async fn a_missing_granted_prefix_degrades_gracefully() {
    let tmp = tempfile::tempdir().unwrap();
    let host = host_in(&tmp);
    let component = host.compile(&noop_bytes()).expect("compiles");

    // Grant a read prefix that does not exist. The WASI view build skips the
    // bad preopen (logged), the data dir still mounts, and the plugin loads.
    let manifest = PluginManifest::new(
        "hopeful",
        vec![Capability::FsRead(PathBuf::from("/no/such/dir/lattice-ph72"))],
        CapabilitySet::empty(),
    );
    let mut plugin = host
        .instantiate_plugin(&component, &manifest, TrustTier::Bundled, PluginBudget::default())
        .await
        .expect("instantiates despite the missing prefix");
    plugin
        .activate()
        .await
        .expect("activate runs despite missing prefix");
}

#[tokio::test]
async fn provenance_ids_are_host_issued_unique_and_stamp_the_plugin_layer() {
    let tmp = tempfile::tempdir().unwrap();
    let host = host_in(&tmp);
    let component = host.compile(&noop_bytes()).expect("compiles");

    // Two plugins from the *same* manifest string id — a guest-controlled
    // field. Their host-issued numeric ids must still differ: the numeric
    // provenance id is issued by the host per instance, never derived from any
    // guest-controlled input. That is the structural non-forgeability property
    // (there is no public API that accepts a SourceLocation — the host stamps
    // it from `PluginId`).
    let manifest = PluginManifest::new("dup", vec![], CapabilitySet::empty());
    let a = host
        .instantiate_plugin(&component, &manifest, TrustTier::Bundled, PluginBudget::default())
        .await
        .unwrap();
    let b = host
        .instantiate_plugin(&component, &manifest, TrustTier::Bundled, PluginBudget::default())
        .await
        .unwrap();

    assert_ne!(a.id(), b.id(), "ids are unique per instance");

    // Provenance is always the Plugin layer, carrying the host-issued id.
    match a.source_layer() {
        SourceLayer::Plugin(id) => assert_eq!(id, a.id().0),
        other => panic!("expected SourceLayer::Plugin, got {other:?}"),
    }
    match b.source_layer() {
        SourceLayer::Plugin(id) => assert_eq!(id, b.id().0),
        other => panic!("expected SourceLayer::Plugin, got {other:?}"),
    }
}

#[tokio::test]
async fn editor_capabilities_are_carried_onto_the_grant() {
    let tmp = tempfile::tempdir().unwrap();
    let host = host_in(&tmp);
    let component = host.compile(&noop_bytes()).expect("compiles");
    let manifest = PluginManifest::new(
        "moded",
        vec![],
        CapabilitySet::TREE_SITTER | CapabilitySet::LSP,
    );
    let plugin = host
        .instantiate_plugin(&component, &manifest, TrustTier::Bundled, PluginBudget::default())
        .await
        .unwrap();
    assert_eq!(
        plugin.grant().editor,
        CapabilitySet::TREE_SITTER | CapabilitySet::LSP
    );
}
