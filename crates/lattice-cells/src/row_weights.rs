//! IM.1 — how much vertical space a source line's display rows occupy,
//! measured in **line-heights**.
//!
//! ## Why this exists
//!
//! Scroll arithmetic counts display rows and assumes every row is one unit
//! tall. That holds for the TUI forever — a terminal cell has one size — and
//! it held for the GPUI peer until rows started differing in height (scaled
//! markdown headings today, inline media blocks at IM.3).
//!
//! The IM.0 audit found the damage is narrow but real: `Editor::scroll` is a
//! row *index* and survives untouched, but `Editor::viewport_height` is a row
//! *count*, computed as `available_px / row_px` against a uniform `row_px`.
//! Once rows differ, "how many rows fit" is not a constant — it depends on
//! which rows — so the seven functions that spend that number as a budget are
//! wrong. See `docs/dev/architecture/inline-media.md` §4.0.
//!
//! ## Why line-heights and not pixels
//!
//! Pixels are a concept the TUI does not have, and this rides in shared host
//! state that both peers read. A line-height is a unit both peers have: the
//! TUI's rows are all exactly 1.0, and GPUI's are `row_scale`. Keeping the
//! core in line-heights is what stops a renderer concern leaking into
//! renderer-neutral types.
//!
//! ## Why keyed by source line
//!
//! The scroll walks (`bottom_anchored_scroll` and friends) step by source
//! line and ask "how many display rows does this line cost". Keying the
//! override the same way lets the weight replace that answer directly,
//! rather than forcing the walk to first resolve each line to a range of
//! display-row indices.
//!
//! ## The empty case is the important one
//!
//! Almost every line is ordinary, so the map is sparse and
//! [`RowWeights::is_uniform`] is true in the overwhelming majority of
//! buffers — including every TUI buffer, always. When it is, [`cost`] returns
//! exactly `default_rows as f32`, so the converted arithmetic reduces to the
//! integer arithmetic it replaced. That property is the regression guard for
//! the whole slice: the TUI must behave identically before and after.
//!
//! [`cost`]: RowWeights::cost

use std::collections::HashMap;

/// Per-source-line vertical cost overrides, in line-heights.
///
/// Absent line ⇒ the line costs its display-row count, unchanged.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RowWeights {
    /// Source line → total height of that line's display rows, in
    /// line-heights. Only lines that differ from their row count appear.
    overrides: HashMap<u32, f32>,
}

impl RowWeights {
    /// The uniform map: every line costs its display-row count. What the TUI
    /// publishes, always, and what GPUI publishes for a buffer with no scaled
    /// or media rows.
    pub fn uniform() -> Self {
        Self::default()
    }

    /// True when nothing overrides the default. Callers use it to keep the
    /// common path free of float work entirely.
    pub fn is_uniform(&self) -> bool {
        self.overrides.is_empty()
    }

    /// Declare that `line`'s display rows together occupy `line_heights`.
    ///
    /// Non-finite and negative values are ignored rather than stored: a bad
    /// weight would poison a scroll budget and wedge the viewport, and a
    /// silently-uniform line is a far better failure than a stuck buffer.
    pub fn set(&mut self, line: u32, line_heights: f32) {
        if line_heights.is_finite() && line_heights >= 0.0 {
            self.overrides.insert(line, line_heights);
        }
    }

    /// The vertical cost of `line`, given the display-row count the caller
    /// already computed (wrap segments + virtual rows).
    ///
    /// Returns exactly `default_rows as f32` when unoverridden, which is what
    /// makes the uniform case bit-identical to the integer arithmetic this
    /// replaced.
    pub fn cost(&self, line: u32, default_rows: u32) -> f32 {
        match self.overrides.get(&line) {
            Some(w) => *w,
            None => default_rows as f32,
        }
    }

    /// How many lines carry an override. Diagnostics and tests.
    pub fn len(&self) -> usize {
        self.overrides.len()
    }

    /// True when no line carries an override — the alias of
    /// [`is_uniform`](Self::is_uniform) that clippy expects beside `len`.
    pub fn is_empty(&self) -> bool {
        self.overrides.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_uniform_map_returns_the_row_count_untouched() {
        let w = RowWeights::uniform();
        assert!(w.is_uniform());
        // The property the whole slice rests on: with no overrides the
        // converted arithmetic is the integer arithmetic it replaced. Small
        // integers are exact in f32, so this is equality, not approximation.
        for rows in [0u32, 1, 2, 7, 40] {
            assert_eq!(w.cost(0, rows), rows as f32);
        }
    }

    #[test]
    fn an_override_replaces_the_row_count_for_that_line_only() {
        let mut w = RowWeights::uniform();
        w.set(4, 8.25);
        assert!(!w.is_uniform());
        assert_eq!(w.cost(4, 1), 8.25, "the overridden line");
        assert_eq!(w.cost(3, 1), 1.0, "its neighbour is untouched");
        assert_eq!(w.cost(5, 2), 2.0);
    }

    /// A NaN or negative weight would poison a budget subtraction and could
    /// wedge the viewport. Dropping it degrades that line to uniform, which
    /// is a visible-but-harmless wrong height rather than a stuck buffer.
    #[test]
    fn a_nonsense_weight_is_refused_rather_than_stored() {
        let mut w = RowWeights::uniform();
        w.set(1, f32::NAN);
        w.set(2, f32::INFINITY);
        w.set(3, -4.0);
        assert!(w.is_uniform(), "none of those were stored");
        assert_eq!(w.cost(1, 1), 1.0);

        // Zero IS legal — a collapsed / concealed row really can occupy no
        // vertical space, and refusing it would be the wrong call.
        w.set(5, 0.0);
        assert_eq!(w.cost(5, 3), 0.0);
    }
}
