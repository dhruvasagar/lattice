//! D.3.f.1 (2026-05-29): `HunkFoldProvider`.
//!
//! Overlay fold provider that emits one [`Fold`] per non-
//! empty current-side hunk range in the active diff session.
//! Registered at editor boot; emits empty when no diff
//! session is active for the current buffer (driven by
//! [`crate::fold_provider::FoldContext::diff_hunks`] being
//! `None`). Composes with whatever primary foldmethod is
//! active per `:set foldmethod=`.
//!
//! See `docs/dev/architecture/fold-architecture.md` §2 and
//! `docs/dev/architecture/diff-system.md` §6.5.
//!
//! ## Why current-side only
//!
//! Hunks classify changes against an earlier baseline; the
//! foldable region is the *current* document's lines, which
//! live in `Hunk::ranges[1]`. Pure-`Remove` hunks have an
//! empty current-side range (no current-side text to fold —
//! the deletion is surfaced via a virtual row, not a fold).
//! `Add` / `Change` / `Conflict` hunks all have non-empty
//! current-side ranges, but a single-line hunk (`ranges[1]`
//! covers exactly one line) is also non-foldable — the
//! `z*` grammar treats a 1-line fold as a no-op. Filter both
//! cases.
//!
//! ## Identity
//!
//! `hash(("diff:hunk", start_line, end_line))` is namespaced
//! with the literal `"diff:hunk"` so that a syntax fold and
//! a hunk fold covering the same `(start, end)` produce
//! distinct identity hashes. Closed-state survives across
//! diff publishes when a hunk's range is unchanged.

use std::hash::{DefaultHasher, Hash, Hasher};

use lattice_core::{Fold, ProviderId, ProviderKind};
use lattice_diff::Hunk;

use crate::fold_provider::{FoldContext, FoldProvider};

/// Stable identifier for the hunk-fold overlay. Distinct
/// from the five primary providers (0..=4 — see
/// `crate::folds`). Future overlay providers (excerpt M.7,
/// file-boundary M.8) will pick distinct ids in the 200+
/// range.
pub const HUNK_FOLD_PROVIDER_ID: ProviderId = ProviderId(100);

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

/// D.3.f.1: overlay provider for diff hunk folds.
///
/// `compute()` reads the published hunks via
/// `FoldContext::diff_hunks` (loaded by
/// `Editor::recompute_folds` from the active
/// `DiffSession::current_hunks()`). When the field is `None`
/// — no diff session for the active buffer — the provider
/// emits nothing.
pub struct HunkFoldProvider;

impl FoldProvider for HunkFoldProvider {
    fn id(&self) -> ProviderId {
        HUNK_FOLD_PROVIDER_ID
    }

    fn kind(&self) -> ProviderKind {
        ProviderKind::Overlay
    }

