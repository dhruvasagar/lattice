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
use lattice_grammar::reflow::{ReflowConfig, reflow_range};
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
fn bench_break_point(c: &mut Criterion) {
    let line = format!("    // {}", "word ".repeat(24).trim_end());
    let refs = [line.as_str()];
    c.bench_function("reflow_break_point/one_line", |b| {
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

criterion_group!(benches, bench_paragraph, bench_break_point);
criterion_main!(benches);
