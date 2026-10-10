//! LH.0.5 — a plugin output buffer fills, and repaints, with no keypress.
//!
//! The store (`lattice-plugin-host::output`) and the fold a view applies to
//! its events (`lattice-plugin-trace::output`) are unit-tested where they
//! live. What only a booted editor can show is the path between them: that
//! the loader's store is the one the mode seeds from, that its publisher is on
//! the bus the mode subscribes to, that the drain's writes land in the real
//! buffer, and — the property the whole design is for — that each of them
//! fires `async_landed`, so an install's progress reaches the screen while the
//! user's hands are off the keyboard.
//!
//! ## Why no test here presses a key before asserting
//!
//! A drain that writes but never wakes passes any test that dispatches a
//! chord first: the keystroke's own tail runs the tick and repaints. The
//! symptom in the editor is "it works, but only after I hit something". So
//! every assertion below is made after waiting on the wake and nothing else.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::time::Duration;

use lattice_core::Document as CoreDocument;
use lattice_host::chord::KeyChord;
use lattice_host::editor::Editor;
use lattice_mode::BufferStoreHandle;
use lattice_plugin_host::output::{OutputState, OutputStatus, PluginOutputHandle};
use lattice_plugin_trace::{OUTPUT_MODE_ID, PluginOutputMode};

const NAME: &str = "*lsp-install:rust-analyzer*";
const PLUGIN: &str = "lighthouse";

fn output(editor: &Editor) -> PluginOutputHandle {
    (*editor
        .services
        .get::<PluginOutputHandle>()
        .expect("the plugin-output store is registered at boot"))
    .clone()
}

/// The buffer's text as the document actor has it.
fn body(editor: &Editor, name: &str) -> String {
    let id = editor.buffers.by_name(name).expect("the buffer exists");
    let store = editor.services.get::<BufferStoreHandle>().unwrap();
    let snap = store.handle_for(id).expect("a document handle").snapshot();
    (0..snap.buffer.rope_line_count())
        .map(|n| snap.buffer.line(n).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Wait until the buffer reads `want`, driven ONLY by `async_landed`. Returns
/// the last text seen, so a failure shows what was there instead.
async fn shows(editor: &Editor, name: &str, want: &str) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let text = body(editor, name);
        if text == want || tokio::time::Instant::now() >= deadline {
            return text;
        }
        let _ =
            tokio::time::timeout(Duration::from_millis(250), editor.async_landed.notified()).await;
    }
}

/// Drain the wakes accumulated so far and wait for quiet, so a later wait
/// measures only what the test does next.
async fn settle(editor: &Editor) {
    while tokio::time::timeout(Duration::from_millis(150), editor.async_landed.notified())
        .await
        .is_ok()
    {}
}

