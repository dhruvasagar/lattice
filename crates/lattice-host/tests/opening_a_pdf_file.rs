//! PF.2: `:e paper.pdf` opens a page list in `pdf-mode`, and cannot destroy
//! the PDF.
//!
//! The buffer is built empty, as for a picture, and the mode writes one label
//! per page once the count is read off-thread. So the test that matters most
//! after "never truncates the file" is that the labels reach the buffer with
//! NO key pressed: a write that waits for the next keystroke looks like an
//! empty PDF until the user does something.
//!
//! `lattice-media`'s `pdf` feature is on for this test build (a
//! dev-dependency), so the probe is the real one.

#![allow(clippy::unwrap_used)]

use std::time::{Duration, Instant};

use lattice_core::Document as CoreDocument;
use lattice_host::chord::KeyChord;
use lattice_host::dispatch::DispatchOutcome;
use lattice_host::editor::Editor;

/// A minimal, well-formed PDF of `pages` US-letter pages, each one red fill.
fn pdf_bytes(pages: usize) -> Vec<u8> {
    let mut objects: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        format!(
            "<< /Type /Pages /Kids [{}] /Count {pages} >>",
            (0..pages)
                .map(|i| format!("{} 0 R", 4 + i))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        "<< /Length 28 >>\nstream\n1 0 0 rg 0 0 612 792 re f\nendstream".into(),
    ];
    for _ in 0..pages {
        objects
            .push("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 3 0 R >>".into());
    }
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for off in offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

fn write(dir: &std::path::Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn press(editor: &mut Editor, keys: &str) {
    let mut partial = Vec::new();
    for c in keys.chars() {
        let _ = editor.dispatch_chord(KeyChord::char(c), &mut partial);
    }
}

fn ex(editor: &mut Editor, line: &str) {
    let mut out = DispatchOutcome::default();
    editor.execute_ex_line(line, &mut out);
}

fn text_of(editor: &Editor, id: lattice_core::BufferId) -> String {
    editor
        .buffers
        .document_handle(id)
        .map(|h| h.text())
        .unwrap_or_default()
}

/// Tick the editor the way its actor does, until `done` or a deadline. Ticks
/// only when `async_landed` fired, so a write that wakes nothing fails here
/// instead of passing on a timer.
async fn settle_woken(editor: &mut Editor, mut done: impl FnMut(&Editor) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if done(editor) {
            return true;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        let notified = editor.async_landed.clone();
        if tokio::time::timeout(left, notified.notified())
            .await
            .is_err()
        {
            return done(editor);
        }
        editor.run_tick_pending();
    }
}

/// Open `path` and wait for `pdf-mode` to write its text.
async fn open_and_label(editor: &mut Editor, path: &std::path::Path) -> lattice_core::BufferId {
    editor.do_edit(Some(path.to_path_buf()), false);
    editor.run_tick_pending();
    let id = editor.document_buffer_id;
    assert!(
        settle_woken(editor, |e| !text_of(e, id).is_empty()).await,
        "pdf-mode never wrote the buffer"
    );
    id
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_page_labels_land_without_a_keypress_and_leave_the_buffer_clean() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = write(dir.path(), "paper.pdf", &pdf_bytes(3));
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));

    let id = open_and_label(&mut editor, &pdf).await;

    assert_eq!(
        editor.active_modes.get(&id).and_then(|a| a.major()),
        Some(lattice_mode::modes::PdfMode::mode_id())
    );
    assert_eq!(
        text_of(&editor, id),
        "Page 1 of 3\nPage 2 of 3\nPage 3 of 3",
        "line N is page N"
    );
    assert_eq!(editor.buffers.document_path(id), Some(pdf));
    assert!(
        !editor.buffers.document_dirty(id),
        "the mode wrote the labels, the user changed nothing: `:q` must not refuse"
    );
}

