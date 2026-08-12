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

use crate::Hunk;
use lattice_core::{BufferId, Fold, FoldSource, ProviderId};

use crate::subsystem::DiffSession;

/// Namespace for per-buffer hunk-fold provider ids. OR'd with the
/// buffer's id (low 32 bits) so simultaneous diff buffers register
/// distinct overlay ids — `FoldOverlayService::add_source` keys removal
/// on the id, so a shared id would let one buffer's deregistration evict
/// another's folds. Distinct from multibuffer's `0xBBBB_*` namespaces.
pub const HUNK_FOLD_NAMESPACE: u64 = 0xD1FF_0001_0000_0000;

/// D-fix.5: namespace for per-buffer *unchanged*-fold provider ids.
/// Distinct high bits from [`HUNK_FOLD_NAMESPACE`] so a buffer's hunk
/// fold source and its unchanged fold source register under different
/// overlay ids (both OR in the buffer's low-32 id) — they coexist on the
/// same buffer covering disjoint regions (hunks vs. the gaps between).
pub const UNCHANGED_FOLD_NAMESPACE: u64 = 0xD1FF_0002_0000_0000;

/// D-fix.5: the minimum number of unchanged lines a gap must span before
/// it is folded (VS Code's `minimumLineCount`). A `Fold` already needs
/// `end_line > start_line` (≥ 2 lines) to be meaningful, so this is the
/// natural floor: a 1-line gap between two changes stays visible rather
/// than collapsing to a fold marker that hides nothing useful.
const MIN_UNCHANGED_FOLD_LINES: u32 = 2;

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
    /// D-fix.5: the `Hunk::ranges` slot this buffer occupies (0 =
    /// baseline / two-way left, 1 = current / right, 2 = remote). Was
    /// hard-coded to `1` (current side) when the source only ever
    /// registered on the session's primary buffer; now that
    /// `diff-mode::on_activate` registers a source on EVERY participant
    /// (so both panes fold in lockstep), each folds its OWN side.
    slot: usize,
}

impl HunkFoldSource {
    /// Build a source for `session`, namespaced by `buffer_id` so it is
    /// distinct from other buffers' hunk-fold sources in the registry.
    /// `slot` is the buffer's position in `Hunk::ranges` (resolved by
    /// the mode via `DiffSubsystem::participant_slot`).
    pub fn new(session: Arc<DiffSession>, buffer_id: BufferId, slot: usize) -> Self {
        Self {
            id: ProviderId(HUNK_FOLD_NAMESPACE | buffer_id.0 as u64),
            session,
            slot,
        }
    }
}

impl FoldSource for HunkFoldSource {
    fn id(&self) -> ProviderId {
        self.id
    }

    fn compute_folds(&self) -> Vec<Fold> {
        let hunks = self.session.current_hunks();
        let slot = self.slot;
        hunks
            .hunks
            .iter()
            .filter_map(|h| fold_from_hunk(h, slot))
            .collect()
    }
}

