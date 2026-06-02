//! D.4.c (2026-05-29) / D.6.b (2026-05-31): hunk-alignment
//! filler rows.
//!
//! `FillerRowProvider` is a [`VirtualRowProvider`] that
//! emits blank virtual rows on whichever side of a
//! side-by-side diff is shorter for a given hunk, so the
//! hunks align visually across all panes.
//!
//! Composes with D.4.a ([`crate::pane_group`]), D.4.b
//! ([`crate::diff::pane_group`]), and D.0a virtual rows
//! ([`lattice_cells::VirtualRowProvider`]); consumed by
//! D.4.d (`:diffsplit` / `:diffthis` two-way) and D.6.c
//! (three-way `:diffsplit base remote`).
//!
//! See `docs/dev/architecture/diff-system.md` §5.2.
//!
//! ## One provider per pane
//!
//! A side-by-side session has N panes (2 in two-way, 3 in
//! three-way), each showing a different buffer. Filler
//! rows for a pane depend on that buffer's row coordinates;
//! the virtual-rows worker is per-document
//! (`docs/dev/architecture/virtual-rows.md` §1), so we
//! register one provider per pane, each parameterised by
//! [`Side`].
//!
//! ## Algorithm
//!
//! For each hunk in the session's published `HunkIndex`,
//! given the pane's [`Side`]:
//!
//! - Let `lens[i] = hunk.ranges[i].len()` for each
//!   participating side.
//! - Let `max_len = max(lens)`.
//! - This pane's `this_len = lens[side.pane_index()]`.
//! - Emit `max_len - this_len` filler rows on this pane
//!   (zero when this pane is already the longest).
//! - **Anchor:** if `this_len == 0` (this pane has no
//!   lines in this hunk), anchor at `range.start` with
//!   [`AnchorPosition::Above`] — fillers paint
//!   immediately before the insertion point. If
//!   `this_len > 0`, anchor at `range.end - 1` with
//!   [`AnchorPosition::Below`] — fillers paint after the
//!   last changed line so the rest of the buffer aligns.
//!
//! Conflict hunks: in two-way (`ranges.len() == 2`) they
//! shouldn't be emitted by `compute_two_way`, so the
//! provider defensively skips them. In three-way
//! (`ranges.len() >= 3`) Conflict hunks have the same
//! row-geometry as Change for alignment purposes, so they
//! contribute fillers normally.

use std::sync::Arc;

use lattice_cells::{AnchorPosition, Cell, ProviderId, VirtualRow, VirtualRowProvider};
use lattice_core::BufferId;
use lattice_diff::{HunkIndex, HunkKind};

use crate::diff::subsystem::DiffSession;

/// Which pane of a side-by-side session this provider
/// emits rows for. Each variant maps to a slot index in
/// `Hunk::ranges`:
/// - `Baseline` ⇒ `ranges[0]` (also the base / common
///   ancestor in three-way merges)
/// - `Current` ⇒ `ranges[1]` (also "local" in three-way)
/// - `Remote` ⇒ `ranges[2]` (D.6.b three-way only)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Baseline,
    Current,
    /// D.6.b (2026-05-31): the third participant of a
    /// three-way merge.
    Remote,
}

impl Side {
    fn namespace_bit(self) -> u64 {
        match self {
            Side::Baseline => DIFF_FILLER_BASELINE_NAMESPACE,
            Side::Current => DIFF_FILLER_CURRENT_NAMESPACE,
            Side::Remote => DIFF_FILLER_REMOTE_NAMESPACE,
        }
    }

    /// D.6.b: slot index into `Hunk::ranges` for this side.
    /// 0 = baseline / base, 1 = current / local, 2 = remote.
    pub fn pane_index(self) -> usize {
        match self {
            Side::Baseline => 0,
            Side::Current => 1,
            Side::Remote => 2,
        }
    }
}

/// D.4.c: deterministic [`ProviderId`] for the filler-row
/// provider on `buffer_id` and `side`. Exposed as a free
/// function so `:diffoff` / D.4.d teardown can unregister
/// without holding the provider — the namespace encoding
/// makes the id reproducible.
pub fn diff_filler_provider_id(buffer_id: BufferId, side: Side) -> ProviderId {
    side.namespace_bit() | u64::from(buffer_id.0)
}

