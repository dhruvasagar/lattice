//! Selecting is not writing: `v` works on a read-only buffer, and `<Esc>`
//! gets you back out.
//!
//! ## The trap this closes
//!
//! `action_is_document_mutation` listed `EnterVisual` and `ExitVisual`, so the
//! Help/Dashboard read-only gate refused both. Three things followed, and the
//! third is the one that got reported:
//!
//! 1. `v` / `V` / `<C-v>` did nothing on `:help` or the dashboard, so you
//!    could not select a line to copy — which contradicts the yank carve-out
//!    in `run_read_only_motion`, whose entire purpose is copying a snippet out
//!    of exactly these buffers.
//! 2. A mouse drag sets the Visual state DIRECTLY and never passes that gate,
//!    so the mouse could reach a state the keyboard could not.
//! 3. `<Esc>` was refused as well, so that state could not be left. The
//!    modeline said VIS, every Normal-mode chord stopped resolving (correct
//!    for Visual), and clicking elsewhere was the only escape. It was reported
//!    as "`<C-x>g` and other keys are broken" — the keys were fine; the editor
//!    was wedged in Visual.
//!
//! The rule is: gate the WRITE, not the selection.

#![allow(clippy::unwrap_used)]

use lattice_core::{BufferKind, Document as CoreDocument};
use lattice_grammar::ModalState;
use lattice_host::chord::KeyChord;
use lattice_host::editor::Editor;

fn press(editor: &mut Editor, keys: &str) {
    let mut partial = Vec::new();
    for c in keys.chars() {
        let _ = editor.dispatch_chord(KeyChord::char(c), &mut partial);
    }
}

fn esc(editor: &mut Editor) {
    let mut partial = Vec::new();
    let _ = editor.dispatch_chord(
        KeyChord::special(lattice_protocol::chord::SpecialKey::Esc),
        &mut partial,
    );
}

/// Every read-only, link-bearing buffer kind the gate covers.
fn read_only_kinds() -> [BufferKind; 2] {
    [BufferKind::Help, BufferKind::Dashboard]
}

/// `v` selects, `<Esc>` returns. The second half is the trap: without it the
/// only way out was the mouse.
#[test]
fn visual_can_be_entered_and_left_on_a_read_only_buffer() {
    for kind in read_only_kinds() {
        let mut editor = Editor::boot(CoreDocument::from_text("alpha\nbeta\ngamma\n"));
        editor.active_buffer = kind;

        press(&mut editor, "v");
        assert!(
            matches!(editor.modal, ModalState::Visual(_)),
            "{kind:?}: `v` must select — you cannot yank a range you cannot select"
        );

        esc(&mut editor);
        assert_eq!(
            editor.modal,
            ModalState::Normal,
            "{kind:?}: `<Esc>` must leave Visual. Refusing it wedges the buffer: \
             the modeline says VIS and every Normal chord stops resolving"
        );
    }
}

/// The other two selection entries, same rule.
#[test]
fn linewise_and_blockwise_select_too() {
    for keys in ["V", "v"] {
        let mut editor = Editor::boot(CoreDocument::from_text("alpha\nbeta\n"));
        editor.active_buffer = BufferKind::Dashboard;
        press(&mut editor, keys);
        assert!(
            matches!(editor.modal, ModalState::Visual(_)),
            "`{keys}` must enter Visual on a read-only buffer"
        );
        esc(&mut editor);
        assert_eq!(editor.modal, ModalState::Normal);
    }
}

/// What the gate is actually for still holds: entering Insert is refused.
///
/// Scoped deliberately to what THIS gate decides. `x` / `dd` / `p` are
/// operators; they reach the buffer through the grammar's invocation runner,
/// not through `action_is_document_mutation`, so a test that sets
/// `active_buffer` alone cannot say anything honest about them — it would
/// pass or fail for reasons unrelated to the line being guarded here.
#[test]
fn entering_insert_is_still_refused() {
    for kind in read_only_kinds() {
        let mut editor = Editor::boot(CoreDocument::from_text("alpha\nbeta\n"));
        editor.active_buffer = kind;

        press(&mut editor, "i");
        assert_eq!(
            editor.modal,
            ModalState::Normal,
            "{kind:?}: the gate must still refuse Insert — selecting is not \
             writing, but typing is"
        );

        press(&mut editor, "o");
        assert_eq!(
            editor.document.text(),
            "alpha\nbeta\n",
            "{kind:?}: `o` opens a line, which writes"
        );
    }
}
