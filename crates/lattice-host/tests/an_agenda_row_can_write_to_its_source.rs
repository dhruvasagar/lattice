//! OA.23b — `Effect::ApplyEdit` can name a multibuffer SOURCE.
//!
//! Slice plan: `docs/dev/operations/slice-plans/org-agenda.md` phase 7.
//!
//! The agenda's `s` / `d` is the case. An agenda row is ONE line — the
//! headline — and a `SCHEDULED:` line goes BELOW it, outside every excerpt.
//! So there is no composed coordinate to write at, and the document that
//! holds the line is a multibuffer source: owned by the view, saved by the
//! view's `:w`, and absent from the buffer store because it is not a buffer
//! the user opened.
//!
//! Before this slice `apply_targeted_edit` looked in the store and nowhere
//! else, so such a target answered `Cancelled` and the edit vanished — a
//! keystroke that silently did nothing.
//!
//! The peer read (`source-line`) and the id that makes the target knowable
//! (`source-location.buffer`) are covered in `lattice-multibuffer`'s
//! `excerpt_source_tests`; this file is the write half, through a real
//! `Editor`.

#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::sync::Arc;

use lattice_core::{BufferFlags, BufferId, Document as CoreDocument};
use lattice_host::editor::Editor;
use lattice_multibuffer::{Excerpt, MultibufferRegistryHandle, create_multibuffer_view};
use lattice_runtime::{Document, spawn_document};

const SOURCE: BufferId = BufferId(211);

/// An editor holding a view shaped like an agenda: one source file, and an
/// excerpt covering ONLY its first line — the headline row.
fn boot_with_an_agenda_shaped_view() -> (Editor, BufferId) {
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    let cmd_registry: lattice_grammar::CommandRegistryHandle = editor.registry.clone();

    let source = spawn_document(
        SOURCE,
        CoreDocument::from_text("* TODO write it\n* TODO and it\n"),
        cmd_registry.clone(),
    );
    let mut sources: HashMap<BufferId, Arc<dyn lattice_runtime::Document>> = HashMap::new();
    sources.insert(
        SOURCE,
        Arc::new(source) as Arc<dyn lattice_runtime::Document>,
    );

    let view_id = create_multibuffer_view(
        &mut editor,
        sources,
        vec![Excerpt::new(SOURCE, 0, 0)],
        Some("*test:agenda*".into()),
        BufferFlags::default(),
        cmd_registry,
        None,
        lattice_multibuffer::FoldGrouping::SourceFile,
    );
    (editor, view_id)
}

fn source_text(editor: &Editor, view: BufferId) -> String {
    editor
        .services
        .get::<MultibufferRegistryHandle>()
        .and_then(|reg| reg.handle(view))
        .and_then(|v| v.source_text(SOURCE))
        .expect("the view owns the source")
}

#[test]
fn a_source_is_not_a_buffer_the_store_holds() {
    // The premise the rest of the file rests on. If this ever stops being
    // true the fallback below is dead code, not a fix — so it is asserted
    // rather than assumed.
    let (editor, _view) = boot_with_an_agenda_shaped_view();
    assert!(
        editor.buffers.document_handle(SOURCE).is_none(),
        "a multibuffer source must not be in the buffer store; if it is, \
         plain `Effect::ApplyEdit` already reached it and this seam is moot"
    );
}

#[test]
fn an_edit_targeting_a_source_lands_at_a_line_no_excerpt_composes() {
    let (mut editor, view) = boot_with_an_agenda_shaped_view();

    // Line 1 of the FILE — below the excerpted headline, invisible in the
    // view. This is exactly where a planning line goes.
    editor.apply_edit_effect_inline(
        SOURCE,
        lattice_protocol::edit::Edit::insert(
            lattice_protocol::position::Position::new(1, 0),
            "  SCHEDULED: <2026-09-03 Thu>\n",
        ),
        None,
    );

    assert_eq!(
        source_text(&editor, view),
        "* TODO write it\n  SCHEDULED: <2026-09-03 Thu>\n* TODO and it\n",
    );
}

#[test]
fn the_view_still_shows_only_its_excerpt() {
    // The UX contract's half of it: writing a planning line must not make
    // the agenda grow a row. The line is outside the excerpt, so the
    // composed text is unchanged — pixel-stable, nothing the user did not
    // ask for.
    let (mut editor, view) = boot_with_an_agenda_shaped_view();
    let before = editor
        .services
        .get::<MultibufferRegistryHandle>()
        .and_then(|reg| reg.handle(view))
        .map(|v| v.snapshot().buffer.as_string())
        .unwrap();

    editor.apply_edit_effect_inline(
        SOURCE,
        lattice_protocol::edit::Edit::insert(
            lattice_protocol::position::Position::new(1, 0),
            "  SCHEDULED: <2026-09-03 Thu>\n",
        ),
        None,
    );

    let after = editor
        .services
        .get::<MultibufferRegistryHandle>()
        .and_then(|reg| reg.handle(view))
        .map(|v| v.snapshot().buffer.as_string())
        .unwrap();
    assert_eq!(before, after, "the agenda must not grow a row");
}

#[test]
fn an_unknown_target_is_still_refused() {
    // The fallback widens what can be reached; it must not turn a bad id
    // into a write somewhere. A target that is neither a buffer nor any
    // view's source touches nothing.
    let (mut editor, view) = boot_with_an_agenda_shaped_view();
    let before = source_text(&editor, view);

    editor.apply_edit_effect_inline(
        BufferId(9_999),
        lattice_protocol::edit::Edit::insert(
            lattice_protocol::position::Position::new(0, 0),
            "clobber",
        ),
        None,
    );

    assert_eq!(source_text(&editor, view), before);
}
