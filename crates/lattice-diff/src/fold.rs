//! D.3.f.1 (2026-05-29) / DX.3-C7 (2026-06-24): `HunkFoldSource`.
//!
//! A [`lattice_core::FoldSource`] that emits one [`Fold`] per non-empty,
//! multi-line current-side hunk range of a diff session. **Mode-owned**:
//! `diff-mode`'s `on_activate` constructs one per participating buffer
//! (holding that buffer's `Arc<DiffSession>`) and registers it via the
//! `FoldOverlayService`; the mode's `Drop` guard removes it. This is the
//! same shape multibuffer's `ExcerptFoldProvider` / `FileBoundaryFoldProvider`
//! use — a self-contained `FoldSource` (no `FoldContext`) wrapped by the
//! host's `FoldSourceAdapter`, which gates `compute_folds` to the target
//! buffer. (Before C7 this was a context-driven `FoldProvider` pre-seeded
//! into `FoldRegistry::with_builtins`, reading `FoldContext::diff_hunks`;
//! that coupling between the host fold substrate and `lattice-diff` is now
//! gone.)
//!
//! See `docs/dev/architecture/fold-architecture.md` §2 and
//! `docs/dev/architecture/diff-system.md` §6.5.
//!
//! ## Why current-side only
//!
//! Hunks classify changes against an earlier baseline; the foldable
//! region is the *current* document's lines, which live in
//! `Hunk::ranges[1]`. Pure-`Remove` hunks have an empty current-side
//! range (no current-side text to fold — the deletion is surfaced via a
//! virtual row, not a fold). `Add` / `Change` / `Conflict` hunks all have
//! non-empty current-side ranges, but a single-line hunk (`ranges[1]`
//! covers exactly one line) is also non-foldable — the `z*` grammar
//! treats a 1-line fold as a no-op. Filter both cases.
//!
//! ## Identity
//!
//! `hash(("diff:hunk", start_line, end_line))` is namespaced with the
//! literal `"diff:hunk"` so that a syntax fold and a hunk fold covering
//! the same `(start, end)` produce distinct identity hashes. Closed-state
//! survives across diff publishes when a hunk's range is unchanged.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use lattice_core::{BufferId, Fold, FoldSource, ProviderId};
use crate::Hunk;

use crate::subsystem::DiffSession;

/// Namespace for per-buffer hunk-fold provider ids. OR'd with the
/// buffer's id (low 32 bits) so simultaneous diff buffers register
/// distinct overlay ids — `FoldOverlayService::add_source` keys removal
/// on the id, so a shared id would let one buffer's deregistration evict
/// another's folds. Distinct from multibuffer's `0xBBBB_*` namespaces.
pub const HUNK_FOLD_NAMESPACE: u64 = 0xD1FF_0001_0000_0000;

/// Compute the stable identity hash for a single hunk fold.
///
/// Namespaced with `"diff:hunk"` so the hash doesn't collide
/// with a primary provider's hash for the same `(start_line,
/// end_line)`. Two publishes that produce a hunk at the same
/// span hash identically, so closed-state carries over.
pub fn hunk_fold_identity(start_line: u32, end_line: u32) -> u64 {
    let mut h = DefaultHasher::new();
    "diff:hunk".hash(&mut h);
    start_line.hash(&mut h);
    end_line.hash(&mut h);
    h.finish()
}

/// DX.3-C7: per-buffer hunk-fold source.
///
/// Holds the buffer's `Arc<DiffSession>`; `compute_folds` reads the
/// session's currently-published `HunkIndex` (lock-free
/// `current_hunks()`), so folds track every republish without a context.
/// `diff-mode::on_activate` registers one via `FoldOverlayService`;
/// `DiffModeGuard::drop` removes it.
pub struct HunkFoldSource {
    id: ProviderId,
    session: Arc<DiffSession>,
}

impl HunkFoldSource {
    /// Build a source for `session`, namespaced by `buffer_id` so it is
    /// distinct from other buffers' hunk-fold sources in the registry.
    pub fn new(session: Arc<DiffSession>, buffer_id: BufferId) -> Self {
        Self {
            id: ProviderId(HUNK_FOLD_NAMESPACE | buffer_id.0 as u64),
            session,
        }
    }
}

impl FoldSource for HunkFoldSource {
    fn id(&self) -> ProviderId {
        self.id
    }

    fn compute_folds(&self) -> Vec<Fold> {
        let hunks = self.session.current_hunks();
        hunks.hunks.iter().filter_map(fold_from_hunk).collect()
    }
}

