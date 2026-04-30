#![allow(clippy::unwrap_used, clippy::panic)]
//! Criterion benchmarks for `lattice_core::Buffer` edit + position
//! conversion. These are the hottest hot paths -- every keystroke
//! mutates the rope.

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};

use lattice_core::Buffer;
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range};

fn build_buffer(n_lines: usize) -> String {
    let mut s = String::with_capacity(n_lines * 64);
    for i in 0..n_lines {
        s.push_str(&format!("line {i}: the quick brown fox jumps over\n"));
    }
    s
}

fn insert_at_origin(c: &mut Criterion) {
    let mut g = c.benchmark_group("buffer::insert_at_origin");
    for size in [10usize, 1_000, 100_000] {
        let text = build_buffer(size);
        g.throughput(Throughput::Bytes(text.len() as u64));
        g.bench_with_input(BenchmarkId::from_parameter(size), &text, |bencher, t| {
            bencher.iter_with_setup(
                || Buffer::from_text(t),
                |mut buf| {
                    buf.apply_edit(&Edit::insert(black_box(Position::ZERO), "x"))
                        .unwrap();
                },
            );
        });
    }
    g.finish();
}

fn insert_at_middle(c: &mut Criterion) {
    let mut g = c.benchmark_group("buffer::insert_at_middle");
    for size in [10usize, 1_000, 100_000] {
        let text = build_buffer(size);
        let mid_line = (size / 2) as u32;
        g.throughput(Throughput::Bytes(text.len() as u64));
        g.bench_with_input(BenchmarkId::from_parameter(size), &text, |bencher, t| {
            bencher.iter_with_setup(
                || Buffer::from_text(t),
                |mut buf| {
                    buf.apply_edit(&Edit::insert(Position::new(mid_line, 0), "x"))
                        .unwrap();
                },
            );
        });
    }
    g.finish();
}

fn delete_one_byte(c: &mut Criterion) {
    let mut g = c.benchmark_group("buffer::delete_one_byte");
    for size in [10usize, 1_000, 100_000] {
        let text = build_buffer(size);
        let mid_line = (size / 2) as u32;
        let range = Range::new(Position::new(mid_line, 0), Position::new(mid_line, 1));
        g.bench_with_input(BenchmarkId::from_parameter(size), &text, |bencher, t| {
            bencher.iter_with_setup(
                || Buffer::from_text(t),
                |mut buf| {
                    buf.apply_edit(&Edit::delete(black_box(range))).unwrap();
                },
            );
        });
    }
    g.finish();
}

fn position_to_byte_round_trip(c: &mut Criterion) {
    let mut g = c.benchmark_group("buffer::position_byte_round_trip");
    for size in [10usize, 1_000, 100_000] {
        let text = build_buffer(size);
        let buffer = Buffer::from_text(&text);
        let pos = Position::new((size / 2) as u32, 5);
        g.bench_with_input(
            BenchmarkId::from_parameter(size),
            &buffer,
            |bencher, buf| {
                bencher.iter(|| {
                    let b = buf.position_to_byte(black_box(pos)).unwrap();
                    let _ = buf.byte_to_position(b).unwrap();
                });
            },
        );
    }
    g.finish();
}

criterion_group!(
    benches,
    insert_at_origin,
    insert_at_middle,
    delete_one_byte,
    position_to_byte_round_trip,
);
criterion_main!(benches);