/// Pages are lines, so vim's own motions move by page.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn line_motions_move_by_page() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = write(dir.path(), "paper.pdf", &pdf_bytes(5));
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    open_and_label(&mut editor, &pdf).await;

    press(&mut editor, "G");
    assert_eq!(editor.cursor.line, 4, "G is the last page");
    press(&mut editor, "k");
    assert_eq!(editor.cursor.line, 3, "k is the previous page");
    press(&mut editor, "gg");
    assert_eq!(editor.cursor.line, 0, "gg is the first page");
    press(&mut editor, "j");
    assert_eq!(editor.cursor.line, 1, "j is the next page");
}

/// The data-loss case: deleting, typing or undoing the labels and saving
/// must leave the PDF on disk byte-identical, and the save says why it did
/// not happen.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edits_and_a_save_cannot_replace_the_pdf_with_its_labels() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = pdf_bytes(2);
    let pdf = write(dir.path(), "paper.pdf", &bytes);
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    let id = open_and_label(&mut editor, &pdf).await;
    let labels = text_of(&editor, id);

    for keys in ["dd", "x", "u", "p"] {
        press(&mut editor, keys);
        editor.run_tick_pending();
        assert_eq!(text_of(&editor, id), labels, "{keys:?} must be refused");
    }
    press(&mut editor, "ihello");
    editor.run_tick_pending();
    assert_eq!(text_of(&editor, id), labels, "insert must be refused");
    let mut partial = Vec::new();
    let _ = editor.dispatch_chord(
        lattice_protocol::parse_chord_sequence("<Esc>").unwrap()[0],
        &mut partial,
    );

    ex(&mut editor, "w");
    assert_eq!(std::fs::read(&pdf).unwrap(), bytes, "the PDF is untouched");
    let message = editor
        .last_message
        .as_ref()
        .map(|m| m.text.clone())
        .unwrap_or_default();
    assert!(
        message.contains("read-only"),
        "refused out loud: {message:?}"
    );
}

/// A file that is not a PDF says so in one line naming it, rather than
/// opening as an empty buffer, which would look like a PDF with no pages.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_broken_pdf_says_why_in_one_line() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = write(dir.path(), "broken.pdf", b"%PDF-1.7\nnot really\n");
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    let id = open_and_label(&mut editor, &pdf).await;

    let text = text_of(&editor, id);
    assert_eq!(text.lines().count(), 1, "{text:?}");
    assert!(
        text.starts_with("broken.pdf: not a readable PDF"),
        "{text:?}"
    );
}

/// An ordinary buffer: listed, and `:bd` closes it like any other.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pdf_buffer_is_listed_and_closes_like_any_other() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = write(dir.path(), "paper.pdf", &pdf_bytes(1));
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    let id = open_and_label(&mut editor, &pdf).await;

    assert!(
        editor.buffers.listed_ids_sorted().contains(&id),
        ":ls shows it"
    );
    ex(&mut editor, "bd");
    editor.run_tick_pending();
    assert!(!editor.buffers.contains(id), ":bd closes it");
}

/// The wake, pinned on its own. Other async work during boot also wakes the
/// editor, so the labels test above passes even with this wake removed (it
/// was checked): it cannot tell whether THIS event wakes anything. So: the
/// mode announces its write with `PdfPagesLabelled`, and that announcement,
/// published into a quiet editor, wakes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_labels_announce_themselves_and_the_announcement_wakes_the_editor() {
    use lattice_mode::modes::PdfPagesLabelled;

    let dir = tempfile::tempdir().unwrap();
    let pdf = write(dir.path(), "paper.pdf", &pdf_bytes(3));
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<PdfPagesLabelled>();
    editor.event_bus.subscribe_typed(tx);

    let id = open_and_label(&mut editor, &pdf).await;
    let announced = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("the write is announced")
        .unwrap();
    assert_eq!((announced.buffer, announced.pages), (id, 3));

    while tokio::time::timeout(Duration::from_millis(100), editor.async_landed.notified())
        .await
        .is_ok()
    {}
    editor.event_bus.publish_typed(announced);
    assert!(
        tokio::time::timeout(Duration::from_secs(1), editor.async_landed.notified())
            .await
            .is_ok(),
        "PdfPagesLabelled must wake the editor, or the labels wait for a keypress"
    );
}