/// Translate one hunk's current-side range into a [`Fold`].
///
/// Returns `None` when the current-side range is empty (pure
/// `Remove`) or covers a single line (not meaningfully
/// foldable). `LineRange::end` is exclusive in
/// `lattice-diff`; `Fold::end_line` is inclusive in
/// `lattice-core`, so we subtract one from the end.
fn fold_from_hunk(hunk: &Hunk) -> Option<Fold> {
    // Current side lives at `ranges[1]` for both two-way and
    // three-way diffs (base / earlier side at `ranges[0]`).
    // Defensive: a malformed `HunkIndex` from a buggy
    // upstream could have `ranges.len() < 2`; treat it as
    // unfoldable rather than panic.
    let range = hunk.ranges.get(1)?;
    // Require at least two current-side lines for the fold
    // to be meaningful — collapsing a single line to itself
    // is a no-op the `z*` family wouldn't surface.
    if range.end <= range.start + 1 {
        return None;
    }
    let start_line = range.start;
    let end_line = range.end - 1;
    Some(Fold {
        start_line,
        end_line,
        closed: false,
        identity: Some(hunk_fold_identity(start_line, end_line)),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::{DiffAlgorithm, Hunk, HunkIndex, HunkKind, LineRange};
    use smallvec::smallvec;

    fn hunk(kind: HunkKind, baseline: LineRange, current: LineRange) -> Hunk {
        Hunk {
            kind,
            ranges: smallvec![baseline, current],
        }
    }

    // ── fold_from_hunk: the per-hunk mapping (the real logic) ──────────

    #[test]
    fn add_hunk_yields_fold_with_inclusive_end() {
        // Add of current-side lines [2, 5) — 3 lines.
        let f = fold_from_hunk(&hunk(
            HunkKind::Add,
            LineRange::new(2, 2),
            LineRange::new(2, 5),
        ))
        .expect("multi-line add is foldable");
        assert_eq!(f.start_line, 2);
        assert_eq!(f.end_line, 4, "end_line is inclusive (range.end - 1)");
        assert!(!f.closed, "freshly-emitted overlay folds start open");
        assert!(f.identity.is_some());
    }

    #[test]
    fn remove_hunk_is_not_foldable() {
        // Pure Remove: current-side range is empty.
        assert!(
            fold_from_hunk(&hunk(
                HunkKind::Remove,
                LineRange::new(1, 4),
                LineRange::new(1, 1),
            ))
            .is_none(),
            "pure-Remove hunks have no current-side range to fold"
        );
    }

    #[test]
    fn single_line_hunk_is_not_foldable() {
        assert!(
            fold_from_hunk(&hunk(
                HunkKind::Change,
                LineRange::new(1, 2),
                LineRange::new(1, 2),
            ))
            .is_none(),
            "single-line hunks aren't foldable"
        );
    }

    #[test]
    fn malformed_hunk_with_missing_current_range_is_skipped() {
        let h = Hunk {
            kind: HunkKind::Add,
            ranges: smallvec![LineRange::new(0, 0)],
        };
        assert!(
            fold_from_hunk(&h).is_none(),
            "missing current range → no fold, no panic"
        );
    }

    #[test]
    fn identity_is_stable_across_spans_and_distinct_between_spans() {
        // Span-only hash: a republish that produces the same span hashes
        // identically (closed-state survives); different spans differ.
        assert_eq!(hunk_fold_identity(3, 6), hunk_fold_identity(3, 6));
        assert_ne!(hunk_fold_identity(0, 4), hunk_fold_identity(0, 5));
        assert_ne!(hunk_fold_identity(0, 4), hunk_fold_identity(1, 4));
    }

    // ── HunkFoldSource: reads the session's published hunks ────────────

    fn session_with(hunks: Vec<Hunk>) -> Arc<DiffSession> {
        let session = Arc::new(DiffSession::new(BufferId(1), DiffAlgorithm::Histogram));
        session.publish(Arc::new(HunkIndex {
            hunks,
            algorithm: DiffAlgorithm::Histogram,
            revision: 1,
        }));
        session
    }

    #[test]
    fn no_published_hunks_emits_nothing() {
        let session = Arc::new(DiffSession::new(BufferId(1), DiffAlgorithm::Histogram));
        let src = HunkFoldSource::new(session, BufferId(1));
        assert!(src.compute_folds().is_empty());
    }

    #[test]
    fn change_and_conflict_hunks_both_foldable() {
        let src = HunkFoldSource::new(
            session_with(vec![
                hunk(HunkKind::Change, LineRange::new(2, 5), LineRange::new(2, 6)),
                hunk(
                    HunkKind::Conflict,
                    LineRange::new(10, 12),
                    LineRange::new(10, 14),
                ),
            ]),
            BufferId(1),
        );
        let folds = src.compute_folds();
        assert_eq!(folds.len(), 2);
        assert_eq!((folds[0].start_line, folds[0].end_line), (2, 5));
        assert_eq!((folds[1].start_line, folds[1].end_line), (10, 13));
    }

    #[test]
    fn source_reflects_latest_published_hunks() {
        let session = Arc::new(DiffSession::new(BufferId(1), DiffAlgorithm::Histogram));
        let src = HunkFoldSource::new(Arc::clone(&session), BufferId(1));
        assert!(src.compute_folds().is_empty(), "no publish yet → no folds");
        session.publish(Arc::new(HunkIndex {
            hunks: vec![hunk(HunkKind::Add, LineRange::new(3, 3), LineRange::new(3, 7))],
            algorithm: DiffAlgorithm::Histogram,
            revision: 1,
        }));
        let folds = src.compute_folds();
        assert_eq!(folds.len(), 1, "compute_folds reads current_hunks live");
        assert_eq!((folds[0].start_line, folds[0].end_line), (3, 6));
    }

    #[test]
    fn id_is_namespaced_per_buffer() {
        let session = Arc::new(DiffSession::new(BufferId(7), DiffAlgorithm::Histogram));
        let src = HunkFoldSource::new(session, BufferId(7));
        assert_eq!(src.id(), ProviderId(HUNK_FOLD_NAMESPACE | 7));
    }
}
