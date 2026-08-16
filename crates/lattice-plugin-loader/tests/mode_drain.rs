//! PL8.B end-to-end: a mode plugin discovered on disk loads at boot and its
//! minor modes register into the (runtime-mutable) mode registry, with each
//! mode's declared keymap landing in its own gated `MinorMode` layer and its
//! provenance recorded for `:list-plugins`.
//!
//! Uses the canonical `modes-guest` fixture the runtime crate builds to a
//! `wasm32-wasip2` component (declares `git-blame-mode` with a `<C-s>` → `ex:write`
//! binding, `lsp-lens-mode`, and a mis-suffixed `not-suffixed` the registry
//! rejects). Skips when that component was not built (no `wasm32-wasip2` target).
//!
//! A registered mode is declarative data (the guest `Store` drops after
//! `register-modes`), so — unlike the grammar seam — this exercises B2's
//! `ModeRegistryHandle` RCU (load → clone → spawn → store) rather than a live
//! trampoline.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use lattice_grammar::CommandRegistryHandle;
use lattice_grammar::registry::CommandRegistry;
use lattice_keymap::{BindingMode, KeymapHandle, LookupResult};
use lattice_mode::{ModeId, ModeRegistry, ModeRegistryHandle, PluginMetaSink};
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader, discover};
use lattice_runtime::EventBus;

/// The modes fixture component, if the `wasm32-wasip2` build produced it. The
/// loader crate can't read the runtime crate's build-script env var, so resolve
/// the artifact by its known path and skip if absent.
fn modes_guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/modes-guest/target/wasm32-wasip2/release/modes_guest.wasm"
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

fn temp_host(base: &std::path::Path) -> Arc<PluginHost> {
    Arc::new(PluginHost::with_dirs(base.join("cache"), base.join("data")).expect("host builds"))
}

/// A fresh empty mode registry handle (no foundation modes) so the plugin's
/// modes are the only entries.
fn empty_mode_registry() -> ModeRegistryHandle {
    Arc::new(arc_swap::ArcSwap::from_pointee(ModeRegistry::default()))
}

/// A command registry handle with the built-in ex-commands populated, so the
/// mode's `<C-s>` → `ex:write` keymap binding resolves at bind time.
fn command_registry_with_builtins() -> CommandRegistryHandle {
    let mut commands = CommandRegistry::new();
    let _ = lattice_grammar::ex_commands::populate(&mut commands);
    Arc::new(arc_swap::ArcSwap::from_pointee(commands))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovered_mode_plugin_registers_its_minor_modes_and_gated_keymap() {
    let Some(wasm) = modes_guest_wasm() else {
        eprintln!("skipping: modes-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "modes-fixture", "modes", &wasm);

    let mode_registry = empty_mode_registry();
    let command_registry = command_registry_with_builtins();
    let keymap = KeymapHandle::new();
    let sink: Arc<RecordingSink> = Arc::new(RecordingSink::default());

    let loader = PluginLoader::with_services(
        temp_host(base.path()),
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            command_registry: Some(command_registry.clone()),
            mode_registry: Some(mode_registry.clone()),
            keymap: Some(keymap.clone()),
            meta_sink: Some(sink.clone() as Arc<dyn PluginMetaSink>),
            decoration_registry: Some(std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(
                lattice_mode::GutterDecorationSourceRegistry::default(),
            ))),
            context_registry: Some(std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(
                lattice_mode::ContextSourceRegistry::new(),
            ))),
            ..Default::default()
        },
    );

    assert_eq!(
        discover(&plugins_dir).len(),
        1,
        "discovery finds the plugin"
    );

    let n = loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(n, 1, "the mode plugin loads");
    assert!(loader.is_loaded("modes-fixture"), "loader tracks it loaded");

    // The two well-formed minor modes are now live in the published registry;
    // the mis-suffixed `not-suffixed` was rejected by the `-mode` gate.
    let modes = mode_registry.load();
    assert!(
        modes.is_registered(ModeId::new("git-blame-mode")),
        "git-blame-mode registered into the published registry"
    );
    assert!(
        modes.is_registered(ModeId::new("lsp-lens-mode")),
        "lsp-lens-mode registered"
    );
    assert!(
        !modes.is_registered(ModeId::new("not-suffixed")),
        "the `-mode` suffix gate rejected the mis-named declaration"
    );

    // The mode's `<C-s>` binding landed in its OWN gated MinorMode layer: it
    // resolves only when git-blame-mode is active, not globally.
    let chord = lattice_protocol::parse_chord_sequence("<C-s>").expect("chord parses");
    let blame = ModeId::new("git-blame-mode");
    assert!(
        matches!(
            keymap.lookup_with_context(BindingMode::Normal, &chord, &[blame]),
            LookupResult::Bound { .. }
        ),
        "the plugin mode's keymap binding resolves when its mode is active"
    );
    assert!(
        matches!(
            keymap.lookup_with_context(BindingMode::Normal, &chord, &[]),
            LookupResult::Unbound
        ),
        "the gated binding does not fire when the mode is inactive"
    );

    // Provenance recorded — `:list-plugins` would show it.
    let recorded = sink.registered.lock().unwrap();
    assert_eq!(recorded.len(), 1, "one plugin's provenance recorded");
    assert_eq!(recorded[0].1, "modes-fixture", "under its manifest id");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mode_plugin_without_a_wired_mode_registry_is_skipped_not_fatal() {
    let Some(wasm) = modes_guest_wasm() else {
        eprintln!("skipping: modes-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "modes-fixture", "modes", &wasm);

    // A loader wired with the keymap + command registry but NO mode registry —
    // the modes drain hits `PluginLoaderError::NotWired("modes")`, which
    // `discover_and_load` logs + skips (graceful degradation: a missing service
    // degrades to "mode plugin skipped", never a boot abort or panic).
    let loader = PluginLoader::with_services(
        temp_host(base.path()),
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            command_registry: Some(command_registry_with_builtins()),
            keymap: Some(KeymapHandle::new()),
            mode_registry: None,
            ..Default::default()
        },
    );

    let n = loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(n, 0, "the mode plugin is skipped, not loaded");
    assert!(!loader.is_loaded("modes-fixture"), "nothing recorded");
}
