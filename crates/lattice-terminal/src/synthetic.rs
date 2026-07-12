//! T-snap-1 (2026-05-27): synthetic, read-only Document built
//! from the alacritty scrollback + visible grid. The central
//! vim grammar operates on this Document during the Normal /
//! Visual sub-state of a terminal buffer; the renderer
//! continues to paint the cell grid. A coord adapter (T-paint-1)
//! translates document-space selection ranges back to cell
//! coordinates at publish time so renderer paths stay
//! buffer-kind-agnostic.
//!
//! Design fragment: `docs/dev/architecture/terminal-as-
//! document.md`. This module owns just the construction +
//! coord-translation primitives; the lifecycle (when the
//! snapshot is built / dropped) lives on `TerminalNormalMode`
//! (T-mode-1).
//!
//! The snapshot is **frozen** for the duration of the Normal
//! sub-state. PTY output continues to feed alacritty in the
//! background; the SyntheticDoc does not auto-refresh. Re-entry
//! to Insert drops the snapshot. This matches vim's `:terminal`
//! semantics and gives the user a stable target for motions /
//! marks / search.

use std::sync::Arc;

use alacritty_terminal::{
    grid::Dimensions,
    index::{Column, Line, Point},
    term::TermMode,
};
use lattice_core::{Buffer, BufferId};
use lattice_protocol::position::Position;

use crate::reader::SharedTerm;

/// Frozen read-only view of the terminal's scrollback + visible
/// grid at the moment of an Insert → Normal transition. See
/// `docs/dev/architecture/terminal-as-document.md` §3.2 for the
/// build rules and §3.3 for the coord-translation invariants.
#[derive(Debug, Clone)]
pub struct SyntheticDoc {
    /// Rope-backed buffer of the snapshot text. One rope line
    /// per grid line. Trailing-blank padding is stripped from
    /// each row so `$` lands on the last visible character
    /// (matches what the user sees).
    pub buffer: Buffer,
    /// Doc-space cursor at the moment of snapshot — the
    /// alacritty grid cursor translated to (rope line, byte
    /// column). For ASCII content, `byte` equals the grid
    /// column; wide-char handling is the open question in
    /// §7 of the design fragment.
    pub cursor: Position,
    /// Topmost alacritty grid line at build time. Doc line `N`
    /// corresponds to grid line `origin_top_line + N`. The
    /// publish-time coord adapter (T-paint-1) uses this to remap
    /// document-space ranges back to cell coordinates.
    pub origin_top_line: i32,
    /// Snapshot sequence captured at build time. Used by the
    /// jumplist re-resolution path to detect when the
    /// underlying scrollback has rolled and a recorded position
    /// needs best-effort re-resolution.
    pub frozen_at: u64,
    /// True if the program was on the alt screen at build time.
    /// Alt-screen snapshots cover only the visible region (alt
    /// screen has no scrollback semantics).
    pub alt_screen: bool,
}

