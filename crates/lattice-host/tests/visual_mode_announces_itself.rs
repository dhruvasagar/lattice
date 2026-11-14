//! Entering Visual says so: `-- VISUAL --` in the echo area and a
//! `ModalModeChanged` on the bus, on the keystroke itself.
//!
//! ## The trap this closes
//!
//! `v` set `editor.modal` directly instead of going through the transition
//! that `i` takes, so it showed no `-- VISUAL --` and published no event.
//! A fresh charwise selection is one cell — the cell under the cursor — and
//! the block cursor covers it, so the modeline's lean `VIS` tag was the only
//! visible sign that Visual had started. Reported as "`v` sometimes didn't
//! start visual mode, but `c` then acted on a selection". `gv`, Select mode
//! and `<C-g>` had the same gap.
//!
//! Each case asserts after the key that changes the mode and before any
//! other: a test that pressed a motion first would pass on the broken
//! version too, since nothing about the motion is wrong.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document as CoreDocument;
use lattice_grammar::{ModalState, VisualKind};
use lattice_host::editor::Editor;
use lattice_protocol::event::{Event, EventKind};
use lattice_runtime::{EventFilter, SubscriptionTarget};
use tokio::sync::mpsc::UnboundedReceiver;

fn press(editor: &mut Editor, keys: &str) {
    let chords = lattice_protocol::parse_chord_sequence(keys)
        .unwrap_or_else(|e| panic!("key sequence {keys:?} does not parse: {e:?}"));
    let mut partial = Vec::new();
    for chord in chords {
        let _ = editor.dispatch_chord(chord, &mut partial);
    }
}

fn echo(editor: &Editor) -> Option<&str> {
    editor.last_message.as_ref().map(|m| m.text.as_str())
}

fn editor_with_mode_events(text: &str) -> (Editor, UnboundedReceiver<Event>) {
    let editor = Editor::boot(CoreDocument::from_text(text));
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    editor.event_bus.subscribe(
        EventFilter::kinds(vec![EventKind::ModalModeChanged]),
        SubscriptionTarget::Channel(tx),
    );
    (editor, rx)
}

/// The `to` of every mode change published since the last call.
fn mode_changes(rx: &mut UnboundedReceiver<Event>) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let Event::ModalModeChanged { to, .. } = event {
            out.push(to);
        }
    }
    out
}

#[test]
fn v_shows_visual_and_publishes_the_change() {
    let (mut editor, mut rx) = editor_with_mode_events("alpha beta\n");

    press(&mut editor, "v");
    assert_eq!(editor.modal, ModalState::Visual(VisualKind::Charwise));
    assert_eq!(echo(&editor), Some("-- VISUAL --"));
    assert_eq!(mode_changes(&mut rx), ["Visual(Charwise)"]);

    press(&mut editor, "<Esc>");
    assert_eq!(editor.modal, ModalState::Normal);
    assert_eq!(echo(&editor), None, "<Esc> clears the indicator");
    assert_eq!(mode_changes(&mut rx), ["Normal"]);
}

#[test]
fn linewise_and_blockwise_show_their_own_names() {
    let (mut editor, _rx) = editor_with_mode_events("alpha\nbeta\n");

    press(&mut editor, "V");
    assert_eq!(echo(&editor), Some("-- VISUAL LINE --"));
    press(&mut editor, "<Esc><C-v>");
    assert_eq!(echo(&editor), Some("-- VISUAL BLOCK --"));
}

#[test]
fn gv_reselects_with_the_indicator() {
    let (mut editor, mut rx) = editor_with_mode_events("alpha beta\n");

    press(&mut editor, "vl<Esc>");
    let _ = mode_changes(&mut rx);
    press(&mut editor, "gv");
    assert_eq!(editor.modal, ModalState::Visual(VisualKind::Charwise));
    assert_eq!(echo(&editor), Some("-- VISUAL --"));
    assert_eq!(mode_changes(&mut rx), ["Visual(Charwise)"]);
}

/// `<C-g>` pivots Visual ↔ Select; each side names itself.
#[test]
fn select_mode_and_its_toggle_announce_themselves() {
    let (mut editor, mut rx) = editor_with_mode_events("alpha beta\n");

    press(&mut editor, "gh");
    assert_eq!(editor.modal, ModalState::Select(VisualKind::Charwise));
    assert_eq!(echo(&editor), Some("-- SELECT --"));

    press(&mut editor, "<C-g>");
    assert_eq!(editor.modal, ModalState::Visual(VisualKind::Charwise));
    assert_eq!(echo(&editor), Some("-- VISUAL --"));
    assert_eq!(
        mode_changes(&mut rx),
        ["Select(Charwise)", "Visual(Charwise)"]
    );
}

/// Leaving Visual clears only the `-- VISUAL --` it put there: a message set
/// while the selection was up is newer feedback, and stays.
#[test]
fn a_newer_message_survives_leaving_visual() {
    let (mut editor, _rx) = editor_with_mode_events("one\ntwo\n");

    press(&mut editor, "V");
    editor.set_message(lattice_host::action::EchoLevel::Warn, "something happened");
    press(&mut editor, "<Esc>");
    assert_eq!(editor.modal, ModalState::Normal);
    assert_eq!(echo(&editor), Some("something happened"));
}
