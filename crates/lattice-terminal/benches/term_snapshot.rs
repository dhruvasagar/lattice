#![allow(clippy::unwrap_used, clippy::panic)]
//! T-snap-1 (2026-05-27): criterion bench guarding the
//! Insert→Normal mode-transition cost called out in
//! `docs/dev/architecture/terminal-as-document.md` §6.
//!
//! Target: `term_snapshot_build` p99 ≤ 2.0 ms at the default
//! 10 000-line scrollback × 200 cols. A regression above that
//! gate means the rope build is no longer free on the
//! mode-transition path and needs to move to a background task
//! (gating Normal-mode motions on completion, same shape as
//! cold-start LSP).
//!
//! Two sizes are measured:
//!   - `default` (10 000 × 200) — CI gate.
//!   - `stress`  (50 000 × 400) — stress observation; not gated.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use lattice_terminal::SharedTerm;

/// Build a fixture SharedTerm with `scrollback × cols` worth of
/// representative shell output already in the grid. Each row is
/// distinct so trailing-blank trim doesn't collapse everything
/// to one line and the rope build hits the realistic path.
fn build_fixture(rows: u16, cols: u16, scrollback: u32) -> SharedTerm {
    let shared = SharedTerm::fixture(rows, cols, scrollback);
    // Fill the grid + scrollback. Each line ~80 chars of mixed
    // content; the rest of the row pads with the grid's blank
    // cell and gets trimmed at snapshot time.
    let total_lines = scrollback as usize + rows as usize;
    let mut bytes = Vec::with_capacity(total_lines * 90);
    for i in 0..total_lines {
        let line = format!("  {i:>6}  the quick brown fox jumps over the lazy dog {i}\r\n",);
        bytes.extend_from_slice(line.as_bytes());
    }
    shared.feed_for_fixture(&bytes);
    shared
}

fn bench_term_snapshot_build(c: &mut Criterion) {
    let mut g = c.benchmark_group("term_snapshot_build");
    g.sample_size(50);
    for (label, rows, cols, scrollback) in [
        ("default_10k_200", 24u16, 200u16, 10_000u32),
        ("stress_50k_400", 24u16, 400u16, 50_000u32),
    ] {
        let shared = build_fixture(rows, cols, scrollback);
        let bytes_per_build = (rows as u64 + scrollback as u64) * cols as u64;
        g.throughput(Throughput::Bytes(bytes_per_build));
        g.bench_with_input(BenchmarkId::from_parameter(label), &shared, |bencher, s| {
            bencher.iter(|| {
                let snap = s.build_normal_snapshot();
                criterion::black_box(snap);
            });
        });
    }
    g.finish();
}

criterion_group!(benches, bench_term_snapshot_build);
criterion_main!(benches);
