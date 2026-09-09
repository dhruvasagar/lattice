//! RF.1: the reflow engine's cost, and the number RF.3's keystroke
//! budget is read against.
//!
//! Two shapes, because they answer different questions:
//!
//! - `reflow_paragraph` — a `gqap`-sized fill. User-initiated, so it has
//!   no frame budget; the number exists so a later change that makes it
//!   quadratic is visible.
//! - `reflow_break_point` — the work auto-wrap does per keystroke
//!   (RF.3): one line, find the break. THIS one has a budget — it runs
//!   inside the typing path, where paramount #1 allows no I/O and no
//!   parse and very little of anything else.

use criterion::{Criterion, criterion_group, criterion_main};
use lattice_grammar::reflow::{ReflowConfig, auto_wrap_break, reflow_range};
use std::hint::black_box;

/// A paragraph of realistic prose — the `gqap` case.
fn paragraph(lines: usize) -> Vec<String> {
    (0..lines)
        .map(|i| {
            format!(
                "/// line {i} of a doc comment that runs past the margin and \
                 therefore has to be re-broken by the fill"
            )
        })
        .collect()
}

fn bench_paragraph(c: &mut Criterion) {
    let mut group = c.benchmark_group("reflow_paragraph");
    for lines in [10usize, 200] {
        let owned = paragraph(lines);
        let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
        group.bench_function(format!("{lines}_lines"), |b| {
            b.iter(|| {
                let out = reflow_range(
                    black_box(&refs),
                    ReflowConfig {
                        textwidth: 80,
                        line_comment: Some("//"),
                    },
                );
                black_box(out)
            })
        });
    }
    group.finish();
}

/// The per-keystroke shape: ONE line, at the moment it grows past the
/// margin. Auto-wrap (RF.3) does exactly this much work per character
/// typed past `textwidth`, so this is the number that has to stay far
/// under a frame.
///
/// Both arms matter and only one of them is rare:
///
/// - `no_break` is the OVERWHELMINGLY common case — every keystroke on
///   a line that has not reached the margin pays this and nothing else.
///   It is the number paramount #1 actually constrains.
/// - `breaking` is the frame where the line does wrap.
fn bench_break_point(c: &mut Criterion) {
    let cfg = ReflowConfig {
        textwidth: 80,
        line_comment: Some("//"),
    };
    let mut group = c.benchmark_group("reflow_break_point");

    // Short line: under the margin, so `auto_wrap_break` returns None
    // after one width measure. What every ordinary keystroke costs.
    let short = "    // a short comment line";
    group.bench_function("no_break", |b| {
        b.iter(|| black_box(auto_wrap_break(black_box(short), short.len(), cfg)))
    });

    // A line that has just passed the margin and must be scanned for
    // its break point.
    let long = format!("    // {}", "word ".repeat(24).trim_end());
    group.bench_function("breaking", |b| {
        b.iter(|| black_box(auto_wrap_break(black_box(&long), long.len(), cfg)))
    });

    // The operator's per-line cost, for comparison.
    let refs = [long.as_str()];
    group.bench_function("reflow_one_line", |b| {
        b.iter(|| black_box(reflow_range(black_box(&refs), cfg)))
    });
    group.finish();
}

criterion_group!(benches, bench_paragraph, bench_break_point);
criterion_main!(benches);
