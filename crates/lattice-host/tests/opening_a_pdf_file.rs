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

/// Publish a drawing peer's cell metrics: `cols` columns of `col_px`, rows of
/// `row_px`. The GPUI peer does this; the TUI never does.
fn with_metrics(editor: &mut Editor, row_px: f32, col_px: f32, cols: u32) {
    editor.pane_tree.active_mut().viewport_width = cols;
    editor.dispatch(lattice_host::action::Action::SetCellMetrics { row_px, col_px });
}

/// `(source, anchor, rows, height_lh, intrinsic)` for each media block the
/// buffer's cache holds.
type Sized = (
    lattice_cells::MediaSource,
    u32,
    u16,
    Option<f32>,
    Option<(u32, u32)>,
);

fn sized_blocks(editor: &Editor, id: lattice_core::BufferId) -> Option<Vec<Sized>> {
    use lattice_host::per_buffer_cache::PerBufferCacheExt;
    let cache = editor.wasm_media.cache.get_for(id)?;
    Some(
        cache
            .blocks
            .iter()
            .map(|(b, anchor, rows)| (b.source.clone(), *anchor, *rows, b.height_lh, b.intrinsic))
            .collect(),
    )
}

/// PF.3: in a peer that draws, every page is a block below its own label,
/// sized from its points to fill the pane (fit page scales UP, unlike a
/// picture), before any pixel exists. That is what keeps the document's
/// height fixed while pages render.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_page_is_a_block_below_its_label_sized_to_fill_the_pane() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = write(dir.path(), "paper.pdf", &pdf_bytes(3));
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    // 80 columns of 10 px and 41 rows of 20 px: an 800 × 800 px area below
    // the anchor line, larger than a 612 × 792 pt page.
    editor.viewport_height = 41;
    with_metrics(&mut editor, 20.0, 10.0, 80);

    let id = open_and_label(&mut editor, &pdf).await;
    assert!(
        settle_woken(&mut editor, |e| sized_blocks(e, id)
            .is_some_and(|b| b.len() == 3))
        .await,
        "the pages never became blocks: {:?}",
        sized_blocks(&editor, id)
    );
    let blocks = sized_blocks(&editor, id).unwrap();
    for (i, (source, anchor, rows, height_lh, intrinsic)) in blocks.into_iter().enumerate() {
        assert_eq!(
            source,
            lattice_cells::MediaSource::PdfPage {
                path: pdf.clone(),
                page: i as u32
            }
        );
        assert_eq!(anchor, i as u32, "page N hangs below line N");
        assert_eq!(intrinsic, Some((612, 792)), "measured in points");
        // Bound by the height: 800 px is 40 line-heights, scaled up from 792.
        let h = height_lh.expect("measured");
        assert!((h - 40.0).abs() < 1e-3, "{h}");
        assert_eq!(rows, 40);
    }
}

/// In a peer that draws no pixels (the TUI), a page reserves nothing: its
/// label line already says what an alt row would.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_drawing_peer_the_pages_reserve_no_rows() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = write(dir.path(), "paper.pdf", &pdf_bytes(3));
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));

    let id = open_and_label(&mut editor, &pdf).await;
    let version = editor.buffers.document_handle(id).unwrap().version();
    // Wait for a refresh against the labelled text to have landed.
    let refreshed = |e: &Editor| {
        use lattice_host::per_buffer_cache::PerBufferCacheExt;
        e.wasm_media
            .cache
            .get_for(id)
            .is_some_and(|c| c.document_version == version)
    };
    editor.run_tick_pending();
    assert!(
        settle_woken(&mut editor, refreshed).await,
        "no refresh landed"
    );
    assert_eq!(sized_blocks(&editor, id), Some(vec![]));
}

/// Press a chord sequence spelled as in a keymap: `"<C-f>"`, `"3J"`.
fn keys(editor: &mut Editor, spelling: &str) {
    let mut partial = Vec::new();
    for chord in lattice_protocol::chord::parse_chord_sequence(spelling).unwrap() {
        let _ = editor.dispatch_chord(chord, &mut partial);
    }
}

/// What the active pane's modeline position element shows.
fn modeline_position(editor: &mut Editor) -> String {
    editor.publish_render_state();
    let rs = editor.render_state.load();
    let pane = *rs
        .panes
        .tree
        .leaves()
        .iter()
        .find(|p| p.buffer_id == editor.active_pane_buffer_id())
        .unwrap();
    let content = lattice_host::modeline::resolve_builtin_content(
        lattice_host::modeline::CORE_POSITION,
        &pane,
        true,
        &rs,
    );
    content.spans.iter().map(|s| s.text.as_str()).collect()
}

