//! MV.1b end-to-end: a view plugin discovered on disk loads at boot and each
//! view it declared becomes reachable by name in the provider-view registry —
//! the same registry `:agenda` and magit's project-diff live in.
//!
//! **Why this test exists at all.** The seam has its own tests, through a real
//! guest, in `lattice-plugin-host`. They prove the boundary crosses. They do
//! not prove the drain wires it to anything, and the failure this codebase
//! keeps re-finding is exactly that gap: a seam with a WIT export, an actor, an
//! adapter and a registration, where nothing ever calls it. OR.6's chord and
//! OR.9's id-follow both looked wired and did nothing.
//!
//! Skips when the fixture component was not built — the loader crate cannot
//! read the host crate's build-script env var, so the artefact is resolved by
//! its known path.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_mode::{ProviderViewRegistry, ProviderViewRegistryHandle};
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader, discover};
use lattice_runtime::EventBus;

fn view_guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/view-guest/target/wasm32-wasip2/release/view_guest.wasm"
    );
    std::fs::read(path).ok()
}

fn write_plugin_dir(root: &std::path::Path, id: &str, wasm: &[u8]) {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        format!("id = \"{id}\"\nprovides = [\"multibuffer-view-source\"]\n"),
    )
    .unwrap();
    std::fs::write(dir.join("component.wasm"), wasm).unwrap();
}

fn temp_host(base: &std::path::Path) -> Arc<PluginHost> {
    Arc::new(PluginHost::with_dirs(base.join("cache"), base.join("data")).expect("host builds"))
}

fn services(providers: &ProviderViewRegistryHandle) -> LoaderServices {
    LoaderServices {
        runtime: Some(tokio::runtime::Handle::current()),
        bus: Some(Arc::new(EventBus::new())),
        provider_view_registry: Some(providers.clone()),
        multibuffer_registry: Some(Arc::new(
            lattice_multibuffer::registry::InMemoryMultibufferRegistry::new(),
        )),
        ..Default::default()
    }
}

/// The property the whole slice turns on: a view a GUEST declared is reachable
/// by name in the registry the host opens views from.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_guest_declared_view_becomes_openable_by_name() {
    let Some(wasm) = view_guest_wasm() else {
        eprintln!("skipping: view-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "view-fixture", &wasm);

    let providers: ProviderViewRegistryHandle = Arc::new(ProviderViewRegistry::new());
    let loader = PluginLoader::with_services(temp_host(base.path()), services(&providers));

    assert_eq!(
        discover(&plugins_dir).len(),
        1,
        "discovery finds the plugin"
    );
    let n = loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(n, 1, "the view plugin loads");

    let names = providers.names();
    assert!(
        names.contains(&"fixture-pull".to_string()),
        "the guest's pull view is openable by name: {names:?}"
    );
    assert!(
        names.contains(&"fixture-scan".to_string()),
        "and so is its second view — one component, several views: {names:?}"
    );
    assert!(
        providers.lookup("fixture-pull").is_some(),
        "and the opener is actually retrievable, not merely named"
    );
}

/// A malformed spec costs only itself. The fixture declares an UNNAMED third
/// view; dropping the plugin's whole contribution over it is the failure this
/// asserts against.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_malformed_view_does_not_cost_the_others() {
    let Some(wasm) = view_guest_wasm() else {
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "view-fixture", &wasm);

    let providers: ProviderViewRegistryHandle = Arc::new(ProviderViewRegistry::new());
    let loader = PluginLoader::with_services(temp_host(base.path()), services(&providers));
    loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;

    assert_eq!(
        providers.names().len(),
        2,
        "two views registered; the unnamed one was refused at the boundary"
    );
}

/// An id a NATIVE provider already owns is refused, and the guest keeps its
/// other views.
///
/// `ProviderViewRegistry::register` refuses rather than replaces, deliberately:
/// last-write-wins would make which view `:foo` opens depend on load order.
/// What this pins is that the refusal is per-view — the plugin's other
/// declarations must still land.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_id_a_native_provider_owns_is_refused_per_view() {
    let Some(wasm) = view_guest_wasm() else {
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "view-fixture", &wasm);

    let providers: ProviderViewRegistryHandle = Arc::new(ProviderViewRegistry::new());
    // Squat on the name the guest is about to declare.
    assert!(providers.register(
        "fixture-pull",
        Arc::new(
            |_: &mut dyn lattice_mode::ModeActivator, _: &lattice_grammar::Args| {
                lattice_mode::ProviderViewOutcome::Declined {
                    message: "native".to_string(),
                }
            }
        ),
    ));

    let loader = PluginLoader::with_services(temp_host(base.path()), services(&providers));
    loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;

    // The squatter still owns the name…
    let opener = providers.lookup("fixture-pull").expect("still registered");
    let mut activator = NoopActivator;
    match opener(&mut activator, &lattice_grammar::Args::None) {
        lattice_mode::ProviderViewOutcome::Declined { message } => {
            assert_eq!(message, "native", "the FIRST registration kept the name");
        }
        other => panic!("expected the native opener, got {other:?}"),
    }
    // …and the guest's other view still registered.
    assert!(
        providers.lookup("fixture-scan").is_some(),
        "one refused id must not cost the plugin its other views"
    );
}

