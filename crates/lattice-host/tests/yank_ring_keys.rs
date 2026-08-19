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

// ── YR.5b: the yank picker over another picker ──
//
// With `:files` (or any picker) open, `<C-r>` opens the yank picker; the
// pick is appended to the ORIGINAL picker's query as a filter. Two things
// have to hold that are easy to get wrong: the picker underneath must
// survive being opened over, and abandoning the yank pick must return you
// to it rather than closing both.

fn open_yank_over(editor: &mut Editor, source: &str) {
    let _ = editor.open_picker(source.to_string(), Vec::new());
    press(editor, &[KeyChord::ctrl('r')]);
}

#[test]
fn c_r_in_a_picker_opens_the_yank_picker_over_it() {
    let mut editor = boot();
    editor.store_yank(
        Register::Unnamed,
        "needle".to_string(),
        YankKind::Charwise,
        true,
    );
    open_yank_over(&mut editor, "buffers");

    assert_eq!(
        editor.picker_fill_target,
        Some(lattice_picker::FillTarget::PickerQuery),
        "the pick should fill the picker it was opened from"
    );
    assert!(
        editor.stashed_picker.is_some(),
        "the picker underneath must be held, not overwritten"
    );
}

/// The flow end to end: pick from the ring, land in the original query.
#[test]
fn the_pick_becomes_the_original_pickers_filter() {
    let mut editor = boot();
    editor.store_yank(
        Register::Unnamed,
        "needle".to_string(),
        YankKind::Charwise,
        true,
    );
    open_yank_over(&mut editor, "buffers");

    // Accept the yank picker's first row.
    let _ = editor.do_picker_accept();

    let picker = editor.picker.as_ref().expect("the original picker is back");
    assert_eq!(picker.title, "buffers", "and it is the ORIGINAL one");
    assert!(
        picker.query.contains("needle"),
        "the pick should have been appended to its query; got {:?}",
        picker.query
    );
    assert!(editor.stashed_picker.is_none(), "stash consumed");
    assert!(editor.picker_fill_target.is_none(), "target consumed");
}

/// Abandoning the yank pick returns you to the list you were filtering.
/// Closing both would lose work the user never asked to abandon.
#[test]
fn dismissing_the_yank_picker_returns_to_the_one_underneath() {
    let mut editor = boot();
    editor.store_yank(
        Register::Unnamed,
        "needle".to_string(),
        YankKind::Charwise,
        true,
    );
    open_yank_over(&mut editor, "buffers");

    let _ = editor.do_picker_dismiss();

    let picker = editor.picker.as_ref().expect("the original picker is back");
    assert_eq!(picker.title, "buffers");
    assert!(
        !picker.query.contains("needle"),
        "an abandoned pick must not have been applied"
    );
    assert!(
        editor.picker_fill_target.is_none(),
        "nothing is waiting now"
    );
}

// ── An empty ring must not strand the caller ──
//
// `YankRingSource::init` errors when nothing has been yanked yet, and
// `open_picker` echoes that and leaves `self.picker` alone. But
// `do_open_yank_picker` has already taken the picker underneath into the
// stash by then — so the list the user was filtering disappears from the
// screen while still being held, with no key that brings it back. Opening a
// picker that fails to open must leave everything exactly as it was.

#[test]
fn an_empty_ring_leaves_the_picker_underneath_alone() {
    let mut editor = boot();
    assert!(editor.yank_ring.is_empty(), "precondition: nothing yanked");

    let _ = editor.open_picker("buffers".to_string(), Vec::new());
    press(&mut editor, &[KeyChord::ctrl('r')]);

    let picker = editor
        .picker
        .as_ref()
        .expect("the picker the user was in must still be there");
    assert_eq!(picker.title, "buffers");
    assert!(
        editor.stashed_picker.is_none(),
        "nothing should be held in the stash"
    );
    assert!(
        editor.picker_fill_target.is_none(),
        "and nothing should be waiting for a value"
    );
}

#[test]
fn an_empty_ring_from_a_document_leaves_no_dangling_target() {
    let mut editor = boot();
    assert!(editor.yank_ring.is_empty(), "precondition");

    insert_mode(&mut editor);
    press(&mut editor, &[KeyChord::ctrl('r'), KeyChord::ctrl('r')]);

    assert!(editor.picker.is_none(), "no picker opened");
    assert!(
        editor.picker_fill_target.is_none(),
        "a target left set here would be consumed by the next unrelated \
         FillCaller, landing text in a caller that never asked for it"
    );
    let msg = editor
        .last_message
        .as_ref()
        .map(|m| m.text.clone())
        .unwrap_or_default();
    assert!(
        msg.contains("yank"),
        "the user pressed two keys and is owed an answer; got {msg:?}"
    );
}

// ── The `:` line ──
//
// Command mode routes through `dispatch_insert`, so the Insert-layer
// bindings apply there without a second registration. That is a fact about
// the dispatcher rather than something this slice arranged, which is exactly
// why it is worth a test: nothing here would fail if that routing changed.

#[test]
fn c_r_then_a_register_inserts_into_the_command_line() {
    let mut editor = boot();
    editor.store_yank(
        Register::Named('a'),
        "REG-A".to_string(),
        YankKind::Charwise,
        true,
    );

    press(&mut editor, &[KeyChord::char(':')]);
    assert!(editor.command_line_active(), "precondition");
    press(&mut editor, &[KeyChord::ctrl('r'), KeyChord::char('a')]);

    assert_eq!(editor.command_line(), "REG-A");
}

#[test]
fn the_picker_opened_from_the_command_line_fills_the_command_line() {
    let mut editor = boot();
    editor.store_yank(
        Register::Unnamed,
        "ringed".to_string(),
        YankKind::Charwise,
        true,
    );

    press(&mut editor, &[KeyChord::char(':')]);
    press(&mut editor, &[KeyChord::ctrl('r'), KeyChord::ctrl('r')]);

    assert!(editor.picker.is_some(), "the picker opened");
    assert_eq!(
        editor.picker_fill_target,
        Some(lattice_picker::FillTarget::CommandLine),
        "the pick belongs to the `:` line it was opened from, not the document"
    );
}
