//! `:{range}!cmd` end to end: the `:` line, the range, a real shell, and the
//! result coming back off-thread into the buffer.
//!
//! Unix only — the commands are `sort`, `tr` and friends. What is under test
//! is which lines go out and what replaces them, which is the same code on
//! every platform.
#![cfg(unix)]
#![allow(clippy::unwrap_used)]

use std::time::{Duration, Instant};

use lattice_core::Document as CoreDocument;
use lattice_host::dispatch::DispatchOutcome;
use lattice_host::editor::Editor;
use lattice_protocol::position::Position;

const TEXT: &str = "delta\ncharlie\nbravo\nalpha\necho\n";

fn editor_at(line: u32) -> Editor {
    let mut editor = Editor::boot(CoreDocument::from_text(TEXT));
    editor.cursor = Position::new(line, 0);
    editor
}

fn ex(editor: &mut Editor, line: &str) {
    let mut out = DispatchOutcome::default();
    editor.execute_ex_line(line, &mut out);
}

fn text(editor: &Editor) -> String {
    editor.document.text()
}

fn echo(editor: &Editor) -> String {
    editor
        .last_message
        .as_ref()
        .map(|m| m.text.clone())
        .unwrap_or_default()
}

/// Run the tick until the filter's result has been consumed. Nothing is
/// dispatched while waiting: a result that needed a keypress to land would
/// pass a test that pressed one.
async fn land(editor: &mut Editor) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while editor.pending_filter_rx.is_some() && !editor.drain_pending_filter() {
        assert!(Instant::now() < deadline, "the filter never landed");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_range_is_what_goes_through_the_command() {
    let mut editor = editor_at(0);
    ex(&mut editor, "2,4!sort");
    land(&mut editor).await;
    assert_eq!(text(&editor), "delta\nalpha\nbravo\ncharlie\necho\n");
    assert_eq!(
        editor.cursor.line, 1,
        "the cursor goes to the first filtered line"
    );
    assert_eq!(echo(&editor), "3 lines filtered, 3 lines out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dot_is_the_cursor_line_and_percent_the_whole_buffer() {
    let mut editor = editor_at(2);
    ex(&mut editor, ".!tr a-z A-Z");
    land(&mut editor).await;
    assert_eq!(text(&editor), "delta\ncharlie\nBRAVO\nalpha\necho\n");

    let mut editor = editor_at(2);
    ex(&mut editor, "%!sort");
    land(&mut editor).await;
    assert_eq!(text(&editor), "alpha\nbravo\ncharlie\ndelta\necho\n");
}

/// The output need not be as many lines as went in: fewer, more, or none.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_output_replaces_the_lines_whatever_its_length() {
    let mut editor = editor_at(0);
    ex(&mut editor, "1,3!wc -l | tr -d ' '");
    land(&mut editor).await;
    assert_eq!(text(&editor), "3\nalpha\necho\n");

    let mut editor = editor_at(0);
    ex(&mut editor, "2!printf 'x\\ny\\nz\\n'");
    land(&mut editor).await;
    assert_eq!(text(&editor), "delta\nx\ny\nz\nbravo\nalpha\necho\n");

    // No output deletes the lines — in the middle, and at the end.
    let mut editor = editor_at(0);
    ex(&mut editor, "2,3!true");
    land(&mut editor).await;
    assert_eq!(text(&editor), "delta\nalpha\necho\n");

    let mut editor = editor_at(0);
    ex(&mut editor, "4,$!true");
    land(&mut editor).await;
    assert_eq!(text(&editor), "delta\ncharlie\nbravo\n");
}

/// One filter is one undo step.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_filter_is_undone_in_one_step() {
    let mut editor = editor_at(0);
    ex(&mut editor, "%!sort");
    land(&mut editor).await;
    assert_ne!(text(&editor), TEXT);
    let _ = editor.undo_blocking();
    assert_eq!(text(&editor), TEXT);
}

/// A command that fails changes nothing and says what it said.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failing_command_leaves_the_buffer_alone() {
    let mut editor = editor_at(0);
    ex(&mut editor, "%!echo broken >&2; exit 1");
    land(&mut editor).await;
    assert_eq!(text(&editor), TEXT);
    assert!(echo(&editor).contains("broken"), "{}", echo(&editor));
}

/// The command runs off-thread, so the buffer can change before it lands.
/// Its output describes lines that may no longer be those lines; it is
/// dropped rather than written over what was typed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_result_for_a_buffer_that_has_since_changed_is_not_applied() {
    let mut editor = editor_at(0);
    ex(&mut editor, "%!sleep 0.3; sort");
    ex(&mut editor, "1d");
    let edited = text(&editor);
    land(&mut editor).await;
    assert_eq!(text(&editor), edited, "the edit made meanwhile survives");
    assert!(
        echo(&editor).contains("changed while it ran"),
        "{}",
        echo(&editor)
    );
}

/// One filter at a time: a second while the first is still out is refused,
/// and the first still lands.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_filter_is_refused_while_the_first_is_running() {
    let mut editor = editor_at(0);
    ex(&mut editor, "2,4!sleep 0.3; sort");
    ex(&mut editor, "%!tr a-z A-Z");
    assert!(echo(&editor).contains("still running"), "{}", echo(&editor));
    land(&mut editor).await;
    assert_eq!(text(&editor), "delta\nalpha\nbravo\ncharlie\necho\n");
    // And once it has landed, the next one runs.
    ex(&mut editor, "%!tr a-z A-Z");
    land(&mut editor).await;
    assert_eq!(text(&editor), "DELTA\nALPHA\nBRAVO\nCHARLIE\nECHO\n");
}

/// With no range, `:!cmd` runs the command and shows its output — the
/// `*compilation*` buffer — and filters nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_range_the_command_runs_and_its_output_is_shown() {
    let mut editor = editor_at(0);
    ex(&mut editor, "!echo hello");
    assert!(editor.buffers.by_name("*compilation*").is_some());
    assert!(editor.pending_filter_rx.is_none(), "nothing is filtered");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bang_with_no_command_is_an_error() {
    let mut editor = editor_at(0);
    for line in ["!", "%!", "2,3!  "] {
        ex(&mut editor, line);
        assert!(echo(&editor).contains("E471"), "{line}: {}", echo(&editor));
    }
}
