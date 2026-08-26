//! PM.3 — the manifest `default_mode` + `<id>.enabled` config gate. A loaded
//! plugin declaring a `default_mode` auto-registers a `<id>.enabled` bool option
//! (default true) and, gated by it, requests its mode's enablement via
//! `Event::ModeEnablementRequested` (the CI.4 path the editor drains). Toggling
//! `<id>.enabled` re-requests to match — the batteries-included, user-overridable
//! enablement of a core plugin (auto-pair on out of the box; `:set
//! auto-pair.enabled=false` turns it off). Skips when auto-pair wasn't built.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use lattice_config::ConfigRegistry;
use lattice_grammar::CommandRegistryHandle;
use lattice_grammar::registry::CommandRegistry;
use lattice_keymap::KeymapHandle;
use lattice_mode::{ModeRegistry, ModeRegistryHandle};
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader};
use lattice_protocol::{Event, EventKind};
use lattice_runtime::{EventBus, EventFilter, SubscriptionTarget};

fn auto_pair_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../plugins/auto-pair/target/wasm32-wasip2/release/auto_pair.wasm"
    );
    std::fs::read(path).ok()
}

/// Stage an auto-pair plugin dir whose manifest declares `default_mode` — the
/// PM.3 gate trigger (auto-pair provides `auto-pair-mode`).
fn write_plugin_dir(root: &std::path::Path, wasm: &[u8]) {
    let dir = root.join("auto-pair");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        "id = \"auto-pair\"\n\
         provides = [\"grammar\", \"modes\", \"config\"]\n\
         editor_capabilities = [\"tree-sitter\"]\n\
         default_mode = \"auto-pair-mode\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("component.wasm"), wasm).unwrap();
}

fn empty_mode_registry() -> ModeRegistryHandle {
    Arc::new(arc_swap::ArcSwap::from_pointee(ModeRegistry::default()))
}
fn empty_command_registry() -> CommandRegistryHandle {
    Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new()))
}

