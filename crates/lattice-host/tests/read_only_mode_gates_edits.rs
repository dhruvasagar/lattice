//! PD.4 — `read-only-mode` refuses edits through the modal path.
//!
//! `lattice_config::ReadOnly` and its enforcement (`read_only_edit_rejected`)
//! both predate this mode by a long way. What did not exist was a way to turn
//! the option on for a buffer that is unwritable because of *what it is
//! showing* rather than because of its kind — a project-diff over the index is
//! the same kind, the same major and the same view as one over the working
//! tree, and only one of them has a file to propagate an edit into.
//!
//! ## Why these tests drive chords rather than call the gate
//!
//! `read_only_edit_rejected` returning `true` proves nothing a user cares
//! about. The claim is that **a keystroke does not change the text**, and every
//! step between the keystroke and that gate — keymap resolution, insert-mode
//! self-insert, the operator path, `apply_edit_blocking` — is a step where the
//! refusal could be lost. A test asserting on the option instead of on the text
//! would pass against a mode that contributed nothing at all.
//!
//! Every "must not edit" assertion is therefore preceded by a **control** that
//! makes the same keystroke change the same buffer with the mode off. Without
//! it, an assertion that the text is unchanged also passes when the key was
//! never wired to that buffer in the first place — which is the failure mode
//! that makes a read-only test worthless.
//!
//! ## Why two buffer kinds
//!
//! The multibuffer is the kind PD.4 needs this for, and it is the kind whose
//! edits travel furthest before landing (view row → excerpt → source document),
//! so the insert path is exercised there. Operators are exercised on a plain
//! `Document` as well, because the two reach the gate by different routes:
//! insert-mode self-insert goes through `apply_edit_blocking`, while an operator
//! is stopped earlier, by the mode's invocation runner.
//!
//! `operators_edit_a_multibuffer_view` was ignored when this file was written —
//! operator edits were dropped in every multibuffer view, for reasons that had
//! nothing to do with this mode (grammar dispatch ran against a scratch rope and
//! nothing wrote back). Fixed since; the test stays as the control that keeps
//! the read-only assertions from passing against an operator that does nothing
//! anywhere.

#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::sync::Arc;

use lattice_core::{BufferFlags, BufferId, Document as CoreDocument};
use lattice_host::chord::KeyChord;
use lattice_host::editor::Editor;
use lattice_mode::ModeActivator;
use lattice_mode::modes::ReadOnlyMode;
use lattice_multibuffer::{Excerpt, create_multibuffer_view};
use lattice_runtime::spawn_document;

/// Boot an editor over a one-source multibuffer view and make it active.
fn boot_with_view() -> (Editor, BufferId) {
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    let cmd_registry: lattice_grammar::CommandRegistryHandle = editor.registry.clone();
    let source = spawn_document(
        BufferId(201),
        CoreDocument::from_text("alpha\nbravo\ncharlie\ndelta\n"),
        cmd_registry,
    );
    let mut sources: HashMap<BufferId, Arc<dyn lattice_runtime::Document>> = HashMap::new();
    sources.insert(
        BufferId(201),
        Arc::new(source) as Arc<dyn lattice_runtime::Document>,
    );

    let registry_for_view = editor.registry.clone();
    let view = create_multibuffer_view(
        &mut editor,
        sources,
        vec![Excerpt::new(BufferId(201), 0, 4)],
        Some("*test:read-only*".into()),
        BufferFlags::default(),
        registry_for_view,
        None,
        lattice_multibuffer::FoldGrouping::SourceFile,
    );
    editor.activate_document(view);
    (editor, view)
}

/// Boot an editor over an ordinary `Document`, returning its buffer id.
fn boot_plain() -> (Editor, BufferId) {
    let editor = Editor::boot(CoreDocument::from_text("alpha\nbravo\ncharlie\n"));
    let id = editor.document_buffer_id;
    (editor, id)
}

fn press(editor: &mut Editor, chords: &[KeyChord]) {
    let mut partial = Vec::new();
    for c in chords {
        let _ = editor.dispatch_chord(c.clone(), &mut partial);
    }
}

/// Type `Z` at the cursor and return to Normal. Insert-mode self-insert is the
/// edit path that works in every buffer kind, so it is the one the multibuffer
/// tests below drive.
fn insert_a_char(editor: &mut Editor) {
    press(
        editor,
        &[
            KeyChord::char('i'),
            KeyChord::char('Z'),
            KeyChord::special(lattice_protocol::chord::SpecialKey::Esc),
        ],
    );
}

fn body(editor: &Editor) -> String {
    editor.document.snapshot().buffer.as_string()
}

#[test]
fn typing_edits_a_multibuffer_until_the_mode_is_active() {
    let (mut editor, view) = boot_with_view();
    let before = body(&editor);

    insert_a_char(&mut editor);
    assert_ne!(
        body(&editor),
        before,
        "typing must edit a writable multibuffer, or this test proves nothing"
    );

    let after_control = body(&editor);
    editor.activate_minor_by_id(view, ReadOnlyMode::mode_id());
    insert_a_char(&mut editor);
    assert_eq!(
        body(&editor),
        after_control,
        "typing must not edit once read-only-mode is active"
    );
}

