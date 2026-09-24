//! `*messages*` is syntax-highlighted by its MODE, in every renderer.
//!
//! ## What this replaces
//!
//! The TUI composed `*messages*` bodies itself, behind `if
//! is_messages_buffer`, calling a renderer-private `messages_line_spans`. The
//! GPUI peer had no equivalent, so the log was coloured in one renderer and
//! plain in the other. The TUI's own comment named the problem: "the deeper
//! issue is that this branch exists at all: a kind-specific body composer".
//!
//! `messages-mode` owns the syntax now and publishes renderer-neutral
//! `StyledSpan`s through `PendingSyntheticHighlights` — the same pipeline
//! magit, help and the listings use, which both peers consume through the
//! cells / `DisplayMatrix` build with no per-kind renderer code. Neither
//! renderer knows what a log line is, which is exactly why they agree.
//!
//! These assert the HOST half: draining a record publishes the mode's spans
//! against the `*messages*` buffer. The mode's own tests cover which token
//! gets which style.

#![allow(clippy::unwrap_used)]

use lattice_core::Document as CoreDocument;
use lattice_host::action::EchoLevel;
use lattice_host::editor::Editor;

fn published_spans(
    editor: &Editor,
    buffer: lattice_core::BufferId,
) -> Vec<Vec<lattice_cells::style::StyledSpan>> {
    let pending = editor
        .services
        .get::<lattice_mode::PendingSyntheticHighlights>()
        .expect("boot registers the synthetic-highlight store");
    let map = pending.map.lock().unwrap();
    match map.get(&buffer).map(|u| &u.op) {
        Some(lattice_mode::HighlightsOp::Replace(spans)) => spans.clone(),
        Some(lattice_mode::HighlightsOp::InsertAt { spans, .. }) => spans.clone(),
        _ => Vec::new(),
    }
}

/// A drained record arrives with its level styled — no renderer involved.
#[test]
fn draining_a_message_publishes_the_modes_spans() {
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    editor.set_message(EchoLevel::Error, "something went wrong".to_string());
    let buffer = editor.ensure_messages_buffer();
    editor.drain_message_events();

    let spans = published_spans(&editor, buffer);
    assert!(
        !spans.is_empty(),
        "the messages drain must publish spans; without them the log is plain \
         in every renderer that does not special-case it"
    );
    assert!(
        spans
            .iter()
            .flatten()
            .any(|s| s.style == lattice_cells::style::Style::MessagesError),
        "an ERROR record must carry the error style, got {spans:?}"
    );
}

/// Severity survives the round trip, level by level.
#[test]
fn each_level_reaches_the_buffer_with_its_own_style() {
    use lattice_cells::style::Style;
    for (level, expected) in [
        (EchoLevel::Error, Style::MessagesError),
        (EchoLevel::Warn, Style::MessagesWarn),
        (EchoLevel::Info, Style::MessagesInfo),
    ] {
        let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
        editor.set_message(level, "a record".to_string());
        let buffer = editor.ensure_messages_buffer();
        editor.drain_message_events();

        let spans = published_spans(&editor, buffer);
        assert!(
            spans.iter().flatten().any(|s| s.style == expected),
            "{level:?} must publish {expected:?}, got {spans:?}"
        );
    }
}
