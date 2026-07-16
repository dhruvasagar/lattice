//! PL8.C.2: the loader-owned `:plugin-load` / `:plugin-unload` / `:plugin-reload`
//! ex-command surface (option A — the loader self-registers into the
//! runtime-mutable command registry; zero host code). The apply closures are
//! driven directly off the registered `ExCommandSpec` (the same `apply` the
//! `:`-line dispatcher calls), asserting effects + loader state.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lattice_config::ConfigRegistry;
use lattice_grammar::registry::CommandRegistry;
use lattice_grammar::{
    Args, CancellationToken, CommandRegistryHandle, Count, EchoLevel, Effect, ExCommandContext,
    Register,
};
use lattice_keymap::KeymapHandle;
use lattice_mode::{ModeRegistry, PluginMetaSink};
use lattice_picker::PickerRegistry;
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader, PluginLoaderHandle};
use lattice_runtime::EventBus;

fn picker_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/picker-guest/target/wasm32-wasip2/release/picker_guest.wasm"
    );
    std::fs::read(path).ok()
}

#[derive(Default)]
struct RecordingSink {
    registered: Mutex<Vec<(u32, String)>>,
}
impl PluginMetaSink for RecordingSink {
    fn register_plugin(&self, id: u32, name: String, _doc: String) {
        self.registered.lock().unwrap().push((id, name));
    }
    fn unregister_plugin(&self, id: u32) {
        self.registered.lock().unwrap().retain(|(i, _)| *i != id);
    }
}

fn write_plugin_dir(root: &std::path::Path, id: &str, provides: &str, wasm: &[u8]) {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        format!("id = \"{id}\"\nprovides = [\"{provides}\"]\n"),
    )
    .unwrap();
    std::fs::write(dir.join("component.wasm"), wasm).unwrap();
}

/// A fully-wired `Arc<PluginLoader>` that has self-registered its ex-commands,
/// plus the command-registry handle the assertions drive the `apply` closures
/// through.
fn loader_with_ex_commands(base: &std::path::Path) -> (PluginLoaderHandle, CommandRegistryHandle) {
    let host = Arc::new(PluginHost::with_dirs(base.join("cache"), base.join("data")).unwrap());
    let commands: CommandRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new()));
    let loader: PluginLoaderHandle = Arc::new(PluginLoader::with_services(
        host,
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            picker_registry: Some(Arc::new(arc_swap::ArcSwap::from_pointee(PickerRegistry::new()))),
            command_registry: Some(commands.clone()),
            mode_registry: Some(Arc::new(arc_swap::ArcSwap::from_pointee(ModeRegistry::default()))),
            config_registry: Some(Arc::new(ConfigRegistry::default())),
            keymap: Some(KeymapHandle::new()),
            meta_sink: Some(Arc::new(RecordingSink::default()) as Arc<dyn PluginMetaSink>),
        },
    ));
    loader.register_ex_commands();
    (loader, commands)
}

/// Drive a registered ex-command's `apply` with an optional string arg — the
/// same `ExCommandSpec::apply` the `:`-line dispatcher calls.
fn invoke(commands: &CommandRegistryHandle, name: &str, arg: Option<&str>) -> Effect {
    let snapshot = commands.load();
    let id = snapshot
        .id_by_name(name)
        .unwrap_or_else(|| panic!("`{name}` registered"));
    let spec = snapshot
        .ex_command_spec(id)
        .unwrap_or_else(|| panic!("`{name}` is an ex-command"));
    let ctx = ExCommandContext {
        bang: false,
        args: match arg {
            Some(a) => Args::String(a.to_string()),
            None => Args::None,
        },
        range: None,
        register: Register::default(),
        count: Count(1),
        cancel: CancellationToken::never(),
    };
    (spec.apply)(&ctx).expect("apply returns an effect")
}

