//! Switching back to a buffer puts the caret where this pane left it, as
//! vim's `:b` / `:bn` / `:bp` do.
//!
//! A pane must not carry its caret onto a DIFFERENT buffer (magit's `<CR>`
//! would open the commit buffer at the status list's line), so a switch
//! resets it, and a return restores it. The position comes from the pane's
//! buffer trail, which records where it left each buffer
//! (`pane_buffer_history.rs` pins the recording); these pin that
//! `activate_document` reads it back. Without the read, every return to a
//! buffer lands on line 1.

use std::time::{Duration, Instant};

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_protocol::position::Position;

fn boot() -> Editor {
    lattice_plugin_loader::disable_autoload();
    Editor::boot(CoreDocument::from_text("scratch\n"))
}

fn press(editor: &mut Editor, spelling: &str) {
    let mut partial = Vec::new();
    for chord in lattice_protocol::chord::parse_chord_sequence(spelling).expect("a chord") {
        let _ = editor.dispatch_chord(chord, &mut partial);
    }
    editor.run_tick_pending();
}

fn text(editor: &Editor) -> String {
    editor.document.snapshot().buffer.as_string()
}

/// `:e` a file of `lines` numbered lines, waiting on the wake for it to load.
async fn open(editor: &mut Editor, dir: &std::path::Path, name: &str, lines: usize) {
    let path = dir.join(name);
    let body: String = (1..=lines).map(|i| format!("{name} {i}\n")).collect();
    std::fs::write(&path, &body).expect("write the file");
    editor.do_edit(Some(path), false);
    let deadline = Instant::now() + Duration::from_secs(10);
    while text(editor) != body {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "{name} never loaded");
        let woke = editor.async_landed.clone();
        let _ = tokio::time::timeout(left, woke.notified()).await;
        editor.run_tick_pending();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn returning_to_a_buffer_puts_the_caret_where_it_was_left() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let mut editor = boot();
    open(&mut editor, dir.path(), "a.txt", 30).await;
    press(&mut editor, ":12<CR>");
    assert_eq!(editor.cursor.line, 11);
    open(&mut editor, dir.path(), "b.txt", 30).await;
    press(&mut editor, ":5<CR>");

    press(&mut editor, ":bp<CR>");
    assert!(text(&editor).starts_with("a.txt 1\n"), "back on a.txt");
    assert_eq!(editor.cursor, Position::new(11, 0), "where a.txt was left");

    press(&mut editor, ":bn<CR>");
    assert!(text(&editor).starts_with("b.txt 1\n"), "back on b.txt");
    assert_eq!(editor.cursor, Position::new(4, 0), "and b.txt where it was");
}

/// The reset half: a buffer this pane has never shown opens at the top,
/// whatever line the pane was on before.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_buffer_the_pane_never_showed_opens_at_the_top() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let mut editor = boot();
    open(&mut editor, dir.path(), "a.txt", 30).await;
    press(&mut editor, ":20<CR>");
    open(&mut editor, dir.path(), "b.txt", 30).await;
    assert_eq!(editor.cursor, Position::ZERO);
}

/// A buffer that shrank while it was away clamps the remembered caret onto
/// a line that still exists.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_buffer_that_shrank_while_away_clamps_the_caret() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let mut editor = boot();
    open(&mut editor, dir.path(), "a.txt", 30).await;
    let a = editor.active_pane_buffer_id();
    press(&mut editor, ":25<CR>");
    open(&mut editor, dir.path(), "b.txt", 30).await;

    let handle = editor.buffers.document_handle(a).expect("a.txt is open");
    let snap = handle.snapshot();
    let last = snap.buffer.rope_line_count() - 1;
    let end = Position::new(last, snap.buffer.line_byte_len(last));
    handle
        .apply_edit_batch(vec![lattice_protocol::edit::Edit::replace(
            lattice_protocol::Range::new(Position::ZERO, end),
            "x\ny\nz\n".to_string(),
        )])
        .await
        .expect("rewrite a.txt");

    press(&mut editor, ":bp<CR>");
    assert_eq!(text(&editor), "x\ny\nz\n");
    assert_eq!(editor.cursor, Position::new(2, 0), "on its last line");
}

/// The case that found this: a plugin re-opens its synthetic buffer by name
/// (an export's output, `open-synthetic-buffer`) from another buffer. The
/// reader's place in it survives.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reopening_a_synthetic_buffer_keeps_its_caret() {
    let mut editor = boot();
    let scratch = editor.active_pane_buffer_id();
    let body: String = (1..=20).map(|i| format!("<p>{i}</p>\n")).collect();
    editor.open_synthetic_buffer_seeded("*Export*", "text-mode", Some(&body), None, None);
    press(&mut editor, ":6<CR>");
    // A synthetic buffer is unlisted, so `:bp` would not leave it; the
    // export is run from the org buffer, which is where this goes.
    editor.do_focus_buffer(scratch);
    assert!(!text(&editor).starts_with("<p>"), "left the export buffer");

    editor.open_synthetic_buffer_seeded("*Export*", "text-mode", Some(&body), None, None);
    assert!(text(&editor).starts_with("<p>1</p>"), "back on it");
    assert_eq!(editor.cursor, Position::new(5, 0));
}
