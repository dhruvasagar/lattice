//! A chord whose TAIL is another map's prefix must not fire both.
//!
//! Reported against org's `C-c C-x C-b` (toggle-checkbox over a region or
//! subtree) while `emacs-keys-mode` binds `C-x C-b` → `:buffers`: the
//! checkbox toggled AND the buffer list opened. The double-fire did not
//! reproduce — this pins the property so a future dispatch change cannot
//! introduce it silently.
//!
//! The shape is general, not org's: emacs-keys owns a `<C-x>` prefix map,
//! and every `C-c C-x …` chord any mode contributes has that map's prefix
//! sitting inside it. If the dispatcher ever restarted a sequence on an
//! interior chord — or re-translated the tail after firing — every one of
//! those chords would fire twice.

#![allow(clippy::unwrap_used)]

use lattice_core::Document as CoreDocument;
use lattice_host::action::Action;
use lattice_host::chord::KeyChord;
use lattice_host::editor::Editor;

/// A buffer with `emacs-keys-mode` genuinely ACTIVE — not merely
/// registered. The layer is pushed at boot either way, but K.1.c's
/// per-keystroke filter folds it out unless the mode is active on this
/// buffer, so a version of this test that skipped the activation proved
/// nothing at all (it passed against a dispatcher that could not have
/// collided).
fn editor_with_emacs_keys() -> (Editor, lattice_mode::ModeId) {
    let mut editor = Editor::boot(CoreDocument::from_text("- [ ] one\n- [ ] two\n"));
    let id = editor.document_buffer_id;
    let _ = editor.activate_major_for_buffer_kind(id, lattice_core::BufferKind::Document);
    let _ = editor.do_set("emacs-keys");

    let proto = lattice_protocol::ids::BufferId::new(id.0 as u64);
    let mut active = editor.active_modes.remove(&id).unwrap_or_default();
    let caps = editor.capabilities_for_proto(proto);
    let _ = editor.mode_registry.load_full().activate_minor(
        &mut active,
        &editor.mode_guards,
        &editor.config,
        &editor.event_bus,
        &editor.services,
        proto,
        lattice_mode::EmacsKeysMode::mode_id(),
        caps,
    );
    editor.active_modes.insert(id, active);

    let modes = editor
        .active_modes
        .get(&id)
        .map(|m| m.keymap_gated_ids())
        .unwrap_or_default();
    assert!(
        modes.contains(&lattice_mode::EmacsKeysMode::mode_id()),
        "precondition: emacs-keys must be ACTIVE, or its <C-x> map is folded \
         out and this test cannot fail: {modes:?}"
    );
    let major = *modes.first().expect("a major is active");
    (editor, major)
}

/// Bind `<C-c><C-x><C-b>` on the buffer's active major, the way org's
/// plugin contributes it.
fn bind_three_chord(
    editor: &Editor,
    major: lattice_mode::ModeId,
) -> lattice_protocol::ids::CommandId {
    let cmd = editor
        .registry
        .load()
        .lookup_by_name("ex:messages")
        .map(|s| s.id)
        .expect("ex:messages is registered");
    editor.keymap.bind(
        lattice_keymap::KeymapLayer::MajorMode(major),
        lattice_keymap::BindingMode::Normal,
        &[
            lattice_keymap::ChordPattern::Literal(KeyChord::ctrl('c')),
            lattice_keymap::ChordPattern::Literal(KeyChord::ctrl('x')),
            lattice_keymap::ChordPattern::Literal(KeyChord::ctrl('b')),
        ],
        lattice_grammar::CommandInvocation::of(cmd),
        lattice_grammar::SourceLocation::synthetic("test:major-three-chord"),
    );
    cmd
}

#[test]
fn a_three_chord_binding_fires_once_and_not_its_tail() {
    let (mut editor, major) = editor_with_emacs_keys();
    let cmd = bind_three_chord(&editor, major);
    let buffers_cmd = editor
        .registry
        .load()
        .lookup_by_name("ex:buffers")
        .map(|s| s.id)
        .expect("ex:buffers is registered — emacs-keys binds <C-x><C-b> to it");
    assert_ne!(cmd, buffers_cmd);

    let mut partial = Vec::new();
    let mut invoked: Vec<lattice_protocol::ids::CommandId> = Vec::new();
    for c in [
        KeyChord::ctrl('c'),
        KeyChord::ctrl('x'),
        KeyChord::ctrl('b'),
    ] {
        let (action, _out) = editor.dispatch_chord_with_outcome(c, &mut partial);
        if let Action::Invoke(inv) = action {
            invoked.push(inv.command);
        }
    }

    assert_eq!(
        invoked,
        vec![cmd],
        "the three chords are ONE binding. `<C-x><C-b>` sits inside the \
         sequence but is not a sequence of its own here — firing it too \
         would run two commands for one chord"
    );
    assert!(
        !invoked.contains(&buffers_cmd),
        "emacs-keys' <C-x> map must not be re-entered by the tail of a \
         longer chord"
    );
    assert!(
        partial.is_empty(),
        "a resolved sequence leaves no partial chord behind"
    );
}

/// The other half: `<C-x><C-b>` pressed on its own STILL works. A fix for
/// the collision that suppressed the prefix map would trade one bug for a
/// worse one.
#[test]
fn the_prefix_map_still_fires_on_its_own() {
    let (mut editor, major) = editor_with_emacs_keys();
    let _ = bind_three_chord(&editor, major);
    let buffers_cmd = editor
        .registry
        .load()
        .lookup_by_name("ex:buffers")
        .map(|s| s.id)
        .expect("ex:buffers is registered");

    let mut partial = Vec::new();
    let mut invoked: Vec<lattice_protocol::ids::CommandId> = Vec::new();
    for c in [KeyChord::ctrl('x'), KeyChord::ctrl('b')] {
        let (action, _out) = editor.dispatch_chord_with_outcome(c, &mut partial);
        if let Action::Invoke(inv) = action {
            invoked.push(inv.command);
        }
    }

    assert_eq!(
        invoked,
        vec![buffers_cmd],
        "`<C-x><C-b>` on its own is still the buffer list"
    );
}
