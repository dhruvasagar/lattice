//! MG.14 bench: the magit headerline's per-tick and per-repaint cost.
//!
//! Two numbers, matching the two things the cells worker actually does
//! with a [`Headerline`]:
//!
//! - `magit_headerline_version_ns` — `version()`, called on EVERY tick
//!   for every magit buffer on screen. This is the one that must stay
//!   flat: it takes a read-lock on the theme registry (magit's header
//!   resolves colours live so `:colorscheme` lands, unlike the
//!   capture-at-activation headerlines that shipped before it), so the
//!   bench exists to keep that choice honest.
//! - `magit_headerline_render_ns` — `render()`, called only when the
//!   version advanced: builds the row's cells.
//! - `magit_headerline_set_unchanged_ns` — the no-work refresh path.
//!   `gr` and every future auto-refresh land here whenever git reports
//!   the same state, and it must not bump the version (paramount goal
//!   #1: no repaint for no change).
//!
//! Runs off the UI thread in production (the cells worker), so the bar
//! is "cheap enough to do every tick", not the frame budget itself.

use std::sync::Arc;

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use lattice_cells::Headerline;
use lattice_magit::headerline::{Field, MagitHeaderline};
use lattice_theme::{InMemoryThemeRegistry, ThemeRegistryHandle, register_builtins};

/// A status-buffer row: repo, branch with ahead/behind, three counts —
/// the widest row any magit view produces.
fn status_row() -> Vec<Field> {
    vec![
        Field::label("lattice"),
        Field::branch("feature/magit-design \u{2191}2 \u{2193}1"),
        Field::label("3 staged"),
        Field::label("5 unstaged"),
        Field::label("2 untracked"),
    ]
}

fn themed() -> Arc<MagitHeaderline> {
    let registry = InMemoryThemeRegistry::with_defaults();
    register_builtins(&registry);
    let theme: ThemeRegistryHandle = Arc::new(registry);
    let hl = MagitHeaderline::new(Some(theme), "bench");
    hl.set(status_row());
    hl
}

fn bench_headerline(c: &mut Criterion) {
    let hl = themed();

    c.bench_function("magit_headerline_version_ns", |b| {
        b.iter(|| black_box(hl.version()));
    });

    c.bench_function("magit_headerline_render_ns", |b| {
        b.iter(|| black_box(hl.render().map(|r| r.cells.len())));
    });

    c.bench_function("magit_headerline_set_unchanged_ns", |b| {
        b.iter(|| black_box(hl.set(status_row())));
    });
}

criterion_group!(benches, bench_headerline);
criterion_main!(benches);
