#![allow(clippy::unwrap_used, clippy::panic)]
//! DB.7: dashboard creation-time + idle-frame benches (design.md §13).
//!
//! The dashboard is not on the keystroke path — it composes once at
//! creation, never per keystroke — so there is no keystroke→glyph bench
//! here. Coverage is the two assertions design.md §13 calls for:
//!
//! - `dashboard_creation` — cold compose + seed of the default page
//!   (`Editor::do_open_dashboard` from a freshly booted, not-yet-opened
//!   editor). Recorded under a threshold in `BENCHMARKS.md`.
//! - `dashboard_idle_tick` — `run_tick_pending` on an editor with the
//!   dashboard ALREADY open and nothing new published: the idle-frame
//!   cost. Near-zero here is the numeric half of the "zero recompose /
//!   zero I/O when idle" guarantee; `dashboard_idle_ticks_do_not_recompose`
//!   in `tests/dashboard.rs` is the correctness half — an "enforced, not
//!   asserted" pin that the buffer's document version literally does not
//!   change across idle ticks, not just that they're fast.
//!
//! Run:
//!
//!   cargo bench -p lattice-host --bench dashboard

use criterion::{BatchSize, Criterion, black_box, criterion_group, criterion_main};
use lattice_core::Document;
use lattice_host::editor::Editor;

fn bench_dashboard_creation(c: &mut Criterion) {
    c.bench_function("dashboard_creation", |b| {
        b.iter_batched(
            || Editor::boot(Document::from_text("scratch\n")),
            |mut editor| {
                black_box(editor.do_open_dashboard());
            },
            BatchSize::SmallInput,
        );
    });
}

fn bench_dashboard_idle_tick(c: &mut Criterion) {
    let mut editor = Editor::boot(Document::from_text("scratch\n"));
    editor.do_open_dashboard();
    c.bench_function("dashboard_idle_tick", |b| {
        b.iter(|| {
            black_box(editor.run_tick_pending());
        });
    });
}

criterion_group!(benches, bench_dashboard_creation, bench_dashboard_idle_tick);
criterion_main!(benches);
