//! AP.0.2 end-to-end: a grammar plugin action that returns `Effect::Declined`
//! makes the dispatcher RE-RESOLVE the chord as if the action's mode layer
//! weren't there — falling through to the builtin binding.
//!
//! The `multiseam-guest` fixture binds `x` (Normal) in `multiseam-mode` to a
//! `multiseam-declines` action. With the mode active, dispatching `x` fires the
//! plugin action, which declines; the dispatcher then re-resolves `x` without the
//! minor layer → the builtin `x` (delete char) runs. This is the primitive the
//! manual close key / backspace need (config-and-init.md is unrelated; see the
//! auto-pair fragment §5.2). Skips when the fixture wasn't built.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_config::ConfigRegistry;
use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_mode::ModeId;
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader};

fn multiseam_guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/multiseam-guest/target/wasm32-wasip2/release/multiseam_guest.wasm"
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
    let host =
        Arc::new(PluginHost::with_dirs(base.join("cache"), base.join("data")).expect("host builds"));
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

/// Bring the buffer's minors live (publish MajorEntered + drain the resolver).
fn activate_modes(editor: &mut Editor) {
    let _ = editor.drain_minor_activation();
    let proto = lattice_protocol::ids::BufferId::new(editor.document_buffer_id.0 as u64);
    editor
        .event_bus
        .publish(lattice_protocol::Event::MajorEntered {
            buffer: proto,
            major: "text-mode".into(),
        });
    let _ = editor.drain_minor_activation();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_declining_plugin_action_falls_through_to_the_builtin() {
    let Some(wasm) = multiseam_guest_wasm() else {
        eprintln!("skipping: multiseam-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(
        &plugins_dir,
        "multiseam",
        "\"grammar\", \"modes\", \"config\"",
        &wasm,
    );

    let mut editor = Editor::boot(CoreDocument::from_text("abc\n"));
    editor.cursor = lattice_protocol::position::Position::new(0, 0);

    let loaded = loader_over_editor(&editor, base.path())
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(loaded, 1, "the multiseam plugin loads");

    // Sanity: `x` in Normal binds to the plugin's declining action in the mode's
    // layer (the mode owns it), but NOT globally.
    let x = lattice_protocol::parse_chord_sequence("x").expect("chord parses");
    let mode = ModeId::new("multiseam-mode");
    assert!(
        matches!(
            editor.keymap.lookup_with_context(
                lattice_keymap::BindingMode::Normal,
                &x,
                &[mode.clone()]
            ),
            lattice_keymap::LookupResult::Bound { .. }
        ),
        "`x` binds to the plugin action when the mode is active"
    );

    // Enable + activate the mode (available-but-off, CI.3).
    {
        let mut next = (**editor.mode_registry.load()).clone();
        next.set_minor_enabled(mode.clone(), true);
        editor.mode_registry.store(std::sync::Arc::new(next));
    }
    activate_modes(&mut editor);

    // Dispatch `x`. The plugin action fires and DECLINES → the dispatcher
    // re-resolves without the minor layer → the builtin `x` deletes the char.
    let x_chord = lattice_protocol::parse_chord_sequence("x")
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let mut partial = Vec::new();
    let _ = editor.dispatch_chord(x_chord, &mut partial);

    assert_eq!(
        editor.active_text().as_string(),
        "bc\n",
        "the declined chord fell through to the builtin `x`, deleting the first char"
    );
}
