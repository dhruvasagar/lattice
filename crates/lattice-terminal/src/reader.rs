//! Reader task — drains the master PTY's read side into the
//! published [`TerminalSnapshot`] cell.
//!
//! T1 (2026-05-22): naive byte→cell mapping. Strips
//! ANSI/CSI escape sequences (so they don't appear as
//! literal `^[[31m`); does NOT honor color/attribute SGR
//! parameters yet. T2 swaps the body for
//! `alacritty_terminal::Term` + `vte::Parser` so SGR colors,
//! cursor moves, alt-screen, OSC titles, etc. all flow.
//!
//! The renderer-facing contract (writes to
//! `Arc<ArcSwap<TerminalSnapshot>>`) is locked at T1 so the
//! T2 upgrade is a swap in this file only.

use std::io::Read;
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;

use crate::cell::Cell;
use crate::snapshot::TerminalSnapshot;

/// Throttle: rebuild + publish a snapshot at most every 16 ms
/// (≈60 Hz). Programs that spew thousands of lines/sec
/// (cargo build, npm install, `cat huge.log`) would otherwise
/// dominate the render thread.
const REFRESH_WINDOW: Duration = Duration::from_millis(16);

/// Spawn the reader task. Returns its `JoinHandle` so the
/// caller can abort it.
pub fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    snapshot: Arc<ArcSwap<TerminalSnapshot>>,
    rows: u16,
    cols: u16,
    paint_request: Option<Arc<tokio::sync::Notify>>,
) -> tokio::task::JoinHandle<()> {
    tracing::info!(
        target: "lattice_terminal::reader",
        rows, cols,
        "spawn_reader: spawning blocking reader task",
    );
    tokio::task::spawn_blocking(move || {
        tracing::info!(
            target: "lattice_terminal::reader",
            "reader task entered; waiting for first read",
        );
        let mut grid = TerminalGrid::new(rows, cols);
        let mut buf = [0u8; 32 * 1024];
        let mut last_publish = Instant::now() - REFRESH_WINDOW;
        let mut seq: u64 = 0;
        let mut total_bytes: u64 = 0;
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => {
                    tracing::info!(
                        target: "lattice_terminal::reader",
                        total_bytes, seq,
                        "reader: EOF (child exited)",
                    );
                    break;
                }
                Ok(n) => n,
                Err(e) => {
                    tracing::warn!(
                        target: "lattice_terminal::reader",
                        error = %e, total_bytes,
                        "pty read error",
                    );
                    break;
                }
            };
            total_bytes += n as u64;
            if total_bytes <= 256 {
                tracing::info!(
                    target: "lattice_terminal::reader",
                    n, total_bytes,
                    "reader: read bytes",
                );
            }
            grid.advance(&buf[..n]);
            let now = Instant::now();
            if now.duration_since(last_publish) >= REFRESH_WINDOW {
                last_publish = now;
                seq += 1;
                snapshot.store(Arc::new(grid.to_snapshot(seq)));
                if let Some(n) = paint_request.as_ref() {
                    // Wake event-driven renderers (GPUI) so
                    // they repaint on new terminal output;
                    // per-tick renderers (TUI) observe the
                    // store on their next tick and don't
                    // depend on this notify.
                    n.notify_one();
                }
            }
        }
        // Final publish so the renderer sees the very last
        // bytes even if they arrived mid-throttle-window.
        seq += 1;
        snapshot.store(Arc::new(grid.to_snapshot(seq)));
        if let Some(n) = paint_request.as_ref() {
            n.notify_one();
        }
    })
}

/// Minimal grid for T1: tracks a writable cursor + a fixed-
/// size cell ring. Handles `\n` (newline-scroll), `\r`
/// (carriage return), `\t` (tab-to-next-8-col), `\x08`
/// (backspace), and STRIPS CSI/OSC escape sequences (so the
/// rendered output doesn't show `^[[0m` literals).
///
/// Replaced in T2 by an `alacritty_terminal::Term` wrapper
/// that handles full VT/xterm + alt-screen + colors.
struct TerminalGrid {
    rows: u16,
    cols: u16,
    cells: Vec<Cell>,
    cur_row: u16,
    cur_col: u16,
    /// 0 = no escape; 1 = saw ESC; 2 = inside CSI (after `[`);
    /// 3 = inside OSC (after `]`).
    esc_state: u8,
}

impl TerminalGrid {
    fn new(rows: u16, cols: u16) -> Self {
        let total = rows as usize * cols as usize;
        Self {
            rows,
            cols,
            cells: vec![Cell::default(); total],
            cur_row: 0,
            cur_col: 0,
            esc_state: 0,
        }
    }

    fn idx(&self, row: u16, col: u16) -> usize {
        row as usize * self.cols as usize + col as usize
    }