impl SharedTerm {
    /// Build a [`SyntheticDoc`] from the current grid state.
    ///
    /// - **Primary screen**: includes scrollback (`top..=bot` =
    ///   `topmost_line..=bottommost_line`).
    /// - **Alt screen**: visible region only (alt screen has no
    ///   scrollback semantics; programs like vim / less / htop
    ///   own the canvas).
    ///
    /// Trailing-blank padding is stripped per row before the
    /// rope is built. The alacritty grid cursor is translated to
    /// document-space `(line, byte)` coordinates and clamped to
    /// the row's visible length (vim's "cursor can't sit in
    /// virtual whitespace by default" rule).
    ///
    /// Cost is O(rows × cols). Bounded by
    /// `terminal.scrollback-lines × cols`; default 10 000 × 200.
    /// See the `term_snapshot_build` bench for the perf gate.
    pub fn build_normal_snapshot(&self) -> SyntheticDoc {
        let term = self.inner.lock();
        let grid = term.grid();
        let alt_screen = term.mode().contains(TermMode::ALT_SCREEN);
        let (top, bot) = if alt_screen {
            // Visible region only — alt screen has no scrollback.
            (0i32, grid.screen_lines() as i32 - 1)
        } else {
            (grid.topmost_line().0, grid.bottommost_line().0)
        };
        let cols = grid.columns();
        // Pre-size for the worst case (no trim) so the rope
        // build is a single allocation.
        let mut text = String::with_capacity(((bot - top + 1).max(0) as usize) * (cols + 1));
        let cursor_grid_line = grid.cursor.point.line.0;
        let cursor_grid_col = grid.cursor.point.column.0 as u32;
        let mut cursor_line_doc: u32 = 0;
        let mut cursor_col_doc: u32 = 0;
        let mut cursor_found = false;
        for line_idx in top..=bot {
            let mut row_text = String::with_capacity(cols);
            for c in 0..cols {
                let p = Point::new(Line(line_idx), Column(c));
                row_text.push(grid[p].c);
            }
            let trimmed = row_text.trim_end_matches(' ');
            if line_idx == cursor_grid_line {
                cursor_line_doc = (line_idx - top) as u32;
                // Clamp to trimmed.len() so the cursor doesn't
                // sit in stripped padding. For ASCII the byte
                // count matches the grid column; wide-char
                // handling is the §7 open question.
                cursor_col_doc = std::cmp::min(cursor_grid_col, trimmed.len() as u32);
                cursor_found = true;
            }
            text.push_str(trimmed);
            text.push('\n');
        }
        // Drop the trailing newline so the rope has exactly N
        // lines for an N-row grid (vim convention: last line is
        // content-bearing, not a phantom empty line). Without
        // this, ropey would see N+1 lines and the publish-time
        // coord adapter's `origin_top_line + doc_line = grid_line`
        // round-trip would slip by one at the bottom edge.
        if text.ends_with('\n') {
            text.pop();
        }
        // Defensive: if the cursor sits outside `(top..=bot)` for
        // any reason (alt-screen edge cases, future grid
        // weirdness), land it at (0, 0) rather than panicking.
        if !cursor_found {
            cursor_line_doc = 0;
            cursor_col_doc = 0;
        }
        let frozen_at = self.seq.load(std::sync::atomic::Ordering::Relaxed);
        drop(term);
        SyntheticDoc {
            buffer: Buffer::from_text(&text),
            cursor: Position::new(cursor_line_doc, cursor_col_doc),
            origin_top_line: top,
            frozen_at,
            alt_screen,
        }
    }
}

/// T-mode-1 (2026-05-27): service trait that `TerminalNormalMode`
/// uses to build / drop the SyntheticDoc on a `TerminalBuffer`
/// from its lifecycle hooks. The implementation lives on the
/// host's `BufferRegistry`; the mode pulls a handle to it via
/// `ModeContext::service::<TerminalStoreHandle>()`.
///
/// Two methods only: install (build + stash) and clear (drop).
/// Keeping it minimal avoids leaking the `with_terminal_mut`
/// closure surface across the crate boundary; the host wraps
/// `with_terminal_mut` internally to satisfy these calls.
pub trait TerminalStore: Send + Sync {
    /// Build a SyntheticDoc from the terminal buffer's current
    /// grid state and stash it on the buffer. Returns `true` if
    /// a terminal buffer existed for `id` (and the doc was
    /// installed); `false` otherwise. Idempotent — re-calling
    /// rebuilds the doc.
    fn install_synthetic(&self, id: BufferId) -> bool;

    /// Drop any existing SyntheticDoc on the buffer. Idempotent.
    /// Returns `true` if a terminal buffer existed for `id`;
    /// `false` otherwise.
    fn clear_synthetic(&self, id: BufferId) -> bool;
}

/// Cheap-clone handle to a [`TerminalStore`]. Mirrors the
/// `BufferStoreHandle` pattern — the host registers a wrapping
/// handle in the `ServiceRegistry` at boot; modes pull it via
/// `ModeContext::service::<TerminalStoreHandle>()`.
#[derive(Clone)]
pub struct TerminalStoreHandle {
    inner: Arc<dyn TerminalStore>,
}

impl TerminalStoreHandle {
    pub fn new(store: Arc<dyn TerminalStore>) -> Self {
        Self { inner: store }
    }

    pub fn install_synthetic(&self, id: BufferId) -> bool {
        self.inner.install_synthetic(id)
    }

    pub fn clear_synthetic(&self, id: BufferId) -> bool {
        self.inner.clear_synthetic(id)
    }
}