fn lines(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opening_an_output_buffer_shows_what_was_written_before_it_was_open() {
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    // The install started, and wrote, before anything showed the buffer.
    output(&editor)
        .append(PLUGIN, NAME, lines(&["resolving", "downloading"]))
        .unwrap();

    editor.open_synthetic_buffer(NAME, OUTPUT_MODE_ID);

    let id = editor.buffers.by_name(NAME).unwrap();
    assert_eq!(
        editor.active_modes.get(&id).and_then(|m| m.major()),
        Some(PluginOutputMode::mode_id())
    );
    assert_eq!(
        shows(&editor, NAME, "resolving\ndownloading\n").await,
        "resolving\ndownloading\n",
        "the seed is the store's lines, with no keypress"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lines_written_while_open_arrive_and_wake_the_editor() {
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    let out = output(&editor);
    out.append(PLUGIN, NAME, lines(&["one"])).unwrap();
    editor.open_synthetic_buffer(NAME, OUTPUT_MODE_ID);
    assert_eq!(shows(&editor, NAME, "one\n").await, "one\n");
    settle(&editor).await;

    out.append(PLUGIN, NAME, lines(&["two", "three"])).unwrap();

    assert!(
        tokio::time::timeout(Duration::from_secs(5), editor.async_landed.notified())
            .await
            .is_ok(),
        "an append must fire `async_landed` — without it the line is in the \
         buffer and not on the screen until the next keypress"
    );
    assert_eq!(
        shows(&editor, NAME, "one\ntwo\nthree\n").await,
        "one\ntwo\nthree\n",
        "appended after the seed, each line once"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_status_change_alone_wakes_the_editor() {
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    let out = output(&editor);
    out.append(PLUGIN, NAME, lines(&["one"])).unwrap();
    editor.open_synthetic_buffer(NAME, OUTPUT_MODE_ID);
    assert_eq!(shows(&editor, NAME, "one\n").await, "one\n");
    settle(&editor).await;

    // No text changes: a download ticking from 42% to 43% moves only the
    // headerline, and that must repaint too.
    out.set_status(
        PLUGIN,
        NAME,
        OutputStatus {
            state: OutputState::Running,
            text: "downloading\u{2026} 43%".into(),
        },
    )
    .unwrap();

    assert!(
        tokio::time::timeout(Duration::from_secs(5), editor.async_landed.notified())
            .await
            .is_ok(),
        "a headerline-only change must fire `async_landed`"
    );
    assert_eq!(body(&editor, NAME), "one\n", "and leaves the text alone");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reset_replaces_the_page_instead_of_appending_under_it() {
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    let out = output(&editor);
    out.append(PLUGIN, NAME, lines(&["first run", "failed"]))
        .unwrap();
    editor.open_synthetic_buffer(NAME, OUTPUT_MODE_ID);
    assert_eq!(
        shows(&editor, NAME, "first run\nfailed\n").await,
        "first run\nfailed\n"
    );

    out.reset(PLUGIN, NAME).unwrap();
    out.append(PLUGIN, NAME, lines(&["second run"])).unwrap();

    assert_eq!(
        shows(&editor, NAME, "second run\n").await,
        "second run\n",
        "the second run's page holds only the second run"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_buffers_output_does_not_leak_in() {
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    let out = output(&editor);
    out.append(PLUGIN, NAME, lines(&["mine"])).unwrap();
    editor.open_synthetic_buffer(NAME, OUTPUT_MODE_ID);
    assert_eq!(shows(&editor, NAME, "mine\n").await, "mine\n");

    out.append(PLUGIN, "*lsp-install:gopls*", lines(&["theirs"]))
        .unwrap();
    out.append(PLUGIN, NAME, lines(&["mine again"])).unwrap();

    assert_eq!(
        shows(&editor, NAME, "mine\nmine again\n").await,
        "mine\nmine again\n"
    );
}

/// `ReadOnly = true` stops typing and nothing else; operators are refused only
/// through the implied `read-only-mode`. This is the one that was missed on
/// four buffers before it was written down.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_operator_does_not_edit_an_output_buffer() {
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    output(&editor)
        .append(PLUGIN, NAME, lines(&["keep me"]))
        .unwrap();
    editor.open_synthetic_buffer(NAME, OUTPUT_MODE_ID);
    assert_eq!(shows(&editor, NAME, "keep me\n").await, "keep me\n");

    let mut partial = Vec::new();
    for chord in [
        KeyChord::char('x'),
        KeyChord::char('d'),
        KeyChord::char('d'),
    ] {
        let _ = editor.dispatch_chord(chord, &mut partial);
    }
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert_eq!(
        body(&editor, NAME),
        "keep me\n",
        "`x` and `dd` must not change the log"
    );
}

/// A buffer nobody has written to opens empty rather than failing: a plugin
/// may show its buffer and write its first line in either order.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_buffer_opened_before_its_first_line_fills_when_the_line_comes() {
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    editor.open_synthetic_buffer(NAME, OUTPUT_MODE_ID);
    settle(&editor).await;
    assert_eq!(body(&editor, NAME), "");

    output(&editor)
        .append(PLUGIN, NAME, lines(&["late"]))
        .unwrap();

    assert_eq!(shows(&editor, NAME, "late\n").await, "late\n");
}