    fn advance(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.feed_byte(b);
        }
    }

    fn feed_byte(&mut self, b: u8) {
        match self.esc_state {
            1 => {
                // After ESC: distinguish CSI / OSC / single-char
                // sequences.
                match b {
                    b'[' => self.esc_state = 2,
                    b']' => self.esc_state = 3,
                    // Other single-char escapes (`\x1bD`,
                    // `\x1bM`, etc.) — swallow the second
                    // byte and exit escape state.
                    _ => self.esc_state = 0,
                }
                return;
            }
            2 => {
                // Inside CSI: bytes 0x20..=0x3F are
                // parameter/intermediate; 0x40..=0x7E is the
                // terminator. Anything else (control char)
                // breaks us out.
                if (0x40..=0x7E).contains(&b) {
                    self.esc_state = 0;
                } else if !(0x20..=0x3F).contains(&b) {
                    // Malformed; bail to printable state.
                    self.esc_state = 0;
                }
                return;
            }
            3 => {
                // Inside OSC: terminated by BEL (0x07) or
                // ST (ESC \).
                if b == 0x07 {
                    self.esc_state = 0;
                } else if b == 0x1B {
                    // OSC followed by another ESC — treat as
                    // ST-coming; the next byte will be
                    // consumed as an unknown escape and exit.
                    self.esc_state = 1;
                }
                return;
            }
            _ => {}
        }
        match b {
            0x1B => self.esc_state = 1, // ESC
            b'\n' => self.newline(),
            b'\r' => self.cur_col = 0,
            b'\t' => {
                // Tab → next column that's a multiple of 8,
                // clamped to last column.
                let next = (self.cur_col + 8) & !7;
                self.cur_col = next.min(self.cols.saturating_sub(1));
            }
            0x08 => {
                // Backspace
                if self.cur_col > 0 {
                    self.cur_col -= 1;
                }
            }
            0x07 => { /* BEL — ignore for T1 */ }
            b if b.is_ascii() && !b.is_ascii_control() => {
                self.put_char(b as char);
            }
            _ => {
                // Non-ASCII byte. T1 just renders as
                // replacement char; T2 with the real parser
                // handles UTF-8 properly.
                self.put_char('?');
            }
        }
    }

    fn put_char(&mut self, ch: char) {
        if self.cur_col >= self.cols {
            // Auto-wrap.
            self.newline();
        }
        let idx = self.idx(self.cur_row, self.cur_col);
        if idx < self.cells.len() {
            self.cells[idx] = Cell {
                ch,
                ..Cell::default()
            };
        }
        self.cur_col += 1;
    }

    fn newline(&mut self) {
        self.cur_row += 1;
        self.cur_col = 0;
        if self.cur_row >= self.rows {
            // Scroll up by one row: shift cells up, clear last row.
            let cols = self.cols as usize;
            self.cells.copy_within(cols.., 0);
            let last_start = (self.rows as usize - 1) * cols;
            for cell in &mut self.cells[last_start..] {
                *cell = Cell::default();
            }
            self.cur_row = self.rows - 1;
        }
    }

    fn to_snapshot(&self, seq: u64) -> TerminalSnapshot {
        TerminalSnapshot {
            cells: Arc::from(self.cells.clone()),
            rows: self.rows,
            cols: self.cols,
            cursor_row: self.cur_row,
            cursor_col: self.cur_col,
            cursor_visible: true,
            cursor_shape: crate::cell::CursorShape::Block,
            title: None,
            alt_screen: false,
            seq,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(rows: u16, cols: u16) -> TerminalGrid {
        TerminalGrid::new(rows, cols)
    }

    #[test]
    fn plain_ascii_lands_in_cells() {
        let mut g = grid(3, 10);
        g.advance(b"hi");
        assert_eq!(g.cells[0].ch, 'h');
        assert_eq!(g.cells[1].ch, 'i');
        assert_eq!(g.cur_col, 2);
        assert_eq!(g.cur_row, 0);
    }

    #[test]
    fn newline_advances_row() {
        let mut g = grid(3, 10);
        g.advance(b"a\nb");
        assert_eq!(g.cells[0].ch, 'a');
        assert_eq!(g.cells[10].ch, 'b'); // row 1, col 0
        assert_eq!(g.cur_row, 1);
        assert_eq!(g.cur_col, 1);
    }

    #[test]
    fn csi_sequence_is_stripped() {
        let mut g = grid(3, 20);
        g.advance(b"\x1b[31mhello\x1b[0m");
        assert_eq!(g.cells[0].ch, 'h');
        assert_eq!(g.cells[1].ch, 'e');
        assert_eq!(g.cells[2].ch, 'l');
        assert_eq!(g.cells[3].ch, 'l');
        assert_eq!(g.cells[4].ch, 'o');
        assert_eq!(g.cur_col, 5);
    }

    #[test]
    fn osc_title_sequence_is_stripped() {
        let mut g = grid(3, 30);
        g.advance(b"\x1b]0;title\x07after");
        assert_eq!(g.cells[0].ch, 'a');
        assert_eq!(g.cells[1].ch, 'f');
        assert_eq!(g.cur_col, 5);
    }

    #[test]
    fn scroll_on_overflow_keeps_last_row_visible() {
        let mut g = grid(2, 5);
        g.advance(b"a\nb\nc");
        // After 3 lines on a 2-row grid, the first should
        // have scrolled off.
        assert_eq!(g.cells[g.idx(0, 0)].ch, 'b');
        assert_eq!(g.cells[g.idx(1, 0)].ch, 'c');
    }
}
