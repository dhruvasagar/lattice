#![allow(clippy::unwrap_used, clippy::panic)]
//! Criterion benchmarks for `lattice_core::search`.
//!
//! Search is a Reflex-class command (vim's `/`, `n`, `?`); it must
//! complete inside the keystroke budget on representative buffers.

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};

use lattice_core::Buffer;
use lattice_core::search::{Direction, find};
use lattice_protocol::position::Position;

fn build_buffer(n_lines: usize) -> String {
    let mut s = String::with_capacity(n_lines * 64);
    for i in 0..n_lines {
        s.push_str(&format!(
            "fn handler_{i}(input: &str) -> Result<Output, Error> {{ Ok(()) }}\n"
        ));
    }
    s
}

fn search_forward_first_match(c: &mut Criterion) {
    let mut g = c.benchmark_group("search::forward_first_match");
    for size in [10usize, 1_000, 50_000, 200_000] {
        let text = build_buffer(size);
        let buffer = Buffer::from_text(&text);
        g.throughput(Throughput::Bytes(text.len() as u64));
        g.bench_with_input(
            BenchmarkId::from_parameter(size),
            &buffer,
            |bencher, buf| {
                bencher.iter(|| {
                    let _ = find(
                        buf,
                        black_box("Result"),
                        Position::ZERO,
                        Direction::Forward,
                    )
                    .unwrap();
                });
            },
        );
    }
    g.finish();
}

fn search_forward_last_match(c: &mut Criterion) {
    // Worst-case forward scan: pattern is at the very end of the buffer.
    let mut g = c.benchmark_group("search::forward_last_match");
    for size in [10usize, 1_000, 50_000, 200_000] {
        let mut text = build_buffer(size);
        text.push_str("// MARKER\n");
        let buffer = Buffer::from_text(&text);
        g.throughput(Throughput::Bytes(text.len() as u64));
        g.bench_with_input(
            BenchmarkId::from_parameter(size),
            &buffer,
            |bencher, buf| {
                bencher.iter(|| {
                    let _ = find(
                        buf,
                        black_box("MARKER"),
                        Position::ZERO,
                        Direction::Forward,
                    )
                    .unwrap();
                });
            },
        );
    }
    g.finish();
}

fn search_no_match_with_wrap(c: &mut Criterion) {
    // Worst case: pattern not present anywhere -- forces a full forward scan
    // and a full wrap pass before returning None.
    let mut g = c.benchmark_group("search::no_match_with_wrap");
    for size in [10usize, 1_000, 50_000, 200_000] {
        let text = build_buffer(size);
        let buffer = Buffer::from_text(&text);
        g.throughput(Throughput::Bytes(text.len() as u64));
        g.bench_with_input(
            BenchmarkId::from_parameter(size),
            &buffer,
            |bencher, buf| {
                bencher.iter(|| {
                    let _ = find(
                        buf,
                        black_box("ZZZ_MISSING"),
                        Position::new((size as u32) / 2, 0),
                        Direction::Forward,
                    )
                    .unwrap();
                });
            },
        );
    }
    g.finish();
}

fn search_backward(c: &mut Criterion) {
    let mut g = c.benchmark_group("search::backward");
    for size in [10usize, 1_000, 50_000] {
        let text = build_buffer(size);
        let buffer = Buffer::from_text(&text);
        g.throughput(Throughput::Bytes(text.len() as u64));
        g.bench_with_input(
            BenchmarkId::from_parameter(size),
            &buffer,
            |bencher, buf| {
                bencher.iter(|| {
                    let _ = find(
                        buf,
                        black_box("Result"),
                        Position::new((size as u32).saturating_sub(1), 0),
                        Direction::Backward,
                    )
                    .unwrap();
                });
            },
        );
    }
    g.finish();
}

criterion_group!(
    benches,
    search_forward_first_match,
    search_forward_last_match,
    search_no_match_with_wrap,
    search_backward,
);
criterion_main!(benches);
