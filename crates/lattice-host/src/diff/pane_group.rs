//! D.4.b (2026-05-29) / D.6.b (2026-05-31): `HunkRowMapper`.
//!
//! [`RowMapper`] impl that translates rows between two or
//! three sides of a diff session. Composed with the D.4.a
//! [`PaneGroup`] substrate; consumed by `:diffsplit` /
//! `:diffthis` in D.4.d for two-way and D.6.c for three-way.
//!
//! See `docs/dev/architecture/pane-groups.md` and
//! `docs/dev/architecture/diff-system.md` §5.2.
//!
//! ## Algorithm
//!
//! The walk is pane-index parametric: given a (from_pane,
//! to_pane) pair (positions into `Hunk::ranges`), for each
//! hunk the mapper tracks a `cumulative_shift` = sum over
//! preceding hunks of `to_len - from_len`.
//!
//! - **Row before this hunk** (`row < from_r.start`):
//!   apply `cumulative_shift` and return — the row sits in
//!   a "gap" of pure-identity territory shifted by every
//!   prior hunk's length delta.
//! - **Row inside this hunk** (`from_r.start <= row <
//!   from_r.end`):
//!   - Either side empty (Add ⇒ from empty; Remove ⇒
//!     to empty) ⇒ collapse to `to_r.start`.
//!     There's no meaningful proportional mapping when one
//!     side has zero lines to map into.
//!   - Otherwise proportional: `offset * to_len /
//!     from_len`, capped at `to_len - 1`.
//! - **Row past this hunk**: accumulate the length delta
//!   into `cumulative_shift` and continue.
//! - **Past all hunks**: apply final `cumulative_shift`.
//!
//! All directions (2 in two-way, 6 in three-way) collapse
//! to one body via [`map_between`]; the public
//! [`map_baseline_to_current`] / [`map_current_to_baseline`]
//! are thin aliases preserved for back-compat with D.4.b
//! callers.
//!
//! ## Why the trait works with raw indices
//!
//! [`RowMapper::map_row`] receives `(from_idx, to_idx, row)`
//! over `PaneGroup::members`. `HunkRowMapper` is
//! constructed with the member indices for each role
//! ([`HunkRowMapper::new`] for two-way; [`HunkRowMapper::three_pane`]
//! for three-way), and resolves each side of the
//! `(from_idx, to_idx)` pair to its pane-index slot in
//! `Hunk::ranges` (0/1 in two-way; 0/1/2 in three-way).
//! Any (from, to) pair that doesn't match a known role
//! falls back to identity, so unfamiliar member
//! configurations never produce silently wrong scrolling.

use std::sync::Arc;

use lattice_diff::HunkIndex;

use crate::diff::subsystem::DiffSession;
use crate::pane_group::RowMapper;

/// D.4.b / D.6.b: maps rows between two or three sides of a
/// diff session.
///
/// Constructed via [`Self::new`] (two-way) or
/// [`Self::three_pane`] (three-way). Internally stores the
/// member-index → pane-index assignment as
/// [`MapperShape`]; `RowMapper::map_row` looks up each side
/// of the `(from, to)` pair and dispatches into the
/// pane-index-parametric [`map_between`].
pub struct HunkRowMapper {
    session: Arc<DiffSession>,
    shape: MapperShape,
}

/// D.6.b: which member indices in `PaneGroup::members` play
/// which role. Roles correspond to slot positions in
/// `Hunk::ranges` (`ranges[pane_index]`):
/// - **TwoWay**: baseline = 0, current = 1.
/// - **ThreeWay**: base = 0, local = 1, remote = 2.
#[derive(Debug, Clone, Copy)]
enum MapperShape {
    TwoWay {
        baseline_member_idx: usize,
        current_member_idx: usize,
    },
    ThreeWay {
        base_member_idx: usize,
        local_member_idx: usize,
        remote_member_idx: usize,
    },
}

