//! A focused popup's keys resolve against the POPUP, not the buffer behind it.
//!
//! ## The report
//!
//! On the org agenda, `:describe-buffer` opens a help popup. Pressing `/` in it
//! — meaning "search this popup" — fired `org-agenda-filter-by-tag` on the
//! agenda underneath, and the filter prompt that opened then owned the keys, so
//! the popup could no longer be dismissed at all.
//!
//! ## The mechanism, and why it hid
//!
//! Mode-gated keymap layers (K.1.c) only fire on buffers where their mode is
//! active, and the gate is a list of mode ids computed per keystroke. There are
//! two places that list is built, and they did not agree:
//!
//! - `Editor::dispatch_chord` builds it from [`Editor::active_buffer_id`],
//!   which resolves a focused popup to `popup_buffer` and a focused
//!   file-tree / oil / terminal pane to the pane's own buffer. Correct.
//! - `publish_render_state` built it from `document_buffer_id` — the pane's
//!   *document*, which a popup does not replace. Wrong for every focused
//!   surface that is not a plain Document.
//!
//! The **live TUI key path reads the second one** (`runtime.rs`'s
//! `translator.active_minor_modes`), so the correct site was the one that never
//! ran in use. That is why this survived: every host test drives
//! `dispatch_chord` directly and takes the right branch, and the wrong branch
//! is reachable only through a published render state.
//!
//! So these tests assert on the PUBLISHED state rather than on
//! `dispatch_chord`, because the published state is the thing that was broken.
//! A test that pressed a key through the host API would pass on the bug.

#![allow(clippy::unwrap_used)]

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_mode::ModeId;

/// A minor mode id that is registered at boot and can be activated on an
/// ordinary document buffer, standing in for `org-agenda-mode` — which is a
/// plugin mode and would drag the whole loader in for a fact about the host.
fn a_minor_on_the_document(editor: &mut Editor) -> ModeId {
    let id = ModeId::new("table-mode");
    assert!(
        editor.mode_registry.load().is_registered(id),
        "the stand-in mode must exist at boot, or this test proves nothing"
    );
    let doc = editor.document_buffer_id;
    let _ = editor.activate_mode_by_id(doc, id);
    assert!(
        gated_modes_for(editor, doc).contains(&id),
        "the mode must actually be active on the document, or the assertions \
         below are vacuous"
    );
    id
}

/// The mode ids the K.1.c gate would use for `buffer`.
fn gated_modes_for(editor: &Editor, buffer: lattice_core::BufferId) -> Vec<ModeId> {
    editor
        .active_modes
        .get(&buffer)
        .map(|m| m.keymap_gated_ids())
        .unwrap_or_default()
}

/// The mode ids the LIVE key path uses — read back off the published render
/// state, which is what `runtime.rs` hands the translator.
fn published_gate(editor: &mut Editor) -> Vec<ModeId> {
    editor.publish_render_state();
    editor
        .render_state
        .load()
        .translator
        .active_minor_modes
        .to_vec()
}

fn open_describe_buffer_popup(editor: &mut Editor) {
    let content = editor.build_describe_buffer_content();
    let _ = editor.display_buffer(
        content,
        lattice_core::ui::display::BufferDisplayCategory::HelpDescribe,
    );
    assert!(
        editor.popup_focused,
        "`:describe-buffer` opens a focused (Steal) popup by default — if that \
         default changed, this test is measuring the wrong thing"
    );
}

/// The report, reduced: with a popup focused, the buffer behind it must not be
/// the one the keymap is gated on.
#[test]
fn a_focused_popup_does_not_leak_the_underlying_buffers_modes() {
    let mut editor = Editor::boot(CoreDocument::from_text("| a | b |\n"));
    let leaked = a_minor_on_the_document(&mut editor);

    open_describe_buffer_popup(&mut editor);

    let gate = published_gate(&mut editor);
    assert!(
        !gate.contains(&leaked),
        "a mode on the buffer BEHIND the popup must not gate the popup's keys \
         — this is `/` firing `org-agenda-filter-by-tag` inside \
         `:describe-buffer`. Gate was {gate:?}"
    );
}

/// …and the popup's own modes MUST be there, or the fix would be "gate on
/// nothing", which breaks help's own chords instead.
#[test]
fn a_focused_popup_gates_on_its_own_modes() {
    let mut editor = Editor::boot(CoreDocument::from_text("| a | b |\n"));
    a_minor_on_the_document(&mut editor);
    open_describe_buffer_popup(&mut editor);

    let popup = editor.popup_buffer.expect("the popup registered a buffer");
    let expected = gated_modes_for(&editor, popup);
    assert!(
        !expected.is_empty(),
        "the popup buffer has a major mode of its own; if it did not, the \
         gate below would pass for the wrong reason"
    );
    assert_eq!(
        published_gate(&mut editor),
        expected,
        "the published gate must be the POPUP's mode set"
    );
}

/// The two builders have to agree. They are the same question asked twice —
/// `dispatch_chord` computes it one way and the published state another — and
/// the whole defect is that only one of them was right.
#[test]
fn the_published_gate_matches_the_dispatch_gate() {
    let mut editor = Editor::boot(CoreDocument::from_text("| a | b |\n"));
    a_minor_on_the_document(&mut editor);
    open_describe_buffer_popup(&mut editor);

    let dispatch_gate = gated_modes_for(&editor, editor.active_buffer_id());
    assert_eq!(
        published_gate(&mut editor),
        dispatch_gate,
        "the render-state gate and the dispatch gate must not disagree — a \
         chord that resolves differently depending on which path read it is \
         the bug this file pins"
    );
}

/// Dismissing puts it back. Without this the fix could be "always gate on the
/// popup", which would leave the document's own modes dead after one popup.
#[test]
fn dismissing_the_popup_restores_the_documents_modes() {
    let mut editor = Editor::boot(CoreDocument::from_text("| a | b |\n"));
    let mode = a_minor_on_the_document(&mut editor);
    open_describe_buffer_popup(&mut editor);
    assert!(!published_gate(&mut editor).contains(&mode));

    let _ = editor.dismiss_popup();
    assert!(
        published_gate(&mut editor).contains(&mode),
        "the document's own mode must gate its keys again once the popup is gone"
    );
}