    fn compute(&self, ctx: &FoldContext<'_>) -> Vec<Fold> {
        let Some(hunks) = ctx.diff_hunks else {
            return Vec::new();
        };
        let mut folds = Vec::with_capacity(hunks.hunks.len());
        for hunk in &hunks.hunks {
            if let Some(fold) = fold_from_hunk(hunk) {
                folds.push(fold);
            }
        }
        folds
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
    use lattice_core::{Buffer, BufferId};
    use lattice_diff::{DiffAlgorithm, HunkIndex, HunkKind, LineRange};
    use lattice_protocol::Edit;
    use lattice_protocol::position::Position;
    use smallvec::smallvec;

    fn buf(text: &str) -> Buffer {
        let mut b = Buffer::empty();
        if !text.is_empty() {
            b.apply_edit(&Edit::insert(Position::ZERO, text.to_string()))
                .unwrap();
        }
        b
    }

    fn ctx_with_hunks<'a>(buffer: &'a Buffer, hunks: &'a HunkIndex) -> FoldContext<'a> {
        FoldContext {
            buffer,
            buffer_id: BufferId(1),
            path: None,
            syntax: None,
            lsp_folds: None,
            diff_hunks: Some(hunks),
        }
    }

    fn hunk(kind: HunkKind, baseline: LineRange, current: LineRange) -> Hunk {
        Hunk {
            kind,
            ranges: smallvec![baseline, current],
        }
    }

    #[test]
    fn no_session_emits_nothing() {
        let buffer = buf("a\nb\nc\n");
        let ctx = FoldContext {
            buffer: &buffer,
            buffer_id: BufferId(1),
            path: None,
            syntax: None,
            lsp_folds: None,
            diff_hunks: None,
        };
        assert!(HunkFoldProvider.compute(&ctx).is_empty());
    }

    #[test]
    fn empty_hunk_index_emits_nothing() {
        let buffer = buf("a\nb\nc\n");
        let idx = HunkIndex::empty(DiffAlgorithm::Histogram);
        let ctx = ctx_with_hunks(&buffer, &idx);
        assert!(HunkFoldProvider.compute(&ctx).is_empty());
    }

    #[test]
    fn add_hunk_yields_fold_with_inclusive_end() {
        let buffer = buf("a\nb\nc\nd\ne\n");
        // Add of current-side lines [2, 5) — 3 lines.
        let idx = HunkIndex {
            hunks: vec![hunk(
                HunkKind::Add,
                LineRange::new(2, 2),
                LineRange::new(2, 5),
            )],
            algorithm: DiffAlgorithm::Histogram,
            revision: 1,
        };
        let folds = HunkFoldProvider.compute(&ctx_with_hunks(&buffer, &idx));
        assert_eq!(folds.len(), 1);
        let f = folds[0];
        assert_eq!(f.start_line, 2);
        assert_eq!(f.end_line, 4, "end_line is inclusive (range.end - 1)");
        assert!(!f.closed, "freshly-emitted overlay folds start open");
        assert!(f.identity.is_some());
    }

    #[test]
    fn remove_hunk_is_not_foldable() {
        let buffer = buf("a\nb\nc\n");
        // Pure Remove: current-side range is empty.
        let idx = HunkIndex {
            hunks: vec![hunk(
                HunkKind::Remove,
                LineRange::new(1, 4),
                LineRange::new(1, 1),
            )],
            algorithm: DiffAlgorithm::Histogram,
            revision: 1,
        };
        let folds = HunkFoldProvider.compute(&ctx_with_hunks(&buffer, &idx));
        assert!(
            folds.is_empty(),
            "pure-Remove hunks have no current-side range to fold"
        );
    }

    #[test]
    fn single_line_hunk_is_not_foldable() {
        let buffer = buf("a\nb\nc\n");
        // Change spanning exactly one current-side line.
        let idx = HunkIndex {
            hunks: vec![hunk(
                HunkKind::Change,
                LineRange::new(1, 2),
                LineRange::new(1, 2),
            )],
            algorithm: DiffAlgorithm::Histogram,
            revision: 1,
        };
        let folds = HunkFoldProvider.compute(&ctx_with_hunks(&buffer, &idx));
        assert!(folds.is_empty(), "single-line hunks aren't foldable");
    }

    #[test]
    fn change_and_conflict_hunks_both_foldable() {
        let buffer = buf(&"x\n".repeat(20));
        let idx = HunkIndex {
            hunks: vec![
                hunk(HunkKind::Change, LineRange::new(2, 5), LineRange::new(2, 6)),
                hunk(
                    HunkKind::Conflict,
                    LineRange::new(10, 12),
                    LineRange::new(10, 14),
                ),
            ],
            algorithm: DiffAlgorithm::Histogram,
            revision: 1,
        };
        let folds = HunkFoldProvider.compute(&ctx_with_hunks(&buffer, &idx));
        assert_eq!(folds.len(), 2);
        assert_eq!((folds[0].start_line, folds[0].end_line), (2, 5));
        assert_eq!((folds[1].start_line, folds[1].end_line), (10, 13));
    }

    #[test]
    fn identity_is_stable_across_recomputes() {
        let buffer = buf(&"x\n".repeat(10));
        let mk_idx = |rev| HunkIndex {
            hunks: vec![hunk(
                HunkKind::Add,
                LineRange::new(3, 3),
                LineRange::new(3, 6),
            )],
            algorithm: DiffAlgorithm::Histogram,
            revision: rev,
        };
        let a = HunkFoldProvider.compute(&ctx_with_hunks(&buffer, &mk_idx(1)));
        let b = HunkFoldProvider.compute(&ctx_with_hunks(&buffer, &mk_idx(2)));
        assert_eq!(
            a[0].identity, b[0].identity,
            "identity hashes only the fold span, not the publish revision — closed-state must survive a republish that produces the same hunk"
        );
    }

    #[test]
    fn identity_distinguishes_different_spans() {
        assert_ne!(hunk_fold_identity(0, 4), hunk_fold_identity(0, 5));
        assert_ne!(hunk_fold_identity(0, 4), hunk_fold_identity(1, 4));
    }

    #[test]
    fn provider_id_matches_constant() {
        assert_eq!(HunkFoldProvider.id(), HUNK_FOLD_PROVIDER_ID);
        assert_eq!(HunkFoldProvider.kind(), ProviderKind::Overlay);
    }

    // Defensive: a malformed HunkIndex with fewer than 2
    // ranges (shouldn't happen from `compute_two_way` /
    // `compute_three_way` but could from a future bug) must
    // not panic.
    #[test]
    fn malformed_hunk_with_missing_current_range_is_skipped() {
        let buffer = buf("a\nb\n");
        let idx = HunkIndex {
            hunks: vec![Hunk {
                kind: HunkKind::Add,
                ranges: smallvec![LineRange::new(0, 0)],
            }],
            algorithm: DiffAlgorithm::Histogram,
            revision: 1,
        };
        let folds = HunkFoldProvider.compute(&ctx_with_hunks(&buffer, &idx));
        assert!(
            folds.is_empty(),
            "missing current range → no fold, no panic"
        );
    }
}
