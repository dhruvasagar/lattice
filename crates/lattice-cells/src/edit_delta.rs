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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
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
    /// Whether the edit's OLD (pre-edit) range ends at column
    /// (byte) `0` of its last line — i.e. a genuine line boundary
    /// (BOL of the line one past [`Self::pre_edit_end_line`]) —
    /// rather than partway into (or at the very end of) that line.
    ///
    /// This is the bit `lines_removed`/`lines_added` cannot carry:
    /// they are a tree-sitter-shaped *row delta* (`old_end_position.line
    /// - start_line`), which counts the same for "range ends at BOL of
    /// the following line" (the line at `pre_edit_end_line` is
    /// genuinely untouched) and "range ends at EOL of its own last
    /// line" (that line's content WAS the edit). Only this flag
    /// disambiguates the two. See [`Self::suffix_start_line`].
    ///
    /// Defaults to `false` (the conservative reading: treat the
    /// boundary line as edited rather than risk reusing a wrongly
    /// stale row) so construction sites that don't care about this
    /// axis — most tests, most benches — can use
    /// `..Default::default()` without silently opting into the
    /// unsafe reuse.
    pub old_end_at_bol: bool,
}

impl EditDelta {
    /// Net line shift the edit causes for downstream lines.
    /// `lines_added - lines_removed` as an `i32` — can be
    /// negative (deletion shrinks the document).
    pub fn net_delta(&self) -> i32 {
        self.lines_added as i32 - self.lines_removed as i32
    }

    /// First source line past the edit's pre-edit affected range,
    /// counting only *full* lines removed
    /// (`start_line + lines_removed`).
    ///
    /// This does NOT by itself mean "lines `>=` this value were
    /// untouched" — that claim only holds when
    /// [`Self::old_end_at_bol`] is `true` (the old range ended
    /// exactly at BOL of this line). When it's `false`, the edit's
    /// old range ended partway into (or at the very end of) THIS
    /// line, so this line's content was itself part of the edit.
    /// Callers that need the actual safe shift boundary want
    /// [`Self::suffix_start_line`], not this method directly.
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

    /// First source line that is safe to treat as an untouched
    /// SUFFIX of the edit — i.e. every line `>=` this value can be
    /// reused verbatim from the pre-edit cache and shifted
    /// wholesale by [`Self::net_delta`]. This is the value
    /// `pre_edit_end_line`'s doc comment used to (incorrectly)
    /// claim for itself; this method is the one that actually
    /// honours that contract.
    ///
    /// Three shapes, in order:
    ///
    /// - **Pure insert** (`lines_removed == 0`): `pre_edit_end_line()
    ///   == start_line`, which would misclassify the very line the
    ///   edit lands on (typed into, or split by a newline) as an
    ///   untouched suffix. The safe boundary is one line later,
    ///   `start_line + 1`.
    /// - **Boundary line partially or wholly replaced**
    ///   (`lines_removed > 0` and `old_end_at_bol == false`):
    ///   `pre_edit_end_line()` lands ON the line the edit's old
    ///   range actually ends inside — that line's content changed,
    ///   so it is NOT a safe suffix start either. The safe boundary
    ///   is one line later, `pre_edit_end_line() + 1`. Table-mode's
    ///   `rewrite()` and org's `replace_lines` both build edits
    ///   shaped exactly this way (range end = EOL of the last
    ///   affected line, never BOL of the line after) on every
    ///   invocation.
    /// - **Clean line-boundary replace** (`lines_removed > 0` and
    ///   `old_end_at_bol == true`): the old range ended exactly at
    ///   BOL of `pre_edit_end_line()`, so that line is genuinely
    ///   untouched and safe to shift wholesale — `pre_edit_end_line()`
    ///   itself is the answer.
    pub fn suffix_start_line(&self) -> u32 {
        if self.lines_removed == 0 {
            self.start_line.saturating_add(1)
        } else if self.old_end_at_bol {
            self.pre_edit_end_line()
        } else {
            self.pre_edit_end_line().saturating_add(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn net_delta_signs() {
        let insert = EditDelta {
            start_line: 5,
            lines_removed: 0,
            lines_added: 3,
            ..Default::default()
        };
        assert_eq!(insert.net_delta(), 3);
        assert_eq!(insert.pre_edit_end_line(), 5);
        assert_eq!(insert.post_edit_end_line(), 8);

        let delete = EditDelta {
            start_line: 10,
            lines_removed: 4,
            lines_added: 0,
            ..Default::default()
        };
        assert_eq!(delete.net_delta(), -4);
        assert_eq!(delete.pre_edit_end_line(), 14);
        assert_eq!(delete.post_edit_end_line(), 10);

        let replace = EditDelta {
            start_line: 2,
            lines_removed: 2,
            lines_added: 5,
            ..Default::default()
        };
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
            ..Default::default()
        };
        assert_eq!(e.pre_edit_end_line(), u32::MAX);
    }

    #[test]
    fn default_old_end_at_bol_is_conservative_false() {
        // The conservative default causes one EXTRA row to be
        // rebuilt (safe, just wasted work) rather than one row to
        // be WRONGLY reused (the table-align bug this field fixes).
        assert!(!EditDelta::default().old_end_at_bol);
    }

    #[test]
    fn suffix_start_line_pure_insert_skips_the_edited_line() {
        // `lines_removed == 0`: the start line itself was typed
        // into or split, regardless of `old_end_at_bol` — the safe
        // suffix boundary is one line later.
        let e = EditDelta {
            start_line: 3,
            lines_removed: 0,
            lines_added: 1,
            old_end_at_bol: true,
        };
        assert_eq!(e.suffix_start_line(), 4);
    }

    #[test]
    fn suffix_start_line_clean_line_boundary_reuses_pre_edit_end_line() {
        // Old range ends exactly at BOL of the following line (e.g.
        // `dd`-style whole-line deletion including trailing
        // newlines) — that line is genuinely untouched.
        let e = EditDelta {
            start_line: 3,
            lines_removed: 2,
            lines_added: 0,
            old_end_at_bol: true,
        };
        assert_eq!(e.suffix_start_line(), e.pre_edit_end_line());
        assert_eq!(e.suffix_start_line(), 5);
    }

    #[test]
    fn suffix_start_line_mid_line_boundary_extends_by_one() {
        // Old range ends partway into (or at the EOL of) its last
        // line — table-mode's `rewrite()` / org's `replace_lines`
        // shape. `pre_edit_end_line()` lands ON the changed line, so
        // the safe suffix boundary is one line past it.
        let e = EditDelta {
            start_line: 3,
            lines_removed: 2,
            lines_added: 2,
            old_end_at_bol: false,
        };
        assert_eq!(e.suffix_start_line(), e.pre_edit_end_line() + 1);
        assert_eq!(e.suffix_start_line(), 6);
    }
}
