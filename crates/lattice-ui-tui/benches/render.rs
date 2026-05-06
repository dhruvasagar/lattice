#![allow(clippy::unwrap_used, clippy::panic)]
//! Criterion benchmarks for the renderer's view-composition path.
//!
//! Times `compose_visible_lines` end-to-end: highlight cache
//! lookup + per-line styled-span build + visual-overlay
//! composition + ratatui `Line` materialisation. The actual
//! terminal write goes through ratatui's `Terminal::draw` which
//! is hardware-bound and not interesting to bench in this
//! crate -- closing the §8.2 frame-render row needs a more
//! integrated bench (planned).
//!
//! Backs the §8.2 "Frame render (code, TUI)" rationale: the
//! highlight cache + viewport composition is the editor-side
//! cost the user pays per keystroke under the renderer.

use std::sync::Arc;

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};

use lattice_core::Document;
use lattice_runtime::DocumentSnapshot;
use lattice_syntax::{Lang, Syntax};
use lattice_ui_tui::app::App;
use lattice_ui_tui::render::compose_visible_lines;

fn rust_corpus(n_fns: usize) -> String {
    let mut s = String::with_capacity(n_fns * 80);
    for i in 0..n_fns {
        s.push_str(&format!(
            "fn handler_{i}(input: &str) -> Result<Output, Error> {{\n    let mut acc = 0;\n    if input.is_empty() {{\n        return Ok(acc);\n    }}\n    Ok(acc + 1)\n}}\n\n"
        ));
    }
    s
}

/// Build an App with Rust syntax wired up + a fresh highlight
/// cache. Pre-warm by calling `refresh_highlights` so the bench
/// measures the *frame* path, not the cold-cache path.
fn build_app(corpus: &str, viewport: u32) -> App {
    let mut a = App::new(Document::from_text(corpus));
    a.set_viewport_height(viewport);
    // Slice 3 / SyntaxActor: build a `Syntax`, parse synchronously,
    // wrap it in a `SyntaxHandle::seeded` so the bench sees an
    // already-populated snapshot from the first frame.
    let mut syn = Syntax::for_language(Lang::Rust).unwrap().unwrap();
    syn.parse(&a.document.text());
    a.syntax = Some(lattice_syntax::SyntaxHandle::seeded(syn));
    a.refresh_highlights();
    a
}

/// Pin an `Arc<DocumentSnapshot>` once -- mirrors the runtime's
/// per-frame `snapshot_cache.load_arc()`. Re-loading inside `iter`
/// would dominate the measurement and isn't representative of the
/// real frame path (one load amortised across the whole compose).
fn pinned_snapshot(app: &App) -> Arc<DocumentSnapshot> {
    app.document.snapshot()
}

fn frame_render_24(c: &mut Criterion) {
    let mut g = c.benchmark_group("render::frame_24_lines");
    for n in [10usize, 200, 2000] {
        let corpus = rust_corpus(n);
        let app = build_app(&corpus, 24);
        let snap = pinned_snapshot(&app);
        g.bench_with_input(BenchmarkId::from_parameter(n), &(app, snap), |bencher, (a, s)| {
            bencher.iter(|| {
                let lines = compose_visible_lines(black_box(a), black_box(s), 24, 80);
                black_box(lines);
            });
        });
    }
    g.finish();
}

fn frame_render_60(c: &mut Criterion) {
    let mut g = c.benchmark_group("render::frame_60_lines");
    for n in [10usize, 200, 2000] {
        let corpus = rust_corpus(n);
        let app = build_app(&corpus, 60);
        let snap = pinned_snapshot(&app);
        g.bench_with_input(BenchmarkId::from_parameter(n), &(app, snap), |bencher, (a, s)| {
            bencher.iter(|| {
                let lines = compose_visible_lines(black_box(a), black_box(s), 60, 200);
                black_box(lines);
            });
        });
    }
    g.finish();
}

fn frame_render_120(c: &mut Criterion) {
    let mut g = c.benchmark_group("render::frame_120_lines");
    for n in [10usize, 200, 2000] {
        let corpus = rust_corpus(n);
        let app = build_app(&corpus, 120);
        let snap = pinned_snapshot(&app);
        g.bench_with_input(BenchmarkId::from_parameter(n), &(app, snap), |bencher, (a, s)| {
            bencher.iter(|| {
                let lines = compose_visible_lines(black_box(a), black_box(s), 120, 200);
                black_box(lines);
            });
        });
    }
    g.finish();
}

criterion_group!(render, frame_render_24, frame_render_60, frame_render_120);
criterion_main!(render);