/// Namespace prefix for the baseline-side filler provider.
/// Distinct from `diff::overlay::DIFF_OVERLAY_PROVIDER_NAMESPACE`
/// (`0xD1FF_0000_0000_0000`) so the two coexist in the
/// global provider registry without collision.
const DIFF_FILLER_BASELINE_NAMESPACE: u64 = 0xD1FF_0001_0000_0000;

/// Namespace prefix for the current-side filler provider.
const DIFF_FILLER_CURRENT_NAMESPACE: u64 = 0xD1FF_0002_0000_0000;

/// D.6.b: namespace prefix for the remote-side filler
/// provider in a three-way merge.
const DIFF_FILLER_REMOTE_NAMESPACE: u64 = 0xD1FF_0003_0000_0000;

/// D.4.c: one provider per `(session, side)` pair.
///
/// `collect()` is synchronous and pure — it reads the
/// session's published `HunkIndex` via the lock-free
/// `current_hunks()` accessor and translates each hunk to
/// zero or more `VirtualRow`s. No background task is
/// needed (the work is O(hunks) and trivial); cf. the
/// inline diff overlay's `DiffOverlayRefreshTask` which
/// needs the off-thread render because deletion-block
/// content requires baseline-rope reads + tree-sitter
/// highlight.
#[derive(Debug)]
pub struct FillerRowProvider {
    session: Arc<DiffSession>,
    side: Side,
}

impl FillerRowProvider {
    pub fn new(session: Arc<DiffSession>, side: Side) -> Self {
        Self { session, side }
    }

    pub fn side(&self) -> Side {
        self.side
    }
}

impl VirtualRowProvider for FillerRowProvider {
    fn id(&self) -> ProviderId {
        diff_filler_provider_id(self.session.buffer_id(), self.side)
    }

    fn version(&self) -> u64 {
        // Fold session revision with side so the worker's
        // fingerprint distinguishes sides even before any
        // hunks land. XOR with a small per-side constant
        // ensures the version differs across sides at
        // revision 0.
        let rev = self.session.current_hunks().revision;
        let side_salt: u64 = match self.side {
            Side::Baseline => 0,
            Side::Current => 1,
            Side::Remote => 2,
        };
        rev ^ side_salt
    }

    fn collect(&self) -> Vec<VirtualRow> {
        let hunks = self.session.current_hunks();
        compute_filler_rows(&hunks, self.side)
    }
}

