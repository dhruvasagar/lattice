//! D.4.e (2026-05-30): pane-group scroll-binding benches.
//!
//! Three workloads, all per-publish hot path:
//!
//! - `pane_group_no_group` — `Editor::propagate_pane_group_scroll`
//!   on an editor with no pane groups (early-return path). This
//!   is the per-tick floor cost paid by *every* publish, even
//!   when no diff / scrollbind / windo / zen-mode group exists.
//!   Must stay essentially free (a single `Vec::is_empty()`
//!   check) so the "always-on" propagation hook doesn't tax
//!   the keystroke budget for users who aren't using D.4 yet.
//!
//! - `pane_group_identity_propagation` — 2-pane identity-mapper
//!   group, `propagate_pane_group_scroll` ticks. Mirrors a
//!   `:set scrollbind`-style binding. Measures the cost of the
//!   group walk + buffer-mismatch check + identity-mapper call +
//!   stashed-scroll write. The propagation runs at the
//!   dispatch tail every publish, so this is the per-tick cost
//!   that two-pane bound users pay.
//!
//! - `hunk_row_map_p99_us` — `HunkRowMapper::map_row` against a
//!   `DiffSession` carrying 100 published hunks. Measures the
//!   per-call mapper cost the D.4 side-by-side diff pays on
//!   every `propagate` tick. Backs the keystroke-budget claim
//!   in `docs/dev/architecture/diff-system.md` §7 (
//!   `diff_scroll_bind_p99_us` ≤ 50µs at 1k hunks; at 100
//!   hunks we'd expect well under that floor).
//!
//! Run:
//!
//!   cargo bench -p lattice-host --bench pane_group
//!
//! Backs paramount goal #1 (imperceptible keystroke→glyph, within
//! the one-frame ceiling -- 8.3 ms at 120Hz): the propagation tail
//! runs on every publish, so its no-group floor + identity-bound
//! cost both have to fit inside the ceiling alongside cells /
//! virtual-rows / highlights rebuilds.

use std::sync::Arc;

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use smallvec::smallvec;

use lattice_core::BufferId;
use lattice_core::ui::pane::{PaneState, PaneTree, SplitOrientation};
use lattice_diff::{DiffAlgorithm, Hunk, HunkIndex, HunkKind, LineRange};
use lattice_host::diff::pane_group::HunkRowMapper;
use lattice_host::diff::subsystem::DiffSession;
use lattice_host::editor::Editor;
use lattice_host::pane_group::{IdentityRowMapper, PaneGroupMember, RowMapper};
use lattice_host::versioned::Versioned;

/// Editor with a vsplit but no pane groups. Exercises the
/// `pane_groups.is_empty()` early-return path in
/// `propagate_pane_group_scroll` — the cost every publish
/// pays when no group is registered.
fn editor_no_group() -> Editor {
    let mut e = Editor::default();
    let mut tree = PaneTree::single(PaneState::default());
    tree.split_active(SplitOrientation::Vertical);
    e.pane_tree = Versioned::new(tree);
    e
}

/// Editor with a vsplit + one identity-mapper group binding
/// the two leaves. Exercises the full propagation path:
/// active-pane lookup, group walk, buffer-mismatch check,
/// mapper call, stashed-scroll write.
fn editor_with_identity_group() -> Editor {
    let mut e = editor_no_group();
    let (m0, m1) = {
        let leaves = e.pane_tree.leaves();
        (
            PaneGroupMember {
                pane: leaves[0].id,
                buffer: leaves[0].buffer_id,
            },
            PaneGroupMember {
                pane: leaves[1].id,
                buffer: leaves[1].buffer_id,
            },
        )
    };
    let mapper: Arc<dyn RowMapper> = Arc::new(IdentityRowMapper);
    e.add_pane_group(vec![m0, m1], mapper)
        .expect("identity group registration succeeds");
    e
}

/// Construct a two-way `Hunk` with the requested kind and
/// (baseline, current) ranges expressed as (start, len).
fn hunk(kind: HunkKind, base_start: u32, base_len: u32, cur_start: u32, cur_len: u32) -> Hunk {
    Hunk {
        kind,
        ranges: smallvec![
            LineRange::new(base_start, base_start + base_len),
            LineRange::new(cur_start, cur_start + cur_len),
        ],
    }
}

/// Build a `HunkIndex` carrying `n` synthetic hunks
/// distributed roughly evenly across a ~10k-line file with
/// alternating Add / Remove / Change kinds — covers the
/// three per-hunk branches `HunkRowMapper` walks internally
/// (range.is_empty collapse for Add/Remove + proportional
/// scale for Change).
fn hunk_index_with_n_hunks(n: usize) -> HunkIndex {
    let mut hunks = Vec::with_capacity(n);
    for i in 0..n {
        let base = 50 + (i as u32) * 100;
        let cur = base + (i as u32);
        let h = match i % 3 {
            0 => hunk(HunkKind::Add, base, 0, cur, 2),
            1 => hunk(HunkKind::Remove, base, 2, cur, 0),
            _ => hunk(HunkKind::Change, base, 1, cur, 3),
        };
        hunks.push(h);
    }
    HunkIndex {
        hunks,
        algorithm: DiffAlgorithm::Histogram,
        revision: 1,
    }
}

fn bench_pane_group_no_group(c: &mut Criterion) {
    let mut editor = editor_no_group();
    c.bench_function("pane_group_no_group", |b| {
        b.iter(|| {
            editor.propagate_pane_group_scroll();
            black_box(&editor);
        });
    });
}

fn bench_pane_group_identity_propagation(c: &mut Criterion) {
    let mut editor = editor_with_identity_group();
    c.bench_function("pane_group_identity_propagation", |b| {
        b.iter(|| {
            editor.propagate_pane_group_scroll();
            black_box(&editor);
        });
    });
}

fn bench_hunk_row_map_p99_us(c: &mut Criterion) {
    let session = Arc::new(DiffSession::new(BufferId(1), DiffAlgorithm::Histogram));
    session.publish(Arc::new(hunk_index_with_n_hunks(100)));
    let mapper = HunkRowMapper::new(session, 0, 1);
    // Sample rows that fall before, inside, and after the
    // hunk index so each call exercises a different branch of
    // the cumulative-shift walk.
    let sample_rows: [u32; 8] = [0, 25, 250, 1_500, 4_999, 7_500, 9_950, 10_500];
    c.bench_function("hunk_row_map_p99_us", |b| {
        b.iter(|| {
            for row in sample_rows {
                let mapped = mapper.map_row(0, 1, black_box(row));
                black_box(mapped);
            }
        });
    });
}

criterion_group!(
    benches,
    bench_pane_group_no_group,
    bench_pane_group_identity_propagation,
    bench_hunk_row_map_p99_us
);
criterion_main!(benches);