/// A drawing peer's geometry with an 800 px tall pane, and a PDF of `pages`
/// pages opened and drawn in it: each page fills the pane (fit page).
async fn open_drawn(pages: usize) -> (tempfile::TempDir, Editor, lattice_core::BufferId) {
    let dir = tempfile::tempdir().unwrap();
    let pdf = write(dir.path(), "paper.pdf", &pdf_bytes(pages));
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    editor.viewport_height = 41;
    with_metrics(&mut editor, 20.0, 10.0, 80);
    let id = open_and_label(&mut editor, &pdf).await;
    assert!(
        settle_woken(&mut editor, |e| sized_blocks(e, id)
            .is_some_and(|b| b.len() == pages))
        .await,
        "the pages never became blocks"
    );
    (dir, editor, id)
}

/// PF.4: zathura's `J` / `K` turn pages, and `<Space>` /
/// `<S-Space>` scroll a screen, which in fit page is exactly one page, with
/// that page at the top.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn page_keys_turn_pages() {
    let (_dir, mut editor, _id) = open_drawn(6).await;
    keys(&mut editor, "J");
    assert_eq!(editor.cursor.line, 1);
    keys(&mut editor, "JJJ");
    assert_eq!(editor.cursor.line, 4);
    keys(&mut editor, "K");
    assert_eq!(editor.cursor.line, 3);

    keys(&mut editor, "gg");
    keys(&mut editor, "<Space>");
    assert_eq!(editor.cursor.line, 1, "a screen is a page");
    editor.ensure_cursor_visible();
    assert_eq!(editor.scroll, 1, "the page turned to is at the top");
    keys(&mut editor, "<Space>");
    assert_eq!(editor.cursor.line, 2);
    keys(&mut editor, "<S-Space>");
    assert_eq!(editor.cursor.line, 1);
}

/// vim's own screen, half-screen and step scrolls need no binding in a PDF:
/// a page is a line no taller than the pane, so each turns one page.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vims_scroll_keys_turn_one_page_in_fit_page() {
    let (_dir, mut editor, _id) = open_drawn(6).await;
    for (forward, back) in [("<C-f>", "<C-b>"), ("<PageDown>", "<PageUp>")] {
        keys(&mut editor, "gg");
        keys(&mut editor, forward);
        assert_eq!(editor.cursor.line, 1, "{forward}");
        keys(&mut editor, back);
        assert_eq!(editor.cursor.line, 0, "{back}");
    }
    keys(&mut editor, "gg");
    keys(&mut editor, "<C-e>");
    assert_eq!(editor.scroll, 1, "<C-e> scrolls a page off the top");
    assert_eq!(editor.cursor.line, 1, "and the cursor goes with it");
    keys(&mut editor, "<C-y>");
    assert_eq!(editor.scroll, 0);
}

/// `q` closes the PDF's buffer and the pane shows what it showed before. It
/// is `viewer-mode`'s, so a text buffer keeps vim's `J` (join).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn q_puts_the_pdf_away_and_neither_q_nor_j_leaks_into_text() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = write(dir.path(), "paper.pdf", &pdf_bytes(2));
    let txt = write(dir.path(), "notes.txt", b"one\ntwo\n");
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    editor.do_edit(Some(txt.clone()), false);
    editor.run_tick_pending();
    let text_id = editor.active_pane_buffer_id();
    let pdf_id = open_and_label(&mut editor, &pdf).await;

    // `KillBuffer` is applied by the peer (both call `kill_buffer` from
    // their effect tail), so the host hands it back in the outcome.
    let mut partial = Vec::new();
    let q = lattice_protocol::chord::parse_chord_sequence("q").unwrap()[0];
    let (_, outcome) = editor.dispatch_chord_with_outcome(q, &mut partial);
    let queued = outcome.effects;
    assert!(
        queued
            .iter()
            .any(|e| matches!(e, lattice_grammar::Effect::KillBuffer)),
        "q asked to close the buffer: {queued:?}"
    );
    editor.kill_buffer();
    editor.run_tick_pending();
    assert_eq!(editor.active_pane_buffer_id(), text_id, "back to the text");
    assert!(
        editor.buffers.document_handle(pdf_id).is_none(),
        "the PDF's buffer is closed, as :bd closes it"
    );

    keys(&mut editor, "J");
    editor.run_tick_pending();
    assert_eq!(text_of(&editor, text_id), "one two\n", "J still joins");
}

/// The modeline counts pages in a PDF and keeps `line:col` elsewhere.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_modeline_shows_the_cursors_page() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = write(dir.path(), "paper.pdf", &pdf_bytes(12));
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    assert_eq!(modeline_position(&mut editor), "1:0");
    open_and_label(&mut editor, &pdf).await;
    keys(&mut editor, "JJ");
    assert_eq!(modeline_position(&mut editor), "page 3/12");
}
