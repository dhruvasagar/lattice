//! Cell-grid-specific edit delta — the input contract for
//! incremental matrix rebuilds (S2.4.b).
//!
//! `EditDelta` is the substrate's compact view of a single applied
//! edit. The protocol's `lattice_protocol::edit::EditDelta` carries
//! tree-sitter-shaped byte + position fields for incremental
//! reparse; for cell-matrix incremental rebuild the worker only
//! needs line-granular shift info.
//!
//! The cell-builder uses this to:
//! - Identify which chunks intersect the edit's affected range.
//! - Shift downstream chunks (lines past the edit) by
//!   `lines_added - lines_removed` without rebuilding their cells.

/// One applied edit's line-shift impact on the cell matrix.
///
/// All fields are in *logical source lines* (pre-fold). The edit
/// removed `lines_removed` lines starting at `start_line` (in the
/// pre-edit document) and inserted `lines_added` lines starting at
/// `start_line` (in the post-edit document).
///
/// `start_line == 0`, `lines_removed == 0`, `lines_added == 0`
/// represents the no-op identity; constructors don't filter
/// trivial deltas — the cell-builder's eligibility check does.
///
/// `Copy` so the publisher can stamp it onto each
/// `CellsRenderState` without an `Arc` bump.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditDelta {
    /// First source line the edit touches in the pre-edit document
    /// (also: first line in the post-edit document where the
    /// insert content begins). 0-based.
    pub start_line: u32,
    /// Number of *full* source lines removed. A single-line edit
    /// that doesn't cross a newline yields `0`.
    pub lines_removed: u32,
    /// Number of *full* source lines added. A single-line insert
    /// without a newline yields `0`.
    pub lines_added: u32,
}

impl EditDelta {
    /// Net line shift the edit causes for downstream lines.
    /// `lines_added - lines_removed` as an `i32` — can be
    /// negative (deletion shrinks the document).
    pub fn net_delta(&self) -> i32 {
        self.lines_added as i32 - self.lines_removed as i32
    }

    /// First source line past the edit's pre-edit affected range
    /// (exclusive). Lines `>=` this value were untouched by the
    /// edit and can shift wholesale by [`Self::net_delta`].
    pub fn pre_edit_end_line(&self) -> u32 {
        self.start_line.saturating_add(self.lines_removed)
    }

    /// First source line past the edit's post-edit affected range
    /// (exclusive). Lines `>=` this value in the post-edit
    /// document map to lines `>= pre_edit_end_line` in the
    /// pre-edit document.
    pub fn post_edit_end_line(&self) -> u32 {
        self.start_line.saturating_add(self.lines_added)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn net_delta_signs() {
        let insert = EditDelta { start_line: 5, lines_removed: 0, lines_added: 3 };
        assert_eq!(insert.net_delta(), 3);
        assert_eq!(insert.pre_edit_end_line(), 5);
        assert_eq!(insert.post_edit_end_line(), 8);

        let delete = EditDelta { start_line: 10, lines_removed: 4, lines_added: 0 };
        assert_eq!(delete.net_delta(), -4);
        assert_eq!(delete.pre_edit_end_line(), 14);
        assert_eq!(delete.post_edit_end_line(), 10);

        let replace = EditDelta { start_line: 2, lines_removed: 2, lines_added: 5 };
        assert_eq!(replace.net_delta(), 3);
        assert_eq!(replace.pre_edit_end_line(), 4);
        assert_eq!(replace.post_edit_end_line(), 7);
    }

    /// `saturating_add` guards the worst-case
    /// (`start_line` near `u32::MAX`) so the eligibility check
    /// doesn't wrap. Production callers will never hit this; it
    /// exists for defensive correctness.
    #[test]
    fn end_lines_saturate_at_u32_max() {
        let e = EditDelta {
            start_line: u32::MAX - 1,
            lines_removed: 10,
            lines_added: 0,
        };
        assert_eq!(e.pre_edit_end_line(), u32::MAX);
    }
}
