#![allow(clippy::unwrap_used, clippy::panic)]
//! Criterion benchmarks for the fold providers.
//!
//! Three providers, three sizes each (small / medium / large) so a
//! regression on either small-file ergonomics or large-file scaling
//! surfaces in CI. The syntax provider also exercises the
//! tree-sitter parse path because `compute_syntax_folds` consumes
//! the live `Tree`; the indent + markdown providers take the
//! buffer directly.
//!
//! Backs the §8 latency commitment: fold recompute runs on every
//! reparse and must stay sub-frame on realistic buffers.

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};

use lattice_core::Buffer;
use lattice_protocol::edit::Edit;
use lattice_protocol::position::Position;
use lattice_syntax::{Lang, Syntax};
use lattice_ui_tui::folds::{compute_indent_folds, compute_markdown_folds, compute_syntax_folds};

/// Build a Rust-shaped corpus of `n` function items. Each item has
/// a body so the indent + syntax providers actually have something
/// to fold.
fn rust_corpus(n: usize) -> String {
    let mut s = String::with_capacity(n * 80);
    for i in 0..n {
        s.push_str(&format!(
            "fn handler_{i}(input: &str) -> Result<Output, Error> {{\n    let mut acc = 0;\n    if input.is_empty() {{\n        return Ok(acc);\n    }}\n    Ok(acc + 1)\n}}\n\n"
        ));
    }
    s
}

/// Build a markdown corpus of `n` H2 sections. Indent + markdown
/// providers both have something to fold.
fn markdown_corpus(n: usize) -> String {
    let mut s = String::with_capacity(n * 200);
    s.push_str("# Top\n\n");
    for i in 0..n {
        s.push_str(&format!("## Section {i}\n\nbody line one for section {i}.\nbody line two for section {i}.\n\n"));
    }
    s
}

fn make_buffer(text: &str) -> Buffer {
    let mut b = Buffer::empty();
    if !text.is_empty() {
        b.apply_edit(&Edit::insert(Position::ZERO, text.to_string()))
            .unwrap();
    }
    b
}

fn bench_indent(c: &mut Criterion) {
    let mut g = c.benchmark_group("folds::compute_indent");
    for size in [10usize, 200, 2000] {
        let text = rust_corpus(size);
        let buf = make_buffer(&text);
        g.bench_with_input(BenchmarkId::from_parameter(size), &buf, |bencher, b| {
            bencher.iter(|| {
                let folds = compute_indent_folds(black_box(b));
                black_box(folds);
            });
        });
    }
    g.finish();
}

fn bench_markdown(c: &mut Criterion) {
    let mut g = c.benchmark_group("folds::compute_markdown");
    for size in [10usize, 100, 500] {
        let text = markdown_corpus(size);
        let buf = make_buffer(&text);
        g.bench_with_input(BenchmarkId::from_parameter(size), &buf, |bencher, b| {
            bencher.iter(|| {
                let folds = compute_markdown_folds(black_box(b));
                black_box(folds);
            });
        });
    }
    g.finish();
}

fn bench_syntax_rust(c: &mut Criterion) {
    let mut g = c.benchmark_group("folds::compute_syntax_rust");
    for size in [10usize, 200, 2000] {
        let text = rust_corpus(size);
        // Pre-parse so the bench measures fold-query work, not the
        // tree-sitter parse. The parse cost is measured separately
        // by the highlight benches when we add them.
        g.bench_with_input(BenchmarkId::from_parameter(size), &text, |bencher, src| {
            let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
            s.parse(src);
            bencher.iter(|| {
                let folds = compute_syntax_folds(black_box(&s));
                black_box(folds);
            });
        });
    }
    g.finish();
}

criterion_group!(folds, bench_indent, bench_markdown, bench_syntax_rust);
criterion_main!(folds);
