#![allow(clippy::unwrap_used, clippy::panic)]
//! IN.2: what predictive indent costs on the keystroke path.
//!
//! `<CR>`, `o` and `O` now consult the tree-sitter `indents.scm` query
//! before inserting a newline. That is new work on the path paramount
//! goal #1 governs, and "a tree walk is cheap" is exactly the claim a
//! bench exists to stop anyone making on trust.
//!
//! Three measurements. The first is the guard that matters; the second
//! is the evidence that deleted a design branch.
//!
//! - `indent_query/*` — the steady-state cost: ancestor walk plus one
//!   `indents.scm` run against a fresh snapshot. This is what every
//!   `<CR>` in a parsed buffer pays. **It must not scale with file
//!   size.** The first version did — 623 µs at 800 lines, 2.57 ms at
//!   3200 — because the query ran over the whole file; scoping it to
//!   the enclosing top-level item is what fixed that, and this bench is
//!   what would catch the regression.
//! - `indent_reparse/*` — a full re-parse swept over file size. Kept as
//!   a *negative* result: 1.9 ms at 16 KB, 7.6 ms at 64 KB, 15.4 ms at
//!   129 KB. This is why predictive indent has **no** synchronous-
//!   reparse branch. Anyone tempted to add one to fix a stale-indent
//!   report should read these numbers first.
//! - `indent_method/*` — the same call under `syntax` / `keep` / `none`.
//!   The spread between them is the whole cost of the feature.
//!
//! Numbers land in `docs/dev/operations/benchmarks.md`.

use criterion::{BatchSize, Criterion, black_box, criterion_group, criterion_main};
use lattice_core::DocumentBuilder;
use lattice_host::editor::Editor;

/// A Rust source file of roughly `functions * 6` lines, nested deeply
/// enough that the ancestor walk has real work to do.
fn rust_source(functions: usize) -> String {
    let mut s = String::with_capacity(functions * 160);
    for i in 0..functions {
        s.push_str(&format!(
            "fn function_{i}(arg: i32) -> i32 {{\n\
             \x20   let mut total = arg;\n\
             \x20   if total > 0 {{\n\
             \x20       total += compute_{i}(total, \"a string with a {{ brace\");\n\
             \x20   }}\n\
             \x20   total\n\
             }}\n\n"
        ));
    }
    s
}

fn editor_with(src: &str) -> Editor {
    let editor = Editor::boot(
        DocumentBuilder::default()
            .with_path("bench.rs")
            .with_text(src)
            .build(),
    );
    // Let the syntax worker publish before measuring: this bench is
    // about the query, not about racing the parse.
    for _ in 0..200 {
        if editor
            .syntax
            .as_ref()
            .map(|h| h.snapshot().tree().is_some())
            .unwrap_or(false)
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    editor
}

/// Steady-state: the indent answer for a line deep inside the file,
/// against a fresh snapshot.
fn bench_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("indent_query");
    for functions in [10usize, 100, 400] {
        let src = rust_source(functions);
        let editor = editor_with(&src);
        // Aim at the middle of the file so the ancestor walk and the
        // bounded query both do representative work.
        let line = (editor.active_text().rope_line_count() / 2).max(1);
        group.bench_function(format!("lines_{}", src.lines().count()), |b| {
            b.iter(|| black_box(editor.auto_indent_for_new_line(black_box(line), None)))
        });
    }
    group.finish();
}

/// A full re-parse, swept over size. Retained as the evidence that
/// predictive indent must never do this synchronously -- see the module
/// header.
fn bench_reparse(c: &mut Criterion) {
    let mut group = c.benchmark_group("indent_reparse");
    for functions in [10usize, 50, 100, 200, 400, 800] {
        let src = rust_source(functions);
        let bytes = src.len();
        group.bench_function(format!("bytes_{bytes}"), |b| {
            b.iter_batched(
                || src.clone(),
                |text| {
                    let mut syntax =
                        lattice_syntax::Syntax::for_language(lattice_syntax::Lang::Rust)
                            .unwrap()
                            .unwrap();
                    syntax.parse(&text);
                    black_box(lattice_syntax::tree_levels_for_new_line(
                        &syntax.snapshot_owned(),
                        text.len() / 2,
                    ))
                },
                BatchSize::SmallInput,
            )
        });
    }
    group.finish();
}

/// The three `indentmethod` settings, measured on the same call the
/// keystroke path makes. This is the syntax-vs-keep-vs-none comparison.
///
/// An earlier revision benched the end-to-end `o` keystroke instead and
/// had to be thrown away: it built a fresh `Editor` in the setup
/// closure, so it measured editor construction and reported `none` as
/// 3.5× SLOWER than `syntax` with 70% spread. A benchmark whose ranking
/// is backwards is worse than no benchmark — it would have been quoted.
/// Measuring the one call that differs between the settings gives a
/// stable number that means what it says.
fn bench_methods(c: &mut Criterion) {
    let mut group = c.benchmark_group("indent_method");
    let src = rust_source(100);
    let mut editor = editor_with(&src);
    let line = editor.active_text().rope_line_count() / 2;
    for method in ["syntax", "keep", "none"] {
        editor.do_set(&format!("indentmethod={method}"));
        group.bench_function(method, |b| {
            b.iter(|| black_box(editor.auto_indent_for_new_line(black_box(line), None)))
        });
    }
    group.finish();
}

criterion_group!(benches, bench_query, bench_reparse, bench_methods);
criterion_main!(benches);
