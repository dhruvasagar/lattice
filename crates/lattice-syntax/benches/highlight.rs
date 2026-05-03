#![allow(clippy::unwrap_used, clippy::panic)]
//! Criterion benchmarks for the native (post-Option-B) highlight
//! pipeline. Times `Syntax::highlight_lines_native` per language
//! across small / medium / large corpora so regressions on the
//! `QueryCursor` traversal path surface in CI.
//!
//! The bench measures highlight ONLY -- the parse cost is paid in
//! `setup` and isn't counted, so the numbers reflect the pure
//! query-traversal + style-resolution cost. Real-world keystrokes
//! pay parse + highlight together; that's covered by the
//! `runtime::apply_edit_round_trip` and (planned) frame-render
//! benches.
//!
//! Backs §8.2 "Frame render (code, TUI)" rationale: the highlight
//! cost is one component; viewport-window-bounded inputs match
//! the renderer's actual call shape.

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};

use lattice_syntax::{Lang, Syntax};

fn rust_corpus(n_fns: usize) -> String {
    let mut s = String::with_capacity(n_fns * 80);
    for i in 0..n_fns {
        s.push_str(&format!(
            "fn handler_{i}(input: &str) -> Result<Output, Error> {{\n    let mut acc = 0;\n    if input.is_empty() {{\n        return Ok(acc);\n    }}\n    Ok(acc + 1)\n}}\n\n"
        ));
    }
    s
}

fn python_corpus(n_fns: usize) -> String {
    let mut s = String::with_capacity(n_fns * 80);
    for i in 0..n_fns {
        s.push_str(&format!(
            "def handler_{i}(input):\n    if not input:\n        return 0\n    acc = 0\n    return acc + 1\n\n"
        ));
    }
    s
}

fn markdown_corpus(n_sections: usize) -> String {
    let mut s = String::with_capacity(n_sections * 200);
    s.push_str("# Top\n\n");
    for i in 0..n_sections {
        s.push_str(&format!(
            "## Section {i}\n\nSome **bold** text and `code` for section {i}.\nAnother body line.\n\n"
        ));
    }
    s
}

/// Bench helper: parse once outside the timed loop, then time
/// the highlight call across the buffer's full line count. The
/// `Syntax` instance is mutable inside the loop because
/// `highlight_lines_native` takes `&mut self` even though it
/// doesn't reparse -- clean up if/when the API moves to `&self`.
fn bench_highlight(c: &mut Criterion, group_name: &str, lang: Lang, corpus: &str) {
    let mut g = c.benchmark_group(group_name);
    let line_count = corpus.split('\n').count() as u32;
    let mut s = Syntax::for_language(lang).unwrap().unwrap();
    s.parse(corpus);
    g.bench_function(BenchmarkId::from_parameter(line_count), |bencher| {
        bencher.iter(|| {
            let lines = s.highlight_lines_native(0, black_box(line_count)).unwrap();
            black_box(lines);
        });
    });
    g.finish();
}

fn highlight_rust(c: &mut Criterion) {
    for n in [10usize, 200, 2000] {
        bench_highlight(
            c,
            &format!("highlight::rust/{n}"),
            Lang::Rust,
            &rust_corpus(n),
        );
    }
}

fn highlight_python(c: &mut Criterion) {
    for n in [10usize, 200, 2000] {
        bench_highlight(
            c,
            &format!("highlight::python/{n}"),
            Lang::Python,
            &python_corpus(n),
        );
    }
}

fn highlight_markdown(c: &mut Criterion) {
    for n in [10usize, 100, 500] {
        bench_highlight(
            c,
            &format!("highlight::markdown/{n}"),
            Lang::Markdown,
            &markdown_corpus(n),
        );
    }
}

/// Viewport-bounded highlight: realistic call shape from the
/// renderer (~30 visible lines). The renderer never asks for the
/// full document at once -- this is the size that matters for
/// the keystroke-to-glyph budget.
fn highlight_rust_viewport(c: &mut Criterion) {
    let mut g = c.benchmark_group("highlight::rust_viewport");
    let corpus = rust_corpus(2000);
    let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
    s.parse(&corpus);
    for &(label, height) in &[("24_lines", 24u32), ("60_lines", 60), ("120_lines", 120)] {
        g.bench_with_input(BenchmarkId::from_parameter(label), &height, |bencher, &h| {
            bencher.iter(|| {
                let lines = s.highlight_lines_native(0, black_box(h)).unwrap();
                black_box(lines);
            });
        });
    }
    g.finish();
}

criterion_group!(
    highlight,
    highlight_rust,
    highlight_python,
    highlight_markdown,
    highlight_rust_viewport,
);
criterion_main!(highlight);
