//! PMF.1: a plugin can be one file — its manifest embedded in the component as
//! a `lattice-manifest` custom section — and a manifest can say which editor it
//! needs (`min_lattice`).
//!
//! The fixture is the real `config-guest` component with a manifest appended as
//! a top-level custom section, so wasmtime's own validation of a component
//! carrying one is exercised too, not only the section reader.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_config::ConfigRegistry;
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader};

fn config_guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/config-guest/target/wasm32-wasip2/release/config_guest.wasm"
    );
    std::fs::read(path).ok()
}

fn leb(mut n: usize) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

/// `wasm` with `manifest` appended as a `lattice-manifest` custom section.
fn with_manifest(wasm: &[u8], manifest: &str) -> Vec<u8> {
    let name = b"lattice-manifest";
    let mut body = leb(name.len());
    body.extend_from_slice(name);
    body.extend_from_slice(manifest.as_bytes());
    let mut out = wasm.to_vec();
    out.push(0);
    out.extend(leb(body.len()));
    out.extend(body);
    out
}

fn loader(base: &std::path::Path) -> PluginLoader {
    PluginLoader::with_services(
        Arc::new(PluginHost::with_dirs(base.join("cache"), base.join("data")).unwrap()),
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            config_registry: Some(Arc::new(ConfigRegistry::default())),
            ..Default::default()
        },
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_component_with_an_embedded_manifest_needs_no_plugin_toml() {
    let Some(wasm) = config_guest_wasm() else {
        eprintln!("skipping: config-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let plugins = base.path().join("plugins");
    let dir = plugins.join("embedded");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("embedded.wasm"),
        with_manifest(&wasm, "id = \"embedded\"\nprovides = [\"config\"]\n"),
    )
    .unwrap();

    let loader = loader(base.path());
    assert_eq!(
        loader.discover_and_load(&plugins, TrustTier::Bundled).await,
        1,
        "one file is a whole plugin: {:?}",
        loader.failed_loads()
    );
    assert!(loader.is_loaded("embedded"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_embedded_manifest_wins_over_a_plugin_toml_beside_it() {
    let Some(wasm) = config_guest_wasm() else {
        eprintln!("skipping: config-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let plugins = base.path().join("plugins");
    let dir = plugins.join("both");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        "id = \"from-the-file\"\nprovides = [\"config\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("both.wasm"),
        with_manifest(
            &wasm,
            "id = \"from-the-component\"\nprovides = [\"config\"]\n",
        ),
    )
    .unwrap();

    let loader = loader(base.path());
    assert_eq!(
        loader.discover_and_load(&plugins, TrustTier::Bundled).await,
        1
    );
    assert!(
        loader.is_loaded("from-the-component"),
        "the component is what was built; the file is only what was copied"
    );
    assert!(!loader.is_loaded("from-the-file"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plugin_needing_a_newer_editor_is_refused_by_name() {
    let Some(wasm) = config_guest_wasm() else {
        eprintln!("skipping: config-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let plugins = base.path().join("plugins");
    let dir = plugins.join("future");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("future.wasm"),
        with_manifest(
            &wasm,
            "id = \"future\"\nprovides = [\"config\"]\nmin_lattice = \"999.0.0\"\n",
        ),
    )
    .unwrap();

    let loader = loader(base.path());
    assert_eq!(
        loader.discover_and_load(&plugins, TrustTier::Bundled).await,
        0
    );
    let failed = loader.failed_loads();
    assert_eq!(
        failed.len(),
        1,
        "refused, and visible in :plugins: {failed:?}"
    );
    assert!(
        failed[0].error.contains(&format!(
            "needs lattice ≥ 999.0.0; this is {}",
            env!("CARGO_PKG_VERSION")
        )),
        "{}",
        failed[0].error
    );
}

/// A `Prebuilt` download is given no capabilities on purpose; the component's
/// own manifest must not grant it some by winning over the synthesised file.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_prebuilt_download_keeps_its_synthesised_grant() {
    let Some(wasm) = config_guest_wasm() else {
        eprintln!("skipping: config-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let plugins = base.path().join("plugins");
    let dir = plugins.join("fetched");
    std::fs::create_dir_all(&dir).unwrap();
    // What `resolve_prebuilt` writes, plus the seam a user adds by hand to
    // make it load: still no capabilities.
    std::fs::write(
        dir.join("plugin.toml"),
        "id = \"fetched\"\nprovides = [\"config\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.join(".source"),
        "kind = prebuilt\nurl = https://example.invalid/fetched.wasm\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("fetched.wasm"),
        with_manifest(
            &wasm,
            "id = \"fetched\"\nprovides = [\"config\"]\ncapabilities = [\"fs:write:/\"]\n",
        ),
    )
    .unwrap();

    let loader = loader(base.path());
    loader
        .discover_and_load(&plugins, TrustTier::UserInstalled)
        .await;
    let status = loader
        .plugin_status()
        .into_iter()
        .find(|s| s.name == "fetched")
        .unwrap_or_else(|| panic!("loaded: {:?}", loader.failed_loads()));
    assert!(
        status.granted.is_empty(),
        "the embedded `fs:write:/` was not granted: {:?}",
        status.granted
    );
}

/// `require` and `:plugin-load` load one directory through `load_path`, not
/// the directory scan. A refusal raised while reading the manifest must reach
/// `:plugins` there too — at boot it was the only path a `require`d plugin
/// took, and the refusal vanished.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refusal_through_load_path_is_recorded_too() {
    let Some(wasm) = config_guest_wasm() else {
        eprintln!("skipping: config-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("plugins").join("future");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("future.wasm"),
        with_manifest(
            &wasm,
            "id = \"future\"\nprovides = [\"config\"]\nmin_lattice = \"999.0.0\"\n",
        ),
    )
    .unwrap();

    let loader = loader(base.path());
    assert!(loader.load_path(&dir, TrustTier::Bundled).await.is_err());
    let failed = loader.failed_loads();
    assert_eq!(failed.len(), 1, "visible in :plugins: {failed:?}");
    assert_eq!(failed[0].name, "future", "named from the embedded manifest");
    assert!(
        failed[0].error.contains("needs lattice ≥ 999.0.0"),
        "{}",
        failed[0].error
    );

    // A directory that is not a plugin at all is still the caller's answer,
    // not a `:plugins` row.
    let empty = base.path().join("plugins").join("nothing");
    std::fs::create_dir_all(&empty).unwrap();
    assert!(loader.load_path(&empty, TrustTier::Bundled).await.is_err());
    assert_eq!(loader.failed_loads().len(), 1);
}
