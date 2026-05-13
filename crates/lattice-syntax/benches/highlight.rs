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

use criterion::{BatchSize, BenchmarkId, Criterion, black_box, criterion_group, criterion_main};

use lattice_protocol::Position;
use lattice_protocol::edit::EditDelta;
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
        g.bench_with_input(
            BenchmarkId::from_parameter(label),
            &height,
            |bencher, &h| {
                bencher.iter(|| {
                    let lines = s.highlight_lines_native(0, black_box(h)).unwrap();
                    black_box(lines);
                });
            },
        );
    }
    g.finish();
}

// ---- Slice B.2: incremental reparse benches ---------------------
//
// Backs §8.2's Read+Write-path rows that B.2 lights up. Reuses
// `rust_corpus(N)` from above so the fixture matches the existing
// `highlight::rust/N` rows -- numbers compose cleanly.

/// `Tree::edit` cost in isolation across corpus sizes.
///
/// **Important calibration note:** `tree.edit()` walks every
/// node whose byte range crosses the edit point. This is
/// O(tree_nodes), NOT constant. The §8.2 row's initial "500ns
/// floor" estimate was wrong -- the floor is per-node-count, and
/// modern parsers produce thousands of nodes per ~hundreds of
/// lines. The bench surfaces the real scaling.
///
/// Bypasses `Syntax` to time tree-sitter's primitive directly --
/// `Syntax::tree()` returns `&Tree` (immutable), and exposing a
/// mut accessor purely for benching would pollute the public
/// API. We drive `tree_sitter::Parser` + `Tree::edit` directly.
fn tree_edit_single_char(c: &mut Criterion) {
    use tree_sitter::Parser;
    let mut g = c.benchmark_group("tree_edit_single_char");
    let lang_fn = tree_sitter_rust::LANGUAGE;
    let inp = lattice_syntax::syntax::edit_delta_to_input_edit(EditDelta {
        start_byte: 100,
        old_end_byte: 100,
        new_end_byte: 101,
        start_position: Position::new(2, 12),
        old_end_position: Position::new(2, 12),
        new_end_position: Position::new(2, 13),
    });
    for &size in &[10usize, 200, 2000] {
        let corpus = rust_corpus(size);
        let mut parser = Parser::new();
        parser.set_language(&lang_fn.into()).unwrap();
        g.bench_with_input(
            BenchmarkId::from_parameter(size),
            &corpus,
            |bencher, src| {
                bencher.iter_batched(
                    || parser.parse(src, None).unwrap(),
                    |mut tree| {
                        tree.edit(black_box(&inp));
                        black_box(tree);
                    },
                    BatchSize::PerIteration,
                );
            },
        );
    }
    g.finish();
}

/// Realistic incremental keystroke: parse, then apply a single-
/// char InputEdit + reparse using the cached tree. Across corpus
/// sizes so scaling is visible. Backs §8.2's "Tree-sitter
/// incremental reparse" row -- numbers grow with document size
/// (because `tree.edit` walks all nodes), so we report 10/200/
/// 2000-fn variants alongside `reparse_full_baseline` to make
/// the speedup ratio visible.
fn reparse_incremental_single_char_change(c: &mut Criterion) {
    let mut g = c.benchmark_group("reparse_incremental_single_char_change");
    for &size in &[10usize, 200, 2000] {
        let corpus_a = rust_corpus(size);
        let mut corpus_b = corpus_a.clone();
        let insert_byte = corpus_a.len() / 2;
        corpus_b.insert(insert_byte, 'x');
        let prefix = &corpus_a[..insert_byte];
        let line = prefix.matches('\n').count() as u32;
        let col = (insert_byte - prefix.rfind('\n').map(|i| i + 1).unwrap_or(0)) as u32;
        let delta = EditDelta {
            start_byte: insert_byte as u32,
            old_end_byte: insert_byte as u32,
            new_end_byte: (insert_byte + 1) as u32,
            start_position: Position::new(line, col),
            old_end_position: Position::new(line, col),
            new_end_position: Position::new(line, col + 1),
        };
        g.bench_with_input(
            BenchmarkId::from_parameter(size),
            &(corpus_a, corpus_b, delta),
            |bencher, (a, b, d)| {
                bencher.iter_batched(
                    || {
                        let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
                        s.parse_at(a, 1);
                        s
                    },
                    |mut s| {
                        s.parse_at_with_edits(black_box(b), 2, 1, std::slice::from_ref(d));
                        black_box(s);
                    },
                    BatchSize::PerIteration,
                );
            },
        );
    }
    g.finish();
}

/// Falsification anchor: full reparse across the same sizes. If
/// incremental converges with full, the deltas are wrong or the
/// corpus is too small for tree-sitter's incremental path to
/// win. Target acceptance: incremental beats full at every size,
/// with the gap widening as N grows.
fn reparse_full_baseline(c: &mut Criterion) {
    let mut g = c.benchmark_group("reparse_full_baseline");
    for &size in &[10usize, 200, 2000] {
        let corpus = rust_corpus(size);
        g.bench_with_input(
            BenchmarkId::from_parameter(size),
            &corpus,
            |bencher, src| {
                bencher.iter_batched(
                    || Syntax::for_language(Lang::Rust).unwrap().unwrap(),
                    |mut s| {
                        s.parse_at(black_box(src), 1);
                        black_box(s);
                    },
                    BatchSize::PerIteration,
                );
            },
        );
    }
    g.finish();
}

criterion_group!(
    highlight,
    highlight_rust,
    highlight_python,
    highlight_markdown,
    highlight_rust_viewport,
    tree_edit_single_char,
    reparse_incremental_single_char_change,
    reparse_full_baseline,
);
criterion_main!(highlight);
