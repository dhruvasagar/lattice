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
use lattice_keymap::{BindingMode, LookupResult};
use lattice_mode::ModeId;
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

/// PL8.H.3: the in-view chords bind to the mode's `action:plugins-*` handlers in
/// the `MajorMode(plugins-mode)` layer — proving BOTH that the mode's `keymap()`
/// was pushed at boot AND that its `cmd:` literals resolve to registered actions
/// (an unregistered command would never bind).
#[test]
fn plugins_mode_chords_override_in_its_layer() {
    let editor = Editor::boot(CoreDocument::from_text("x\n"));
    let registry = editor.registry.load();

    // The four action commands self-register at boot.
    for name in [
        "action:plugins-reload",
        "action:plugins-unload",
        "action:plugins-describe",
        "action:plugins-refresh",
    ] {
        assert!(
            registry.id_by_name(name).is_some(),
            "`{name}` registered at boot"
        );
    }

    // `x` is the vim delete-char builtin; with plugins-mode active its
    // MajorMode layer overrides it to `action:plugins-unload`.
    let x = lattice_protocol::parse_chord_sequence("x").unwrap();
    let plugins_mode = ModeId::new("plugins-mode");
    let LookupResult::Bound { command, .. } =
        editor
            .keymap
            .lookup_with_context(BindingMode::Normal, &x, &[plugins_mode.clone()])
    else {
        panic!("`x` should be bound in the plugins-mode layer");
    };
    assert_eq!(
        command.command.command,
        registry.id_by_name("action:plugins-unload").unwrap(),
        "`x` in plugins-mode unloads the plugin under the cursor, not delete-char"
    );

    // Without the mode active, `x` is NOT the plugin-unload override (it falls
    // through to the builtin) — the gate holds.
    let global = editor
        .keymap
        .lookup_with_context(BindingMode::Normal, &x, &[]);
    let unload_id = registry.id_by_name("action:plugins-unload").unwrap();
    let overridden_globally = matches!(
        global,
        LookupResult::Bound { ref command, .. } if command.command.command == unload_id
    );
    assert!(
        !overridden_globally,
        "the plugins-mode `x` override must be gated to the mode, not global"
    );
}
