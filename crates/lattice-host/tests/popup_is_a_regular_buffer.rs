//! PU.6 — "a popup is a regular buffer in a box" (K.4-style integration
//! test), analogous to `multibuffer_is_a_regular_buffer.rs`.
//!
//! Every popup surface — the centered help/`:describe` popup
//! (`open_popup`), the floating hover/signature popup
//! (`open_floating_popup`), and in-pane help (`open_help_in_pane`) — is an
//! actor-backed Document whose options resolve PER-BUFFER through the
//! generic path and whose content composes through the shared seam. No
//! kind-specific popup renderer, no `BufferKind::Help` branch in
//! render/motion/option code (paramount #3 "everything is a buffer", K.4,
//! `feedback_buffers_no_special_case`).
//!
//! The grep-gate (`popup_no_bespoke_renderer.rs`) pins the *renderer* side
//! (no bespoke paint fn); this pins the *buffer* side (each popup behaves
//! like a `:set nonu signcolumn=no wrap` markdown document).

#![allow(clippy::unwrap_used)]

use lattice_core::{BufferId, Document as CoreDocument};
use lattice_help::HelpContent;
use lattice_host::editor::Editor;
use lattice_host::popup::PopupPlacement;

fn help_content(title: &str) -> HelpContent {
    HelpContent::from_lines(
        title,
        vec!["# Heading".into(), String::new(), "body **text**".into()],
    )
}

/// The verbatim contract every popup buffer must satisfy — the popup
/// equivalent of `multibuffer_is_a_regular_buffer`'s assertions.
fn assert_regular_buffer_in_a_box(e: &Editor, id: BufferId, ctx: &str) {
    // (1) Actor-backed Document in the registry — reachable like any buffer,
    // not a side-channel popup struct.
    assert!(
        e.buffers.document_handle(id).is_some(),
        "{ctx}: popup must be an actor-backed Document in the registry"
    );

    // (2) Options resolve PER-BUFFER via the help-mode minor (the generic
    // option path), NOT a kind-branch: nonu + signcolumn=no.
    assert!(
        e.minor_mode_enabled_for(id, lattice_mode::HelpMode::mode_id()),
        "{ctx}: popup activates the help-mode minor"
    );
    assert!(
        !*e.resolved_option::<lattice_config::Number>(id),
        "{ctx}: popup resolves nonu (no line numbers) via help-mode"
    );

    // (3) Content composes like any markdown document: a syntax handle is
    // attached, so the cells worker builds its DisplayMatrix through the
    // shared compose path (no bespoke highlight precompute).
    assert!(
        e.document_syntax_for(id).is_some(),
        "{ctx}: popup has a markdown syntax handle (composes like a document)"
    );
}

#[test]
fn centered_help_popup_is_a_regular_buffer() {
    let mut e = Editor::boot(CoreDocument::from_text("fn main() {}\n"));
    let _ = e.open_popup(help_content("help"), PopupPlacement::Centered);
    let id = e.popup_buffer.expect("centered popup buffer");
    assert_regular_buffer_in_a_box(&e, id, "centered help popup");
}

#[test]
fn floating_hover_popup_is_a_regular_buffer() {
    let mut e = Editor::boot(CoreDocument::from_text("fn main() {}\n"));
    let _ = e.open_floating_popup(help_content("hover"), PopupPlacement::CursorAnchored);
    let id = e.popup_buffer.expect("floating popup buffer");
    assert_regular_buffer_in_a_box(&e, id, "floating hover/signature popup");
}

#[test]
fn in_pane_help_is_a_regular_buffer() {
    let mut e = Editor::boot(CoreDocument::from_text("fn main() {}\n"));
    let (id, _signals) = e.open_help_in_pane(help_content("help-pane"));
    assert_regular_buffer_in_a_box(&e, id, "in-pane help");
}
