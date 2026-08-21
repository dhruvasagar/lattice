//! PM.7b end-to-end: a config guest declaring `plugin-manager` in its
//! `provides` has its `require` specs drained during the ORDINARY load, and
//! the queue drains exactly once.
//!
//! This is the integration PM.7a deliberately left out: the seam and the
//! pipeline were each tested in isolation, and what needed proving is that
//! `load_path` routes a `plugin-manager` guest through `drain_require` using
//! the component it already compiled — rather than needing a second spawn.
//!
//! Skips when the fixture wasn't built.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_config::ConfigRegistry;
use lattice_grammar::CommandRegistryHandle;
use lattice_grammar::registry::CommandRegistry;
use lattice_mode::{ModeRegistry, ModeRegistryHandle, PluginMetaSink};
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{
    LoaderServices, PluginLoader, PluginLoaderHandle, PluginSource, RequiredSpec,
};
use lattice_runtime::EventBus;

/// The PM.7 fixture guest, or `None` when it wasn't built (skip).
///
/// Read by path rather than through `PLUGIN_MANAGER_GUEST_WASM`: that variable
/// is exported by the *host* crate's build.rs and is not visible to this
/// crate's compilation, so `option_env!` would silently be `None` here and
/// both real tests would pass by skipping. The sibling loader tests
/// (`init_config.rs`) resolve the same way for the same reason.
fn guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/plugin-manager-guest",
        "/target/wasm32-wasip2/release/plugin_manager_guest.wasm"
    );
    std::fs::read(path).ok()
}

#[derive(Default)]
struct Sink;
impl PluginMetaSink for Sink {
    fn register_plugin(&self, _id: u32, _name: String, _doc: String) {}
    fn unregister_plugin(&self, _id: u32) {}
}

fn commands_with_builtins() -> CommandRegistryHandle {
    let mut r = CommandRegistry::new();
    let _ = lattice_grammar::ex_commands::populate(&mut r);
    Arc::new(arc_swap::ArcSwap::from_pointee(r))
}

/// An `init/` dir declaring the `plugin-manager` seam.
fn write_init_dir(dir: &std::path::Path, wasm: &[u8]) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        "id = \"init\"\nprovides = [\"plugin-manager\"]\n",
    )
    .unwrap();
    std::fs::write(dir.join("init.wasm"), wasm).unwrap();
}

fn loader(base: &std::path::Path) -> PluginLoaderHandle {
    let host = Arc::new(PluginHost::with_dirs(base.join("cache"), base.join("data")).unwrap());
    Arc::new(PluginLoader::with_services(
        host,
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            command_registry: Some(commands_with_builtins()),
            mode_registry: Some(
                Arc::new(arc_swap::ArcSwap::from_pointee(ModeRegistry::default()))
                    as ModeRegistryHandle,
            ),
            config_registry: Some(Arc::new(ConfigRegistry::default())),
            meta_sink: Some(Arc::new(Sink) as Arc<dyn PluginMetaSink>),
            ..Default::default()
        },
    ))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loading_a_plugin_manager_guest_queues_its_required_specs() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: plugin-manager fixture guest not built");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let init_dir = base.path().join("init");
    write_init_dir(&init_dir, &wasm);
    let loader = loader(base.path());

    loader
        .load_path(&init_dir, TrustTier::Bundled)
        .await
        .expect("init loads");

    let specs = loader.take_required();
    assert_eq!(
        specs.len(),
        3,
        "the guest's three well-formed requires are queued: {specs:?}"
    );

    // The traversal-named one never reaches the queue — the host's name gate
    // refused it inside the guest call.
    assert!(!specs.iter().any(|s| s.name.contains("escape")));

    // Field fidelity across the whole path: WIT → host → loader.
    let git = specs.iter().find(|s| s.name == "git_demo").unwrap();
    assert_eq!(
        git,
        &RequiredSpec {
            name: "git_demo".into(),
            source: PluginSource::Git {
                url: "https://example.invalid/demo.git".into(),
                rev: Some("abc123".into()),
            },
            enable_mode: None,
            pinned: true,
        }
    );
    let local = specs.iter().find(|s| s.name == "local-demo").unwrap();
    assert_eq!(local.enable_mode.as_deref(), Some("demo-mode"));
}

/// The queue drains, it does not merely read.
///
/// A second drain returning the same specs would resolve, build and load every
/// declared plugin twice — a second clone, a second compile, a second load of
/// an already-loaded id. Cheap to get wrong, expensive to notice.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_required_queue_drains_exactly_once() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: plugin-manager fixture guest not built");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let init_dir = base.path().join("init");
    write_init_dir(&init_dir, &wasm);
    let loader = loader(base.path());

    loader
        .load_path(&init_dir, TrustTier::Bundled)
        .await
        .expect("init loads");

    assert_eq!(loader.take_required().len(), 3);
    assert!(
        loader.take_required().is_empty(),
        "a second drain must be empty, or every declared plugin installs twice"
    );
}

/// A guest that does not declare the seam queues nothing — the overwhelmingly
/// common case (no init.rs, or an init.rs that only sets options).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_guest_without_the_seam_queues_nothing() {
    let base = tempfile::tempdir().unwrap();
    let loader = loader(base.path());
    assert!(
        loader.take_required().is_empty(),
        "an editor with no init.rs must not queue any install work"
    );
}

/// The `enable-mode` sugar reaches the event bus.
///
/// It travelled faithfully from WIT to `RequiredSpec` from PM.7 onward, but
/// nothing consumed it — a `require` with `enable-mode` installed and loaded
/// the plugin and left its mode off. This pins the last hop.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enable_mode_publishes_a_mode_enablement_request() {
    use lattice_protocol::{Event, EventKind};
    use lattice_runtime::{EventFilter, SubscriptionTarget};

    let base = tempfile::tempdir().unwrap();
    let bus = Arc::new(EventBus::new());
    let host = Arc::new(
        PluginHost::with_dirs(base.path().join("cache"), base.path().join("data")).unwrap(),
    );
    let loader = Arc::new(PluginLoader::with_services(
        host,
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(bus.clone()),
            ..Default::default()
        },
    ));

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    bus.subscribe(
        EventFilter::kind(EventKind::ModeEnablementRequested),
        SubscriptionTarget::Channel(tx),
    );

    loader.request_mode_enablement("demo-mode");

    match rx.try_recv() {
        Ok(Event::ModeEnablementRequested { mode, enabled }) => {
            assert_eq!(mode, "demo-mode", "the mode-id arrives opaque and intact");
            assert!(enabled, "a require asks to enable, never to disable");
        }
        other => panic!("expected a ModeEnablementRequested, got {other:?}"),
    }
}
