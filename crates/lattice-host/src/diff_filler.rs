//! D.4.c (2026-05-29): hunk-alignment filler rows.
//!
//! `FillerRowProvider` is a [`VirtualRowProvider`] that
//! emits blank virtual rows on whichever side of a
//! side-by-side two-way diff is shorter for a given hunk,
//! so the hunks align visually between the two panes.
//!
//! Composes with D.4.a ([`crate::pane_group`]), D.4.b
//! ([`crate::diff_pane_group`]), and D.0a virtual rows
//! ([`lattice_cells::VirtualRowProvider`]); consumed by
//! D.4.d (`:diffsplit` / `:diffthis`).
//!
//! See `docs/dev/architecture/diff-system.md` §5.2.
//!
//! ## Why two providers per session
//!
//! A side-by-side session has two panes, each showing a
//! different buffer. Filler rows for the baseline pane
//! depend on the baseline buffer's row coordinates; filler
//! rows for the current pane depend on the current
//! buffer's. The virtual-rows worker is per-document
//! (`docs/dev/architecture/virtual-rows.md` §1), so we
//! register one provider per side, each parameterised by
//! [`Side`].
//!
//! ## Algorithm
//!
//! For each hunk in the session's published `HunkIndex`:
//!
//! - `baseline_len = hunk.ranges[0].len()`,
//!   `current_len = hunk.ranges[1].len()`.
//! - Emit `|baseline_len - current_len|` filler rows on
//!   the shorter side; nothing on the longer side.
//! - **Anchor:** if the shorter side's range is empty
//!   (pure Add for baseline, pure Remove for current),
//!   anchor at `range.start` with [`AnchorPosition::Above`]
//!   — the fillers paint immediately before the line that
//!   sits at the insertion point. If the shorter side's
//!   range is non-empty (a Change with one side shorter),
//!   anchor at `range.end - 1` with
//!   [`AnchorPosition::Below`] — fillers paint after the
//!   last changed line so the rest of the buffer aligns.
//!
//! Conflict hunks are treated the same as Change for the
//! two-way axis (D.4 is two-way only; three-way conflicts
//! land in D.6 with their own provider).

use std::sync::Arc;

use lattice_cells::{AnchorPosition, Cell, ProviderId, VirtualRow, VirtualRowProvider};
use lattice_core::BufferId;
use lattice_diff::{HunkIndex, HunkKind};

use crate::diff_subsystem::DiffSession;

/// Which pane of a side-by-side session this provider
/// emits rows for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Baseline,
    Current,
}

impl Side {
    fn namespace_bit(self) -> u64 {
        match self {
            Side::Baseline => DIFF_FILLER_BASELINE_NAMESPACE,
            Side::Current => DIFF_FILLER_CURRENT_NAMESPACE,
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
/// Distinct from `diff_overlay::DIFF_OVERLAY_PROVIDER_NAMESPACE`
/// (`0xD1FF_0000_0000_0000`) so the two coexist in the
/// global provider registry without collision.
const DIFF_FILLER_BASELINE_NAMESPACE: u64 = 0xD1FF_0001_0000_0000;

/// Namespace prefix for the current-side filler provider.
const DIFF_FILLER_CURRENT_NAMESPACE: u64 = 0xD1FF_0002_0000_0000;

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
        // fingerprint distinguishes the two sides even
        // before any hunks land. XOR with a small constant
        // for the side ensures the version differs across
        // sides at revision 0.
        let rev = self.session.current_hunks().revision;
        let side_salt: u64 = match self.side {
            Side::Baseline => 0,
            Side::Current => 1,
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
pub fn compute_filler_rows(index: &HunkIndex, side: Side) -> Vec<VirtualRow> {
    let mut rows = Vec::new();
    for hunk in &index.hunks {
        if matches!(hunk.kind, HunkKind::Conflict) {
            // Conflict hunks belong to three-way diff
            // (D.6); D.4 is two-way only. Skip to avoid
            // emitting filler against the wrong axis.
            continue;
        }
        let (Some(br), Some(cr)) = (hunk.ranges.first(), hunk.ranges.get(1)) else {
            // Malformed: < 2 ranges. Skip rather than panic.
            continue;
        };
        let baseline_len = br.end.saturating_sub(br.start);
        let current_len = cr.end.saturating_sub(cr.start);
        let (this_range, other_len) = match side {
            Side::Baseline => (br, current_len),
            Side::Current => (cr, baseline_len),
        };
        let this_len = match side {
            Side::Baseline => baseline_len,
            Side::Current => current_len,
        };
        if other_len <= this_len {
            // This side is the longer (or equal) one for
            // this hunk — no fillers needed.
            continue;
        }
        let filler_count = other_len - this_len;
        let (anchor_line, position) = if this_len == 0 {
            // Empty range on this side (Add hunk on
            // baseline, Remove on current). Anchor at the
            // insertion-point line with `Above`.
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
            hunk(
                HunkKind::Add,
                LineRange::new(5, 5),
                LineRange::new(5, 7),
            ),
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
        let overlay = crate::diff_overlay::diff_overlay_provider_id(bid);
        assert_ne!(diff_filler_provider_id(bid, Side::Baseline), overlay);
        assert_ne!(diff_filler_provider_id(bid, Side::Current), overlay);
    }

    #[test]
    fn provider_collect_reads_published_session_hunks() {
        let session = Arc::new(DiffSession::new(
            BufferId(1),
            DiffAlgorithm::Histogram,
        ));
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
        let session = Arc::new(DiffSession::new(
            BufferId(1),
            DiffAlgorithm::Histogram,
        ));
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
        let session = Arc::new(DiffSession::new(
            BufferId(1),
            DiffAlgorithm::Histogram,
        ));
        let baseline = FillerRowProvider::new(session.clone(), Side::Baseline);
        let current = FillerRowProvider::new(session, Side::Current);
        assert_ne!(
            baseline.version(),
            current.version(),
            "side salt must distinguish baseline / current at rev 0"
        );
    }
}
