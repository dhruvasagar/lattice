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

// ── MU.1: undo reaches the source ───────────────────────────────────────

/// **`u` in a multibuffer must undo the FILE, not just the picture of it.**
///
/// This is the bug the test exists for, and its shape is what made it
/// survivable: M.11 dropped the source fan-out to fix a deadlock, leaving
/// undo operating on the composed doc alone. The view rolled back, so on
/// screen `u` looked like it worked — while the source kept the change and a
/// later `:w` persisted it. A visual undo over a real edit, with nothing
/// saying so.
///
/// So the assertion is on `source_text`, deliberately. Asserting the composed
/// view would have passed on the broken version.
#[test]
fn undo_in_the_view_rolls_the_source_back_too() {
    let (mut editor, view) = boot_with_an_agenda_shaped_view();
    let before = source_text(&editor, view);

    // Edit through the VIEW (composed coordinates), which is what the user
    // does when they type in an agenda row — not the targeted-source path the
    // rest of this file exercises.
    editor.activate_buffer(view);
    editor
        .apply_edit_blocking(lattice_protocol::edit::Edit {
            range: lattice_protocol::position::Range::new(
                lattice_protocol::position::Position::new(0, 2),
                lattice_protocol::position::Position::new(0, 6),
            ),
            kind: lattice_protocol::edit::EditKind::Replace {
                text: "DONE".to_string(),
            },
        })
        .expect("the edit applies to the view");
    settle(&editor);
    assert!(
        source_text(&editor, view).starts_with("* DONE"),
        "precondition: the edit reached the source"
    );

    editor.undo_blocking().expect("undo");
    settle(&editor);

    assert_eq!(
        source_text(&editor, view),
        before,
        "`u` must roll the SOURCE back, not only the composed view"
    );
}

/// Redo is symmetric, and the asymmetry would be the nastier half of the same
/// bug: `u` then `<C-r>` leaving the file holding the undone text.
#[test]
fn redo_reapplies_to_the_source() {
    let (mut editor, view) = boot_with_an_agenda_shaped_view();
    editor.activate_buffer(view);
    editor
        .apply_edit_blocking(lattice_protocol::edit::Edit {
            range: lattice_protocol::position::Range::new(
                lattice_protocol::position::Position::new(0, 2),
                lattice_protocol::position::Position::new(0, 6),
            ),
            kind: lattice_protocol::edit::EditKind::Replace {
                text: "DONE".to_string(),
            },
        })
        .expect("the edit applies");
    settle(&editor);
    editor.undo_blocking().expect("undo");
    settle(&editor);
    // Assert the TROUGH before the peak. Without it this test passes on a
    // build where NEITHER undo nor redo forwards: the source would simply sit
    // at `DONE` throughout and the final assertion could not tell that apart
    // from redo working. It caught exactly that here.
    assert!(
        source_text(&editor, view).starts_with("* TODO"),
        "the undo must have reached the source first, got {:?}",
        source_text(&editor, view)
    );

    editor.redo_blocking().expect("redo");
    settle(&editor);
    assert!(
        source_text(&editor, view).starts_with("* DONE"),
        "redo must reach the source too, got {:?}",
        source_text(&editor, view)
    );
}

/// The forwarder is a FIFO and the queue is drained asynchronously, so a test
/// that read the source immediately would race it. `:w`'s flush barrier is
/// the production answer to the same problem; here a short spin is enough and
/// keeps the test off the save path it is not testing.
fn settle(editor: &Editor) {
    let _ = editor;
    std::thread::sleep(std::time::Duration::from_millis(50));
}

// ── HB.2b: the read half, through the gate a chord actually takes ──────────

/// **What a grammar action is told it is looking at, when the agenda is what
/// is on screen.**
///
/// A plugin action asks `excerpt-source(ctx.buffer-id, ctx.cursor.line)` to
/// find the file behind an agenda row. The answer is `none` for any id that
/// is not a view's, so the id the host puts in that context decides whether
/// the seam can work at all — and no test looked at it: every agenda test
/// goes through a mode's `ActionHandler`, which is a different arm with a
/// different context, and the multibuffer crate's resolver tests build the
/// context themselves.
///
/// A native `CommandKind::Action` is the guest's stand-in. It reaches the
/// same gate (`dispatch_invocation`'s Action branch) and gets the same
/// `ActionContext`, without a WASM component in the test.
#[test]
fn a_grammar_action_is_told_the_view_it_fired_in() {
    use std::sync::Mutex;

    let (mut editor, view) = boot_with_an_agenda_shaped_view();
    editor.activate_buffer(view);

    let seen: Arc<Mutex<Option<(BufferId, String)>>> = Arc::new(Mutex::new(None));
    let sink = Arc::clone(&seen);
    let mut id = None;
    editor.registry.rcu(|current| {
        let mut next = (**current).clone();
        let sink = Arc::clone(&sink);
        id = Some(next.register_action(
            "hb2b-probe",
            "HB.2b fixture: record the context the gate hands a grammar action.",
            lattice_grammar::ActionSpec {
                apply: Arc::new(move |ctx: &lattice_grammar::ActionContext| {
                    let line = ctx
                        .buffer
                        .line(ctx.cursor.line)
                        .unwrap_or_default()
                        .to_string();
                    *sink.lock().unwrap() = Some((ctx.buffer_id, line));
                    Ok(lattice_grammar::Effect::None)
                }),
                args_schema: Vec::new(),
            },
        ));
        Arc::new(next)
    });

    let mut out = lattice_host::dispatch::DispatchOutcome::default();
    editor.dispatch_invocation(
        lattice_grammar::CommandInvocation::of(id.expect("registered")),
        &mut out,
    );

    let (buffer_id, line) = seen.lock().unwrap().clone().expect("the action ran");
    assert_eq!(
        line.trim_end(),
        "* TODO write it",
        "precondition: the action reads the COMPOSED row it fired on"
    );
    assert_eq!(
        buffer_id, view,
        "the text comes from the view, so the id must name the view too — an id \
         that is not a view's makes `excerpt-source` answer none, and a plugin \
         acting on an agenda row falls back to editing the picture of the file"
    );
}
