#![allow(clippy::unwrap_used, clippy::panic)]
//! Criterion benchmarks for `lattice_core::Buffer` edit + position
//! conversion. These are the hottest hot paths -- every keystroke
//! mutates the rope.

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};

use lattice_core::Buffer;
use lattice_protocol::edit::{Edit, EditDelta};
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

/// Open large file: `Buffer::from_text` against synthetic ~100MB
/// payloads. Backs §8.2's "Open 100MB log (first paint)" target
/// -- ropey rope construction is the floor; the editor's `:e`
/// path adds language detection + tree-sitter parse on top, so
/// the floor for "first paint of viewport" is roughly this number
/// + ratatui draw.
fn open_large_buffer(c: &mut Criterion) {
    let mut g = c.benchmark_group("buffer::open_large");
    // Pre-build the source strings once outside the timing loop --
    // we want to measure ropey, not memory allocation of the
    // String.
    for &(label, target_bytes) in &[("10mb", 10 * 1024 * 1024), ("100mb", 100 * 1024 * 1024)] {
        let mut text = String::with_capacity(target_bytes + 64);
        let line = "line: the quick brown fox jumps over the lazy dog\n";
        while text.len() < target_bytes {
            text.push_str(line);
        }
        g.throughput(Throughput::Bytes(text.len() as u64));
        g.bench_with_input(BenchmarkId::from_parameter(label), &text, |bencher, t| {
            bencher.iter(|| {
                let buf = Buffer::from_text(black_box(t));
                black_box(buf);
            });
        });
    }
    g.finish();
}

/// Slice B.1: isolate the cost of constructing an [`EditDelta`].
/// Backs §8.2's "InputEdit construction (per `Document::apply_edit`)"
/// row -- floor ~2ns, target <10ns. This is the new struct literal
/// `Buffer::apply_edit` builds at its tail; values come from
/// already-computed locals so the construction site is just six
/// u32 writes + three Position copies.
fn input_edit_construction(c: &mut Criterion) {
    c.bench_function("input_edit_construction", |bencher| {
        bencher.iter(|| {
            let d = EditDelta {
                start_byte: black_box(0),
                old_end_byte: black_box(5),
                new_end_byte: black_box(10),
                start_position: black_box(Position::new(0, 0)),
                old_end_position: black_box(Position::new(0, 5)),
                new_end_position: black_box(Position::new(0, 10)),
            };
            black_box(d);
        });
    });
}

/// Slice B.5: input-thread cost of preparing a buffer for the
/// syntax worker. Pre-B.5: `Document::text()` materializes a
/// fresh `String` of the entire buffer -- O(n) alloc + memcpy.
/// Post-B.5: `Buffer::clone()` bumps ropey's internal Arc -- O(1).
///
/// Benches both at the same sizes so the savings are visible
/// at-a-glance in BENCHMARKS.md. The `text` (pre-B.5) path is
/// kept as the falsification anchor -- if the alloc cost ever
/// converges to `clone()`, something changed in ropey.
fn buffer_clone_vs_text(c: &mut Criterion) {
    let mut g = c.benchmark_group("buffer::clone_vs_text");
    for size in [10usize, 1_000, 100_000] {
        let text = build_buffer(size);
        let buffer = Buffer::from_text(&text);
        g.bench_with_input(BenchmarkId::new("clone", size), &buffer, |bencher, buf| {
            bencher.iter(|| {
                let cloned = black_box(buf.clone());
                black_box(cloned);
            });
        });
        g.bench_with_input(
            BenchmarkId::new("as_string", size),
            &buffer,
            |bencher, buf| {
                bencher.iter(|| {
                    let s = black_box(buf.as_string());
                    black_box(s);
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
    open_large_buffer,
    buffer_clone_vs_text,
    input_edit_construction,
);
criterion_main!(benches);
