//! PL8.D.4: the init-artifact auto-reload watcher. Rewriting `init.wasm` on disk
//! fires a `sync_init` (reload) without a manual `:reload-config`. This is a real
//! `notify` integration test — it writes the artifact and polls for the reload —
//! so it's inherently timing-dependent; it polls with a generous timeout. Skips
//! when the keymap fixture wasn't built.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lattice_config::ConfigRegistry;
use lattice_grammar::CommandRegistryHandle;
use lattice_grammar::registry::CommandRegistry;
use lattice_keymap::KeymapHandle;
use lattice_mode::{ModeRegistry, ModeRegistryHandle, PluginMetaSink};
use lattice_picker::PickerRegistry;
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::watch::spawn_init_watcher;
use lattice_plugin_loader::{LoaderServices, PluginLoader, PluginLoaderHandle};
use lattice_runtime::EventBus;

fn keymap_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/keymap-guest/target/wasm32-wasip2/release/keymap_guest.wasm"
    );
    std::fs::read(path).ok()
}

/// Counts every `register_plugin` so the test can observe reloads: the initial
/// load is one register; each auto-reload is an unregister + a fresh register.
#[derive(Default)]
struct CountingSink {
    registers: Mutex<usize>,
}
impl PluginMetaSink for CountingSink {
    fn register_plugin(&self, _id: u32, _name: String, _doc: String) {
        *self.registers.lock().unwrap() += 1;
    }
    fn unregister_plugin(&self, _id: u32) {}
}

fn commands_with_builtins() -> CommandRegistryHandle {
    let mut r = CommandRegistry::new();
    let _ = lattice_grammar::ex_commands::populate(&mut r);
    Arc::new(arc_swap::ArcSwap::from_pointee(r))
}

fn write_init_dir(dir: &std::path::Path, wasm: &[u8]) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        "id = \"init\"\nprovides = [\"keymap\"]\n",
    )
    .unwrap();
    std::fs::write(dir.join("init.wasm"), wasm).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rewriting_the_init_artifact_auto_reloads() {
    let Some(wasm) = keymap_wasm() else {
        eprintln!("skipping: keymap-guest fixture not built");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let init_dir = base.path().join("config").join("lattice").join("init");
    write_init_dir(&init_dir, &wasm);

    let keymap = KeymapHandle::new();
    let sink: Arc<CountingSink> = Arc::new(CountingSink::default());
    let host = Arc::new(
        PluginHost::with_dirs(base.path().join("cache"), base.path().join("data")).unwrap(),
    );
    let loader: PluginLoaderHandle = Arc::new(PluginLoader::with_services(
        host,
        LoaderServices {
            parser_factories: Some(lattice_compilation::CompilationParserFactories::new_handle()),
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            command_registry: Some(commands_with_builtins()),
            keymap: Some(keymap.clone()),
            picker_registry: Some(Arc::new(arc_swap::ArcSwap::from_pointee(
                PickerRegistry::new(),
            ))),
            mode_registry: Some(
                Arc::new(arc_swap::ArcSwap::from_pointee(ModeRegistry::default()))
                    as ModeRegistryHandle,
            ),
            config_registry: Some(Arc::new(ConfigRegistry::default())),
            meta_sink: Some(sink.clone() as Arc<dyn PluginMetaSink>),
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
    ));

    // Initial load (as boot would), then start watching.
    loader
        .load_path(&init_dir, TrustTier::Bundled)
        .await
        .unwrap();
    assert_eq!(
        *sink.registers.lock().unwrap(),
        1,
        "one register from the initial load"
    );
    spawn_init_watcher(
        loader.clone(),
        init_dir.clone(),
        &tokio::runtime::Handle::current(),
    );

    // Give the watcher a moment to establish, then simulate a rebuild by
    // rewriting the artifact.
    tokio::time::sleep(Duration::from_millis(300)).await;
    std::fs::write(init_dir.join("init.wasm"), &wasm).unwrap();

    // Poll for the auto-reload: a second `register_plugin` (the reload's fresh
    // load). Generous timeout — notify + the 300ms settle window are timing
    // dependent (FSEvents latency on macOS especially).
    let mut reloaded = false;
    for _ in 0..100 {
        if *sink.registers.lock().unwrap() >= 2 {
            reloaded = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(reloaded, "rewriting init.wasm auto-reloaded the config");
    assert!(
        loader.is_loaded("init"),
        "init still loaded after the auto-reload"
    );
}
