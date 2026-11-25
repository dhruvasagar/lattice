//! PF.6b: what a screen scroll costs in a long PDF.
//!
//! Since PF.4a the scroll walks charge each line the rows its media reserves
//! (`weighted_doc_line_cost` → `media_rows_below`), so `<C-f>` in a PDF reads
//! the buffer's media cache once per line it walks. These rows put that cost
//! beside the same keys in a plain text file, where the walk never finds
//! media, on a peer that draws pixels (cell metrics published) in both.
//!
//! - `screen_500_pages` / `half_screen_500_pages`: `<C-f>` then `<C-b>`, and
//!   `<C-d>` then `<C-u>`, in a 500-page PDF in fit page. A pair, so every
//!   iteration starts from the same place.
//! - `screen_text_2000_lines`: the same `<C-f>` / `<C-b>` pair in a 2000-line
//!   file, the baseline.
//!
//! Each row is a keystroke on the actor, so the bar is the keystroke one:
//! nowhere near a frame.
//!
//! Run:
//!
//!   cargo bench -p lattice-host --bench pdf_scroll

use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};

use lattice_core::Document;
use lattice_host::editor::Editor;

/// A minimal, well-formed PDF of `pages` US-letter pages (the test fixture's
/// generator).
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

/// A pane 40 rows of 20 px, 80 columns of 10 px, as the GPUI peer publishes.
fn drawing_editor() -> Editor {
    let mut editor = Editor::boot(Document::from_text("x\n"));
    editor.viewport_height = 41;
    editor.pane_tree.active_mut().viewport_width = 80;
    editor.dispatch(lattice_host::action::Action::SetCellMetrics {
        row_px: 20.0,
        col_px: 10.0,
    });
    editor
}

/// Tick on every wake until `done`, as the actor does.
async fn settle(editor: &mut Editor, mut done: impl FnMut(&Editor) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done(editor) {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "the document never settled");
        let woke = editor.async_landed.clone();
        let _ = tokio::time::timeout(left, woke.notified()).await;
        editor.run_tick_pending();
    }
}

fn media_blocks(editor: &Editor) -> usize {
    use lattice_host::per_buffer_cache::PerBufferCacheExt;
    editor
        .wasm_media
        .cache
        .get_for(editor.document_buffer_id)
        .map_or(0, |c| c.blocks.len())
}

fn press(editor: &mut Editor, spelling: &str) {
    let mut partial = Vec::new();
    for chord in lattice_protocol::chord::parse_chord_sequence(spelling).expect("a chord") {
        let _ = editor.dispatch_chord(chord, &mut partial);
    }
}

fn bench_pdf_scroll(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a runtime");
    let dir = tempfile::tempdir().expect("a temp dir");

    let pdf = dir.path().join("paper.pdf");
    std::fs::write(&pdf, pdf_bytes(500)).expect("write the pdf");
    let mut paper = drawing_editor();
    rt.block_on(async {
        paper.do_edit(Some(pdf.clone()), false);
        settle(&mut paper, |e| media_blocks(e) == 500).await;
    });
    press(&mut paper, "<C-f>");
    assert_eq!(paper.cursor.line, 1, "a screen is a page");
    press(&mut paper, "gg");

    let txt = dir.path().join("notes.txt");
    std::fs::write(&txt, "a line of text\n".repeat(2000)).expect("write the text");
    let mut notes = drawing_editor();
    rt.block_on(async {
        notes.do_edit(Some(txt.clone()), false);
        notes.run_tick_pending();
    });

    let mut group = c.benchmark_group("pdf_scroll");
    group.bench_function("screen_500_pages", |b| {
        b.iter(|| press(&mut paper, "<C-f><C-b>"));
    });
    group.bench_function("half_screen_500_pages", |b| {
        b.iter(|| press(&mut paper, "<C-d><C-u>"));
    });
    group.bench_function("screen_text_2000_lines", |b| {
        b.iter(|| press(&mut notes, "<C-f><C-b>"));
    });
    group.finish();
}

criterion_group!(benches, bench_pdf_scroll);
criterion_main!(benches);