impl MapperShape {
    /// Resolve a `PaneGroup::members` index to its
    /// `Hunk::ranges` slot, or `None` if the member isn't
    /// part of this mapper's shape.
    fn pane_index_of(&self, member_idx: usize) -> Option<usize> {
        match *self {
            Self::TwoWay {
                baseline_member_idx,
                current_member_idx,
            } => {
                if member_idx == baseline_member_idx {
                    Some(0)
                } else if member_idx == current_member_idx {
                    Some(1)
                } else {
                    None
                }
            }
            Self::ThreeWay {
                base_member_idx,
                local_member_idx,
                remote_member_idx,
            } => {
                if member_idx == base_member_idx {
                    Some(0)
                } else if member_idx == local_member_idx {
                    Some(1)
                } else if member_idx == remote_member_idx {
                    Some(2)
                } else {
                    None
                }
            }
        }
    }
}

impl std::fmt::Debug for HunkRowMapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HunkRowMapper")
            .field("shape", &self.shape)
            .finish()
    }
}

impl HunkRowMapper {
    /// Construct a mapper for a two-way side-by-side diff
    /// session.
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
            shape: MapperShape::TwoWay {
                baseline_member_idx,
                current_member_idx,
            },
        }
    }

    /// D.6.b: construct a mapper for a three-way merge
    /// session. Member indices correspond to the three
    /// `Hunk::ranges` slots — `base` = `ranges[0]` (common
    /// ancestor), `local` = `ranges[1]` (the side the
    /// session is keyed under), `remote` = `ranges[2]` (the
    /// third party). All three indices must be distinct.
    pub fn three_pane(
        session: Arc<DiffSession>,
        base_member_idx: usize,
        local_member_idx: usize,
        remote_member_idx: usize,
    ) -> Self {
        debug_assert_ne!(base_member_idx, local_member_idx);
        debug_assert_ne!(base_member_idx, remote_member_idx);
        debug_assert_ne!(local_member_idx, remote_member_idx);
        Self {
            session,
            shape: MapperShape::ThreeWay {
                base_member_idx,
                local_member_idx,
                remote_member_idx,
            },
        }
    }
}

impl RowMapper for HunkRowMapper {
    fn map_row(&self, from: usize, to: usize, row: u32) -> u32 {
        let (Some(from_pane), Some(to_pane)) = (
            self.shape.pane_index_of(from),
            self.shape.pane_index_of(to),
        ) else {
            // Member pair the mapper wasn't constructed for —
            // fall back to identity rather than guess.
            return row;
        };
        if from_pane == to_pane {
            return row;
        }
        let hunks = self.session.current_hunks();
        map_between(&hunks, from_pane, to_pane, row)
    }
}

/// D.6.b: translate a row from `from_pane` to `to_pane`
/// where each pane is a slot index into `Hunk::ranges`
/// (0/1 in two-way; 0/1/2 in three-way). Pure function of
/// the published `HunkIndex` + indices; exposed for direct
/// unit testing without round-tripping through `RowMapper`
/// and as the body all directional aliases call into.
pub fn map_between(index: &HunkIndex, from_pane: usize, to_pane: usize, row: u32) -> u32 {
    if from_pane == to_pane {
        return row;
    }
    let mut shift: i32 = 0;
    for hunk in &index.hunks {
        let Some(from_r) = hunk.ranges.get(from_pane) else {
            continue;
        };
        let Some(to_r) = hunk.ranges.get(to_pane) else {
            continue;
        };
        if row < from_r.start {
            return apply_shift(row, shift);
        }
        if row < from_r.end {
            return map_inside(row, from_r.start, from_r.end, to_r.start, to_r.end);
        }
        // Past this hunk's `from_pane` range — accumulate
        // delta for the next iteration / fall-through.
        shift +=
            (to_r.end as i32 - to_r.start as i32) - (from_r.end as i32 - from_r.start as i32);
    }
    apply_shift(row, shift)
}

/// Translate a baseline-side row to its current-side
/// counterpart. Pure function of the published `HunkIndex`
/// and the input row; exposed for direct unit testing
/// without round-tripping through `RowMapper`. D.4.b-shape
/// alias for `map_between(index, 0, 1, row)`.
pub fn map_baseline_to_current(index: &HunkIndex, row: u32) -> u32 {
    map_between(index, 0, 1, row)
}

