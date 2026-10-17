//! `*notifications*`'s chords resolve to the mode's actions.
//!
//! `notifications-mode` bound `<CR>` and `d` to `action:notification-run` /
//! `-dismiss` and declared `action:notification-refresh` for `gr`, but none of
//! the three names was ever registered as a command. The keymap install skips
//! a binding whose name does not resolve (with a boot warning), and the mode's
//! handlers were skipped for the same reason, so every chord in the buffer did
//! nothing. Asserted here the way it failed: by lookup, in the mode's layer.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_keymap::{BindingMode, LookupResult};
use lattice_mode::ModeId;

#[test]
fn notifications_chords_resolve_to_registered_actions() {
    let editor = Editor::boot(CoreDocument::from_text("x\n"));
    let registry = editor.registry.load();
    for name in [
        "action:notification-run",
        "action:notification-dismiss",
        "action:notification-refresh",
    ] {
        assert!(
            registry.id_by_name(name).is_some(),
            "`{name}` is registered, so the mode's chord and handler resolve"
        );
    }

    let mode = ModeId::new("notifications-mode");
    for (chord, action) in [
        ("<CR>", "action:notification-run"),
        ("d", "action:notification-dismiss"),
    ] {
        let keys = lattice_protocol::parse_chord_sequence(chord).unwrap();
        let LookupResult::Bound { command, .. } =
            editor
                .keymap
                .lookup_with_context(BindingMode::Normal, &keys, &[mode])
        else {
            panic!("`{chord}` is bound in notifications-mode");
        };
        assert_eq!(
            command.command.command,
            registry.id_by_name(action).unwrap(),
            "`{chord}` in notifications-mode runs `{action}`"
        );
    }
}
