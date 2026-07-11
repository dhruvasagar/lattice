//! D.1 bench: `diff_recompute_p99_us` at v1 P95 file size +
//! stress observation at 50k × 200.
//!
//! Per `docs/dev/architecture/diff-system.md` §7, the CI gate
//! is `<= 1000µs` at 5k × 80 cols (v1 P95). The bench is
//! recorded in D.1 but not yet enforced — gate-enforcement
//! wires in via D.2 when the subsystem owns the bench harness.

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use lattice_diff::{DiffAlgorithm, compute_diff};
use ropey::Rope;

/// Synthesise a rope of `n` lines, each `cols` columns wide.
/// Content varies per line so the diff engine sees real
/// hashing/comparison work rather than a trivial all-equal
/// case.
fn synth_rope(lines: usize, cols: usize) -> Rope {
    let mut s = String::with_capacity(lines * (cols + 1));
    for i in 0..lines {
        let label = i % 8;
        // Each line is a label byte repeated to `cols`.
        let ch = (b'a' + label as u8) as char;
        for _ in 0..cols {
            s.push(ch);
        }
        s.push('\n');
    }
    Rope::from(s.as_str())
}

/// Mutate ~`frac` of lines in `a` by changing the leading
/// char. Produces realistic "small edit on a big file" diff
/// input.
fn mutate(a: &Rope, frac: f64) -> Rope {
    let s = a.to_string();
    let step = ((1.0 / frac) as usize).max(1);
    let mut out = String::with_capacity(s.len());
    for (i, line) in s.split_inclusive('\n').enumerate() {
        if i % step == 0 && line.len() > 1 {
            out.push('Z');
            out.push_str(&line[1..]);
        } else {
            out.push_str(line);
        }
    }
    Rope::from(out.as_str())
}

fn bench_two_way(c: &mut Criterion) {
    let mut group = c.benchmark_group("diff_two_way");

    let a_5k = synth_rope(5_000, 80);
    let b_5k = mutate(&a_5k, 0.01);

    for alg in [
        DiffAlgorithm::Histogram,
        DiffAlgorithm::Myers,
        DiffAlgorithm::MyersMinimal,
    ] {
        group.bench_with_input(
            BenchmarkId::new("5k_x_80_1pct_edit", format!("{alg:?}")),
            &alg,
            |b, &alg| {
                b.iter(|| {
                    compute_diff(black_box(&[a_5k.clone(), b_5k.clone()]), alg)
                        .expect("two-way is supported")
                });
            },
        );
    }

    // Stress observation. Not gated; recorded for sizing.
    let a_50k = synth_rope(50_000, 200);
    let b_50k = mutate(&a_50k, 0.001);
    group.sample_size(10);
    group.bench_function(
        BenchmarkId::new("50k_x_200_0.1pct_edit", "Histogram"),
        |b| {
            b.iter(|| {
                compute_diff(
                    black_box(&[a_50k.clone(), b_50k.clone()]),
                    DiffAlgorithm::Histogram,
                )
                .expect("two-way is supported")
            });
        },
    );

    group.finish();
}

fn bench_three_way(c: &mut Criterion) {
    let mut group = c.benchmark_group("diff_three_way");

    let base = synth_rope(5_000, 80);
    let local = mutate(&base, 0.01);
    let remote = mutate(&base, 0.015);

    group.bench_function("5k_x_80_two_sides_edited", |b| {
        b.iter(|| {
            compute_diff(
                black_box(&[base.clone(), local.clone(), remote.clone()]),
                DiffAlgorithm::Histogram,
            )
            .expect("three-way is supported")
        });
    });

    group.finish();
}

criterion_group!(benches, bench_two_way, bench_three_way);
criterion_main!(benches);