/// Translate one hunk's current-side range into a [`Fold`].
///
/// Returns `None` when the current-side range is empty (pure
/// `Remove`) or covers a single line (not meaningfully
/// foldable). `LineRange::end` is exclusive in
/// `lattice-diff`; `Fold::end_line` is inclusive in
/// `lattice-core`, so we subtract one from the end.
fn fold_from_hunk(hunk: &Hunk, slot: usize) -> Option<Fold> {
    // D-fix.5: fold the hunk's range on `slot`'s OWN side (was
    // hard-coded `ranges[1]` when this only ran on the primary).
    // Defensive: a malformed `HunkIndex` from a buggy upstream — or a
    // slot out of range for this hunk's arity — is treated as
    // unfoldable rather than a panic.
    let range = hunk.ranges.get(slot)?;
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

// ──────────────────────────────────────────────────────────────
// D-fix.5: UnchangedFoldSource — the complement of HunkFoldSource
// ──────────────────────────────────────────────────────────────

/// D-fix.5 (2026-06-26): per-buffer, per-side **unchanged**-fold source
/// — vimdiff `foldmethod=diff` / VS Code "Collapse Unchanged Regions".
///
/// Where [`HunkFoldSource`] folds the *hunks* (open by default — `za`
/// collapses a change), this folds their **complement**: the unchanged
/// gaps between hunks, minus a `context`-line window around each change,
/// **closed by default** so a diff opens showing only the changes. The
/// two coexist on the same buffer over disjoint line regions.
///
/// **Both sides, in lockstep.** `diff-mode::on_activate` registers one
/// per participant buffer (resolving each side's `slot` via
/// `DiffSubsystem::participant_slot`), so a side-by-side `:diffsplit` /
/// openDiff folds the baseline and current panes symmetrically — the
/// scroll-bound panes stay vertically aligned (folding only one side
/// would desync them).
///
/// Option-gated, read live at `compute_folds` time from the
/// `ConfigRegistry` service: `ui.diff.fold-unchanged` (default on) +
/// `ui.diff.context` (default 6). No config service / option absent ⇒
/// the safe defaults (fold on, context 6) — never a panic.
pub struct UnchangedFoldSource {
    id: ProviderId,
    session: Arc<DiffSession>,
    /// The `Hunk::ranges` slot this buffer occupies — folds the
    /// complement on this side. See [`HunkFoldSource::slot`].
    slot: usize,
    /// Live config handle for the `ui.diff.*` reads. `None` in test
    /// harnesses that don't register a `ConfigRegistry` (defaults apply).
    config: Option<Arc<lattice_config::ConfigRegistry>>,
}

impl UnchangedFoldSource {
    /// Build a source for `session` at `slot`, namespaced by `buffer_id`
    /// (distinct from the buffer's hunk-fold source via
    /// [`UNCHANGED_FOLD_NAMESPACE`]). `config` is the `ConfigRegistry`
    /// service handle the mode pulls in `on_activate` (or `None` ⇒
    /// defaults).
    pub fn new(
        session: Arc<DiffSession>,
        buffer_id: BufferId,
        slot: usize,
        config: Option<Arc<lattice_config::ConfigRegistry>>,
    ) -> Self {
        Self {
            id: ProviderId(UNCHANGED_FOLD_NAMESPACE | buffer_id.0 as u64),
            session,
            slot,
            config,
        }
    }
}

impl FoldSource for UnchangedFoldSource {
    fn id(&self) -> ProviderId {
        self.id
    }

    fn compute_folds(&self) -> Vec<Fold> {
        // Toggle (default on): read live so `:set nofold-unchanged`
        // takes effect on the next recompute.
        let enabled = self
            .config
            .as_ref()
            .and_then(|c| c.get_bool_by_name("ui.diff.fold-unchanged"))
            .unwrap_or(true);
        if !enabled {
            return Vec::new();
        }
        // Context window (default 6, vimdiff's). A negative value (can't
        // happen — validator clamps `>= 0`) falls back to the default.
        let context = self
            .config
            .as_ref()
            .and_then(|c| c.get_int_by_name("ui.diff.context"))
            .filter(|n| *n >= 0)
            .map(|n| n as u32)
            .unwrap_or(6);
        // No line count yet (no recompute has published) ⇒ nothing to
        // fold — the complement needs the side's EOF to bound itself.
        let Some(line_count) = self.session.slot_line_count(self.slot) else {
            return Vec::new();
        };
        let hunks = self.session.current_hunks();
        compute_unchanged_folds(&hunks.hunks, self.slot, line_count, context)
    }
}

/// Compute the stable identity hash for a single unchanged fold.
/// Namespaced with `"diff:unchanged"` + the slot so it never collides
/// with a hunk fold (`"diff:hunk"`) or the same span on the other side;
/// span-keyed so closed-state (a user `zo`) survives a republish that
/// reproduces the same gap.
pub fn unchanged_fold_identity(slot: usize, start_line: u32, end_line: u32) -> u64 {
    let mut h = DefaultHasher::new();
    "diff:unchanged".hash(&mut h);
    slot.hash(&mut h);
    start_line.hash(&mut h);
    end_line.hash(&mut h);
    h.finish()
}

/// D-fix.5: the pure complement-of-hunks geometry — the testable core
/// of [`UnchangedFoldSource::compute_folds`].
///
/// Given the published `hunks`, the `slot` side, that side's
/// `line_count`, and the `context` window, returns one **closed** fold
/// per unchanged gap that survives the [`MIN_UNCHANGED_FOLD_LINES`]
/// floor. The "kept visible" set is each hunk's slot range padded by
/// `context` (clamped to `[0, line_count)`), merged; the folds are its
/// complement over `[0, line_count)`.
///
/// Graceful edges: empty `hunks` (a clean diff) ⇒ no folds (don't
/// collapse an identical file into one line); `line_count == 0` ⇒ no
/// folds; a `slot` out of range for a hunk's arity skips that hunk.
fn compute_unchanged_folds(
    hunks: &[Hunk],
    slot: usize,
    line_count: u32,
    context: u32,
) -> Vec<Fold> {
    if hunks.is_empty() || line_count == 0 {
        return Vec::new();
    }
    // "Kept visible" windows: each hunk's slot range ± context. An empty
    // slot range (a pure change on the OTHER side — e.g. the baseline
    // side of an Add) still anchors a window at its insertion point so
    // the deletion/insertion marker stays in view.
    let mut kept: Vec<(u32, u32)> = Vec::new();
    for h in hunks {
        let Some(r) = h.ranges.get(slot) else {
            continue;
        };
        let start = r.start.saturating_sub(context);
        let end = (r.end + context).min(line_count);
        kept.push((start, end));
    }
    if kept.is_empty() {
        return Vec::new();
    }
    // Merge overlapping / touching windows.
    kept.sort_by_key(|(s, _)| *s);
    let mut merged: Vec<(u32, u32)> = Vec::with_capacity(kept.len());
    for (s, e) in kept {
        match merged.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    // The complement over [0, line_count) is the unchanged gaps to fold.
    let mut folds = Vec::new();
    push_unchanged_gap(&mut folds, slot, 0, merged[0].0); // leading
    for pair in merged.windows(2) {
        push_unchanged_gap(&mut folds, slot, pair[0].1, pair[1].0); // between
    }
    let last_end = merged[merged.len() - 1].1;
    push_unchanged_gap(&mut folds, slot, last_end, line_count); // trailing
    folds
}

/// Emit a closed unchanged fold for the gap `[gap_start, gap_end)` when
/// it spans at least [`MIN_UNCHANGED_FOLD_LINES`]. `Fold::end_line` is
/// inclusive (lattice-core), so it is `gap_end - 1`.
fn push_unchanged_gap(folds: &mut Vec<Fold>, slot: usize, gap_start: u32, gap_end: u32) {
    if gap_end.saturating_sub(gap_start) < MIN_UNCHANGED_FOLD_LINES {
        return;
    }
    let start_line = gap_start;
    let end_line = gap_end - 1;
    folds.push(Fold {
        start_line,
        end_line,
        closed: true,
        identity: Some(unchanged_fold_identity(slot, start_line, end_line)),
    });
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
            refine: Vec::new(),
        }
    }

    // ── fold_from_hunk: the per-hunk mapping (the real logic) ──────────

    #[test]
    fn add_hunk_yields_fold_with_inclusive_end() {
        // Add of current-side lines [2, 5) — 3 lines.
        let f = fold_from_hunk(
            &hunk(HunkKind::Add, LineRange::new(2, 2), LineRange::new(2, 5)),
            1,
        )
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
            fold_from_hunk(
                &hunk(HunkKind::Remove, LineRange::new(1, 4), LineRange::new(1, 1)),
                1,
            )
            .is_none(),
            "pure-Remove hunks have no current-side range to fold"
        );
    }

    #[test]
    fn single_line_hunk_is_not_foldable() {
        assert!(
            fold_from_hunk(
                &hunk(HunkKind::Change, LineRange::new(1, 2), LineRange::new(1, 2)),
                1,
            )
            .is_none(),
            "single-line hunks aren't foldable"
        );
    }

    #[test]
    fn malformed_hunk_with_missing_current_range_is_skipped() {
        let h = Hunk {
            kind: HunkKind::Add,
            ranges: smallvec![LineRange::new(0, 0)],
            refine: Vec::new(),
        };
        assert!(
            fold_from_hunk(&h, 1).is_none(),
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
        let src = HunkFoldSource::new(session, BufferId(1), 1);
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
            1,
        );
        let folds = src.compute_folds();
        assert_eq!(folds.len(), 2);
        assert_eq!((folds[0].start_line, folds[0].end_line), (2, 5));
        assert_eq!((folds[1].start_line, folds[1].end_line), (10, 13));
    }

    #[test]
    fn source_reflects_latest_published_hunks() {
        let session = Arc::new(DiffSession::new(BufferId(1), DiffAlgorithm::Histogram));
        let src = HunkFoldSource::new(Arc::clone(&session), BufferId(1), 1);
        assert!(src.compute_folds().is_empty(), "no publish yet → no folds");
        session.publish(Arc::new(HunkIndex {
            hunks: vec![hunk(
                HunkKind::Add,
                LineRange::new(3, 3),
                LineRange::new(3, 7),
            )],
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
        let src = HunkFoldSource::new(session, BufferId(7), 1);
        assert_eq!(src.id(), ProviderId(HUNK_FOLD_NAMESPACE | 7));
    }

    // ── HunkFoldSource: per-side (slot) folding ────────────────────────

    #[test]
    fn hunk_fold_source_folds_its_own_side() {
        // baseline [2,6) (4 lines) vs current [2,4) (2 lines): the
        // baseline-slot source (slot 0) folds [2,5]; the current-slot
        // source (slot 1) folds [2,3]. Each side folds ITS OWN range.
        let session = session_with(vec![hunk(
            HunkKind::Change,
            LineRange::new(2, 6),
            LineRange::new(2, 4),
        )]);
        let baseline = HunkFoldSource::new(Arc::clone(&session), BufferId(1), 0);
        let current = HunkFoldSource::new(session, BufferId(1), 1);
        let bf = baseline.compute_folds();
        let cf = current.compute_folds();
        assert_eq!((bf[0].start_line, bf[0].end_line), (2, 5), "baseline side");
        assert_eq!((cf[0].start_line, cf[0].end_line), (2, 3), "current side");
    }

    // ── UnchangedFoldSource: complement-of-hunks geometry ──────────────

    #[test]
    fn unchanged_no_hunks_folds_nothing() {
        // A clean diff has no changes — folding the whole file into one
        // line would be wrong (graceful: 0 hunks → no folds).
        assert!(compute_unchanged_folds(&[], 1, 100, 6).is_empty());
    }

    #[test]
    fn unchanged_folds_leading_and_trailing_gaps() {
        // One Change hunk on the current side at [50, 52), context 6,
        // file of 100 lines. Kept window = [44, 58). Complement folds:
        // leading [0, 44) → fold lines 0..=43; trailing [58, 100) →
        // fold 58..=99.
        let h = hunk(
            HunkKind::Change,
            LineRange::new(50, 52),
            LineRange::new(50, 52),
        );
        let folds = compute_unchanged_folds(&[h], 1, 100, 6);
        assert_eq!(folds.len(), 2, "leading + trailing gap");
        assert_eq!((folds[0].start_line, folds[0].end_line), (0, 43));
        assert_eq!((folds[1].start_line, folds[1].end_line), (58, 99));
        assert!(folds.iter().all(|f| f.closed), "unchanged folds are closed");
        assert!(folds.iter().all(|f| f.identity.is_some()));
    }

    #[test]
    fn unchanged_merges_close_hunks_and_keeps_context() {
        // Two hunks at [20,22) and [28,30), context 6: windows [14,28)
        // and [22,36) overlap → merge to [14,36). Complement: leading
        // [0,14) and trailing [36,100). The 6-line gap between the hunks
        // (22..28) is inside the merged kept window → NOT folded
        // (context preserved).
        let folds = compute_unchanged_folds(
            &[
                hunk(
                    HunkKind::Change,
                    LineRange::new(20, 22),
                    LineRange::new(20, 22),
                ),
                hunk(
                    HunkKind::Change,
                    LineRange::new(28, 30),
                    LineRange::new(28, 30),
                ),
            ],
            1,
            100,
            6,
        );
        assert_eq!(folds.len(), 2);
        assert_eq!((folds[0].start_line, folds[0].end_line), (0, 13));
        assert_eq!((folds[1].start_line, folds[1].end_line), (36, 99));
    }

    #[test]
    fn unchanged_min_gap_floor_skips_tiny_gaps() {
        // A change at the very top ([0,1)) with context 6 → kept [0,7);
        // leading gap [0,0) is empty (no fold). With a 1-line trailing
        // gap (line_count 8 → trailing [7,8) = 1 line) the floor of 2
        // skips it. So: no folds.
        let folds = compute_unchanged_folds(
            &[hunk(
                HunkKind::Add,
                LineRange::new(0, 0),
                LineRange::new(0, 1),
            )],
            1,
            8,
            6,
        );
        assert!(folds.is_empty(), "a 1-line trailing gap is below the floor");
    }

    #[test]
    fn unchanged_empty_range_anchors_a_window() {
        // Pure Add on the OTHER side ⇒ this (baseline, slot 0) side has
        // an EMPTY range [40,40) at the insertion point. It still
        // anchors a kept window [34,46), so the deletion marker stays
        // visible and the surrounding code folds around it.
        let h = hunk(
            HunkKind::Add,
            LineRange::new(40, 40),
            LineRange::new(40, 60),
        );
        let folds = compute_unchanged_folds(&[h], 0, 100, 6);
        assert_eq!(folds.len(), 2);
        assert_eq!((folds[0].start_line, folds[0].end_line), (0, 33));
        assert_eq!((folds[1].start_line, folds[1].end_line), (46, 99));
    }

    #[test]
    fn unchanged_source_respects_toggle_off() {
        // With a ConfigRegistry whose `ui.diff.fold-unchanged` is false,
        // the source emits nothing even when hunks + line counts exist.
        let session = Arc::new(DiffSession::new(BufferId(1), DiffAlgorithm::Histogram));
        session
            .recompute_blocking(&[
                ropey::Rope::from("a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\n"),
                ropey::Rope::from("a\nb\nc\nd\ne\nXX\ng\nh\ni\nj\nk\nl\n"),
            ])
            .expect("publish");
        let cfg = Arc::new(lattice_config::ConfigRegistry::new());
        cfg.init_from_linkme();
        cfg.parse_and_set_command("ui.diff.fold-unchanged=false")
            .expect("toggle off");
        let src = UnchangedFoldSource::new(session, BufferId(1), 1, Some(cfg));
        assert!(
            src.compute_folds().is_empty(),
            "toggle off → no unchanged folds"
        );
    }

    #[test]
    fn unchanged_source_reads_line_count_and_default_context() {
        // No config (defaults: fold on, context 6). A 24-line file with
        // one change at current [10,11) → kept [4,17); folds leading
        // [0,4) and trailing [17,24).
        let session = Arc::new(DiffSession::new(BufferId(1), DiffAlgorithm::Histogram));
        // 24 identical lines, one differing at line 10 → a Change hunk.
        let base: String = (0..24).map(|i| format!("line{i}\n")).collect();
        let mut lines: Vec<String> = (0..24).map(|i| format!("line{i}")).collect();
        lines[10] = "CHANGED".to_string();
        let cur: String = lines.iter().map(|l| format!("{l}\n")).collect();
        session
            .recompute_blocking(&[ropey::Rope::from(base), ropey::Rope::from(cur)])
            .expect("publish");
        let src = UnchangedFoldSource::new(session, BufferId(1), 1, None);
        let folds = src.compute_folds();
        assert!(!folds.is_empty(), "default-on folds the unchanged gaps");
        assert!(folds.iter().all(|f| f.closed));
        // Leading gap starts at 0; the change row 10 sits in a kept
        // window, so no fold covers it.
        assert!(folds.iter().any(|f| f.start_line == 0));
        assert!(
            !folds.iter().any(|f| f.start_line <= 10 && 10 <= f.end_line),
            "the change row is never inside an unchanged fold"
        );
    }

    #[test]
    fn unchanged_identity_distinct_from_hunk_and_per_side() {
        // The unchanged identity is namespaced away from the hunk
        // identity for the same span, and differs per side.
        assert_ne!(unchanged_fold_identity(1, 3, 6), hunk_fold_identity(3, 6));
        assert_ne!(
            unchanged_fold_identity(0, 3, 6),
            unchanged_fold_identity(1, 3, 6)
        );
        assert_eq!(
            unchanged_fold_identity(1, 3, 6),
            unchanged_fold_identity(1, 3, 6)
        );
    }
}
