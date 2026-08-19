//! YR.5 — the keys that reach the ring.
//!
//! `<C-r>` is vim's insert-register and is free in Insert mode; Normal's
//! `<C-r>` is redo and stays that way. `<C-r><C-r>` opens the yank picker over
//! whichever surface you were in.
//!
//! The two bindings share a prefix, which is the shape SU.3e spent a slice on
//! — a shorter path shadowing a longer one. It does not happen here, for two
//! independent reasons: the trie tries an exact child before the char
//! wildcard, and a modifier-bearing chord never matches the wildcard at all.
//! `both_c_r_paths_resolve` pins that rather than trusting it, because the
//! failure would be silent in exactly one direction.

#![allow(clippy::unwrap_used)]

use lattice_core::Document as CoreDocument;
use lattice_grammar::Register;
use lattice_grammar::effect::YankKind;
use lattice_host::chord::KeyChord;
use lattice_host::editor::Editor;

fn boot() -> Editor {
    Editor::boot(CoreDocument::from_text("hello\n"))
}

fn body(editor: &Editor) -> String {
    editor.document.snapshot().buffer.as_string()
}

fn press(editor: &mut Editor, chords: &[KeyChord]) {
    let mut partial = Vec::new();
    for c in chords {
        let _ = editor.dispatch_chord(c.clone(), &mut partial);
    }
}

/// Enter Insert mode, where `<C-r>` means insert-register.
fn insert_mode(editor: &mut Editor) {
    press(editor, &[KeyChord::char('i')]);
}

#[test]
fn c_r_then_a_register_char_inserts_its_contents() {
    let mut editor = boot();
    editor.store_yank(
        Register::Named('a'),
        "REG-A".to_string(),
        YankKind::Charwise,
        true,
    );

    insert_mode(&mut editor);
    press(&mut editor, &[KeyChord::ctrl('r'), KeyChord::char('a')]);

    assert_eq!(body(&editor), "REG-Ahello\n");
}

/// An empty register says so rather than inserting nothing. The user typed
/// two deliberate keys; silence is indistinguishable from a dead binding.
#[test]
fn an_empty_register_echoes_rather_than_doing_nothing() {
    let mut editor = boot();
    let before = body(&editor);
    insert_mode(&mut editor);
    press(&mut editor, &[KeyChord::ctrl('r'), KeyChord::char('z')]);

    assert_eq!(body(&editor), before);
    let msg = editor
        .last_message
        .as_ref()
        .map(|m| m.text.clone())
        .unwrap_or_default();
    assert!(msg.contains("empty"), "expected an echo; got {msg:?}");
}

#[test]
fn c_r_c_r_opens_the_yank_picker() {
    let mut editor = boot();
    editor.store_yank(
        Register::Unnamed,
        "ringed".to_string(),
        YankKind::Charwise,
        true,
    );

    insert_mode(&mut editor);
    press(&mut editor, &[KeyChord::ctrl('r'), KeyChord::ctrl('r')]);

    assert!(editor.picker.is_some(), "the picker should be open");
}

/// The prefix-shadowing guard. `<C-r>a` and `<C-r><C-r>` must reach
/// *different* actions — if the wildcard swallowed the second `<C-r>`, the
/// picker would silently become "insert register named ^R", which is empty,
/// so the symptom would be a keybinding that echoes "register is empty".
#[test]
fn both_c_r_paths_resolve() {
    let mut editor = boot();
    editor.store_yank(
        Register::Named('a'),
        "REG-A".to_string(),
        YankKind::Charwise,
        true,
    );

    // Wildcard path.
    insert_mode(&mut editor);
    press(&mut editor, &[KeyChord::ctrl('r'), KeyChord::char('a')]);
    assert_eq!(body(&editor), "REG-Ahello\n", "the wildcard path works");
    assert!(
        editor.picker.is_none(),
        "...and did not open the picker instead"
    );

    // Literal path, from a fresh editor so the two cannot interfere.
    let mut editor2 = boot();
    editor2.store_yank(Register::Unnamed, "x".to_string(), YankKind::Charwise, true);
    let before = body(&editor2);
    insert_mode(&mut editor2);
    press(&mut editor2, &[KeyChord::ctrl('r'), KeyChord::ctrl('r')]);
    assert!(
        editor2.picker.is_some(),
        "the literal path opens the picker"
    );
    assert_eq!(body(&editor2), before, "...and inserted nothing on the way");
}

/// Opening the picker records where its result should go — at open, while
/// the surface that asked is still the focused one.
#[test]
fn opening_the_picker_captures_the_surface_that_asked() {
    let mut editor = boot();
    editor.store_yank(Register::Unnamed, "x".to_string(), YankKind::Charwise, true);
    insert_mode(&mut editor);
    press(&mut editor, &[KeyChord::ctrl('r'), KeyChord::ctrl('r')]);

    assert_eq!(
        editor.picker_fill_target,
        Some(lattice_picker::FillTarget::Document),
        "a picker opened from a document fills the document"
    );
}

/// Normal-mode `<C-r>` is redo and must stay so: rebinding it would be a
/// silent vim-semantics regression, which is the kind nobody reports as a bug
/// because they assume they misremembered.
#[test]
fn normal_mode_c_r_is_still_redo() {
    let mut editor = boot();
    press(&mut editor, &[KeyChord::char('x')]);
    let after_delete = body(&editor);
    assert_ne!(after_delete, "hello\n", "precondition: `x` deleted");

    press(&mut editor, &[KeyChord::char('u')]);
    assert_eq!(body(&editor), "hello\n", "undo restored it");

    press(&mut editor, &[KeyChord::ctrl('r')]);
    assert_eq!(
        body(&editor),
        after_delete,
        "Normal `<C-r>` must still redo, not open a picker"
    );
    assert!(editor.picker.is_none());
}
