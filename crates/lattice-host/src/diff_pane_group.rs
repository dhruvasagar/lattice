//! D.4.b (2026-05-29): `HunkRowMapper`.
//!
//! [`RowMapper`] impl that translates rows between the
//! baseline and current sides of a two-way diff. Composed
//! with the D.4.a [`PaneGroup`] substrate; consumed by
//! `:diffsplit` / `:diffthis` in D.4.d.
//!
//! See `docs/dev/architecture/pane-groups.md` and
//! `docs/dev/architecture/diff-system.md` §5.2.
//!
//! ## Algorithm
//!
//! For each hunk the mapper walks in index order, tracking
//! a `cumulative_shift` = sum over preceding hunks of
//! `current_len - baseline_len`.
//!
//! - **Row before this hunk** (`row < this_side.start`):
//!   apply `cumulative_shift` and return — the row sits in
//!   a "gap" of pure-identity territory shifted by every
//!   prior hunk's length delta.
//! - **Row inside this hunk** (`this_side.start <= row <
//!   this_side.end`):
//!   - Either side empty (Add ⇒ baseline empty; Remove ⇒
//!     current empty) ⇒ collapse to `other_side.start`.
//!     There's no meaningful proportional mapping when one
//!     side has zero lines to map into.
//!   - Otherwise proportional: `offset * other_len /
//!     this_len`, capped at `other_len - 1`.
//! - **Row past this hunk**: accumulate the length delta
//!   into `cumulative_shift` and continue.
//! - **Past all hunks**: apply final `cumulative_shift`.
//!
//! The two directions are symmetric; only the per-hunk
//! length-delta sign flips.
//!
//! ## Why the trait works with raw indices
//!
//! [`RowMapper::map_row`] receives `(from_idx, to_idx, row)`
//! over `PaneGroup::members`. `HunkRowMapper` is
//! constructed with the indices that correspond to each
//! side — `baseline_member_idx` and `current_member_idx`.
//! Any (from, to) pair that doesn't match a known
//! direction (e.g. a future three-pane composition that
//! threads this mapper) falls back to identity, so
//! unfamiliar member configurations never produce silently
//! wrong scrolling.

use std::sync::Arc;

use lattice_diff::HunkIndex;

use crate::diff_subsystem::DiffSession;
use crate::pane_group::RowMapper;

/// D.4.b: maps rows between the baseline and current sides
/// of a two-way diff session.
pub struct HunkRowMapper {
    session: Arc<DiffSession>,
    baseline_member_idx: usize,
    current_member_idx: usize,
}

impl std::fmt::Debug for HunkRowMapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HunkRowMapper")
            .field("baseline_member_idx", &self.baseline_member_idx)
            .field("current_member_idx", &self.current_member_idx)
            .finish()
    }
}

impl HunkRowMapper {
    /// Construct a mapper for a side-by-side diff session.
    ///
    /// `baseline_member_idx` and `current_member_idx` are
    /// the indices in `PaneGroup::members` that hold the
    /// baseline pane and the current pane respectively.
    /// They must be distinct.
    pub fn new(
        session: Arc<DiffSession>,
        baseline_member_idx: usize,
        current_member_idx: usize,
    ) -> Self {
        debug_assert_ne!(
            baseline_member_idx, current_member_idx,
            "baseline and current member indices must be distinct"
        );
        Self {
            session,
            baseline_member_idx,
            current_member_idx,
        }
    }
}

impl RowMapper for HunkRowMapper {
    fn map_row(&self, from: usize, to: usize, row: u32) -> u32 {
        let hunks = self.session.current_hunks();
        if from == self.baseline_member_idx && to == self.current_member_idx {
            map_baseline_to_current(&hunks, row)
        } else if from == self.current_member_idx && to == self.baseline_member_idx {
            map_current_to_baseline(&hunks, row)
        } else {
            // Member pair the mapper wasn't constructed for —
            // fall back to identity rather than guess.
            row
        }
    }
}

