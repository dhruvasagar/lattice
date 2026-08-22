//! MG.29 — `<Esc>` in a submenu goes back to its parent, not out of the chain.
//!
//! The logic shipped with MG.29 and was unit-tested at the `Picker` level.
//! Nothing ever emitted `Action::TransientDismiss`, so the key that was
//! supposed to drive it (`<Esc>`) kept closing the whole chain — only `<BS>`
//! popped. The unit tests passed throughout, because they called
//! `transient_unwind()` directly.
//!
//! So these go through the KEY, per the standing rule: test with `press`, not
//! with the handler. A test that calls `transient_unwind()` proves nothing
//! about what `<Esc>` does.

#![allow(clippy::unwrap_used)]

use lattice_core::Document as CoreDocument;
use lattice_host::action::Action;
use lattice_host::editor::Editor;
use lattice_picker::{
    Picker, PickerAction, PickerSource, TransientGroup, TransientItem, TransientItemKind,
    TransientSpec,
};
use lattice_protocol::chord::KeyChord;
use std::sync::Arc;

fn spec(title: &str) -> Arc<TransientSpec> {
    Arc::new(TransientSpec {
        title: title.to_string(),
        groups: vec![TransientGroup {
            label: String::new(),
            items: vec![TransientItem {
                key: vec!["x".to_string()],
                label: "noop".to_string(),
                description: String::new(),
                kind: TransientItemKind::Flag {
                    name: "f".to_string(),
                    default: false,
                },
            }],
        }],
        preview: None,
        footer: None,
    })
}

/// A picker showing `child`, opened from `root`.
fn nested() -> Editor {
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    let mut picker = Picker::new("t", PickerSource::Buffers, PickerAction::OpenFile);
    picker.transient = Some(spec("child"));
    picker
        .transient_stack
        .push((spec("root"), Default::default(), 0));
    editor.picker = Some(picker);
    editor
}

/// Drive the REAL path: chord -> `dispatch_chord` -> `input::translate` ->
/// the dispatch arm. Calling `transient_unwind()` directly is what let this
/// bug live behind green unit tests.
fn press(editor: &mut Editor, chord: KeyChord) -> Action {
    let mut partial: Vec<KeyChord> = Vec::new();
    editor.dispatch_chord(chord, &mut partial)
}

fn esc(editor: &mut Editor) {
    let _ = press(
        editor,
        KeyChord::special(lattice_protocol::chord::SpecialKey::Esc),
    );
}

fn showing(editor: &Editor) -> Option<String> {
    editor
        .picker
        .as_ref()
        .and_then(|p| p.transient.as_ref())
        .map(|s| s.title.clone())
}

#[test]
fn esc_in_a_submenu_returns_to_its_parent() {
    let mut editor = nested();
    assert_eq!(showing(&editor).as_deref(), Some("child"));

    esc(&mut editor);
    assert_eq!(
        showing(&editor).as_deref(),
        Some("root"),
        "`<Esc>` pops one level instead of closing the chain"
    );
    assert!(editor.picker.is_some(), "the menu is still up");
}

#[test]
fn esc_at_the_root_closes_the_menu() {
    let mut editor = nested();
    esc(&mut editor); // child -> root
    esc(&mut editor); // root -> closed
    assert!(
        editor.picker.is_none(),
        "with nothing left to unwind, `<Esc>` means what it always did"
    );
}

/// The regression risk of routing `<Esc>` to `TransientDismiss`: a picker
/// with no transient must still close on the first press.
#[test]
fn esc_still_dismisses_a_plain_picker() {
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    editor.picker = Some(Picker::new(
        "files",
        PickerSource::Buffers,
        PickerAction::OpenFile,
    ));

    esc(&mut editor);
    assert!(
        editor.picker.is_none(),
        "a plain picker has no parent to go back to, so `<Esc>` closes it"
    );
}

/// `<C-c>` stays the hard exit, so a deep chain is still one keypress from
/// gone — otherwise unwinding would make leaving a four-level menu worse
/// than it was.
#[test]
fn ctrl_c_still_closes_the_whole_chain() {
    let mut editor = nested();
    let _ = press(&mut editor, KeyChord::ctrl('c'));

    assert!(
        editor.picker.is_none(),
        "`<C-c>` leaves the whole chain in one press"
    );
}
