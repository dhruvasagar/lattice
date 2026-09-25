//! Appending to a synthetic buffer must land record N on buffer line N, so the
//! per-line highlight spans published by `messages-mode` stay aligned with the
//! text they colour.
//!
//! Regression: `append_to_owned_buffer` used to compute its insertion point
//! from `last_addressable_line`, which backs off a trailing-empty line — so the
//! insert landed BEFORE the buffer's terminating `\n` and fused the first
//! appended record onto the previous last line. Each drain then published one
//! span row per record while creating one fewer buffer line, shifting every
//! span after the seam down by one: `*messages*` painted INFO tokens with the
//! ERROR/WARN style and dropped the final record's span onto the empty phantom
//! line, so the last lines rendered unhighlighted. The append now spans to the
//! true rope end (phantom line included), like `replace_owned_buffer`.

#![allow(clippy::unwrap_used)]

use lattice_core::Document as CoreDocument;
use lattice_host::action::EchoLevel;
use lattice_host::editor::Editor;

fn buffer_text(editor: &Editor, id: lattice_core::BufferId) -> String {
    editor
        .buffers
        .document_handle(id)
        .unwrap()
        .snapshot()
        .buffer
        .as_string()
}

/// A record line's level token should map to this style name (None for a
/// non-record / empty line).
fn expected_style_for_line(line: &str) -> Option<&'static str> {
    match line.get(13..18)? {
        "ERROR" => Some("error"),
        " WARN" => Some("warn"),
        " INFO" => Some("info"),
        "DEBUG" => Some("debug"),
        "TRACE" => Some("trace"),
        _ => None,
    }
}

fn level_style_name(style: lattice_cells::style::Style) -> Option<&'static str> {
    use lattice_cells::style::Style::*;
    match style {
        MessagesError => Some("error"),
        MessagesWarn => Some("warn"),
        MessagesInfo => Some("info"),
        MessagesDebug => Some("debug"),
        MessagesTrace => Some("trace"),
        _ => None,
    }
}

/// Two `HH:MM:SS.mmm` timestamps on one line means two records fused.
fn two_timestamps(l: &str) -> bool {
    l.match_indices(':')
        .filter(|(i, _)| *i >= 2 && l.as_bytes().get(i + 3) == Some(&b':'))
        .count()
        >= 2
}

#[test]
fn backlog_seed_then_drain_never_merges_and_keeps_spans_aligned() {
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));

    // Two records accumulate in the ring before the buffer exists.
    editor.set_message(EchoLevel::Info, "backlog one".to_string());
    editor.set_message(EchoLevel::Warn, "backlog two".to_string());

    // First creation seeds the ring backlog into the buffer.
    let id = editor.ensure_messages_buffer();

    // A live drain appends the queued events plus a new record. This is the
    // append-onto-an-already-newline-terminated-buffer case the seam bug hit.
    editor.set_message(EchoLevel::Error, "live three".to_string());
    editor.drain_message_events();

    let text = buffer_text(&editor, id);
    for (i, l) in text.lines().enumerate() {
        assert!(
            !two_timestamps(l),
            "line {i} fused two records onto one buffer line: {l:?}"
        );
    }

    // The last non-empty line must be the ERROR we just appended — proof the
    // tail record was not swallowed into a merge.
    let last = text.lines().rfind(|l| !l.is_empty()).unwrap();
    assert_eq!(expected_style_for_line(last), Some("error"), "got {last:?}");

    // Every published span row's level style must match the level token on the
    // buffer line it targets (`start_line + i`). A one-line shift would paint a
    // WARN line with the INFO style and push the ERROR span onto the phantom
    // empty line.
    let lines: Vec<&str> = text.lines().collect();
    let pending = editor
        .services
        .get::<lattice_mode::PendingSyntheticHighlights>()
        .unwrap();
    let map = pending.map.lock().unwrap();
    let (start_line, spans) = match map.get(&id).map(|u| &u.op) {
        Some(lattice_mode::HighlightsOp::InsertAt { start_line, spans }) => {
            (*start_line as usize, spans.clone())
        }
        other => panic!("expected an InsertAt op, got {other:?}"),
    };
    for (i, span_row) in spans.iter().enumerate() {
        let line_idx = start_line + i;
        let line = lines.get(line_idx).copied().unwrap_or("");
        let got = span_row.iter().find_map(|s| level_style_name(s.style));
        assert_eq!(
            got,
            expected_style_for_line(line),
            "span row {i} → buffer line {line_idx} ({line:?}): level style mismatch"
        );
    }
}
