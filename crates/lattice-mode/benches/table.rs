#![allow(clippy::unwrap_used)]
//! TB.4 — what a table realign actually costs, so the question "can it run on
//! every keystroke?" is answered with a number rather than a feeling.
//!
//! A realign is `Table::at` (walk the contiguous `|` run, parse each row) plus
//! `Table::render` (measure every cell by display width, then re-emit every
//! line). Both are O(cells), and `render` allocates a `String` per row — so
//! the cost is dominated by the table's total size, not by which cell moved.
//!
//! **Why this matters and what it decides.** Paramount goal #1 puts the
//! keystroke→glyph budget inside one display frame — 8.3ms at 120Hz — and
//! nothing on the keystroke path may spend a meaningful slice of it. But the
//! frame budget is the WEAKER of the two constraints here. The stronger one
//! is the keystroke UX contract: only the edited line may visibly change.
//! A realign rewrites every row, which is a pixel change to content the user
//! did not edit, on every character typed. That is the veto, and it holds no
//! matter how fast this comes out. See `docs/dev/architecture/table-mode.md`
//! §8.
//!
//! So the numbers here are not a gate this has to pass — they are the record
//! that the decision was made on the contract rather than on cost, and the
//! baseline that keeps a *field-exit* realign (`<Tab>`, `<CR>`, `<Esc>`)
//! honest as it is the one that does run interactively.

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};

use lattice_mode::modes::table::model::Table;

/// A table of `rows` × `cols`, with ragged-ish content so column widths are
/// not all equal — an all-`x` table would measure a best case nobody has.
fn table_text(rows: usize, cols: usize) -> Vec<String> {
    (0..rows)
        .map(|r| {
            let cells: Vec<String> = (0..cols)
                .map(|c| format!(" cell {r}-{c}{} ", "·".repeat((r + c) % 4)))
                .collect();
            format!("|{}|", cells.join("|"))
        })
        .collect()
}

fn lines_of(text: &[String]) -> impl Fn(u32) -> Option<String> + '_ {
    move |n: u32| text.get(n as usize).cloned()
}

/// Parse + render, which together are one realign.
fn bench_realign(c: &mut Criterion) {
    let mut group = c.benchmark_group("table_realign");
    // 5 rows: a table someone actually typed. 50: a long one. 500: past what
    // anyone writes by hand, included so the linear scaling is visible rather
    // than inferred.
    for rows in [5usize, 50, 500] {
        let text = table_text(rows, 5);
        let n = text.len() as u32;
        group.bench_with_input(BenchmarkId::from_parameter(rows), &rows, |b, _| {
            b.iter(|| {
                let table = Table::at(lines_of(&text), 0, n).unwrap();
                black_box(table.render());
            });
        });
    }
    group.finish();
}

/// The parse alone, so a regression can be attributed to one half or the
/// other rather than to "tables got slower".
fn bench_parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("table_parse");
    for rows in [5usize, 50, 500] {
        let text = table_text(rows, 5);
        let n = text.len() as u32;
        group.bench_with_input(BenchmarkId::from_parameter(rows), &rows, |b, _| {
            b.iter(|| {
                black_box(Table::at(lines_of(&text), 0, n).unwrap());
            });
        });
    }
    group.finish();
}

/// `Table::at` on a line that is NOT a table — the answer every keystroke
/// outside a table gets, and the one that must be free.
///
/// This is the path a declining `<Tab>` takes in an ordinary markdown
/// paragraph, so it runs far more often than a realign ever will. It should
/// be a single `starts_with` and out.
fn bench_miss(c: &mut Criterion) {
    let text: Vec<String> = (0..500).map(|i| format!("prose line {i}")).collect();
    c.bench_function("table_at_miss", |b| {
        b.iter(|| {
            black_box(Table::at(lines_of(&text), 250, 500));
        });
    });
}

/// The code-block scan, which is the one part of recognition that is NOT
/// O(1) on a miss: it counts fence delimiters from the top of the file.
///
/// Benched deliberately, because it is the cost TB.1's fix introduced and the
/// thing someone will reach for if a table chord ever feels slow in a long
/// document. A table 2000 lines down a file pays for all 2000.
fn bench_deep_in_file(c: &mut Criterion) {
    let mut text: Vec<String> = (0..2000).map(|i| format!("prose line {i}")).collect();
    text.extend(table_text(5, 5));
    let n = text.len() as u32;
    c.bench_function("table_at_2000_lines_deep", |b| {
        b.iter(|| {
            black_box(Table::at(lines_of(&text), 2000, n));
        });
    });
}

criterion_group!(
    benches,
    bench_realign,
    bench_parse,
    bench_miss,
    bench_deep_in_file
);
criterion_main!(benches);
