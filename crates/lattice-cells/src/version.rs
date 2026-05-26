//! Version vector for the cell matrix.
//!
//! Each axis tracks one source of cell-content invalidation. The
//! cell-builder worker (S2, lives in `lattice-host`) compares the
//! chunk's captured `MatrixVersion` against `RenderState`'s current
//! version; any field that advanced → that chunk is stale.
//!
//! See `docs/dev/architecture/cell-grid-renderer.md` § Invalidation
//! for the complete trigger table.

/// Per-source version counters that drive matrix rebuild decisions.
///
/// Decoration kinds that *don't* change cell content (cursor,
/// selection, hlsearch, diagnostics, doc highlights, search matches)
/// deliberately don't appear here — they live in `OverlayState`
/// (defined in `lattice-host` in S2) and are read by the paint loop
/// each frame without triggering matrix work.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct MatrixVersion {
    /// Bumped on every rope mutation.
    pub text: u32,
    /// Bumped when `lattice-syntax` publishes new spans for the buffer.
    pub syntax: u32,
    /// Bumped when LSP inlay hints arrive/change for the buffer.
    pub inlay_hints: u32,
    /// Bumped when fold ranges change (collapse, expand, computed).
    pub folds: u32,
    /// Bumped when the active theme is replaced or its palette mutates.
    pub theme: u32,
}

impl MatrixVersion {
    /// Convenience: all-zero version vector. Equivalent to
    /// `Default::default()`; provided for explicit-init call sites
    /// where `Default` would be ambiguous.
    pub const ZERO: Self = Self {
        text: 0,
        syntax: 0,
        inlay_hints: 0,
        folds: 0,
        theme: 0,
    };

    /// `true` when *any* component of `self` is strictly newer than
    /// the corresponding component of `other`. Used by the
    /// cell-builder worker to decide whether a chunk's cached
    /// version is stale relative to the current RenderState
    /// version.
    pub fn any_newer_than(&self, other: &Self) -> bool {
        self.text > other.text
            || self.syntax > other.syntax
            || self.inlay_hints > other.inlay_hints
            || self.folds > other.folds
            || self.theme > other.theme
    }

    /// Component-wise maximum. Used when merging multiple chunks'
    /// captured versions into a `CellMatrix::version` summary.
    pub fn max(&self, other: &Self) -> Self {
        Self {
            text: self.text.max(other.text),
            syntax: self.syntax.max(other.syntax),
            inlay_hints: self.inlay_hints.max(other.inlay_hints),
            folds: self.folds.max(other.folds),
            theme: self.theme.max(other.theme),
        }
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
    fn any_newer_detects_each_axis() {
        let base = MatrixVersion::ZERO;
        let bumps = [
            MatrixVersion { text: 1, ..base },
            MatrixVersion { syntax: 1, ..base },
            MatrixVersion { inlay_hints: 1, ..base },
            MatrixVersion { folds: 1, ..base },
            MatrixVersion { theme: 1, ..base },
        ];
        for v in bumps {
            assert!(v.any_newer_than(&base), "expected newer: {v:?}");
            assert!(!base.any_newer_than(&v), "expected not-newer: {v:?}");
        }
    }

    #[test]
    fn any_newer_false_on_equal() {
        let v = MatrixVersion {
            text: 5,
            syntax: 2,
            inlay_hints: 7,
            folds: 0,
            theme: 1,
        };
        assert!(!v.any_newer_than(&v));
    }

    #[test]
    fn max_takes_component_wise_max() {
        let a = MatrixVersion {
            text: 5,
            syntax: 2,
            inlay_hints: 0,
            folds: 9,
            theme: 1,
        };
        let b = MatrixVersion {
            text: 3,
            syntax: 7,
            inlay_hints: 4,
            folds: 9,
            theme: 0,
        };
        let m = a.max(&b);
        assert_eq!(
            m,
            MatrixVersion {
                text: 5,
                syntax: 7,
                inlay_hints: 4,
                folds: 9,
                theme: 1,
            }
        );
    }
}