/// Translate a baseline-side row to its current-side
/// counterpart. Pure function of the published `HunkIndex`
/// and the input row; exposed for direct unit testing
/// without round-tripping through `RowMapper`.
pub fn map_baseline_to_current(index: &HunkIndex, row: u32) -> u32 {
    let mut shift: i32 = 0;
    for hunk in &index.hunks {
        let Some(br) = hunk.ranges.first() else {
            continue;
        };
        let Some(cr) = hunk.ranges.get(1) else {
            continue;
        };
        if row < br.start {
            return apply_shift(row, shift);
        }
        if row < br.end {
            return map_inside(row, br.start, br.end, cr.start, cr.end);
        }
        // Past this hunk's baseline range — accumulate
        // delta for the next iteration / fall-through.
        shift += (cr.end as i32 - cr.start as i32) - (br.end as i32 - br.start as i32);
    }
    apply_shift(row, shift)
}

/// Translate a current-side row to its baseline-side
/// counterpart. Symmetric to [`map_baseline_to_current`].
pub fn map_current_to_baseline(index: &HunkIndex, row: u32) -> u32 {
    let mut shift: i32 = 0;
    for hunk in &index.hunks {
        let Some(br) = hunk.ranges.first() else {
            continue;
        };
        let Some(cr) = hunk.ranges.get(1) else {
            continue;
        };
        if row < cr.start {
            return apply_shift(row, shift);
        }
        if row < cr.end {
            return map_inside(row, cr.start, cr.end, br.start, br.end);
        }
        shift += (br.end as i32 - br.start as i32) - (cr.end as i32 - cr.start as i32);
    }
    apply_shift(row, shift)
}

/// Map a row inside a hunk's `from`-side range to the
/// corresponding `to`-side row.
///
/// - Either side empty (Add or Remove) ⇒ collapse to
///   `to_start`. Without a non-empty target there's no
///   proportional mapping — the row "lands at" the
///   insertion point on the other side.
/// - Otherwise: `to_start + (offset * to_len / from_len)`,
///   capped at `to_len - 1` so we never spill past the
///   hunk's range on the target side.
fn map_inside(row: u32, from_start: u32, from_end: u32, to_start: u32, to_end: u32) -> u32 {
    let from_len = from_end - from_start;
    let to_len = to_end - to_start;
    if from_len == 0 || to_len == 0 {
        return to_start;
    }
    let offset = row - from_start;
    // Proportional via 64-bit to dodge `offset * to_len`
    // overflow when both are u32-max-ish (won't happen in
    // practice — file sizes are bounded — but the cast is
    // free and removes the only sharp edge).
    let mapped_offset = (offset as u64 * to_len as u64 / from_len as u64) as u32;
    to_start + mapped_offset.min(to_len - 1)
}

