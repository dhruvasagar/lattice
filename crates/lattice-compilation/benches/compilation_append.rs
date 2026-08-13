#![allow(clippy::unwrap_used, clippy::panic)]
//! CM.1 — streaming-append bench for the `*compilation*` buffer.
//!
//! The drain (`CompilationMode`'s spawned task) applies each
//! streamed `OutputChunk` to the buffer through the document actor's
//! `apply_edit_batch`. This bench characterises that write path —
//! specifically that an **end-of-buffer append stays flat as the log
//! grows**: a noisy build that has already streamed thousands of
//! lines must not make each new batch progressively more expensive
//! (paramount goal #1 — the streaming write is off the UI thread, but
//! it still must not degrade, or the drain would fall behind a fast
//! producer and the buffer would lag frames behind the build).
//!
//! Not measured here: the process spawn / pipe read (OS-bound) or the
//! event-bus publish (its own `lattice-runtime` bench). This isolates
//! the actor-mailbox append the drain performs per coalesced batch.

use std::sync::Arc;

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};

use lattice_core::Document;
use lattice_grammar::CommandRegistry;
use lattice_protocol::edit::Edit;
use lattice_protocol::position::Position;
use lattice_runtime::{RopeDocumentHandle, block_on, spawn_document};

fn empty_registry() -> lattice_grammar::CommandRegistryHandle {
    Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new()))
}

/// One coalesced drain batch: `READER_BATCH_LINES`-worth of typical
/// compiler output.
fn batch_text() -> String {
    let mut s = String::new();
    for i in 0..8 {
        s.push_str(&format!(
            "  --> src/module_{i}.rs:{}:{}: warning: unused variable `x`\n",
            i * 7 + 3,
            i * 3 + 1
        ));
    }
    s
}

/// Append `text` at the end of the buffer, exactly as the drain's
/// `append_at_end` does (snapshot → end position → one insert batch).
fn append_at_end(handle: &RopeDocumentHandle, text: &str) {
    let snap = handle.snapshot();
    let last = snap.buffer.rope_line_count().saturating_sub(1);
    let last_line = snap.buffer.line(last).unwrap_or_default();
    let pos = Position::new(last, last_line.len() as u32);
    block_on(handle.apply_edit_batch(vec![Edit::insert(pos, text.to_string())])).unwrap();
}

/// Seed a handle whose buffer already holds `prefill_batches` worth of
/// streamed output, so the measured append lands at the end of a log
/// of that size.
fn seeded_handle(prefill_batches: usize) -> RopeDocumentHandle {
    let handle = spawn_document(
        lattice_core::BufferId(0),
        Document::empty(),
        empty_registry(),
    );
    let batch = batch_text();
    for _ in 0..prefill_batches {
        append_at_end(&handle, &batch);
    }
    handle
}

fn bench_append(c: &mut Criterion) {
    let mut group = c.benchmark_group("compilation_append");
    let batch = batch_text();
    // Append onto logs of growing size — flat curve = no degradation.
    for prefill in [0usize, 250, 2_000, 10_000] {
        let handle = seeded_handle(prefill);
        group.bench_with_input(BenchmarkId::from_parameter(prefill), &prefill, |b, _| {
            b.iter(|| append_at_end(black_box(&handle), black_box(&batch)));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_append);
criterion_main!(benches);
