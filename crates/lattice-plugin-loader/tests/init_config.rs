//! PL8.D.3 end-to-end: the user's `init.rs` is just a plugin loaded from
//! `<config>/lattice/init/` with a boot-capability (`Bundled`) tier. This drives
//! that spine with the keymap fixture standing in for a real init.rs: load the
//! init dir via `load_path`, assert its keybinding lands, then exercise the
//! `:reload-config` path (reload the `init` plugin) and assert it survives.
//! Skips when the keymap fixture wasn't built.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_config::ConfigRegistry;
use lattice_grammar::CommandRegistryHandle;
use lattice_grammar::registry::CommandRegistry;
use lattice_keymap::{BindingMode, KeymapHandle, LookupResult};
use lattice_mode::{ModeRegistry, ModeRegistryHandle, PluginMetaSink};
use lattice_picker::PickerRegistry;
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{
    ConfigBuildStatus, LoaderServices, PluginLoader, PluginLoaderHandle, ReloadConfigReport,
};
use lattice_runtime::EventBus;

fn keymap_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/keymap-guest/target/wasm32-wasip2/release/keymap_guest.wasm"
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

/// Write an `init/` dir: `plugin.toml` (`id = "init"`, `provides = ["keymap"]`) +
/// the component — the shape `<config>/lattice/init/` holds.
fn write_init_dir(dir: &std::path::Path, wasm: &[u8]) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        "id = \"init\"\nprovides = [\"keymap\"]\n",
    )
    .unwrap();
    std::fs::write(dir.join("init.wasm"), wasm).unwrap();
}