impl std::fmt::Debug for TerminalStoreHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalStoreHandle")
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    fn make_shared(rows: u16, cols: u16, scrollback: u32) -> SharedTerm {
        SharedTerm::fixture(rows, cols, scrollback)
    }

    fn feed(shared: &SharedTerm, bytes: &[u8]) {
        shared.feed_for_fixture(bytes);
    }

    #[test]
    fn empty_grid_yields_blank_rope() {
        let shared = make_shared(3, 10, 16);
        let snap = shared.build_normal_snapshot();
        // 3 visible rows, all blank. Build strips the trailing
        // `\n` so the rope holds exactly N lines (vim
        // convention). Three rows → "\n\n" (2 separating
        // newlines + 3 empty content lines).
        assert_eq!(snap.buffer.as_string(), "\n\n");
        assert_eq!(snap.buffer.line_count(), 3);
        assert_eq!(snap.cursor, Position::new(0, 0));
        assert!(!snap.alt_screen);
        assert_eq!(snap.origin_top_line, 0);
    }

    #[test]
    fn ascii_strips_trailing_blanks() {
        let shared = make_shared(3, 10, 16);
        feed(&shared, b"hi\r\nyo\r\nok");
        let snap = shared.build_normal_snapshot();
        // Per-row trailing-blank strip (grid stores 10 cells
        // per row, padded with spaces). Trailing `\n` is
        // dropped at the rope level (vim last-line convention).
        assert_eq!(snap.buffer.as_string(), "hi\nyo\nok");
        assert_eq!(snap.buffer.line_count(), 3);
    }

    #[test]
    fn cursor_translates_to_doc_coords() {
        let shared = make_shared(3, 10, 16);
        feed(&shared, b"hi\r\nworld");
        let snap = shared.build_normal_snapshot();
        // After "hi\r\nworld" on a 3×10 grid the cursor sits at
        // grid row 1, col 5 (just past "world"). Doc cursor
        // should reflect that.
        assert_eq!(snap.cursor.line, 1);
        assert_eq!(snap.cursor.byte, 5);
    }

    #[test]
    fn cursor_clamps_to_trimmed_length() {
        // Cursor parked on a blank row should clamp to col 0
        // (the row trims to empty so byte 0 is the only valid
        // landing). Achieved via ESC [ 3 ; 6 H (CUP to row 3,
        // col 6) on an otherwise-empty row.
        let shared = make_shared(3, 10, 16);
        feed(&shared, b"top\r\n\x1b[3;6H");
        let snap = shared.build_normal_snapshot();
        assert_eq!(snap.cursor.line, 2);
        assert_eq!(
            snap.cursor.byte, 0,
            "cursor on a blank row should clamp to col 0",
        );
    }

    #[test]
    fn scrollback_is_included_on_primary_screen() {
        let shared = make_shared(3, 10, 16);
        for i in 0..8u8 {
            feed(&shared, format!("r{i}\r\n").as_bytes());
        }
        let snap = shared.build_normal_snapshot();
        assert!(
            snap.origin_top_line < 0,
            "primary scrollback should yield negative origin_top_line, got {}",
            snap.origin_top_line,
        );
        let text = snap.buffer.as_string();
        assert!(
            text.contains("r0"),
            "earliest scrollback line missing: {text:?}",
        );
    }

    #[test]
    fn alt_screen_excludes_scrollback() {
        let shared = make_shared(3, 10, 16);
        for i in 0..8u8 {
            feed(&shared, format!("r{i}\r\n").as_bytes());
        }
        // DECSET 1049 = enable alt-screen + save cursor.
        feed(&shared, b"\x1b[?1049h");
        feed(&shared, b"ALT");
        let snap = shared.build_normal_snapshot();
        assert!(snap.alt_screen);
        assert_eq!(
            snap.origin_top_line, 0,
            "alt-screen snapshot should start at grid line 0",
        );
        let text = snap.buffer.as_string();
        assert!(
            !text.contains("r0"),
            "alt-screen snapshot should not include primary scrollback: {text:?}",
        );
        assert!(text.contains("ALT"), "missing alt content: {text:?}");
    }

    #[test]
    fn frozen_at_captures_current_seq() {
        let shared = make_shared(3, 10, 16);
        shared.seq.store(42, Ordering::Relaxed);
        let snap = shared.build_normal_snapshot();
        assert_eq!(snap.frozen_at, 42);
    }

    #[test]
    fn doc_line_to_grid_line_round_trips() {
        // T-paint-1's coord adapter inverts the build:
        // grid_line = origin_top_line + doc_line. Sanity-check
        // the relationship holds at construction time.
        let shared = make_shared(3, 10, 16);
        for i in 0..8u8 {
            feed(&shared, format!("r{i}\r\n").as_bytes());
        }
        let snap = shared.build_normal_snapshot();
        let line_count = snap.buffer.line_count();
        let last_doc_line = line_count - 1;
        let grid_line = snap.origin_top_line + last_doc_line as i32;
        let bot = {
            let t = shared.inner.lock();
            t.grid().bottommost_line().0
        };
        assert_eq!(grid_line, bot);
    }
}