/// Translate a current-side row to its baseline-side
/// counterpart. Symmetric to [`map_baseline_to_current`].
/// Alias for `map_between(index, 1, 0, row)`.
pub fn map_current_to_baseline(index: &HunkIndex, row: u32) -> u32 {
    map_between(index, 1, 0, row)
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

    // ──────────────────────────────────────────────────────
    // D.6.b (2026-05-31): three-pane mapping
    // ──────────────────────────────────────────────────────

    /// Construct a three-way hunk with `[base, local, remote]`
    /// ranges. Mirrors the engine's `compute_three_way`
    /// emission shape.
    fn hunk3(kind: HunkKind, base: LineRange, local: LineRange, remote: LineRange) -> Hunk {
        Hunk {
            kind,
            ranges: smallvec![base, local, remote],
        }
    }

    #[test]
    fn map_between_is_pane_index_parametric_for_two_way() {
        // Same Add hunk; baseline=pane 0, current=pane 1.
        // Verifies the new generic shape produces identical
        // results to the D.4.b-shape aliases.
        let i = idx(vec![hunk(
            HunkKind::Add,
            LineRange::new(5, 5),
            LineRange::new(5, 8),
        )]);
        for row in [0, 4, 5, 6, 7, 8, 100] {
            assert_eq!(map_between(&i, 0, 1, row), map_baseline_to_current(&i, row));
            assert_eq!(map_between(&i, 1, 0, row), map_current_to_baseline(&i, row));
            // Identity for same-pane.
            assert_eq!(map_between(&i, 0, 0, row), row);
            assert_eq!(map_between(&i, 1, 1, row), row);
        }
    }

    #[test]
    fn three_way_change_hunk_maps_all_six_directions() {
        // base [10, 13) = 3 lines; local [10, 14) = 4 lines;
        // remote [10, 16) = 6 lines. All three differ from
        // one another past the hunk: every pair has its own
        // cumulative shift.
        let i = HunkIndex {
            hunks: vec![hunk3(
                HunkKind::Change,
                LineRange::new(10, 13),
                LineRange::new(10, 14),
                LineRange::new(10, 16),
            )],
            algorithm: DiffAlgorithm::Histogram,
            revision: 1,
        };

        // Before the hunk: identity in all directions.
        for (from, to) in [(0, 1), (1, 0), (0, 2), (2, 0), (1, 2), (2, 1)] {
            assert_eq!(map_between(&i, from, to, 5), 5);
        }

        // After the hunk: shift = to_len - from_len.
        // base→local: +1; base→remote: +3; local→base: -1;
        // local→remote: +2; remote→base: -3; remote→local: -2.
        assert_eq!(map_between(&i, 0, 1, 30), 31);
        assert_eq!(map_between(&i, 0, 2, 30), 33);
        assert_eq!(map_between(&i, 1, 0, 30), 29);
        assert_eq!(map_between(&i, 1, 2, 30), 32);
        assert_eq!(map_between(&i, 2, 0, 30), 27);
        assert_eq!(map_between(&i, 2, 1, 30), 28);
    }

    #[test]
    fn three_way_conflict_hunk_contributes_to_shift_like_change() {
        // Conflict hunks have the same row-geometry semantics
        // as Change hunks — the conflict is about content,
        // not layout. Both should drive cumulative shifts.
        let conflict_idx = HunkIndex {
            hunks: vec![hunk3(
                HunkKind::Conflict,
                LineRange::new(10, 13),
                LineRange::new(10, 14),
                LineRange::new(10, 16),
            )],
            algorithm: DiffAlgorithm::Histogram,
            revision: 1,
        };
        let change_idx = HunkIndex {
            hunks: vec![hunk3(
                HunkKind::Change,
                LineRange::new(10, 13),
                LineRange::new(10, 14),
                LineRange::new(10, 16),
            )],
            algorithm: DiffAlgorithm::Histogram,
            revision: 1,
        };
        for (from, to) in [(0, 1), (1, 0), (0, 2), (2, 0), (1, 2), (2, 1)] {
            assert_eq!(
                map_between(&conflict_idx, from, to, 30),
                map_between(&change_idx, from, to, 30),
                "Conflict and Change should map identically for row geometry ({from}→{to})"
            );
        }
    }

    #[test]
    fn three_way_add_on_one_side_collapses_inside_to_anchor() {
        // local adds 3 lines at row 10; base + remote both
        // empty there.
        let i = HunkIndex {
            hunks: vec![hunk3(
                HunkKind::Add,
                LineRange::new(10, 10),
                LineRange::new(10, 13),
                LineRange::new(10, 10),
            )],
            algorithm: DiffAlgorithm::Histogram,
            revision: 1,
        };
        // local→base for rows INSIDE the added block: collapse
        // to base.start (the insertion point, since base is
        // empty here).
        assert_eq!(map_between(&i, 1, 0, 10), 10);
        assert_eq!(map_between(&i, 1, 0, 11), 10);
        assert_eq!(map_between(&i, 1, 0, 12), 10);
        // local→remote: collapses to remote.start = 10.
        assert_eq!(map_between(&i, 1, 2, 10), 10);
        assert_eq!(map_between(&i, 1, 2, 12), 10);
        // base→remote when neither has lines for this hunk:
        // identity (both empty; nothing to shift inside).
        assert_eq!(map_between(&i, 0, 2, 10), 10);
    }

    #[test]
    fn three_pane_mapper_dispatches_all_six_member_pair_directions() {
        use crate::pane_group::RowMapper;
        let session = Arc::new(DiffSession::new(
            lattice_core::BufferId(1),
            DiffAlgorithm::Histogram,
        ));
        session.publish(Arc::new(HunkIndex {
            hunks: vec![hunk3(
                HunkKind::Change,
                LineRange::new(10, 13),
                LineRange::new(10, 14),
                LineRange::new(10, 16),
            )],
            algorithm: DiffAlgorithm::Histogram,
            revision: 1,
        }));
        // Non-zero base/local/remote member indices — make
        // sure the lookup matches by member-idx identity,
        // not by accidentally hard-coded 0/1/2.
        let mapper = HunkRowMapper::three_pane(session, 7, 4, 9);
        // Member 7 = base (pane 0); 4 = local (pane 1); 9 = remote (pane 2).
        assert_eq!(mapper.map_row(7, 4, 30), 31, "base→local +1");
        assert_eq!(mapper.map_row(7, 9, 30), 33, "base→remote +3");
        assert_eq!(mapper.map_row(4, 7, 30), 29, "local→base -1");
        assert_eq!(mapper.map_row(4, 9, 30), 32, "local→remote +2");
        assert_eq!(mapper.map_row(9, 7, 30), 27, "remote→base -3");
        assert_eq!(mapper.map_row(9, 4, 30), 28, "remote→local -2");
        // Same-member identity.
        assert_eq!(mapper.map_row(4, 4, 42), 42);
        // Member not in the shape ⇒ identity.
        assert_eq!(mapper.map_row(100, 4, 42), 42);
        assert_eq!(mapper.map_row(4, 100, 42), 42);
    }

    #[test]
    fn three_pane_mapper_with_two_way_hunks_is_still_safe() {
        // Two-way hunks (ranges.len() == 2) routed through a
        // three-pane mapper: any direction involving pane 2
        // (remote) skips the hunk (missing range) and returns
        // identity (no shifts accumulate).
        use crate::pane_group::RowMapper;
        let session = Arc::new(DiffSession::new(
            lattice_core::BufferId(1),
            DiffAlgorithm::Histogram,
        ));
        // Publish a two-way Add hunk.
        session.publish(Arc::new(idx(vec![hunk(
            HunkKind::Add,
            LineRange::new(5, 5),
            LineRange::new(5, 8),
        )])));
        let mapper = HunkRowMapper::three_pane(session, 0, 1, 2);
        // base↔local mapping still works (the existing 2-way
        // pair).
        assert_eq!(mapper.map_row(0, 1, 100), 103);
        assert_eq!(mapper.map_row(1, 0, 100), 97);
        // Any direction involving the absent remote slot:
        // hunks are skipped (no ranges[2]) → identity.
        assert_eq!(mapper.map_row(0, 2, 100), 100);
        assert_eq!(mapper.map_row(2, 0, 100), 100);
        assert_eq!(mapper.map_row(1, 2, 100), 100);
        assert_eq!(mapper.map_row(2, 1, 100), 100);
    }
}