fn loader(base: &std::path::Path, keymap: KeymapHandle) -> PluginLoaderHandle {
    let host = Arc::new(PluginHost::with_dirs(base.join("cache"), base.join("data")).unwrap());
    Arc::new(PluginLoader::with_services(
        host,
        LoaderServices {
            parser_factories: Some(lattice_compilation::CompilationParserFactories::new_handle()),
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            command_registry: Some(commands_with_builtins()),
            keymap: Some(keymap),
            picker_registry: Some(Arc::new(arc_swap::ArcSwap::from_pointee(
                PickerRegistry::new(),
            ))),
            mode_registry: Some(
                Arc::new(arc_swap::ArcSwap::from_pointee(ModeRegistry::default()))
                    as ModeRegistryHandle,
            ),
            config_registry: Some(Arc::new(ConfigRegistry::default())),
            meta_sink: Some(Arc::new(Sink) as Arc<dyn PluginMetaSink>),
            decoration_registry: Some(std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(
                lattice_mode::GutterDecorationSourceRegistry::default(),
            ))),
            context_registry: Some(std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(
                lattice_mode::ContextSourceRegistry::new(),
            ))),
            theme_registry: Some(std::sync::Arc::new(
                lattice_theme::InMemoryThemeRegistry::new(lattice_theme::default_palette()),
            )),
            tracer: None,
            ..Default::default()
        },
    ))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_loads_as_a_plugin_and_survives_reload_config() {
    let Some(wasm) = keymap_wasm() else {
        eprintln!("skipping: keymap-guest fixture not built");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let init_dir = base.path().join("config").join("lattice").join("init");
    write_init_dir(&init_dir, &wasm);

    let keymap = KeymapHandle::new();
    let loader = loader(base.path(), keymap.clone());

    // The boot path: `load_path(<config>/lattice/init, Bundled)`.
    let id = loader
        .load_path(&init_dir, TrustTier::Bundled)
        .await
        .expect("init.rs loads from the config dir");
    assert!(
        loader.is_loaded("init"),
        "loaded under its `init` manifest id"
    );
    assert_eq!(keymap.binding_count(), 1, "init's keybinding is live");
    let chord = lattice_protocol::parse_chord_sequence("<C-s>").unwrap();
    assert!(matches!(
        keymap.lookup(BindingMode::Normal, &chord),
        LookupResult::Bound { .. }
    ));

    // `:reload-config` → `reload("init")`: unbinds the old binding + re-binds
    // from disk. Still exactly one binding (no accumulation), still loaded.
    let new_id = loader
        .reload("init", TrustTier::Bundled)
        .await
        .expect("reload-config re-instantiates init from disk");
    assert!(
        loader.is_loaded("init"),
        "init still loaded after reload-config"
    );
    assert_ne!(new_id.0, id.0, "a fresh host id (fresh Store) after reload");
    assert_eq!(
        keymap.binding_count(),
        1,
        "reload unbound the old binding and re-bound once — no accumulation"
    );
    assert!(matches!(
        keymap.lookup(BindingMode::Normal, &chord),
        LookupResult::Bound { .. }
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_init_loads_when_absent_then_reloads_when_present() {
    let Some(wasm) = keymap_wasm() else {
        eprintln!("skipping: keymap-guest fixture not built");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let init_dir = base.path().join("config").join("lattice").join("init");
    write_init_dir(&init_dir, &wasm);

    let keymap = KeymapHandle::new();
    let loader = loader(base.path(), keymap.clone());

    // Not loaded → sync_init loads.
    assert!(!loader.is_loaded("init"));
    let id1 = loader
        .sync_init(&init_dir, TrustTier::Bundled)
        .await
        .unwrap();
    assert!(loader.is_loaded("init"));
    assert_eq!(keymap.binding_count(), 1);

    // Loaded → sync_init reloads (fresh id, no binding accumulation).
    let id2 = loader
        .sync_init(&init_dir, TrustTier::Bundled)
        .await
        .unwrap();
    assert!(loader.is_loaded("init"));
    assert_ne!(id1.0, id2.0, "reload minted a fresh Store/id");
    assert_eq!(
        keymap.binding_count(),
        1,
        "reload did not accumulate bindings"
    );
}

/// `:reload-config` → `reload_config()`: for a hand-built `init.wasm` (no cargo
/// project) the build is a no-op and the artifact is reloaded, reported as
/// [`ConfigBuildStatus::HandBuilt`] — the edited config applied, no error. This
/// is the fixture's shape; the cargo-project rebuild path needs a real toolchain
/// and is exercised by the boot build, not here.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reload_config_reloads_hand_built_init_and_reports_handbuilt() {
    let Some(wasm) = keymap_wasm() else {
        eprintln!("skipping: keymap-guest fixture not built");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let init_dir = base.path().join("config").join("lattice").join("init");
    write_init_dir(&init_dir, &wasm);

    let keymap = KeymapHandle::new();
    let loader = loader(base.path(), keymap.clone());

    // First load from the config dir, then `reload_config`. It resolves the
    // loaded record's source dir (this tempdir), so it does not touch the real
    // `~/.config/lattice/init`.
    loader
        .load_path(&init_dir, TrustTier::Bundled)
        .await
        .unwrap();
    assert_eq!(keymap.binding_count(), 1);

    let report = loader
        .reload_config()
        .await
        .expect("reload_config reloads the hand-built init.wasm");
    assert_eq!(
        report.build,
        ConfigBuildStatus::HandBuilt,
        "no cargo project → nothing to compile, artifact reloaded as-is"
    );
    assert!(
        report.applied_new_config(),
        "a hand-built reload applied the config (no build failure)"
    );
    assert!(loader.is_loaded("init"), "init still loaded after reload");
    assert_eq!(
        keymap.binding_count(),
        1,
        "reload did not accumulate bindings"
    );

    // The BuildFailed surface (which a real cargo error produces) reports the
    // failure and carries the compiler detail — verified as a pure report, so
    // the assertion needs no toolchain.
    let failed = ReloadConfigReport {
        id: report.id,
        build: ConfigBuildStatus::BuildFailed("error[E0308]: mismatched types".to_string()),
    };
    assert!(
        !failed.applied_new_config(),
        "a build failure means the edit did NOT take"
    );
    let summary = failed.summary();
    assert!(summary.contains("FAILED"), "summary flags the failure");
    assert!(
        summary.contains("error[E0308]"),
        "summary carries the compiler diagnostics for the user to see"
    );
}

/// Regression: the plugins view's `b` (rebuild) on the `init` row routes through
/// `reload_config` rather than the generic build pipeline. Before the fix,
/// `rebuild("init")` refused with "no buildable source" because init's
/// `SourceRecord` is `Unknown` — so the button did nothing and the user had to
/// restart. It must now succeed (here: a hand-built reload).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rebuild_of_the_init_row_succeeds_instead_of_no_buildable_source() {
    let Some(wasm) = keymap_wasm() else {
        eprintln!("skipping: keymap-guest fixture not built");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let init_dir = base.path().join("config").join("lattice").join("init");
    write_init_dir(&init_dir, &wasm);

    let keymap = KeymapHandle::new();
    let loader = loader(base.path(), keymap.clone());
    loader
        .load_path(&init_dir, TrustTier::Bundled)
        .await
        .unwrap();

    // The `b` handler calls `loader.rebuild(name)`. For "init" this used to be
    // `Err("`init` has no buildable source (—)")`.
    loader
        .rebuild("init")
        .await
        .expect("rebuild of the init row succeeds (routes through reload_config)");
    assert!(loader.is_loaded("init"), "init still loaded after rebuild");
    assert_eq!(keymap.binding_count(), 1);
}