/// Apply a (possibly-negative) shift to a row, saturating
/// at zero. Same shape as the identity mapper's no-op when
/// `shift == 0`.
fn apply_shift(row: u32, shift: i32) -> u32 {
    if shift >= 0 {
        row.saturating_add(shift as u32)
    } else {
        row.saturating_sub(shift.unsigned_abs())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use lattice_diff::{DiffAlgorithm, Hunk, HunkKind, LineRange};
    use smallvec::smallvec;

    fn hunk(kind: HunkKind, baseline: LineRange, current: LineRange) -> Hunk {
        Hunk {
            kind,
            ranges: smallvec![baseline, current],
        }
    }

    fn idx(hunks: Vec<Hunk>) -> HunkIndex {
        HunkIndex {
            hunks,
            algorithm: DiffAlgorithm::Histogram,
            revision: 1,
        }
    }

    // ── Empty index ────────────────────────────────────────

    #[test]
    fn empty_index_is_identity_in_both_directions() {
        let i = HunkIndex::empty(DiffAlgorithm::Histogram);
        for row in [0, 1, 42, 10_000] {
            assert_eq!(map_baseline_to_current(&i, row), row);
            assert_eq!(map_current_to_baseline(&i, row), row);
        }
    }

    // ── Single Add hunk ────────────────────────────────────

    #[test]
    fn add_hunk_shifts_baseline_rows_past_it() {
        // Add 3 lines on current at position 5; baseline
        // has nothing there.
        let i = idx(vec![hunk(
            HunkKind::Add,
            LineRange::new(5, 5),
            LineRange::new(5, 8),
        )]);
        // Before the hunk: identity.
        assert_eq!(map_baseline_to_current(&i, 0), 0);
        assert_eq!(map_baseline_to_current(&i, 4), 4);
        // At the hunk insertion point: cumulative shift
        // applies (row >= br.start, row >= br.end since
        // br is zero-length). Shift = current_len - 0 = +3.
        assert_eq!(map_baseline_to_current(&i, 5), 8);
        // Well past the hunk: shifted by +3.
        assert_eq!(map_baseline_to_current(&i, 100), 103);
    }

    #[test]
    fn add_hunk_current_to_baseline_collapses_inserted_rows() {
        let i = idx(vec![hunk(
            HunkKind::Add,
            LineRange::new(5, 5),
            LineRange::new(5, 8),
        )]);
        // Current rows before the inserted block: identity.
        assert_eq!(map_current_to_baseline(&i, 0), 0);
        assert_eq!(map_current_to_baseline(&i, 4), 4);
        // Current rows INSIDE the inserted block: collapse
        // to the baseline insertion point (the lines don't
        // exist on baseline at all).
        assert_eq!(map_current_to_baseline(&i, 5), 5);
        assert_eq!(map_current_to_baseline(&i, 6), 5);
        assert_eq!(map_current_to_baseline(&i, 7), 5);
        // After the inserted block: shift back by -3.
        assert_eq!(map_current_to_baseline(&i, 8), 5);
        assert_eq!(map_current_to_baseline(&i, 100), 97);
    }

    // ── Single Remove hunk ─────────────────────────────────

    #[test]
    fn remove_hunk_shifts_current_rows_past_it() {
        // Remove 3 lines from baseline at position 5;
        // current has nothing there.
        let i = idx(vec![hunk(
            HunkKind::Remove,
            LineRange::new(5, 8),
            LineRange::new(5, 5),
        )]);
        // Before the hunk: identity.
        assert_eq!(map_current_to_baseline(&i, 0), 0);
        assert_eq!(map_current_to_baseline(&i, 4), 4);
        // At/past the deletion point: shift by +3
        // (current → baseline reverses Add semantics).
        assert_eq!(map_current_to_baseline(&i, 5), 8);
        assert_eq!(map_current_to_baseline(&i, 100), 103);
    }

    #[test]
    fn remove_hunk_baseline_inside_collapses_to_current_start() {
        let i = idx(vec![hunk(
            HunkKind::Remove,
            LineRange::new(5, 8),
            LineRange::new(5, 5),
        )]);
        assert_eq!(map_baseline_to_current(&i, 5), 5);
        assert_eq!(map_baseline_to_current(&i, 6), 5);
        assert_eq!(map_baseline_to_current(&i, 7), 5);
        // After the removed block: shift -3.
        assert_eq!(map_baseline_to_current(&i, 8), 5);
        assert_eq!(map_baseline_to_current(&i, 100), 97);
    }

    // ── Change hunk: proportional inside ───────────────────

    #[test]
    fn change_hunk_maps_inside_proportionally() {
        // baseline [10, 13) -> current [10, 15)
        // 3 lines compressed against 5 lines.
        let i = idx(vec![hunk(
            HunkKind::Change,
            LineRange::new(10, 13),
            LineRange::new(10, 15),
        )]);
        // baseline → current: 3 lines into 5.
        assert_eq!(map_baseline_to_current(&i, 10), 10); // 0/3 → 0
        assert_eq!(map_baseline_to_current(&i, 11), 11); // 1*5/3=1
        assert_eq!(map_baseline_to_current(&i, 12), 13); // 2*5/3=3
                                                          // After the hunk: shift = 5 - 3 = +2.
        assert_eq!(map_baseline_to_current(&i, 20), 22);
    }

    #[test]
    fn change_hunk_compresses_when_target_is_shorter() {
        // baseline [10, 15) -> current [10, 12)
        let i = idx(vec![hunk(
            HunkKind::Change,
            LineRange::new(10, 15),
            LineRange::new(10, 12),
        )]);
        // 5 lines into 2: most map to start.
        assert_eq!(map_baseline_to_current(&i, 10), 10); // 0*2/5=0
        assert_eq!(map_baseline_to_current(&i, 11), 10); // 1*2/5=0
        assert_eq!(map_baseline_to_current(&i, 12), 10); // 2*2/5=0
        assert_eq!(map_baseline_to_current(&i, 13), 11); // 3*2/5=1
        assert_eq!(map_baseline_to_current(&i, 14), 11); // 4*2/5=1
                                                          // After: shift = 2 - 5 = -3.
        assert_eq!(map_baseline_to_current(&i, 20), 17);
    }

    // ── Multiple hunks: cumulative shift ───────────────────

    #[test]
    fn cumulative_shift_across_multiple_hunks() {
        // Two Adds: +3 then +2.
        let i = idx(vec![
            hunk(
                HunkKind::Add,
                LineRange::new(5, 5),
                LineRange::new(5, 8),
            ),
            hunk(
                HunkKind::Add,
                LineRange::new(20, 20),
                LineRange::new(23, 25), // 23 = 20 + 3 (already shifted)
            ),
        ]);
        // baseline 0 → current 0 (before first hunk)
        assert_eq!(map_baseline_to_current(&i, 0), 0);
        // baseline 10 → current 13 (shifted by first hunk's +3)
        assert_eq!(map_baseline_to_current(&i, 10), 13);
        // baseline 30 → current 35 (shifted by +3 + +2)
        assert_eq!(map_baseline_to_current(&i, 30), 35);
    }

    #[test]
    fn mixed_add_and_remove_cumulative_shifts_cancel() {
        // Add 3, then Remove 3 — cumulative shift returns to 0.
        let i = idx(vec![
            hunk(
                HunkKind::Add,
                LineRange::new(5, 5),
                LineRange::new(5, 8),
            ),
            hunk(
                HunkKind::Remove,
                LineRange::new(15, 18),
                LineRange::new(18, 18), // baseline 15 == current 18 after +3 shift
            ),
        ]);
        // Before either hunk.
        assert_eq!(map_baseline_to_current(&i, 0), 0);
        // Between hunks: shift +3.
        assert_eq!(map_baseline_to_current(&i, 10), 13);
        // After both: net shift 0.
        assert_eq!(map_baseline_to_current(&i, 100), 100);
    }

    // ── Defensive ──────────────────────────────────────────

    #[test]
    fn malformed_hunk_with_fewer_than_two_ranges_is_skipped() {
        let i = idx(vec![Hunk {
            kind: HunkKind::Add,
            ranges: smallvec![LineRange::new(0, 0)], // only one range
        }]);
        assert_eq!(map_baseline_to_current(&i, 42), 42);
        assert_eq!(map_current_to_baseline(&i, 42), 42);
    }

    #[test]
    fn unfamiliar_member_pair_falls_back_to_identity() {
        use crate::pane_group::RowMapper;
        // We don't have a real DiffSession in the test;
        // but the (from, to) != (baseline, current) branch
        // never touches the session — it returns `row`
        // directly. Construct minimally and exercise that.
        let session = Arc::new(DiffSession::new(
            lattice_core::BufferId(1),
            DiffAlgorithm::Histogram,
        ));
        let mapper = HunkRowMapper::new(session, 0, 1);
        // (from, to) = (2, 3) — neither baseline nor current
        // direction. Must be identity, regardless of the
        // (empty) HunkIndex on the session.
        assert_eq!(mapper.map_row(2, 3, 42), 42);
        assert_eq!(mapper.map_row(0, 2, 42), 42);
        assert_eq!(mapper.map_row(2, 1, 42), 42);
    }

    #[test]
    fn round_trip_through_session_uses_published_hunks() {
        use crate::pane_group::RowMapper;
        let session = Arc::new(DiffSession::new(
            lattice_core::BufferId(1),
            DiffAlgorithm::Histogram,
        ));
        // Publish an Add hunk; mapper should consult it.
        session.publish(Arc::new(idx(vec![hunk(
            HunkKind::Add,
            LineRange::new(5, 5),
            LineRange::new(5, 8),
        )])));
        let mapper = HunkRowMapper::new(session.clone(), 0, 1);
        assert_eq!(
            mapper.map_row(0, 1, 100),
            103,
            "baseline-to-current shift +3 from the Add hunk"
        );
        assert_eq!(
            mapper.map_row(1, 0, 100),
            97,
            "current-to-baseline shift -3"
        );
    }
}