fn echo_text(effect: &Effect) -> (&EchoLevel, &str) {
    match effect {
        Effect::Echo { level, text } => (level, text.as_str()),
        other => panic!("expected Effect::Echo, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn all_three_commands_register_with_plain_names() {
    let base = tempfile::tempdir().unwrap();
    let (_loader, commands) = loader_with_ex_commands(base.path());
    let snapshot = commands.load();
    // Plain names resolve directly via `id_by_name` — no `expand_alias` host
    // entry (option A: zero host code).
    for name in ["plugin-load", "plugin-unload", "plugin-reload", "reload-config"] {
        assert!(
            snapshot.id_by_name(name).is_some(),
            "`{name}` registered under its plain name"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reload_config_echoes_and_takes_no_arg() {
    let base = tempfile::tempdir().unwrap();
    let (_loader, commands) = loader_with_ex_commands(base.path());
    // `:reload-config` takes no argument — invoking it (no `init` loaded here)
    // still echoes the "reloading…" acknowledgement and spawns the reload (which
    // no-ops on a missing `init`, reported in *messages*), never a panic.
    let effect = invoke(&commands, "reload-config", None);
    let (level, text) = echo_text(&effect);
    assert_eq!(*level, EchoLevel::Info);
    assert!(text.contains("init.rs"), "acknowledges the config reload: {text}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_arg_echoes_a_usage_hint() {
    let base = tempfile::tempdir().unwrap();
    let (_loader, commands) = loader_with_ex_commands(base.path());
    for name in ["plugin-load", "plugin-unload", "plugin-reload"] {
        let (level, text) = {
            let e = invoke(&commands, name, None);
            let (l, t) = echo_text(&e);
            (*l, t.to_string())
        };
        assert_eq!(level, EchoLevel::Warn, "{name} with no arg warns");
        assert!(text.contains("usage:"), "{name} echoes a usage hint: {text}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unload_command_removes_a_loaded_plugin_synchronously() {
    let Some(wasm) = picker_wasm() else {
        eprintln!("skipping: picker fixture not built");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let (loader, commands) = loader_with_ex_commands(base.path());

    let dir = base.path().join("plugins");
    write_plugin_dir(&dir, "picker-fixture", "picker-source", &wasm);
    assert_eq!(loader.discover_and_load(&dir, TrustTier::Bundled).await, 1);
    assert!(loader.is_loaded("picker-fixture"));

    // `:plugin-unload picker-fixture` — synchronous, so the effect reflects the
    // completed unload immediately.
    let effect = invoke(&commands, "plugin-unload", Some("picker-fixture"));
    let (level, text) = echo_text(&effect);
    assert_eq!(*level, EchoLevel::Info);
    assert!(text.contains("unloaded"), "reports the unload: {text}");
    assert!(!loader.is_loaded("picker-fixture"), "plugin gone after :plugin-unload");

    // `:plugin-unload` of an unknown target warns, no panic.
    let effect = invoke(&commands, "plugin-unload", Some("ghost"));
    assert_eq!(*echo_text(&effect).0, EchoLevel::Warn);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn load_command_echoes_then_loads_asynchronously() {
    let Some(wasm) = picker_wasm() else {
        eprintln!("skipping: picker fixture not built");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let (loader, commands) = loader_with_ex_commands(base.path());

    let dir = base.path().join("plugins").join("picker-fixture");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        "id = \"picker-fixture\"\nprovides = [\"picker-source\"]\n",
    )
    .unwrap();
    std::fs::write(dir.join("component.wasm"), &wasm).unwrap();

    // `:plugin-load <dir>` returns an immediate "loading…" echo and spawns the
    // async load on the loader's runtime.
    let effect = invoke(&commands, "plugin-load", Some(dir.to_str().unwrap()));
    let (level, text) = echo_text(&effect);
    assert_eq!(*level, EchoLevel::Info);
    assert!(text.contains("loading"), "immediate loading echo: {text}");

    // The spawned load completes shortly after — poll until it lands.
    let mut loaded = false;
    for _ in 0..50 {
        if loader.is_loaded("picker-fixture") {
            loaded = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(loaded, "the async :plugin-load eventually registers the plugin");
}