/// Drain one `ModeEnablementRequested` with a short timeout.
async fn next_enablement(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<Event>,
) -> Option<(String, bool)> {
    let ev = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .ok()??;
    match ev {
        Event::ModeEnablementRequested { mode, enabled } => Some((mode, enabled)),
        _ => None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn default_mode_gate_enables_on_load_and_toggles_with_the_option() {
    let Some(wasm) = auto_pair_wasm() else {
        eprintln!("skipping: auto-pair wasm not built");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, &wasm);

    let bus = Arc::new(EventBus::new());
    // Watch the enablement requests BEFORE loading, so the load-time request lands.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    bus.subscribe(
        EventFilter::kind(EventKind::ModeEnablementRequested),
        SubscriptionTarget::Channel(tx),
    );

    let config = Arc::new(ConfigRegistry::default());
    let mode_registry = empty_mode_registry();
    let host = Arc::new(
        PluginHost::with_dirs(base.path().join("cache"), base.path().join("data")).unwrap(),
    );
    let loader = Arc::new(PluginLoader::with_services(
        host,
        LoaderServices {
            // The core plugins now `provides = [… "help"]` (CR.3), and an
            // unwired seam fails the WHOLE load — so a harness that loads a
            // real core plugin has to wire this or it silently gets zero
            // plugins.
            help_topics: Some(lattice_help::topics::builtin_topics().into_handle()),
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(bus.clone()),
            command_registry: Some(empty_command_registry()),
            mode_registry: Some(mode_registry.clone()),
            config_registry: Some(config.clone()),
            keymap: Some(KeymapHandle::new()),
            ..Default::default()
        },
    ));
    // Start the `<id>.enabled` toggle reactivity (install() does this at boot).
    loader.subscribe_mode_gates();

    let n = loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(n, 1, "auto-pair loads");

    // The modes seam registered the declared mode (available for the gate to
    // enable) — the full composition: discovery → modes drain → gate.
    assert!(
        mode_registry
            .load()
            .is_registered(lattice_mode::ModeId::new("auto-pair-mode")),
        "auto-pair-mode registered from the modes seam"
    );

    // The gate auto-registered `auto-pair.enabled` (default true).
    assert!(
        config.lookup("auto-pair.enabled").is_some(),
        "the <id>.enabled gate option registered"
    );
    assert_eq!(
        config.lookup("auto-pair.enabled").unwrap().get_formatted(),
        "true"
    );

    // On load, the gate requested enablement of the declared mode.
    let (mode, enabled) = next_enablement(&mut rx)
        .await
        .expect("a ModeEnablementRequested on load");
    assert_eq!(mode, "auto-pair-mode");
    assert!(enabled, "default-true gate enables the mode on load");

    // Toggle the gate off — a `:set auto-pair.enabled=false` publishes
    // OptionChanged; `subscribe_mode_gates` maps it back to the mode and requests
    // a disable.
    bus.publish(Event::OptionChanged {
        name: "auto-pair.enabled".to_string(),
        old: Some("true".to_string()),
        new: "false".to_string(),
    });
    let (mode, enabled) = next_enablement(&mut rx)
        .await
        .expect("a ModeEnablementRequested on toggle");
    assert_eq!(mode, "auto-pair-mode");
    assert!(!enabled, "toggling the gate off disables the mode");

    // And back on.
    bus.publish(Event::OptionChanged {
        name: "auto-pair.enabled".to_string(),
        old: Some("false".to_string()),
        new: "true".to_string(),
    });
    let (_, enabled) = next_enablement(&mut rx).await.expect("a re-enable request");
    assert!(enabled, "toggling the gate on re-enables the mode");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_default_mode_means_no_gate_option() {
    let Some(wasm) = auto_pair_wasm() else {
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    // A manifest WITHOUT default_mode — no gate, no auto-registered option.
    let dir = plugins_dir.join("auto-pair");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        "id = \"auto-pair\"\nprovides = [\"grammar\", \"modes\", \"config\"]\neditor_capabilities = [\"tree-sitter\"]\n",
    )
    .unwrap();
    std::fs::write(dir.join("component.wasm"), &wasm).unwrap();

    let bus = Arc::new(EventBus::new());
    let config = Arc::new(ConfigRegistry::default());
    let host = Arc::new(
        PluginHost::with_dirs(base.path().join("cache"), base.path().join("data")).unwrap(),
    );
    let loader = Arc::new(PluginLoader::with_services(
        host,
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(bus.clone()),
            command_registry: Some(empty_command_registry()),
            mode_registry: Some(empty_mode_registry()),
            config_registry: Some(config.clone()),
            keymap: Some(KeymapHandle::new()),
            ..Default::default()
        },
    ));
    loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert!(
        config.lookup("auto-pair.enabled").is_none(),
        "no default_mode ⇒ no gate option registered"
    );
}

/// OC.1a — a plugin with TWO on-by-default modes gets both enabled, from ONE
/// `<id>.enabled` gate.
///
/// The blocker this closes: `default_mode` was a single string, so org — which
/// contributes `org-todo-mode` for org files *and* a universal `org-global-mode`
/// for the `<C-x>o` prefix — could name only one. The other registered
/// correctly, reported the right kind, and never activated, because
/// `auto_activatable_minors` filters on enablement. The symptom is a chord that
/// does nothing at all, with no message anywhere to say why.
///
/// One gate rather than one per mode: `<id>.enabled` is the plugin's switch,
/// and the user is turning org on or off, not curating its internals.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_default_modes_are_both_enabled_by_the_one_gate() {
    let Some(wasm) = auto_pair_wasm() else {
        eprintln!("skipping: auto-pair wasm not built");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    let dir = plugins_dir.join("auto-pair");
    std::fs::create_dir_all(&dir).unwrap();
    // Both spellings at once, which is also the merge under test: the singular
    // names the primary mode, the list adds the second.
    std::fs::write(
        dir.join("plugin.toml"),
        "id = \"auto-pair\"\n\
         provides = [\"grammar\", \"modes\", \"config\"]\n\
         editor_capabilities = [\"tree-sitter\"]\n\
         default_mode = \"auto-pair-mode\"\n\
         default_modes = [\"auto-pair-global-mode\"]\n",
    )
    .unwrap();
    std::fs::write(dir.join("component.wasm"), &wasm).unwrap();

    let bus = Arc::new(EventBus::new());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    bus.subscribe(
        EventFilter::kind(EventKind::ModeEnablementRequested),
        SubscriptionTarget::Channel(tx),
    );
    let config = Arc::new(ConfigRegistry::default());
    let host = Arc::new(
        PluginHost::with_dirs(base.path().join("cache"), base.path().join("data")).unwrap(),
    );
    let loader = Arc::new(PluginLoader::with_services(
        host,
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(bus.clone()),
            command_registry: Some(empty_command_registry()),
            mode_registry: Some(empty_mode_registry()),
            config_registry: Some(config.clone()),
            keymap: Some(KeymapHandle::new()),
            ..Default::default()
        },
    ));
    loader.subscribe_mode_gates();
    loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;

    let mut on = Vec::new();
    for _ in 0..2 {
        on.push(next_enablement(&mut rx).await.expect("an enable request"));
    }
    assert_eq!(
        on,
        vec![
            ("auto-pair-mode".to_string(), true),
            ("auto-pair-global-mode".to_string(), true),
        ],
        "both modes are enabled on load, singular first"
    );

    // ONE option, not two — the gate is the plugin's, not the mode's.
    assert!(config.lookup("auto-pair.enabled").is_some());
    assert!(config.lookup("auto-pair-global-mode.enabled").is_none());

    // And toggling that one gate off must reach BOTH: a plugin half-disabled
    // is worse than one that stayed on, because half its chords keep firing.
    bus.publish(Event::OptionChanged {
        name: "auto-pair.enabled".to_string(),
        old: Some("true".to_string()),
        new: "false".to_string(),
    });
    let mut off = Vec::new();
    for _ in 0..2 {
        off.push(next_enablement(&mut rx).await.expect("a disable request"));
    }
    off.sort();
    assert_eq!(
        off,
        vec![
            ("auto-pair-global-mode".to_string(), false),
            ("auto-pair-mode".to_string(), false),
        ],
    );
}
