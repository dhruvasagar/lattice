//! Version vector for the cell matrix.
//!
//! Each axis tracks one source of cell-content invalidation. The
//! cell-builder worker (S2, lives in `lattice-host`) compares the
//! chunk's captured `MatrixVersion` against `RenderState`'s current
//! version via [`MatrixVersion::differs_from`]; any field that
//! changed → that chunk is stale and needs rebuild.
//!
//! ## Semantics: monotonic + hash-style fields coexist
//!
//! Some axes are monotonically-increasing counters (`text` is
//! `document.text_version()`, bumped on every rope edit). Others
//! are content hashes (`inlay_hints`, `folds`) — they don't
//! compare with `<`/`>`, only with `==`/`!=`. The version vector
//! treats every axis identically: "differs" is the only check
//! that matters for the rebuild decision, and `differs_from` is
//! correct for both kinds.
//!
//! See `docs/dev/architecture/cell-grid-renderer.md` § Invalidation
//! for the complete trigger table.

/// Per-source version stamps that drive matrix rebuild decisions.
///
/// Decoration kinds that *don't* change cell content (cursor,
/// selection, hlsearch, diagnostics, doc highlights, search matches)
/// deliberately don't appear here — they live in `OverlayState`
/// (defined in `lattice-host` in S2) and are read by the paint loop
/// each frame without triggering matrix work.
///
/// Fields are `u64` to match the source counters/hashes in
/// `lattice-host` (`document.text_version()`,
/// `compute_fold_hash`, `inlay_hints_version`, etc.) — no lossy
/// cast at the boundary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct MatrixVersion {
    /// Source of truth: `document.text_version()`. Monotonic.
    pub text: u64,
    /// Source of truth: content stamp of the syntax span set
    /// covering the buffer. Hash-style (not monotonic).
    pub syntax: u64,
    /// Source of truth: `inlay_hints_version(&inlays)`. Hash-style.
    pub inlay_hints: u64,
    /// Source of truth: `compute_fold_hash(&folds)`. Hash-style.
    pub folds: u64,
    /// Source of truth: theme palette revision. Monotonic in
    /// practice (theme replacements are discrete events).
    pub theme: u64,
    /// 2026-05-27: `display.whitespace.*` snapshot hash. Bumps
    /// when `display.show_whitespace` toggles or any of the
    /// `display.whitespace.*` glyphs change. The cell-builder
    /// substitutes whitespace bytes with marker glyphs + the
    /// `WS_MARKER` flag at emission time, so the glyph ends up
    /// in `Cell.ch` — folding the config into this axis
    /// invalidates the cached matrix when the user re-configures.
    pub whitespace: u64,
}

impl MatrixVersion {
    /// All-zero version stamp. Equivalent to `Default::default()`.
    pub const ZERO: Self = Self {
        text: 0,
        syntax: 0,
        inlay_hints: 0,
        folds: 0,
        theme: 0,
        whitespace: 0,
    };

    /// `true` when any component differs from `other`. Used by the
    /// cell-builder worker to decide if a cached chunk's version
    /// is stale relative to the current RenderState version. Works
    /// uniformly across monotonic and hash-style axes.
    pub fn differs_from(&self, other: &Self) -> bool {
        self != other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_all_zero() {
        let v = MatrixVersion::default();
        assert_eq!(v, MatrixVersion::ZERO);
    }

    #[test]
    fn differs_from_detects_each_axis() {
        let base = MatrixVersion::ZERO;
        let bumps = [
            MatrixVersion { text: 1, ..base },
            MatrixVersion { syntax: 1, ..base },
            MatrixVersion { inlay_hints: 1, ..base },
            MatrixVersion { folds: 1, ..base },
            MatrixVersion { theme: 1, ..base },
        ];
        for v in bumps {
            assert!(v.differs_from(&base), "expected differs: {v:?}");
            assert!(base.differs_from(&v), "differs is symmetric: {v:?}");
        }
    }

    #[test]
    fn differs_false_on_equal() {
        let v = MatrixVersion {
            text: 5,
            syntax: 2,
            inlay_hints: 7,
            folds: 0,
            theme: 1,
            whitespace: 0,
        };
        assert!(!v.differs_from(&v));
    }

    #[test]
    fn differs_catches_whitespace_axis() {
        let base = MatrixVersion::ZERO;
        let v = MatrixVersion {
            whitespace: 1,
            ..base
        };
        assert!(v.differs_from(&base));
        assert!(base.differs_from(&v));
    }

    /// Hash-style axes can DECREASE (a hash of fewer inlays might
    /// be numerically smaller than the previous hash). `differs_from`
    /// must catch that too, where the old `any_newer_than` did not.
    #[test]
    fn differs_catches_decreasing_hash_field() {
        let a = MatrixVersion {
            inlay_hints: 1_000_000,
            ..MatrixVersion::ZERO
        };
        let b = MatrixVersion {
            inlay_hints: 42,
            ..MatrixVersion::ZERO
        };
        assert!(a.differs_from(&b));
        assert!(b.differs_from(&a));
    }
}