/// Operators travel a different path than insert-mode self-insert — through the
/// grammar dispatcher and a motion — so the gate has to hold on both. Driven on
/// a plain `Document` because that is where operators reach the gate at all.
#[test]
fn x_does_not_delete_a_character_under_the_mode() {
    let (mut editor, buffer) = boot_plain();
    let before = body(&editor);

    press(&mut editor, &[KeyChord::char('x')]);
    assert_ne!(
        body(&editor),
        before,
        "`x` must edit a writable document, or this test proves nothing"
    );

    let after_control = body(&editor);
    editor.activate_minor_by_id(buffer, ReadOnlyMode::mode_id());
    press(&mut editor, &[KeyChord::char('x')]);
    assert_eq!(
        body(&editor),
        after_control,
        "`x` must not edit once read-only-mode is active"
    );
}

/// The symmetric half, and the one PD.4 actually needs: a provider view is
/// reused across triggers, so a buffer that was read-only for one comparison
/// has to become writable again for the next.
#[test]
fn deactivating_makes_the_buffer_writable_again() {
    let (mut editor, view) = boot_with_view();
    editor.activate_minor_by_id(view, ReadOnlyMode::mode_id());
    let while_locked = body(&editor);
    insert_a_char(&mut editor);
    assert_eq!(body(&editor), while_locked);

    editor.deactivate_minor_by_id(view, ReadOnlyMode::mode_id());
    insert_a_char(&mut editor);
    assert_ne!(
        body(&editor),
        while_locked,
        "deactivating must restore editing, or a reused view stays stuck"
    );
}

/// Deactivating a mode that was never active is a no-op rather than a
/// disturbance — the provider calls it unconditionally on every editable
/// trigger, including the first.
#[test]
fn deactivating_when_never_active_is_a_no_op() {
    let (mut editor, view) = boot_with_view();
    let before = body(&editor);
    editor.deactivate_minor_by_id(view, ReadOnlyMode::mode_id());
    insert_a_char(&mut editor);
    assert_ne!(body(&editor), before, "the buffer was writable throughout");
}

// ─────────────────────────────────────────────────────────────
//  Pinned defects — found while writing the tests above
// ─────────────────────────────────────────────────────────────

/// Operator edits never land in a multibuffer view, read-only or not.
///
/// `MultibufferDocumentHandle::dispatch_composed` runs the grammar against a
/// **scratch** `Document` built from the composed snapshot, so the returned
/// `Effect::Edits` describes edits applied to a throwaway rope. The host's
/// `Effect::Edits` arm is `handle_edits`, which records the cursor and publishes
/// the deltas on the premise — true for a real `Document` actor, false here —
/// that "the document actor has already applied them". Nothing writes back, so
/// `x` / `dd` / `cw` are silently inert in every multibuffer view (search
/// results, references, project-diff) while insert-mode typing works.
///
/// K.4.11's comment on `dispatch_composed` asserts the opposite ("operators
/// return `Effect::Edits` … that the host's `apply_edit_blocking` routes through
/// this handle's `apply_edit`"); that route does not exist in the code.
#[test]
fn operators_edit_a_multibuffer_view() {
    let (mut editor, _view) = boot_with_view();
    let before = body(&editor);
    press(&mut editor, &[KeyChord::char('x')]);
    assert_ne!(body(&editor), before, "`x` must delete a character");
}

/// PD.4's actual scenario, testable only since operators started landing
/// in multibuffers: a project diff over the index is a multibuffer, and
/// `x` in it must be refused. Until the K.4.11 fix this could not be
/// asserted — `x` did nothing in a multibuffer whether the mode was
/// active or not, so a passing test would have proved nothing.
#[test]
fn x_does_not_delete_in_a_read_only_multibuffer() {
    let (mut editor, view) = boot_with_view();

    press(&mut editor, &[KeyChord::char('x')]);
    let after_control = body(&editor);

    editor.activate_minor_by_id(view, ReadOnlyMode::mode_id());
    press(&mut editor, &[KeyChord::char('x')]);
    assert_eq!(
        body(&editor),
        after_control,
        "`x` must not edit a read-only multibuffer"
    );
}

/// The refusal is explained, not merely enforced. A buffer that silently
/// ignores keystrokes reads as a hang.
///
/// This is the second thing the invocation runner buys. `read_only_edit_rejected`
/// returns `RuntimeError::ReadOnly` to a caller that drops it, and the only
/// "buffer is read-only" echo in the host is the 5.5.D guard in `handle_action`,
/// which fires on `BufferKind::Help | BufferKind::Dashboard` — a kind-branch, not
/// the `ReadOnly` option. `run_read_only_motion` is where the message comes from
/// for every other buffer.
#[test]
fn the_refusal_reaches_the_echo_area() {
    let (mut editor, buffer) = boot_plain();
    editor.activate_minor_by_id(buffer, ReadOnlyMode::mode_id());
    press(&mut editor, &[KeyChord::char('x')]);
    let message = editor
        .last_message
        .as_ref()
        .map(|m| m.text.to_lowercase())
        .unwrap_or_default();
    assert!(
        message.contains("read-only") || message.contains("readonly"),
        "the user must be told why the keystroke did nothing; echo was {message:?}"
    );
}
