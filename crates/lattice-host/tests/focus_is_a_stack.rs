//! FS.1 — focus nests, so the thing that records it is a stack.
//!
//! Design: `docs/dev/architecture/focused-surface.md` §2. Slice plan:
//! `docs/dev/operations/slice-plans/focused-surface.md`.
//!
//! `minibuffer_focus` was a single `Option` written behind an `is_none()`
//! guard. That is correct while only one surface can hold focus — the `:`
//! line, the `/` line, a prompt — and false the moment one can open inside
//! another. A popup holds focus; `/` inside it takes focus again; the second
//! push recorded nothing and one restore returned to the FILE, skipping the
//! popup entirely.
//!
//! The single-frame behaviour is what every existing minibuffer test already
//! covers, so this file asserts the part that had no coverage: two frames,
//! popped in order, landing where each was entered.

#![allow(clippy::unwrap_used)]

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;

fn editor_with_two_buffers() -> (Editor, lattice_core::BufferId, lattice_core::BufferId) {
    let mut editor = Editor::boot(CoreDocument::from_text("alpha\nbeta\ngamma\n"));
    let file = editor.document_buffer_id;
    // A second registry buffer to stand in for the inner surface. Any
    // buffer will do — the stack is about frames, not about kinds.
    let inner = editor.ensure_named_synthetic_document(
        "*inner*",
        lattice_host::search_line_mode::SearchLineMode::mode_id(),
        lattice_core::BufferFlags {
            listed: false,
            hidden: false,
            ephemeral: true,
        },
    );
    (editor, file, inner)
}

/// Two frames, popped in order. The `Option` could not express this: the
/// second focus recorded nothing, so the first restore jumped to the file.
#[test]
fn focus_nests_and_unwinds_one_frame_at_a_time() {
    let (mut editor, file, inner) = editor_with_two_buffers();
    editor.cursor.line = 2;

    // Frame 1: something takes focus from the file.
    editor.focus_editing_buffer(inner);
    assert_eq!(editor.focus_stack.len(), 1);
    assert_eq!(editor.document_buffer_id, inner);

    // Frame 2: a second surface takes focus from THAT.
    let search = editor.ensure_named_synthetic_document(
        "*search-line*",
        lattice_host::search_line_mode::SearchLineMode::mode_id(),
        lattice_core::BufferFlags {
            listed: false,
            hidden: false,
            ephemeral: true,
        },
    );
    editor.focus_editing_buffer(search);
    assert_eq!(
        editor.focus_stack.len(),
        2,
        "the second push must record a frame — as an `Option` behind an \
         `is_none()` guard it recorded nothing, which is the whole defect"
    );

    // Unwind: the inner surface first…
    editor.restore_editing_buffer();
    assert_eq!(
        editor.document_buffer_id, inner,
        "one restore lands on the surface that was focused when the second \
         one opened — NOT past it to the file"
    );

    // …then the file, with the cursor it never lost.
    editor.restore_editing_buffer();
    assert_eq!(editor.document_buffer_id, file);
    assert_eq!(
        editor.cursor.line, 2,
        "the file's own cursor survives both frames"
    );
    assert!(editor.focus_stack.is_empty());
}

/// `focused_surface()` answers about the INNERMOST frame — "is anything
/// focused" and "what is focused" have to be one question, or they drift.
#[test]
fn the_accessor_reads_the_innermost_frame() {
    let (mut editor, file, inner) = editor_with_two_buffers();
    assert!(
        editor.focused_surface().is_none(),
        "the pane's own buffer is not a focused surface"
    );

    editor.focus_editing_buffer(inner);
    assert_eq!(
        editor.focused_surface().map(|f| f.prior_buffer_id),
        Some(file),
        "frame 1 remembers the file it took focus from"
    );

    let third = editor.ensure_named_synthetic_document(
        "*third*",
        lattice_host::search_line_mode::SearchLineMode::mode_id(),
        lattice_core::BufferFlags {
            listed: false,
            hidden: false,
            ephemeral: true,
        },
    );
    editor.focus_editing_buffer(third);
    assert_eq!(
        editor.focused_surface().map(|f| f.prior_buffer_id),
        Some(inner),
        "frame 2 remembers the SURFACE it took focus from, not the file"
    );
}

/// Popping an empty stack is a no-op. `restore_editing_buffer` is called
/// from several cancel paths that cannot all prove a frame exists, and a
/// panic there would take the editor down on a stray `<Esc>`.
#[test]
fn restoring_with_nothing_focused_is_a_no_op() {
    let (mut editor, file, _) = editor_with_two_buffers();
    editor.cursor.line = 1;
    editor.restore_editing_buffer();
    assert_eq!(editor.document_buffer_id, file);
    assert_eq!(editor.cursor.line, 1, "and it changes nothing");
}

/// The single-frame path is unchanged — the case every existing minibuffer
/// test exercises. Asserted here too so a future stack refactor cannot
/// quietly break the common case while the nesting tests still pass.
#[test]
fn one_frame_behaves_exactly_as_before() {
    let (mut editor, file, inner) = editor_with_two_buffers();
    editor.cursor.line = 2;
    editor.scroll = 1;

    editor.focus_editing_buffer(inner);
    assert_eq!(editor.cursor.line, 0, "a focused surface starts at its top");

    editor.restore_editing_buffer();
    assert_eq!(editor.document_buffer_id, file);
    assert_eq!(editor.cursor.line, 2);
    assert_eq!(editor.scroll, 1, "…and the scroll it was left at");
}
