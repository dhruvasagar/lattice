//! LA.4 — the acceptance test for late-language activation: a file passed on
//! argv gets its plugin major's KEYMAP once the plugin loads, without
//! reopening it and without a keypress to prompt the work.
//!
//! This is the reported bug end to end. `lattice todo.org` opened with no org
//! anything — plugin discovery is spawned off the boot thread on purpose, so
//! the initial document resolves its major against a catalog that does not yet
//! contain org — while `:e todo.org` moments later worked. Every fix before
//! this plan asserted a PROXY (a syntax handle appears, a registry pointer
//! changed) and left the symptom the user actually hit, `<M-Down>` doing
//! nothing, in place. So this file asserts a **chord fires**.
//!
//! Fixture: `modes-guest`, which already declares `fixture-lang-mode` — a
//! MAJOR with `target_language = "fixturelang"` binding Normal `<C-y>` to
//! `ex:write`. That is precisely the org shape (a plugin contributing a
//! language contributes its major too, which is the only route a plugin
//! language has to one), so no new fixture is minted for this. Skips when the
//! fixture wasn't built (no `wasm32-wasip2` target), the convention every
//! guest-fixture test in this repo follows.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_config::ConfigRegistry;
use lattice_host::editor::Editor;
use lattice_mode::ModeId;
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader};

/// A provenance nothing else in this binary claims. The plugin-language
/// registry is process-global; integration tests each get their own process,
/// but two tests in THIS file would collide.
const PROV: u64 = 0x1A4_0000;

fn modes_guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/modes-guest/target/wasm32-wasip2/release/modes_guest.wasm"
    );
    std::fs::read(path).ok()
}

fn write_plugin_dir(root: &std::path::Path, id: &str, provides: &str, wasm: &[u8]) {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        format!("id = \"{id}\"\nprovides = [{provides}]\n"),
    )
    .unwrap();
    std::fs::write(dir.join("component.wasm"), wasm).unwrap();
}

fn loader_over_editor(editor: &Editor, base: &std::path::Path) -> PluginLoader {
    let host = Arc::new(
        PluginHost::with_dirs(base.join("cache"), base.join("data")).expect("host builds"),
    );
    PluginLoader::with_services(
        host,
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(editor.event_bus.clone()),
            command_registry: Some(editor.registry.clone()),
            mode_registry: Some(editor.mode_registry.clone()),
            keymap: Some(editor.keymap.clone()),
            config_registry: Some(Arc::new(ConfigRegistry::default())),
            ..Default::default()
        },
    )
}

/// Dispatch a chord through the real editor path. A name-dispatch test would
/// pass against a mode-scoping bug, a missing binding, or a dead prefix alike
/// — the `plugin_insert_mode_chords.rs` precedent, and the reason this whole
/// slice exists.
fn press(editor: &mut Editor, keys: &str) {
    let expanded = editor.keymap.expand_leader(keys);
    let seq = lattice_protocol::parse_chord_sequence(&expanded).expect("parses");
    let mut partial = Vec::new();
    for c in seq {
        let _ = editor.dispatch_chord(c, &mut partial);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_on_argv_gets_its_plugin_majors_keymap_once_the_plugin_loads() {
    let Some(wasm) = modes_guest_wasm() else {
        eprintln!("skipping: modes-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };
    lattice_plugin_loader::disable_autoload();

    let base = tempfile::tempdir().unwrap();
    // A real on-disk path, because the chord under test is `ex:write` and the
    // file appearing is what proves it ran. Asserting on an echo would pass if
    // some *other* command happened to echo.
    let file = base.path().join("argv.fixturelangext");
    std::fs::write(&file, "one\ntwo\n").unwrap();

    let mut editor = Editor::boot(lattice_core::Document::open(&file).unwrap());
    let id = editor.document_buffer_id;
    let _ = editor.activate_major_for_buffer_kind(id, lattice_core::BufferKind::Document);

    // Boot's world: nothing claims this extension, so the buffer is on the
    // fallback major and the plugin's chord cannot possibly be bound. This is
    // the state `lattice todo.org` was stuck in forever.
    assert_eq!(
        editor.active_modes.get(&id).and_then(|m| m.major()),
        Some(lattice_mode::TextMode::mode_id()),
        "sanity: the buffer boots on the fallback major"
    );
    // Delete the file so the assertion at the end is about THIS write, not
    // about the file we seeded the document from.
    std::fs::remove_file(&file).unwrap();

    // ── the plugin loads, off the boot thread as it does in production ────
    //
    // The language identity first (`modes-guest` provides `modes` only, so it
    // has no `language` seam of its own to register `fixturelang` from), then
    // the plugin — whose load is what publishes `LanguagesRegistered`.
    lattice_syntax::plugin_lang::register("fixturelang", &["fixturelangext"], PROV)
        .expect("the fixture language registers");

    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "modes-fixture", "\"modes\"", &wasm);
    let loaded = loader_over_editor(&editor, base.path())
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(loaded, 1, "the fixture plugin loads");

    // ── ONE tick, no keypress ────────────────────────────────────────────
    //
    // `run_tick_pending` is what the actor's `async_landed` arm runs
    // off-keystroke. Pressing a key first would pass on the broken version
    // too, which is the trap `test_helpers::settle` was added for.
    editor.run_tick_pending();

    assert_eq!(
        editor.active_modes.get(&id).and_then(|m| m.major()),
        Some(ModeId::new("fixture-lang-mode")),
        "the argv buffer must land on the plugin's major once the plugin \
         registers it"
    );

    // ── and the chord the user actually reaches for ──────────────────────
    press(&mut editor, "<C-y>");
    assert!(
        file.exists(),
        "`<C-y>` is bound by `fixture-lang-mode` to `ex:write`; the file it \
         writes is the proof the mode's KEYMAP came with the mode. This is the \
         assertion every earlier fix substituted a proxy for."
    );

    lattice_syntax::plugin_lang::unregister_plugin(PROV);
}