/// Pure function from `(HunkIndex, side)` to filler rows.
/// Exposed for direct unit testing without round-tripping
/// through a `DiffSession`.
///
/// **Two-way vs three-way.** Per-hunk shape is inferred
/// from `hunk.ranges.len()`:
/// - `len() == 2` (two-way): this pane's len vs the *one*
///   other side's len. Conflict hunks shouldn't occur
///   (`compute_two_way` doesn't emit them) — skipped
///   defensively.
/// - `len() >= 3` (three-way): this pane's len vs the
///   *max* of all participating sides' lens. Conflict
///   hunks contribute filler the same way Change hunks do
///   — the conflict is about content, not geometry.
///
/// If the requested `side`'s slot isn't present in the
/// hunk (e.g. `Side::Remote` on a two-way hunk), the hunk
/// is skipped — the provider degrades gracefully when a
/// session shape doesn't match the hunk shape.
pub fn compute_filler_rows(index: &HunkIndex, side: Side) -> Vec<VirtualRow> {
    let mut rows = Vec::new();
    let pane_idx = side.pane_index();
    for hunk in &index.hunks {
        let participating = hunk.ranges.len();
        if participating < 2 {
            // Malformed: need at least 2 ranges. Skip
            // rather than panic.
            continue;
        }
        let is_three_way_hunk = participating >= 3;
        if matches!(hunk.kind, HunkKind::Conflict) && !is_three_way_hunk {
            // Defensive: two-way Conflict shouldn't exist.
            // Skip to avoid emitting filler against an
            // engine output the two-way axis won't produce.
            continue;
        }
        let Some(this_range) = hunk.ranges.get(pane_idx).copied() else {
            // This side's slot isn't in the hunk (e.g.
            // Side::Remote on a 2-way hunk). Skip.
            continue;
        };
        let this_len = this_range.end.saturating_sub(this_range.start);
        let target_len = hunk
            .ranges
            .iter()
            .map(|r| r.end.saturating_sub(r.start))
            .max()
            .unwrap_or(0);
        if target_len <= this_len {
            // This pane is already the longest (or tied)
            // for this hunk — no fillers needed.
            continue;
        }
        let filler_count = target_len - this_len;
        let (anchor_line, position) = if this_len == 0 {
            // Empty range on this side (Add hunk's
            // baseline, Remove hunk's current, or a
            // three-way side that didn't touch the base
            // region). Anchor at the insertion-point line
            // with `Above`.
            (this_range.start, AnchorPosition::Above)
        } else {
            // Non-empty range — anchor at the last
            // changed line, position `Below`.
            (this_range.end - 1, AnchorPosition::Below)
        };
        let blank_cells: Arc<[Cell]> = Arc::from([Cell::BLANK]);
        for _ in 0..filler_count {
            rows.push(VirtualRow {
                anchor_line,
                position,
                cells: blank_cells.clone(),
                height: 1,
                // D.6.i: filler rows paint with no backdrop —
                // they're visual padding for side-by-side
                // alignment, not deleted content.
                kind: lattice_cells::VirtualRowKind::Filler,
            });
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use lattice_diff::{DiffAlgorithm, Hunk, LineRange};
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

    #[test]
    fn empty_index_emits_no_fillers_on_either_side() {
        let i = HunkIndex::empty(DiffAlgorithm::Histogram);
        assert!(compute_filler_rows(&i, Side::Baseline).is_empty());
        assert!(compute_filler_rows(&i, Side::Current).is_empty());
    }

    #[test]
    fn add_hunk_emits_fillers_on_baseline_only() {
        // Add 3 lines on current at position 5; baseline
        // has 0 lines there. Baseline needs 3 filler rows
        // above its insertion line (5).
        let i = idx(vec![hunk(
            HunkKind::Add,
            LineRange::new(5, 5),
            LineRange::new(5, 8),
        )]);
        let baseline_fillers = compute_filler_rows(&i, Side::Baseline);
        let current_fillers = compute_filler_rows(&i, Side::Current);
        assert_eq!(baseline_fillers.len(), 3);
        assert!(current_fillers.is_empty());
        for row in &baseline_fillers {
            assert_eq!(row.anchor_line, 5);
            assert_eq!(row.position, AnchorPosition::Above);
            assert_eq!(row.height, 1);
        }
    }

    #[test]
    fn remove_hunk_emits_fillers_on_current_only() {
        // Remove 3 lines from baseline at position 5;
        // current has nothing.
        let i = idx(vec![hunk(
            HunkKind::Remove,
            LineRange::new(5, 8),
            LineRange::new(5, 5),
        )]);
        let baseline_fillers = compute_filler_rows(&i, Side::Baseline);
        let current_fillers = compute_filler_rows(&i, Side::Current);
        assert!(baseline_fillers.is_empty());
        assert_eq!(current_fillers.len(), 3);
        for row in &current_fillers {
            assert_eq!(row.anchor_line, 5);
            assert_eq!(row.position, AnchorPosition::Above);
        }
    }

    #[test]
    fn change_hunk_baseline_longer_emits_fillers_on_current() {
        // baseline [10, 15) = 5 lines; current [10, 12) = 2 lines.
        // Current is shorter by 3 ⇒ 3 fillers on current,
        // anchored at cr.end-1 = 11 with Below.
        let i = idx(vec![hunk(
            HunkKind::Change,
            LineRange::new(10, 15),
            LineRange::new(10, 12),
        )]);
        let baseline_fillers = compute_filler_rows(&i, Side::Baseline);
        let current_fillers = compute_filler_rows(&i, Side::Current);
        assert!(baseline_fillers.is_empty());
        assert_eq!(current_fillers.len(), 3);
        for row in &current_fillers {
            assert_eq!(row.anchor_line, 11);
            assert_eq!(row.position, AnchorPosition::Below);
        }
    }

    #[test]
    fn change_hunk_current_longer_emits_fillers_on_baseline() {
        // baseline [10, 12) = 2 lines; current [10, 15) = 5 lines.
        // Baseline is shorter by 3 ⇒ 3 fillers on baseline,
        // anchored at br.end-1 = 11 with Below.
        let i = idx(vec![hunk(
            HunkKind::Change,
            LineRange::new(10, 12),
            LineRange::new(10, 15),
        )]);
        let baseline_fillers = compute_filler_rows(&i, Side::Baseline);
        let current_fillers = compute_filler_rows(&i, Side::Current);
        assert_eq!(baseline_fillers.len(), 3);
        assert!(current_fillers.is_empty());
        for row in &baseline_fillers {
            assert_eq!(row.anchor_line, 11);
            assert_eq!(row.position, AnchorPosition::Below);
        }
    }

    #[test]
    fn change_hunk_equal_lengths_emits_no_fillers() {
        let i = idx(vec![hunk(
            HunkKind::Change,
            LineRange::new(10, 13),
            LineRange::new(10, 13),
        )]);
        assert!(compute_filler_rows(&i, Side::Baseline).is_empty());
        assert!(compute_filler_rows(&i, Side::Current).is_empty());
    }

    #[test]
    fn multiple_hunks_accumulate_independently() {
        // Add (+2 on current) then Remove (+3 on baseline).
        // Baseline-side fillers: 2 from the Add only.
        // Current-side fillers: 3 from the Remove only.
        let i = idx(vec![
            hunk(HunkKind::Add, LineRange::new(5, 5), LineRange::new(5, 7)),
            hunk(
                HunkKind::Remove,
                LineRange::new(20, 23),
                LineRange::new(22, 22),
            ),
        ]);
        let baseline_fillers = compute_filler_rows(&i, Side::Baseline);
        let current_fillers = compute_filler_rows(&i, Side::Current);
        assert_eq!(baseline_fillers.len(), 2);
        assert_eq!(current_fillers.len(), 3);
        // Baseline fillers anchored at the Add insertion
        // point (5), Above.
        for row in &baseline_fillers {
            assert_eq!(row.anchor_line, 5);
            assert_eq!(row.position, AnchorPosition::Above);
        }
        // Current fillers anchored at the Remove insertion
        // point on current side (22), Above.
        for row in &current_fillers {
            assert_eq!(row.anchor_line, 22);
            assert_eq!(row.position, AnchorPosition::Above);
        }
    }

    #[test]
    fn conflict_hunks_skipped_in_two_way_provider() {
        let i = idx(vec![hunk(
            HunkKind::Conflict,
            LineRange::new(5, 10),
            LineRange::new(5, 8),
        )]);
        assert!(compute_filler_rows(&i, Side::Baseline).is_empty());
        assert!(compute_filler_rows(&i, Side::Current).is_empty());
    }

    #[test]
    fn malformed_hunk_with_fewer_than_two_ranges_is_skipped() {
        let i = idx(vec![Hunk {
            kind: HunkKind::Add,
            ranges: smallvec![LineRange::new(0, 0)],
        }]);
        assert!(compute_filler_rows(&i, Side::Baseline).is_empty());
        assert!(compute_filler_rows(&i, Side::Current).is_empty());
    }

    // ── Provider plumbing ─────────────────────────────────

    #[test]
    fn provider_ids_distinct_per_side() {
        let bid = BufferId(7);
        let baseline_id = diff_filler_provider_id(bid, Side::Baseline);
        let current_id = diff_filler_provider_id(bid, Side::Current);
        assert_ne!(baseline_id, current_id);
        // Buffer-id bits visible in the low 32:
        assert_eq!(baseline_id as u32, bid.0);
        assert_eq!(current_id as u32, bid.0);
    }

    #[test]
    fn provider_id_does_not_collide_with_overlay_namespace() {
        let bid = BufferId(7);
        let overlay = crate::diff::overlay::diff_overlay_provider_id(bid);
        assert_ne!(diff_filler_provider_id(bid, Side::Baseline), overlay);
        assert_ne!(diff_filler_provider_id(bid, Side::Current), overlay);
    }

    #[test]
    fn provider_collect_reads_published_session_hunks() {
        let session = Arc::new(DiffSession::new(BufferId(1), DiffAlgorithm::Histogram));
        // Empty session ⇒ no fillers.
        let provider = FillerRowProvider::new(session.clone(), Side::Baseline);
        assert!(provider.collect().is_empty());
        // Publish an Add hunk; baseline side now sees fillers.
        session.publish(Arc::new(idx(vec![hunk(
            HunkKind::Add,
            LineRange::new(5, 5),
            LineRange::new(5, 8),
        )])));
        assert_eq!(provider.collect().len(), 3);
        // Current-side provider sees nothing for the same Add.
        let other = FillerRowProvider::new(session, Side::Current);
        assert!(other.collect().is_empty());
    }

    #[test]
    fn provider_version_changes_with_session_revision() {
        let session = Arc::new(DiffSession::new(BufferId(1), DiffAlgorithm::Histogram));
        let p = FillerRowProvider::new(session.clone(), Side::Baseline);
        let v0 = p.version();
        session.publish(Arc::new(idx(vec![hunk(
            HunkKind::Add,
            LineRange::new(0, 0),
            LineRange::new(0, 1),
        )])));
        let v1 = p.version();
        assert_ne!(v0, v1);
    }

    #[test]
    fn provider_version_differs_across_sides_at_rev_zero() {
        let session = Arc::new(DiffSession::new(BufferId(1), DiffAlgorithm::Histogram));
        let baseline = FillerRowProvider::new(session.clone(), Side::Baseline);
        let current = FillerRowProvider::new(session, Side::Current);
        assert_ne!(
            baseline.version(),
            current.version(),
            "side salt must distinguish baseline / current at rev 0"
        );
    }

    // ──────────────────────────────────────────────────────
    // D.6.b (2026-05-31): three-pane filler rows
    // ──────────────────────────────────────────────────────

    fn hunk3(kind: HunkKind, base: LineRange, local: LineRange, remote: LineRange) -> Hunk {
        Hunk {
            kind,
            ranges: smallvec![base, local, remote],
        }
    }

    #[test]
    fn three_way_change_aligns_all_three_panes_to_max_length() {
        // base [10, 13) = 3 lines; local [10, 14) = 4 lines;
        // remote [10, 16) = 6 lines. max = 6. So:
        // - base: 6 - 3 = 3 fillers
        // - local: 6 - 4 = 2 fillers
        // - remote: 0 fillers
        let i = idx(vec![hunk3(
            HunkKind::Change,
            LineRange::new(10, 13),
            LineRange::new(10, 14),
            LineRange::new(10, 16),
        )]);
        let base_fillers = compute_filler_rows(&i, Side::Baseline);
        let local_fillers = compute_filler_rows(&i, Side::Current);
        let remote_fillers = compute_filler_rows(&i, Side::Remote);
        assert_eq!(base_fillers.len(), 3, "base shorter by 3 lines");
        assert_eq!(local_fillers.len(), 2, "local shorter by 2 lines");
        assert!(remote_fillers.is_empty(), "remote is longest");
        // Anchor for non-empty range: range.end - 1, Below.
        for row in &base_fillers {
            assert_eq!(row.anchor_line, 12);
            assert_eq!(row.position, AnchorPosition::Below);
        }
        for row in &local_fillers {
            assert_eq!(row.anchor_line, 13);
            assert_eq!(row.position, AnchorPosition::Below);
        }
    }

    #[test]
    fn three_way_add_on_local_emits_fillers_on_base_and_remote() {
        // base [10, 10) empty; local [10, 13) = 3 lines;
        // remote [10, 10) empty. max = 3. base + remote
        // each need 3 fillers anchored Above at row 10.
        let i = idx(vec![hunk3(
            HunkKind::Add,
            LineRange::new(10, 10),
            LineRange::new(10, 13),
            LineRange::new(10, 10),
        )]);
        let base_fillers = compute_filler_rows(&i, Side::Baseline);
        let local_fillers = compute_filler_rows(&i, Side::Current);
        let remote_fillers = compute_filler_rows(&i, Side::Remote);
        assert_eq!(base_fillers.len(), 3);
        assert!(local_fillers.is_empty(), "local is longest");
        assert_eq!(remote_fillers.len(), 3);
        for row in base_fillers.iter().chain(remote_fillers.iter()) {
            assert_eq!(row.anchor_line, 10);
            assert_eq!(row.position, AnchorPosition::Above);
        }
    }

    #[test]
    fn three_way_conflict_hunk_emits_fillers_for_alignment() {
        // Three-way Conflict — both local and remote
        // mutated the base region differently. The
        // alignment problem is the same as Change; fillers
        // pad whichever side(s) are shorter.
        let i = idx(vec![hunk3(
            HunkKind::Conflict,
            LineRange::new(10, 12), // 2 lines
            LineRange::new(10, 14), // 4 lines
            LineRange::new(10, 15), // 5 lines
        )]);
        let base_fillers = compute_filler_rows(&i, Side::Baseline);
        let local_fillers = compute_filler_rows(&i, Side::Current);
        let remote_fillers = compute_filler_rows(&i, Side::Remote);
        assert_eq!(base_fillers.len(), 3, "base shorter by 3");
        assert_eq!(local_fillers.len(), 1, "local shorter by 1");
        assert!(remote_fillers.is_empty(), "remote is longest");
    }

    #[test]
    fn three_way_equal_lengths_emit_no_fillers() {
        let i = idx(vec![hunk3(
            HunkKind::Change,
            LineRange::new(10, 13),
            LineRange::new(10, 13),
            LineRange::new(10, 13),
        )]);
        for side in [Side::Baseline, Side::Current, Side::Remote] {
            assert!(
                compute_filler_rows(&i, side).is_empty(),
                "no fillers when all three lengths match ({side:?})"
            );
        }
    }

    #[test]
    fn two_way_hunk_via_remote_side_is_skipped_not_panic() {
        // A two-way hunk (ranges.len() == 2) queried for
        // Side::Remote (pane 2) must skip the hunk
        // gracefully — the slot doesn't exist.
        let i = idx(vec![hunk(
            HunkKind::Add,
            LineRange::new(5, 5),
            LineRange::new(5, 8),
        )]);
        assert!(
            compute_filler_rows(&i, Side::Remote).is_empty(),
            "remote-side filler on a 2-way hunk should be empty, not panic"
        );
    }

    #[test]
    fn three_way_malformed_single_range_skipped() {
        let i = idx(vec![Hunk {
            kind: HunkKind::Change,
            ranges: smallvec![LineRange::new(0, 5)],
        }]);
        for side in [Side::Baseline, Side::Current, Side::Remote] {
            assert!(compute_filler_rows(&i, side).is_empty());
        }
    }

    // ── Provider plumbing ─────────────────────────────────

    #[test]
    fn remote_provider_id_distinct_from_baseline_and_current() {
        let bid = BufferId(7);
        let base = diff_filler_provider_id(bid, Side::Baseline);
        let cur = diff_filler_provider_id(bid, Side::Current);
        let rem = diff_filler_provider_id(bid, Side::Remote);
        assert_ne!(rem, base);
        assert_ne!(rem, cur);
        assert_ne!(base, cur);
        // Buffer-id bits still visible in the low 32.
        assert_eq!(rem as u32, bid.0);
    }

    #[test]
    fn remote_provider_id_does_not_collide_with_overlay_namespace() {
        let bid = BufferId(7);
        let overlay = crate::diff::overlay::diff_overlay_provider_id(bid);
        assert_ne!(diff_filler_provider_id(bid, Side::Remote), overlay);
    }

    /// D.6.i (2026-05-31): filler-row provider emits
    /// `VirtualRowKind::Filler` so renderers skip the
    /// deletion-block backdrop on padding rows.
    #[test]
    fn filler_rows_carry_filler_kind() {
        let i = idx(vec![hunk(
            HunkKind::Add,
            LineRange::new(5, 5),
            LineRange::new(5, 8),
        )]);
        let rows = compute_filler_rows(&i, Side::Baseline);
        assert!(!rows.is_empty());
        for row in &rows {
            assert_eq!(
                row.kind,
                lattice_cells::VirtualRowKind::Filler,
                "filler rows must be tagged Filler so renderers skip the \
                 deletion-block backdrop"
            );
        }
    }

    #[test]
    fn provider_version_distinct_across_all_three_sides_at_rev_zero() {
        let session = Arc::new(DiffSession::new(BufferId(1), DiffAlgorithm::Histogram));
        let baseline = FillerRowProvider::new(session.clone(), Side::Baseline);
        let current = FillerRowProvider::new(session.clone(), Side::Current);
        let remote = FillerRowProvider::new(session, Side::Remote);
        // All three salts must be pairwise distinct.
        let v = (baseline.version(), current.version(), remote.version());
        assert_ne!(v.0, v.1);
        assert_ne!(v.0, v.2);
        assert_ne!(v.1, v.2);
    }

    #[test]
    fn remote_provider_collect_reads_published_three_way_hunks() {
        let session = Arc::new(DiffSession::new(BufferId(1), DiffAlgorithm::Histogram));
        session.publish(Arc::new(idx(vec![hunk3(
            HunkKind::Add,
            LineRange::new(10, 10),
            LineRange::new(10, 13),
            LineRange::new(10, 10),
        )])));
        let remote = FillerRowProvider::new(session, Side::Remote);
        assert_eq!(
            remote.collect().len(),
            3,
            "remote pane gets 3 fillers to align with local's Add"
        );
    }

    #[test]
    fn side_pane_index_matches_ranges_slot() {
        assert_eq!(Side::Baseline.pane_index(), 0);
        assert_eq!(Side::Current.pane_index(), 1);
        assert_eq!(Side::Remote.pane_index(), 2);
    }
}
