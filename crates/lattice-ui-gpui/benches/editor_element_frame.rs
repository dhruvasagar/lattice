//! Phase 5.8.AF.6 / Slice X5: GPUI frame-budget bench.
//!
//! Measures the pre-paint editor work the `EditorElement` does
//! per visible row -- the part goal-#1 ("UI thread does no I/O,
//! no parsing, no shaping") forbids dropping out of budget. The
//! actual `ShapedLine` materialisation + glyph rasterisation is
//! GPU-bound and not the regression target here; we bench the
//! string materialisation + span filtering + inlay splicing +
//! decoration column-remapping that runs on the renderer thread
//! before any `shape_line` call.
//!
//! Target: at 120Hz the per-frame budget is 8 ms. The editor-
//! side per-row work below should fit ~hundreds of rows inside
//! ~1 ms so the remaining 7 ms covers shaping + paint + tail.
//! Run:
//!
//!   cargo bench -p lattice-ui-gpui \
//!     --features window,bench-internals \
//!     --bench editor_element_frame
//!
//! The default suite covers three viewport sizes (24, 60, 120
//! visible lines) so a regression in any of them shows up as a
//! per-row latency ramp.

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};

use lattice_ui_gpui::editor_element::{build_line_with_inlays, byte_to_combined_col};

/// Synthetic Rust-like line ~80 chars long with two styled
/// regions (a keyword and a type) so the per-character `style_at`
/// walk has to advance through realistic span data.
fn rust_line(idx: usize) -> String {
    format!(
        "fn handler_{idx:04}(input: &str) -> Result<Output, Error> {{ Ok(()) }}",
    )
}

fn styled_spans(line: &str) -> Vec<lattice_syntax::StyledSpan> {
    // Approximate: mark `fn ` as Keyword, `Result<...>` as Type,
    // `Output` and `Error` as Type. We deliberately set up enough
    // spans that `style_at`'s linear walk dominates the per-row
    // cost the same way it does in production.
    let mut spans = Vec::new();
    if let Some(start) = line.find("fn ") {
        spans.push(lattice_syntax::StyledSpan {
            start,
            end: start + 2,
            style: lattice_syntax::Style::Keyword,
        });
    }
    if let Some(start) = line.find("Result") {
        spans.push(lattice_syntax::StyledSpan {
            start,
            end: start + "Result<Output, Error>".len(),
            style: lattice_syntax::Style::Type,
        });
    }
    spans
}

fn frame_budget(c: &mut Criterion) {
    // gpui::Font is opaque; built via the public `gpui::font()`
    // helper. The bench-internals build of the editor-element
    // helpers requires this type. Since `font()` constructs a
    // Font without touching the window system, we can call it
    // from a bench (it doesn't need a TestAppContext).
    let font = gpui::font("monospace");
    let inlay_color: u32 = 0x7f849c;

    let mut g = c.benchmark_group("editor_element_frame_pre_paint");
    for viewport in [24usize, 60, 120] {
        // One full-viewport pass: for each row build the run-
        // collapsed (text, runs, inlay_offsets) tuple, then walk
        // cursor + 6 decoration overlays through
        // `byte_to_combined_col` so every per-row code path the
        // EditorElement exercises in production is touched.
        g.bench_with_input(
            BenchmarkId::from_parameter(viewport),
            &viewport,
            |bencher, &n_rows| {
                let lines: Vec<String> = (0..n_rows).map(rust_line).collect();
                let all_spans: Vec<Vec<lattice_syntax::StyledSpan>> =
                    lines.iter().map(|l| styled_spans(l)).collect();
                bencher.iter(|| {
                    for (i, line) in lines.iter().enumerate() {
                        let spans = &all_spans[i];
                        // Inlay-free hot path (the common case in
                        // a Rust file with rust-analyzer off): no
                        // inlay splicing, but exercises the per-
                        // char `style_at` walk + run collapse.
                        let (text, runs, offsets) =
                            build_line_with_inlays(line, spans, &[], &font, inlay_color);
                        black_box(&text);
                        black_box(&runs);
                        // Cursor + 6 decoration column lookups.
                        let cursor_byte = line.len() / 2;
                        for byte in [
                            cursor_byte,
                            cursor_byte / 2,
                            cursor_byte / 4,
                            0,
                            line.len(),
                            line.len() - 1,
                            line.len() / 3,
                        ] {
                            black_box(byte_to_combined_col(line, byte, &offsets));
                        }
                    }
                });
            },
        );
    }
    g.finish();
}

fn frame_budget_with_inlays(c: &mut Criterion) {
    // The LSP-on case: ~3 inlay hints per row spliced into the
    // shaped line. Each inlay shifts every downstream
    // `byte_to_combined_col` lookup, so the per-row cost climbs
    // with both span count and inlay count.
    let font = gpui::font("monospace");
    let inlay_color: u32 = 0x7f849c;

    let mut g = c.benchmark_group("editor_element_frame_with_inlays");
    for viewport in [24usize, 60, 120] {
        g.bench_with_input(
            BenchmarkId::from_parameter(viewport),
            &viewport,
            |bencher, &n_rows| {
                let lines: Vec<String> = (0..n_rows).map(rust_line).collect();
                let all_spans: Vec<Vec<lattice_syntax::StyledSpan>> =
                    lines.iter().map(|l| styled_spans(l)).collect();
                // Pre-compute inlay tuples per row -- 3 inlays at
                // ~25 / 40 / 55 byte offsets, simulating LSP
                // parameter/type hints.
                let inlay_text_1 = ": &str".to_string();
                let inlay_text_2 = ": Output".to_string();
                let inlay_text_3 = " /* hint */".to_string();
                bencher.iter(|| {
                    for (i, line) in lines.iter().enumerate() {
                        let spans = &all_spans[i];
                        let n = line.len();
                        let inlays: [(usize, &str); 3] = [
                            (n.min(25), inlay_text_1.as_str()),
                            (n.min(40), inlay_text_2.as_str()),
                            (n.min(55), inlay_text_3.as_str()),
                        ];
                        let (text, runs, offsets) =
                            build_line_with_inlays(line, spans, &inlays, &font, inlay_color);
                        black_box(&text);
                        black_box(&runs);
                        for byte in [0, n / 4, n / 2, n.saturating_sub(1)] {
                            black_box(byte_to_combined_col(line, byte, &offsets));
                        }
                    }
                });
            },
        );
    }
    g.finish();
}

criterion_group!(frame, frame_budget, frame_budget_with_inlays);
criterion_main!(frame);
