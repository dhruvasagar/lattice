//! TerminalSnapshot — the immutable per-frame view the
//! renderer paints. Published via `ArcSwap` from the reader
//! task; loaded wait-free by the paint hot path.

use std::sync::Arc;

use crate::cell::{Cell, CursorShape};

/// One published frame. Renderer reads via
/// `Arc<ArcSwap<TerminalSnapshot>>` — `.load()` is wait-free.
///
/// The `cells` slice is row-major: cell at (row, col) lives at
/// `cells[row * cols + col]`. Always exactly `rows * cols`
/// long.
///
/// `seq` is a monotonic frame counter — renderers can skip a
/// repaint when the loaded snapshot's `seq` matches the
/// last-painted one.
#[derive(Debug, Clone)]
pub struct TerminalSnapshot {
    pub cells: Arc<[Cell]>,
    pub rows: u16,
    pub cols: u16,
    pub cursor_row: u16,
    pub cursor_col: u16,
    pub cursor_visible: bool,
    pub cursor_shape: CursorShape,
    /// `None` = no OSC 0/2 title received yet; renderer falls
    /// back to the buffer's label.
    pub title: Option<String>,
    /// True while the program is on the xterm "alternate
    /// screen" buffer (vim, less, htop, etc.). Renderers can
    /// use this to disable the modeline-scrollback indicator.
    pub alt_screen: bool,
    /// Monotonic per-terminal frame counter.
    pub seq: u64,
}

impl TerminalSnapshot {
    /// Empty snapshot for a freshly-spawned terminal that
    /// hasn't received any output yet. `rows × cols` defaults
    /// to 24 × 80 — overridden as soon as the renderer's
    /// first layout pass resizes.
    pub fn empty() -> Self {
        let rows = 24u16;
        let cols = 80u16;
        let cells = vec![Cell::default(); rows as usize * cols as usize];
        Self {
            cells: cells.into(),
            rows,
            cols,
            cursor_row: 0,
            cursor_col: 0,
            cursor_visible: true,
            cursor_shape: CursorShape::Block,
            title: None,
            alt_screen: false,
            seq: 0,
        }
    }

    /// Cell at (row, col). Returns the default cell (space on
    /// default bg/fg) for out-of-range indices so renderers
    /// can iterate naively without bounds checks.
    pub fn cell_at(&self, row: u16, col: u16) -> Cell {
        if row >= self.rows || col >= self.cols {
            return Cell::default();
        }
        let idx = row as usize * self.cols as usize + col as usize;
        self.cells.get(idx).copied().unwrap_or_default()
    }
}

impl Default for TerminalSnapshot {
    fn default() -> Self {
        Self::empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_snapshot_is_24_x_80_of_default_cells() {
        let s = TerminalSnapshot::empty();
        assert_eq!(s.rows, 24);
        assert_eq!(s.cols, 80);
        assert_eq!(s.cells.len(), 24 * 80);
        assert!(s.cells.iter().all(|c| *c == Cell::default()));
        assert_eq!(s.seq, 0);
    }

    #[test]
    fn cell_at_out_of_range_returns_default() {
        let s = TerminalSnapshot::empty();
        assert_eq!(s.cell_at(99, 99), Cell::default());
        assert_eq!(s.cell_at(s.rows, 0), Cell::default());
    }
}
