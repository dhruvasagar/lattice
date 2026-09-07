//! LA.1: a plugin load that changes the mode/language catalog publishes
//! exactly ONE `LanguagesRegistered`, and a load that cannot change it
//! publishes none.
//!
//! The count is the whole test. "Once per registered language" is the
//! plausible wrong answer, and it is the one that breaks the subscriber
//! (LA.2): a re-resolution that runs while the catalog is half-installed
//! resolves against a language whose major mode has not registered yet, gets
//! the fallback, and — because the buffer is no longer on the fallback after
//! that — never revisits it. The bug would look exactly like the one this plan
//! is closing.
//!
//! The `language-guest` fixture is deliberately the subject: it declares FOUR
//! languages of which three are rejected, so a per-language publish would be
//! visible as 1 or 4 rather than a clean 1-vs-N.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_config::ConfigRegistry;
use lattice_grammar::{CommandRegistry, CommandRegistryHandle};
use lattice_keymap::KeymapHandle;
use lattice_mode::{ModeRegistry, ModeRegistryHandle};
use lattice_picker::PickerRegistryHandle;
use lattice_picker::source::PickerRegistry;
use lattice_plugin_host::TrustTier;
use lattice_plugin_loader::{LanguagesRegistered, LoaderServices, PluginLoader};
use lattice_runtime::EventBus;

fn language_guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/language-guest/target/wasm32-wasip2/release/language_guest.wasm"
    );
    std::fs::read(path).ok()
}

fn help_guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/help-guest/target/wasm32-wasip2/release/help_guest.wasm"
    );
    std::fs::read(path).ok()
}

fn write_plugin_dir(root: &std::path::Path, id: &str, provides: &[&str], wasm: &[u8]) {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let list = provides
        .iter()
        .map(|p| format!("\"{p}\""))
        .collect::<Vec<_>>()
        .join(", ");
    std::fs::write(
        dir.join("plugin.toml"),
        format!("id = \"{id}\"\nprovides = [{list}]\n"),
    )
    .unwrap();
    std::fs::write(dir.join("component.wasm"), wasm).unwrap();
}

/// The language fixture registers FIXED language names into a process-global
/// registry, so concurrent loads collide — the same serialisation
/// `language_drain.rs` documents.
fn fixture_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn loader(base: &std::path::Path, bus: Arc<EventBus>) -> PluginLoader {
    let commands: CommandRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new()));
    let pickers: PickerRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(PickerRegistry::new()));
    let modes: ModeRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(ModeRegistry::default()));
    let host = Arc::new(
        lattice_plugin_host::PluginHost::with_dirs(base.join("cache"), base.join("data"))
            .expect("host builds"),
    );
    PluginLoader::with_services(
        host,
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(bus),
            picker_registry: Some(pickers),
            command_registry: Some(commands),
            mode_registry: Some(modes),
            config_registry: Some(Arc::new(ConfigRegistry::default())),
            keymap: Some(KeymapHandle::new()),
            help_topics: Some(lattice_help::topics::builtin_topics().into_handle()),
            ..Default::default()
        },
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_language_plugin_load_publishes_exactly_one_languages_registered() {
    let Some(wasm) = language_guest_wasm() else {
        eprintln!("skipping: language-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    let _guard = fixture_lock().lock().await;
    write_plugin_dir(&plugins_dir, "language-guest", &["language"], &wasm);

    let bus = Arc::new(EventBus::new());
    // Subscribe BEFORE the load — the publish is synchronous inside it.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<LanguagesRegistered>();
    bus.subscribe_typed::<LanguagesRegistered>(tx);

    let loader = loader(base.path(), bus.clone());
    assert_eq!(
        loader
            .discover_and_load(&plugins_dir, TrustTier::Bundled)
            .await,
        1,
        "the language plugin loads"
    );

    let mut seen = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        seen.push(ev);
    }
    assert_eq!(
        seen.len(),
        1,
        "exactly one catalog-changed event per load — the fixture declares four \
         languages, so a per-language publish shows up here as 4, and a publish \
         inside the drain loop would arrive before the catalog was complete: {seen:?}"
    );

    let _ = loader.unload("language-guest");
}

/// A plugin that cannot have changed the catalog stays silent, so the
/// subscriber's O(major-modes × open buffers) re-resolution is not paid for
/// every auto-pair-shaped plugin in the user's config.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plugin_declaring_no_languages_or_modes_publishes_nothing() {
    let Some(wasm) = help_guest_wasm() else {
        eprintln!("skipping: help-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "help-only-fixture", &["help"], &wasm);

    let bus = Arc::new(EventBus::new());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<LanguagesRegistered>();
    bus.subscribe_typed::<LanguagesRegistered>(tx);

    let loader = loader(base.path(), bus.clone());
    assert_eq!(
        loader
            .discover_and_load(&plugins_dir, TrustTier::Bundled)
            .await,
        1,
        "the help-only plugin loads"
    );

    assert!(
        rx.try_recv().is_err(),
        "a help-only plugin changes no language or major mode and must not \
         trigger a re-resolution"
    );

    let _ = loader.unload("help-only-fixture");
}
