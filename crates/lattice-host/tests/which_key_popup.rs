//! WK.7 — which-key's lifecycle, asserted the way it fails.
//!
//! The failure mode CLAUDE.md names outright is an async result that
//! reaches the screen only on the NEXT keystroke. A which-key popup with
//! that bug is indistinguishable from a working one in any test that
//! presses a second key — so **no assertion here dispatches another
//! action after the one under test**. The popup must arrive because the
//! idle gate fired and the actor drained, not because the user pressed
//! something.
//!
//! Design: `docs/dev/architecture/which-key.md` §5, §11.

#![allow(clippy::unwrap_used)]

use std::time::Duration;

use lattice_core::Document as CoreDocument;
use lattice_host::action::Action;
use lattice_host::chord::KeyChord;
use lattice_host::editor::Editor;

/// A booted editor with a pane wide enough for a grid.
fn booted() -> Editor {
    let mut editor = Editor::boot(CoreDocument::from_text("hello\nworld\n"));
    let id = editor.document_buffer_id;
    let _ = editor.activate_major_for_buffer_kind(id, lattice_core::BufferKind::Document);
    editor.pane_tree.leaves_mut()[0].viewport_width = 100;
    editor.viewport_height = 40;
    editor
}

/// Drain wakes accumulated during boot so a later wait measures only the
/// gate under test.
async fn quiesce(editor: &Editor) {
    while tokio::time::timeout(Duration::from_millis(50), editor.async_landed.notified())
        .await
        .is_ok()
    {}
}

/// Let the subscription task forward the published event into the
/// inbound bus, then run the per-tick drain the actor would run. This is
/// the arming half — NOT the popup, which only the gate can produce.
async fn settle_arming(editor: &mut Editor) {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    let _ = editor.run_tick_pending();
}

/// Advance past the delay and fire the gate exactly as the actor's
/// `select!` arm does.
async fn fire_gate(editor: &mut Editor) {
    tokio::time::sleep(Duration::from_millis(400)).await;
    let _ = editor.fire_idle_gates();
}

fn popup_text(editor: &Editor) -> Option<String> {
    let id = editor.popup_buffer?;
    Some(
        editor
            .buffers
            .document_handle(id)?
            .snapshot()
            .buffer
            .as_string(),
    )
}

/// The headline: press a prefix, wait, and the popup is there — with no
/// further keystroke.
#[tokio::test]
async fn holding_a_prefix_opens_the_popup_without_another_keystroke() {
    let mut editor = booted();
    quiesce(&editor).await;

    let _ = editor.dispatch(Action::AbsorbPartialChord(KeyChord::char('g')));
    settle_arming(&mut editor).await;
    assert!(
        editor.popup_buffer.is_none(),
        "the popup must not appear before the delay elapses — that would be \
         a stutter, not a hint"
    );
    assert!(
        editor.idle_gate_deadline().is_some(),
        "a pending prefix arms the gate"
    );

    fire_gate(&mut editor).await;

    let text = popup_text(&editor).expect("the popup opened off the gate, with no keypress");
    assert!(
        text.contains('g'),
        "the popup names the prefix it is describing: {text:?}"
    );
    assert!(
        text.lines().count() > 1,
        "…and lists continuations under it: {text:?}"
    );
}

/// Finishing the chord inside the delay window shows nothing at all. The
/// user who knows their chord never sees a frame of popup.
#[tokio::test]
async fn a_chord_completed_before_the_delay_never_shows_a_popup() {
    let mut editor = booted();
    quiesce(&editor).await;

    let _ = editor.dispatch(Action::AbsorbPartialChord(KeyChord::char('g')));
    settle_arming(&mut editor).await;
    assert!(editor.idle_gate_deadline().is_some(), "armed");

    // The chord resolves: any non-absorbing action clears `partial_chord`,
    // which republishes with an empty list.
    let _ = editor.dispatch(Action::ScrollLineDown);
    settle_arming(&mut editor).await;

    assert!(
        editor.idle_gate_deadline().is_none(),
        "resolving disarms the gate — otherwise a popup would appear for a \
         chord the user already finished"
    );
    fire_gate(&mut editor).await;
    assert!(
        editor.popup_buffer.is_none(),
        "and nothing opens even after the delay passes"
    );
}

