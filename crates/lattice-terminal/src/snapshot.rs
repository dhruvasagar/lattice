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
    /// T3 (2026-05-25): rows scrolled back from the live edge.
    /// `0` = live; positive values expose scrollback. Renderers
    /// surface this on the modeline so the user can tell at a
    /// glance they're viewing history.
    pub scroll_offset: u32,
    /// T3 (2026-05-25): current number of rows held in
    /// scrollback (history that has scrolled off the live
    /// screen). Grows as rows roll off; bounded by the
    /// configured `terminal.scrollback-lines`. Used by the
    /// modeline / status line to render a "row N of M"
    /// indicator and to gate scroll-up motions.
    pub scrollback_rows: u32,
    /// Monotonic per-terminal frame counter.
    pub seq: u64,
}

impl TerminalSnapshot {
    /// Empty 24×80 snapshot — the vim/xterm-conventional fallback size for
    /// contexts with no real spawn geometry yet (tests, `Default`). NOT used
    /// by `spawn()`'s initial publish — see [`Self::empty_sized`].
    pub fn empty() -> Self {
        Self::empty_sized(24, 80)
    }

    /// Empty snapshot at a CALLER-SUPPLIED size. `spawn()` publishes this
    /// (at the real `SpawnConfig::{rows,cols}`) as the placeholder shown
    /// before the reader thread processes the child's first output chunk.
    ///
    /// Regression (2026-07-02): `spawn()` used to publish `Self::empty()`
    /// unconditionally — a hardcoded 24×80 disconnected from the real spawn
    /// size. The alacritty `Term` (`build_term`) and the kernel PTY
    /// (`openpty`) were ALWAYS sized correctly from `SpawnConfig`; only this
    /// placeholder lied about it. When the real pane is taller than 24 rows
    /// (the common case), a paint landing in the placeholder's brief window
    /// caps `rows_to_paint` at 24, and — critically — the mismatch does NOT
    /// self-correct via a renderer resize the way the old doc comment
    /// claimed: `SetPaneViewport`'s diff-then-send gate only fires a PTY
    /// resize when the computed row count DIFFERS from the pane's already-
    /// published `viewport_height`, which `do_terminal_spawn` already read
    /// to size the spawn — so there is no delta to trigger a correcting
    /// resize. The window closes only once the reader thread republishes a
    /// snapshot from real output, whenever that first arrives.
    pub fn empty_sized(rows: u16, cols: u16) -> Self {
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
            scroll_offset: 0,
            scrollback_rows: 0,
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

    /// Regression guard for the "last line clipped" bug: `spawn()`'s
    /// initial placeholder must report the REAL spawn geometry, not a
    /// hardcoded 24×80 disconnected from it. A pane taller than 24 rows
    /// (the common case) painting during the placeholder's window would
    /// otherwise cap `rows_to_paint` at 24 and clip everything below —
    /// see `empty_sized`'s doc comment for why this doesn't self-correct
    /// on the next frame the way a stale-size bug normally would.
    #[test]
    fn empty_sized_reports_the_caller_supplied_geometry() {
        let s = TerminalSnapshot::empty_sized(45, 120);
        assert_eq!(s.rows, 45);
        assert_eq!(s.cols, 120);
        assert_eq!(s.cells.len(), 45 * 120);
        assert!(s.cells.iter().all(|c| *c == Cell::default()));
    }
}