/// Unloading reverses the registration, so a reload can re-register.
///
/// Without this, `register` returns `false` against the plugin's OWN stale
/// opener on the second load and its views come back dead — the lifetime
/// assumption `ProviderViewRegistry` documented ("openers live for the
/// process") that is true of native providers and false of plugins.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unloading_frees_the_view_names_for_a_reload() {
    let Some(wasm) = view_guest_wasm() else {
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "view-fixture", &wasm);

    let providers: ProviderViewRegistryHandle = Arc::new(ProviderViewRegistry::new());
    let loader = PluginLoader::with_services(temp_host(base.path()), services(&providers));
    loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(providers.names().len(), 2, "loaded");

    let report = loader.unload("view-fixture");
    assert!(report.is_some(), "the plugin unloads");
    assert!(
        providers.names().is_empty(),
        "its view names are free again: {:?}",
        providers.names()
    );

    // And a reload puts them back — the whole point of reversing.
    let n = loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(n, 1, "reloads");
    assert_eq!(
        providers.names().len(),
        2,
        "and its views are live again rather than refused against their own stale openers"
    );
}

/// A `ModeActivator` that does nothing — enough to invoke an opener that
/// declines before touching the activator.
struct NoopActivator;

impl lattice_mode::ModeActivator for NoopActivator {
    fn services(&self) -> Arc<lattice_mode::ServiceRegistry> {
        Arc::new(lattice_mode::ServiceRegistry::default())
    }
    fn activate_major_for_kind(&mut self, _: lattice_core::BufferId, _: lattice_core::BufferKind) {}
    fn activate_minor_by_id(&mut self, _: lattice_core::BufferId, _: lattice_mode::ModeId) {}
    fn ensure_named_document(
        &mut self,
        _: &str,
        _: lattice_mode::ModeId,
        _: lattice_core::BufferFlags,
    ) -> lattice_core::BufferId {
        lattice_core::BufferId::next()
    }
}

/// SECURITY: an excerpt path outside the plugin's fs grant is refused.
///
/// An excerpt names a PATH the guest chose and the host reads it to build the
/// source document. Without a gate, a plugin holding no fs capability at all
/// could name `/etc/passwd` and have the host read it into a buffer on its
/// behalf — the guest never touches the file, so WASI's sandbox never sees the
/// read. `EffectAuthorizer` states the rule this restores: a guest-named path is
/// checked at the BOUNDARY, where the host still knows which plugin asked.
///
/// Asserted at the gate rather than through a view, because that is where the
/// decision is made and a view would also need a buffer store to observe.
#[test]
fn an_excerpt_outside_the_grant_is_denied() {
    use lattice_plugin_host::host_services::grant_permits_read;

    let tmp = tempfile::tempdir().unwrap();
    let granted = tmp.path().join("notes");
    std::fs::create_dir_all(&granted).unwrap();
    let inside = granted.join("a.org");
    std::fs::write(&inside, "x\n").unwrap();

    let outside = tmp.path().join("secret.txt");
    std::fs::write(&outside, "shh\n").unwrap();

    let manifest = lattice_plugin_host::PluginManifest::from_toml_str(&format!(
        "id = \"viewer\"\nprovides = [\"multibuffer-view-source\"]\ncapabilities = [\"fs:read:{}\"]\n",
        granted.display()
    ))
    .expect("manifest parses");
    let grant = lattice_plugin_host::capability::grant(&manifest, TrustTier::Bundled).grant;

    assert!(
        grant_permits_read(&grant, &inside),
        "a path inside the grant is allowed"
    );
    assert!(
        !grant_permits_read(&grant, &outside),
        "a path outside it is denied — the plugin cannot borrow the host's fs authority"
    );

    // And the classic escape: a symlink INSIDE the granted tree pointing out of
    // it. Denied because the check canonicalises the file itself first rather
    // than trusting its parent — the ordering `grant_permits_read` documents.
    #[cfg(unix)]
    {
        let link = granted.join("innocent.org");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert!(
            !grant_permits_read(&grant, &link),
            "a symlink out of the grant is denied, not followed"
        );
    }
}
