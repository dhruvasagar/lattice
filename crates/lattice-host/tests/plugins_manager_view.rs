//! PL8.H.2 — the `:plugins` manager view wires end-to-end: the ex-command
//! self-registers at boot, and opening the `*plugins*` buffer activates
//! `plugins-mode` (the major mode that projects the status table in
//! `on_activate`). The rendered table itself is unit-tested in
//! `lattice-plugin-manager`'s `render` module; here we prove the host wiring —
//! that `Effect::OpenSyntheticBuffer { mode_id: "plugins-mode" }` lands on the
//! provider's registered mode with zero host-specific code.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_plugin_manager::PluginManagerMode;

#[test]
fn plugins_ex_command_registered_at_boot() {
    let editor = Editor::boot(CoreDocument::from_text("x\n"));
    assert!(
        editor.registry.load().id_by_name("plugins").is_some(),
        "the :plugins ex-command self-registers at boot (plain name resolves)"
    );
}

#[tokio::test]
async fn opening_the_manager_buffer_activates_plugins_mode() {
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));

    // The generic open the `:plugins` ex-command emits
    // (`Effect::OpenSyntheticBuffer`) resolves to this host method — no
    // provider-specific host code.
    editor.open_synthetic_buffer("*plugins*", "plugins-mode");

    let id = editor
        .buffers
        .by_name("*plugins*")
        .expect("*plugins* buffer exists after :plugins");
    let major = editor.active_modes.get(&id).and_then(|m| m.major());
    assert_eq!(
        major,
        Some(PluginManagerMode::mode_id()),
        "the manager buffer's major mode is the provider-registered plugins-mode"
    );
}