/// The popup is PASSIVE: the document keeps focus, so every keystroke
/// still resolves against the trie. This is the property that makes the
/// feature safe to ship — a hint that stole keys would change what
/// chords mean, per-prefix and unpredictably.
#[tokio::test]
async fn the_popup_is_passive_and_leaves_the_document_focused() {
    let mut editor = booted();
    quiesce(&editor).await;
    let doc_before = editor.document_buffer_id;

    let _ = editor.dispatch(Action::AbsorbPartialChord(KeyChord::char('g')));
    settle_arming(&mut editor).await;
    fire_gate(&mut editor).await;
    assert!(editor.popup_buffer.is_some(), "popup open");

    assert_eq!(
        editor.document_buffer_id, doc_before,
        "the document is still the focused editing surface"
    );
    assert!(
        !editor.popup_focused,
        "State A: focus did not move into the hint"
    );
    assert!(
        matches!(editor.modal, lattice_grammar::ModalState::Normal),
        "and the modal state is untouched"
    );
}

/// `which-key.enabled = false` never arms. The option is the off switch,
/// not a filter applied after the work is done.
#[tokio::test]
async fn disabling_the_option_never_arms_the_gate() {
    let mut editor = booted();
    let _ = editor.do_set("which-key.enabled=false");
    quiesce(&editor).await;

    let _ = editor.dispatch(Action::AbsorbPartialChord(KeyChord::char('g')));
    settle_arming(&mut editor).await;

    assert!(
        editor.idle_gate_deadline().is_none(),
        "disabled means no timer at all"
    );
    fire_gate(&mut editor).await;
    assert!(editor.popup_buffer.is_none());
}

/// An unbound prefix has no continuations, so there is nothing to show —
/// and an empty box is worse than no box.
#[tokio::test]
async fn a_prefix_with_no_continuations_opens_nothing() {
    let mut editor = booted();
    quiesce(&editor).await;

    // `<M-k>` is bound nowhere, so the composite has no node for it.
    let _ = editor.dispatch(Action::AbsorbPartialChord(KeyChord::new(
        lattice_protocol::KeyKind::Char('k'),
        lattice_protocol::KeyMods::ALT,
    )));
    settle_arming(&mut editor).await;
    fire_gate(&mut editor).await;

    assert!(
        editor.popup_buffer.is_none(),
        "no continuations ⇒ no popup, rather than an empty grid"
    );
}

/// WK.9: the keys are emphasised, and the emphasis reaches the buffer's
/// highlights rather than stopping at the producer.
///
/// The failure this pins is a silent one: the spans are stored through a
/// service, and asking the registry for the wrong `T` (the `…Handle`
/// alias rather than the bare type) compiles, returns `None`, and leaves
/// the popup permanently unstyled. Only reading the buffer's own
/// `ExtraHighlights` proves the chain ran end to end.
#[tokio::test]
async fn the_keys_in_the_popup_are_highlighted() {
    let mut editor = booted();
    quiesce(&editor).await;

    let _ = editor.dispatch(Action::AbsorbPartialChord(KeyChord::char('g')));
    settle_arming(&mut editor).await;
    fire_gate(&mut editor).await;
    let popup = editor.popup_buffer.expect("popup open");

    // The drain that moves stored spans into the buffer local runs on the
    // tick, exactly as it does for magit's buffers.
    let _ = editor.run_tick_pending();

    let highlights = editor
        .buffer_locals
        .get(&popup)
        .and_then(|l| l.get::<lattice_host::modes::ExtraHighlights>())
        .map(|h| h.0.clone())
        .expect("the popup buffer carries extra highlights");

    let key_spans: Vec<_> = highlights
        .iter()
        .flatten()
        .filter(|s| s.style == lattice_syntax::Style::HelpKey)
        .collect();
    assert!(
        !key_spans.is_empty(),
        "every key in the grid is emphasised — without this the popup is a \
         wall of undifferentiated text: {highlights:?}"
    );

    // And the spans point at real keys, not at padding: slice the rendered
    // text back with them.
    let text = editor
        .buffers
        .document_handle(popup)
        .expect("popup buffer live")
        .snapshot()
        .buffer
        .as_string();
    let lines: Vec<&str> = text.lines().collect();
    for (row, spans) in highlights.iter().enumerate() {
        let Some(line) = lines.get(row) else { continue };
        for s in spans {
            let slice = &line[s.start..s.end];
            assert!(
                !slice.trim().is_empty(),
                "a span must cover a key, not whitespace: {slice:?} in {line:?}"
            );
        }
    }
}
