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

/// Build an App with Rust syntax wired up. display-line B4.2: the
/// `refresh_highlights` prime was removed with the overlay worker's
/// dead span/row cache; syntax now flows through the cells /
/// `DisplayMatrix` substrate (rebuilt on publish), so the frame
/// bench exercises that path via `compose_visible_lines`.
fn build_app(corpus: &str, viewport: u32) -> App {
    let mut a = App::new(Document::from_text(corpus));
    a.set_viewport_height(viewport);
    // Slice 3 / SyntaxActor: build a `Syntax`, parse synchronously,
    // wrap it in a `SyntaxHandle::seeded` so the bench sees an
    // already-populated snapshot from the first frame.
    // Slice 3c.final.E.swap: bench routes through the actor-aware
    // seam (`read_editor` / `mutate_editor`) instead of `&a.editor.X`.
    let mut syn = Syntax::for_language(Lang::Rust).unwrap().unwrap();
    let doc_text = a.read_editor(|e| e.document.text().to_string());
    syn.parse(&doc_text);
    let handle = lattice_syntax::SyntaxHandle::seeded(syn);
    a.mutate_editor(move |e| e.syntax = Some(handle));
    a
}

/// Pin an `Arc<DocumentSnapshot>` once -- mirrors the runtime's
/// per-frame `snapshot_cache.load_arc()`. Re-loading inside `iter`
/// would dominate the measurement and isn't representative of the
/// real frame path (one load amortised across the whole compose).
fn pinned_snapshot(app: &mut App) -> Arc<DocumentSnapshot> {
    // Slice 3c.final.E.swap: snapshot via the published `ad()`
    // mirror (Arc-bump clone off the same source as
    // `editor.snapshot_cache.load_arc()`).
    app.ad().snapshot.clone()
}

fn frame_render_24(c: &mut Criterion) {
    let mut g = c.benchmark_group("render::frame_24_lines");
    for n in [10usize, 200, 2000] {
        let corpus = rust_corpus(n);
        let mut app = build_app(&corpus, 24);
        let snap = pinned_snapshot(&mut app);
        g.bench_with_input(
            BenchmarkId::from_parameter(n),
            &(app, snap),
            |bencher, (a, s)| {
                bencher.iter(|| {
                    let lines = compose_visible_lines(black_box(a), black_box(s), 24, 80);
                    black_box(lines);
                });
            },
        );
    }
    g.finish();
}

fn frame_render_60(c: &mut Criterion) {
    let mut g = c.benchmark_group("render::frame_60_lines");
    for n in [10usize, 200, 2000] {
        let corpus = rust_corpus(n);
        let mut app = build_app(&corpus, 60);
        let snap = pinned_snapshot(&mut app);
        g.bench_with_input(
            BenchmarkId::from_parameter(n),
            &(app, snap),
            |bencher, (a, s)| {
                bencher.iter(|| {
                    let lines = compose_visible_lines(black_box(a), black_box(s), 60, 200);
                    black_box(lines);
                });
            },
        );
    }
    g.finish();
}

fn frame_render_120(c: &mut Criterion) {
    let mut g = c.benchmark_group("render::frame_120_lines");
    for n in [10usize, 200, 2000] {
        let corpus = rust_corpus(n);
        let mut app = build_app(&corpus, 120);
        let snap = pinned_snapshot(&mut app);
        g.bench_with_input(
            BenchmarkId::from_parameter(n),
            &(app, snap),
            |bencher, (a, s)| {
                bencher.iter(|| {
                    let lines = compose_visible_lines(black_box(a), black_box(s), 120, 200);
                    black_box(lines);
                });
            },
        );
    }
    g.finish();
}

// display-line B4.2: `refresh_highlights_cache_hit` was deleted. It
// benched the steady-state cost of `App::refresh_highlights`, which
// was removed with the overlay worker's dead span/row cache. The
// equivalent worker-side cache-hit cost is now covered by
// `lattice-host`'s `overlay_worker` bench (`overlay_worker_cache_hit`).

criterion_group!(render, frame_render_24, frame_render_60, frame_render_120,);
criterion_main!(render);
