//! Reader task — drains the master PTY's read side into the
//! published [`TerminalSnapshot`] cell.
//!
//! T2 substrate swap (2026-05-25): replaced the homegrown
//! `TerminalGrid` placeholder with `alacritty_terminal::Term`
//! + `vte::ansi::Processor`. Lattice now inherits a full
//! VT/xterm parser (SGR colors / alt-screen / DECCKM /
//! cursor-visibility / OSC titles / scrollback) from the
//! battle-tested alacritty stack — same engine alacritty,
//! zed, and neovide use.
//!
//! The renderer-facing contract (writes to
//! `Arc<ArcSwap<TerminalSnapshot>>`) is unchanged. The cells
//! we publish now carry real `TerminalColor::Named / Indexed /
//! Rgb` values via [`map_cell`].

use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use alacritty_terminal::event::{Event as AlacrittyEvent, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags as AlacrittyFlags;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::vte::ansi::{
    Color as AnsiColor, CursorShape as AnsiCursorShape, NamedColor as AnsiNamedColor, Processor,
};
use arc_swap::ArcSwap;
use parking_lot::Mutex;

use crate::cell::{Cell, CellAttrs, CursorShape, NamedColor, TerminalColor};
use crate::snapshot::TerminalSnapshot;

/// Coalesce window: after publishing a snapshot, sleep this
/// long before reading more. Bytes arriving during the sleep
/// accumulate in the kernel pipe buffer and get drained into
/// the grid by the next `reader.read()`, so a high-throughput
/// program (cargo build, `cat huge.log`) batches into ~60
/// publishes/sec instead of one per syscall.
const REFRESH_WINDOW: Duration = Duration::from_millis(16);

/// No-op [`EventListener`] for the embedded alacritty `Term`.
/// We don't yet surface alacritty's events (title changes,
/// bells, mouse cursor) to the rest of Lattice — title support
/// is queued for the T3 polish slice. Until then this swallows
/// every event silently.
#[derive(Clone, Default)]
pub(crate) struct NoopListener;

impl EventListener for NoopListener {
    fn send_event(&self, _event: AlacrittyEvent) {
        // T3 will fan title-changed events into the buffer
        // registry so the modeline / tabline can surface the
        // shell's reported title.
    }
}

/// Minimal [`Dimensions`] adapter for `Term::new`. The T1
/// placeholder didn't track scrollback; T2 leaves the
/// scrollback ring at zero so the grid behaves identically
/// for the screen-only case. Scrollback exposure lands with
/// T3 (`docs/dev/operations/slice-plans/terminal-mode.md`).
struct PtyDimensions {
    rows: u16,
    cols: u16,
}

impl Dimensions for PtyDimensions {
    fn total_lines(&self) -> usize {
        self.rows as usize
    }

    fn screen_lines(&self) -> usize {
        self.rows as usize
    }

    fn columns(&self) -> usize {
        self.cols as usize
    }
}

/// Build a fresh `Term` sized to the requested PTY dimensions.
/// `scrollback_lines` configures alacritty's history ring; `0`
/// disables scrollback entirely. Pulled out for the spawn path
/// and the test helpers.
pub(crate) fn build_term(
    rows: u16,
    cols: u16,
    scrollback_lines: u32,
) -> Term<NoopListener> {
    let dims = PtyDimensions { rows, cols };
    let config = TermConfig {
        scrolling_history: scrollback_lines as usize,
        ..TermConfig::default()
    };
    Term::new(config, &dims, NoopListener)
}

/// Map alacritty's [`AnsiNamedColor`] (which covers extended
/// vocabulary like `Foreground` / `BrightForeground` / dim
/// variants) into Lattice's 16-entry [`NamedColor`] palette.
/// Anything outside the 16 named palette folds to
/// [`TerminalColor::Default`] so the renderer falls back to
/// the theme's fg/bg.
fn map_named_color(named: AnsiNamedColor) -> TerminalColor {
    match named {
        AnsiNamedColor::Black => TerminalColor::Named(NamedColor::Black),
        AnsiNamedColor::Red => TerminalColor::Named(NamedColor::Red),
        AnsiNamedColor::Green => TerminalColor::Named(NamedColor::Green),
        AnsiNamedColor::Yellow => TerminalColor::Named(NamedColor::Yellow),
        AnsiNamedColor::Blue => TerminalColor::Named(NamedColor::Blue),
        AnsiNamedColor::Magenta => TerminalColor::Named(NamedColor::Magenta),
        AnsiNamedColor::Cyan => TerminalColor::Named(NamedColor::Cyan),
        AnsiNamedColor::White => TerminalColor::Named(NamedColor::White),
        AnsiNamedColor::BrightBlack => TerminalColor::Named(NamedColor::BrightBlack),
        AnsiNamedColor::BrightRed => TerminalColor::Named(NamedColor::BrightRed),
        AnsiNamedColor::BrightGreen => TerminalColor::Named(NamedColor::BrightGreen),
        AnsiNamedColor::BrightYellow => TerminalColor::Named(NamedColor::BrightYellow),
        AnsiNamedColor::BrightBlue => TerminalColor::Named(NamedColor::BrightBlue),
        AnsiNamedColor::BrightMagenta => TerminalColor::Named(NamedColor::BrightMagenta),
        AnsiNamedColor::BrightCyan => TerminalColor::Named(NamedColor::BrightCyan),
        AnsiNamedColor::BrightWhite => TerminalColor::Named(NamedColor::BrightWhite),
        // `Foreground` / `Background` / `Cursor` / Dim* /
        // BrightForeground / DimForeground all map to the
        // theme's default — Lattice's terminal renderer picks
        // the theme-supplied fg/bg when the cell carries
        // `TerminalColor::Default`. Dim* additionally sets the
        // `dim` attribute via the cell flags path.
        _ => TerminalColor::Default,
    }
}

fn map_color(c: AnsiColor) -> TerminalColor {
    match c {
        AnsiColor::Named(n) => map_named_color(n),
        AnsiColor::Indexed(idx) => TerminalColor::Indexed(idx),
        AnsiColor::Spec(rgb) => TerminalColor::Rgb(rgb.r, rgb.g, rgb.b),
    }
}

fn map_flags(flags: AlacrittyFlags) -> CellAttrs {
    // Alacritty's `Flags` doesn't carry a BLINK bit — blink is
    // surfaced via cursor mode rather than per-cell attrs.
    // `Lattice::CellAttrs::blink` keeps the slot for future
    // wiring (e.g. terminal-bell or per-cell underline-blink
    // when alacritty grows the bit) but defaults to `false`.
    CellAttrs {
        bold: flags.contains(AlacrittyFlags::BOLD),
        italic: flags.contains(AlacrittyFlags::ITALIC),
        underline: flags.contains(AlacrittyFlags::UNDERLINE),
        reverse: flags.contains(AlacrittyFlags::INVERSE),
        dim: flags.contains(AlacrittyFlags::DIM),
        strikethrough: flags.contains(AlacrittyFlags::STRIKEOUT),
        blink: false,
    }
}

fn map_cell(a: &alacritty_terminal::term::cell::Cell) -> Cell {
    Cell {
        ch: a.c,
        fg: map_color(a.fg),
        bg: map_color(a.bg),
        attrs: map_flags(a.flags),
    }
}

fn map_cursor_shape(s: AnsiCursorShape) -> CursorShape {
    match s {
        AnsiCursorShape::Block | AnsiCursorShape::HollowBlock => CursorShape::Block,
        AnsiCursorShape::Underline => CursorShape::Underline,
        AnsiCursorShape::Beam => CursorShape::Bar,
        AnsiCursorShape::Hidden => CursorShape::Hidden,
    }
}

/// Render a published snapshot from the live `Term` state.
/// T3 (2026-05-25): the visible window now honours
/// `Grid::display_offset` so scrolled-back rows surface in the
/// snapshot the renderer paints. When `display_offset == 0`
/// the snapshot shows the live screen (`Line(0..rows)`);
/// otherwise it shifts up into history.
pub(crate) fn term_to_snapshot<T: EventListener>(term: &Term<T>, seq: u64) -> TerminalSnapshot {
    let grid = term.grid();
    let rows = grid.screen_lines();
    let cols = grid.columns();
    let display_offset = grid.display_offset();
    let scrollback_max = grid.history_size();
    let mut cells = Vec::with_capacity(rows * cols);
    // Alacritty's screen window for the current display offset
    // is `[Line(-display_offset), Line(-display_offset + rows))`.
    // When the user scrolls up by N, the topmost visible row is
    // `Line(-N)` (history row N rows above the live screen);
    // when N == 0 the topmost visible row is `Line(0)` (the
    // current live screen).
    let top_line = -(display_offset as i32);
    for r in 0..rows {
        for c in 0..cols {
            let point = Point::new(Line(top_line + r as i32), Column(c));
            cells.push(map_cell(&grid[point]));
        }
    }
    // Cursor is always reported relative to the live screen
    // even when scrolled back, so the cursor's `cell_at` row
    // may be off-screen. Renderers should hide the cursor
    // splice when `cursor_row >= rows` after the shift.
    let cursor_point = grid.cursor.point;
    let live_cursor_row = cursor_point.line.0;
    let shifted_cursor_row = live_cursor_row + display_offset as i32;
    let cursor_visible_in_view = (0..rows as i32).contains(&shifted_cursor_row);
    let cursor_row = shifted_cursor_row.max(0) as u16;
    let cursor_col = cursor_point.column.0 as u16;
    let mode = term.mode();
    TerminalSnapshot {
        cells: cells.into(),
        rows: rows.min(u16::MAX as usize) as u16,
        cols: cols.min(u16::MAX as usize) as u16,
        cursor_row,
        cursor_col,
        // Hide the cursor when the user has scrolled past it —
        // matches every terminal emulator's UX. Renderers that
        // splice a cursor cell skip the splice when this is
        // false; the hardware-cursor path (TUI) also skips
        // `frame.set_cursor_position`.
        cursor_visible: mode.contains(TermMode::SHOW_CURSOR) && cursor_visible_in_view,
        cursor_shape: map_cursor_shape(term.cursor_style().shape),
        // Title isn't surfaced through a public accessor on
        // `Term` in alacritty_terminal 0.26 (the field is
        // private and `pop_title` mutates the title stack —
        // wrong shape for a per-frame read). T3 wires the
        // EventListener::send_event(SetTitle) into a stable
        // cell so the snapshot can carry it.
        title: None,
        alt_screen: mode.contains(TermMode::ALT_SCREEN),
        scroll_offset: display_offset.min(u32::MAX as usize) as u32,
        scrollback_rows: scrollback_max.min(u32::MAX as usize) as u32,
        seq,
    }
}

/// T3 (2026-05-25): how the caller asks the terminal to
/// reposition the viewport over its scrollback. Mapped 1:1 to
/// alacritty's `Scroll` enum at the dispatch site so the public
/// API doesn't leak alacritty types.
#[derive(Debug, Clone, Copy)]
pub enum TerminalScrollKind {
    /// Positive = up into history; negative = down toward live.
    /// Matches alacritty's `Scroll::Delta` sign convention.
    Delta(i32),
    PageUp,
    PageDown,
    Top,
    Bottom,
}

fn map_scroll(kind: TerminalScrollKind) -> Scroll {
    match kind {
        TerminalScrollKind::Delta(n) => Scroll::Delta(n),
        TerminalScrollKind::PageUp => Scroll::PageUp,
        TerminalScrollKind::PageDown => Scroll::PageDown,
        TerminalScrollKind::Top => Scroll::Top,
        TerminalScrollKind::Bottom => Scroll::Bottom,
    }
}

/// Shared handle to the alacritty `Term` owned by a spawned
/// terminal. The reader task locks the `Mutex` to advance bytes
/// from the PTY; the host's dispatch path locks it to invoke
/// scroll / resize / future-T2.c operations. Contention is
/// minimal — the reader only holds the lock during chunk
/// processing (microseconds per call) and dispatch operations
/// are user-driven (one per keystroke).
///
/// The `snapshot` Arc + `paint_request` notifier match the ones
/// the reader publishes to so dispatch-side state changes
/// (e.g. scrolling into history) republish a fresh snapshot
/// without waiting for the next PTY byte.
#[derive(Clone)]
pub struct SharedTerm {
    pub(crate) inner: Arc<Mutex<Term<NoopListener>>>,
    snapshot: Arc<ArcSwap<TerminalSnapshot>>,
    pub(crate) seq: Arc<AtomicU64>,
    paint_request: Option<Arc<tokio::sync::Notify>>,
}

impl std::fmt::Debug for SharedTerm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedTerm").finish_non_exhaustive()
    }
}

/// T3.b (2026-05-25): result of [`SharedTerm::find_match`].
/// Carries the alacritty `Line` and column of a search hit so
/// the caller can scroll the viewport to the matching row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridSearchHit {
    /// Alacritty grid line. Negative values are scrollback;
    /// `0..=screen_lines-1` are the live screen.
    pub line: i32,
    /// Column index where the match begins (in cells).
    pub column: u16,
    /// Match length in chars (cell-count approximation; CJK
    /// double-wide cells count once).
    pub len: u32,
}

/// T3.b: search direction. Mirrors
/// `lattice_grammar::SearchDirection` without depending on the
/// grammar crate from the substrate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchDir {
    Forward,
    Backward,
}

impl SharedTerm {
    /// T-snap-1 (2026-05-27): in-crate fixture constructor used
    /// by sibling modules' unit tests. **Not for production code
    /// paths** — production constructs via `spawn_reader` so the
    /// OS-thread reader runs. Kept `pub(crate)` because the
    /// signature references the crate-private `NoopListener`;
    /// external callers (e.g. the `term_snapshot` bench) use the
    /// higher-level [`Self::fixture`] helper instead.
    pub(crate) fn from_state(
        inner: Arc<Mutex<Term<NoopListener>>>,
        snapshot: Arc<ArcSwap<TerminalSnapshot>>,
        seq: Arc<AtomicU64>,
    ) -> Self {
        Self {
            inner,
            snapshot,
            seq,
            paint_request: None,
        }
    }

    /// T-snap-1 (2026-05-27): build a fixture `SharedTerm` sized
    /// to `(rows × cols)` with a `scrollback`-line history ring.
    /// Empty grid, zero seq, fresh snapshot. Used by tests and
    /// the `term_snapshot` bench; not for production paths.
    pub fn fixture(rows: u16, cols: u16, scrollback: u32) -> Self {
        let term = Arc::new(Mutex::new(build_term(rows, cols, scrollback)));
        let snapshot = Arc::new(ArcSwap::from_pointee(TerminalSnapshot::empty()));
        let seq = Arc::new(AtomicU64::new(0));
        Self::from_state(term, snapshot, seq)
    }

    /// T-snap-1 (2026-05-27): feed VT bytes into a fixture-built
    /// `SharedTerm`. Mirrors what the production reader task
    /// does on each PTY read — runs the alacritty VT processor
    /// against the byte stream so escape sequences, cursor
    /// motions, and SGR attributes all land in the grid.
    /// Test/bench-only.
    pub fn feed_for_fixture(&self, bytes: &[u8]) {
        let mut processor: alacritty_terminal::vte::ansi::Processor =
            alacritty_terminal::vte::ansi::Processor::new();
        let mut t = self.inner.lock();
        processor.advance(&mut *t, bytes);
    }

    /// T3: re-position the scrollback viewport and republish a
    /// fresh snapshot so the renderer paints history immediately
    /// (no wait for the next PTY byte to wake the reader).
    pub fn scroll(&self, kind: TerminalScrollKind) {
        let mut term = self.inner.lock();
        term.scroll_display(map_scroll(kind));
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let snap = term_to_snapshot(&*term, seq);
        self.snapshot.store(Arc::new(snap));
        if let Some(n) = &self.paint_request {
            n.notify_one();
        }
    }

    /// T3.b (2026-05-25): walk the grid (history + live screen)
    /// row by row, matching each row's cell text against
    /// `regex`. Returns the first hit in `direction`'s walk
    /// order, or `None` if nothing matched.
    ///
    /// Forward = top of scrollback → live edge (oldest first).
    /// Backward = live edge → top of scrollback (newest first).
    /// Trailing-space padding on each row is trimmed before the
    /// match so `$`-anchored regexes behave like vim's.
    pub fn find_match(
        &self,
        regex: &fancy_regex::Regex,
        direction: SearchDir,
    ) -> Option<GridSearchHit> {
        let term = self.inner.lock();
        let grid = term.grid();
        let topmost = grid.topmost_line().0;
        let bottommost = grid.bottommost_line().0;
        let cols = grid.columns();
        let mut lines: Vec<i32> = (topmost..=bottommost).collect();
        if matches!(direction, SearchDir::Backward) {
            lines.reverse();
        }
        for line_idx in lines {
            let mut row_text = String::with_capacity(cols);
            for c in 0..cols {
                let p = Point::new(Line(line_idx), Column(c));
                row_text.push(grid[p].c);
            }
            // Strip the trailing-space padding terminal rows
            // carry so users can anchor with `$` and pattern
            // counts stay sensible.
            let trimmed = row_text.trim_end_matches(' ');
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(Some(m)) = regex.find(trimmed) {
                let column = trimmed[..m.start()].chars().count() as u16;
                let len = m.as_str().chars().count() as u32;
                return Some(GridSearchHit {
                    line: line_idx,
                    column,
                    len,
                });
            }
        }
        None
    }

    /// T3.b.2 (2026-05-25): extract cell text for the inclusive
    /// alacritty grid line range `start_line..=end_line`. Used
    /// by terminal-Visual linewise yank to copy selected rows
    /// into a register. Each row is trimmed of trailing-space
    /// padding (the cell grid pads short rows to column width)
    /// and joined with `\n`; the result always ends with `\n`
    /// so it round-trips as a linewise yank (matches vim's `V`
    /// → `y` behaviour).
    ///
    /// Returns `String::new()` if the range is empty or falls
    /// entirely outside the grid bounds.
    pub fn line_range_text(&self, start_line: i32, end_line: i32) -> String {
        let term = self.inner.lock();
        let grid = term.grid();
        let topmost = grid.topmost_line().0;
        let bottommost = grid.bottommost_line().0;
        let lo = start_line.max(topmost);
        let hi = end_line.min(bottommost);
        if lo > hi {
            return String::new();
        }
        let cols = grid.columns();
        let mut out = String::with_capacity(((hi - lo + 1) as usize) * (cols + 1));
        for line_idx in lo..=hi {
            let mut row_text = String::with_capacity(cols);
            for c in 0..cols {
                let p = Point::new(Line(line_idx), Column(c));
                row_text.push(grid[p].c);
            }
            // Trim only trailing spaces — internal whitespace
            // and tabs in shell output should round-trip.
            out.push_str(row_text.trim_end_matches(' '));
            out.push('\n');
        }
        out
    }

    /// T3.b.2.b (2026-05-25): extract cell text for the
    /// inclusive blockwise rectangle [start_line..=end_line] ×
    /// [start_col..=end_col]. Each row contributes the slice
    /// for its column window. Rows are joined with `\n`; the
    /// trailing newline mirrors `line_range_text` so paste
    /// `p` adds a row below cleanly. Padding spaces are
    /// preserved inside the rectangle (blockwise selections
    /// keep alignment).
    pub fn block_range_text(
        &self,
        start_line: i32,
        end_line: i32,
        start_col: u16,
        end_col: u16,
    ) -> String {
        let term = self.inner.lock();
        let grid = term.grid();
        let topmost = grid.topmost_line().0;
        let bottommost = grid.bottommost_line().0;
        let lo = start_line.max(topmost);
        let hi = end_line.min(bottommost);
        if lo > hi {
            return String::new();
        }
        let cols = grid.columns();
        let lo_col = (start_col as usize).min(cols);
        let hi_col = (end_col as usize + 1).min(cols);
        if lo_col >= hi_col {
            return String::new();
        }
        let mut out = String::with_capacity(((hi - lo + 1) as usize) * (hi_col - lo_col + 1));
        for line_idx in lo..=hi {
            for c in lo_col..hi_col {
                let p = Point::new(Line(line_idx), Column(c));
                out.push(grid[p].c);
            }
            out.push('\n');
        }
        out
    }

    /// T3.b.2.b (2026-05-25): extract cell text for a
    /// character-wise selection from `(start_line, start_col)`
    /// to `(end_line, end_col)` inclusive, in (line, col)
    /// reading order. Multi-line selections include the tail
    /// of the start row, full rows between, and the head of
    /// the end row — same shape as vim's charwise yank. The
    /// final row preserves its trailing-space padding inside
    /// the selection so character-precision is exact.
    pub fn char_range_text(
        &self,
        start_line: i32,
        start_col: u16,
        end_line: i32,
        end_col: u16,
    ) -> String {
        let term = self.inner.lock();
        let grid = term.grid();
        let topmost = grid.topmost_line().0;
        let bottommost = grid.bottommost_line().0;
        let s_line = start_line.max(topmost);
        let e_line = end_line.min(bottommost);
        if s_line > e_line {
            return String::new();
        }
        let cols = grid.columns();
        let mut out = String::new();
        for line_idx in s_line..=e_line {
            let row_lo = if line_idx == s_line {
                start_col as usize
            } else {
                0
            };
            let row_hi = if line_idx == e_line {
                (end_col as usize + 1).min(cols)
            } else {
                cols
            };
            let mut row_text = String::with_capacity(row_hi.saturating_sub(row_lo));
            for c in row_lo..row_hi {
                let p = Point::new(Line(line_idx), Column(c));
                row_text.push(grid[p].c);
            }
            if line_idx == e_line {
                // Last row: keep trailing whitespace verbatim
                // so the selection's right edge is honoured.
                out.push_str(&row_text);
            } else {
                // Intermediate rows: trim the trailing pad,
                // append a newline (matches the visible row
                // break the user sees).
                out.push_str(row_text.trim_end_matches(' '));
                out.push('\n');
            }
        }
        out
    }

    /// T3.b.2: row of the live cursor in alacritty grid coords.
    /// Used as the initial anchor / head when the user enters
    /// Visual mode on the live edge.
    pub fn cursor_line(&self) -> i32 {
        let term = self.inner.lock();
        term.grid().cursor.point.line.0
    }

    /// T3.b.2: scrollback bounds (topmost / bottommost grid
    /// lines). Used by Visual-extend so `j` / `k` can't push
    /// the head past the available history / live edge.
    pub fn line_bounds(&self) -> (i32, i32) {
        let term = self.inner.lock();
        let grid = term.grid();
        (grid.topmost_line().0, grid.bottommost_line().0)
    }

    /// T2.c (2026-05-25): is the program in
    /// application-cursor-keys mode (DECCKM)? Programs that
    /// hand-roll fullscreen UIs (vim / less / htop / fzf) set
    /// this with `ESC [ ? 1 h` so arrow keys arrive as
    /// `ESC O <letter>` (SS3) rather than the default
    /// `ESC [ <letter>` (CSI). The translate layer reads this
    /// per keystroke when encoding arrow keys.
    pub fn cursor_keys_application_mode(&self) -> bool {
        let term = self.inner.lock();
        term.mode().contains(TermMode::APP_CURSOR)
    }

    /// T4.1 (2026-05-25): resize the alacritty grid + republish
    /// a fresh snapshot. Caller separately resizes the PTY via
    /// `PtyHandle::resize` so the child sees a SIGWINCH; this
    /// helper only updates Lattice's view of the grid. Safe to
    /// call when nothing changed (no-op on identical dims).
    pub fn resize(&self, rows: u16, cols: u16) {
        if rows == 0 || cols == 0 {
            return;
        }
        let mut term = self.inner.lock();
        let cur_rows = term.grid().screen_lines();
        let cur_cols = term.grid().columns();
        if cur_rows == rows as usize && cur_cols == cols as usize {
            return;
        }
        struct Dims(u16, u16);
        impl Dimensions for Dims {
            fn total_lines(&self) -> usize {
                self.0 as usize
            }
            fn screen_lines(&self) -> usize {
                self.0 as usize
            }
            fn columns(&self) -> usize {
                self.1 as usize
            }
        }
        term.resize(Dims(rows, cols));
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let snap = term_to_snapshot(&*term, seq);
        self.snapshot.store(Arc::new(snap));
        if let Some(n) = &self.paint_request {
            n.notify_one();
        }
    }

    /// T3.b.3 (2026-05-25): collect every match on every row.
    /// Used by `hlsearch`-style overlay so renderers can paint
    /// all occurrences in the visible window with a softer
    /// highlight than the current-match. Bounded at 1024 hits
    /// to keep the worst-case (`cat /dev/urandom | head -1k`
    /// then `/.`) from running away.
    pub fn find_all_matches(&self, regex: &fancy_regex::Regex) -> Vec<GridSearchHit> {
        const CAP: usize = 1024;
        let term = self.inner.lock();
        let grid = term.grid();
        let topmost = grid.topmost_line().0;
        let bottommost = grid.bottommost_line().0;
        let cols = grid.columns();
        let mut out: Vec<GridSearchHit> = Vec::new();
        for line_idx in topmost..=bottommost {
            let mut row_text = String::with_capacity(cols);
            for c in 0..cols {
                let p = Point::new(Line(line_idx), Column(c));
                row_text.push(grid[p].c);
            }
            let trimmed = row_text.trim_end_matches(' ');
            if trimmed.is_empty() {
                continue;
            }
            // Walk all non-overlapping matches on this row.
            let mut from = 0usize;
            while from <= trimmed.len() {
                let slice = &trimmed[from..];
                match regex.find(slice) {
                    Ok(Some(m)) => {
                        let abs_start = from + m.start();
                        // Convert byte offset → cell column.
                        let column = trimmed[..abs_start].chars().count() as u16;
                        let len = m.as_str().chars().count() as u32;
                        out.push(GridSearchHit {
                            line: line_idx,
                            column,
                            len,
                        });
                        if out.len() >= CAP {
                            return out;
                        }
                        // Advance at least one byte to avoid
                        // zero-width-match infinite loops.
                        let consumed = m.range().len().max(1);
                        from = from + m.start() + consumed;
                    }
                    _ => break,
                }
            }
        }
        out
    }

    /// T3.b: re-position the viewport so `target` is visible at
    /// the top of the screen window. Used by the search-jump
    /// path after [`Self::find_match`] returns a hit on a
    /// scrollback row. Snaps to the live edge if `target` is
    /// already on-screen.
    pub fn scroll_to_line(&self, target: i32) {
        let mut term = self.inner.lock();
        let current_offset = term.grid().display_offset() as i32;
        let desired_offset = (-target).max(0);
        let delta = desired_offset - current_offset;
        if delta != 0 {
            term.scroll_display(Scroll::Delta(delta));
        }
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let snap = term_to_snapshot(&*term, seq);
        self.snapshot.store(Arc::new(snap));
        if let Some(n) = &self.paint_request {
            n.notify_one();
        }
    }
}

/// Spawn the reader task on a detached OS thread. Returns the
/// `SharedTerm` handle the caller stores alongside the snapshot
/// so dispatch-time operations (scroll, resize) can reach into
/// the alacritty `Term`.
///
/// 2026-05-25: dropped the `tokio::task::JoinHandle<()>` return
/// + the abort_handle on TerminalBuffer. Reason: tokio's
/// `Runtime::Drop` for the editor actor's `current_thread`
/// runtime waits for in-flight blocking tasks to complete before
/// finishing — and a PTY reader blocked on `read(&mut buf)`
/// only returns once the child closes its slave fd. That waiter
/// was the real cause of the `:q` freeze: even with SIGKILL
/// firing from `TerminalBuffer::Drop`, the kernel takes some
/// time to deliver the signal + reap the child, and the runtime
/// drop wouldn't proceed until then. A plain `std::thread`
/// detaches the reader from any runtime: the editor can exit
/// at its own pace; the OS reclaims the thread on process
/// teardown.
pub fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    snapshot: Arc<ArcSwap<TerminalSnapshot>>,
    rows: u16,
    cols: u16,
    scrollback_lines: u32,
    paint_request: Option<Arc<tokio::sync::Notify>>,
) -> SharedTerm {
    tracing::info!(
        target: "lattice_terminal::reader",
        rows, cols, scrollback_lines,
        "spawn_reader: spawning detached OS thread",
    );
    let term = Arc::new(Mutex::new(build_term(rows, cols, scrollback_lines)));
    let seq = Arc::new(AtomicU64::new(0));
    let shared = SharedTerm {
        inner: Arc::clone(&term),
        snapshot: Arc::clone(&snapshot),
        seq: Arc::clone(&seq),
        paint_request: paint_request.clone(),
    };
    let _ = std::thread::Builder::new().name("lattice-pty-reader".to_string()).spawn(move || {
        tracing::info!(
            target: "lattice_terminal::reader",
            "reader task entered; waiting for first read",
        );
        let mut processor: Processor = Processor::new();
        let mut buf = [0u8; 32 * 1024];
        let mut total_bytes: u64 = 0;
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => {
                    tracing::info!(
                        target: "lattice_terminal::reader",
                        total_bytes,
                        seq = seq.load(Ordering::Relaxed),
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
            // Take the lock for the parse + publish pair so a
            // concurrent `SharedTerm::scroll` sees a consistent
            // grid. PTY chunks are small (<= 32 KiB) and
            // alacritty's parser is fast (~µs per KiB), so the
            // critical section stays sub-millisecond.
            let snap = {
                let mut term = term.lock();
                processor.advance(&mut *term, &buf[..n]);
                let s = seq.fetch_add(1, Ordering::Relaxed) + 1;
                term_to_snapshot(&*term, s)
            };
            snapshot.store(Arc::new(snap));
            if let Some(n) = paint_request.as_ref() {
                // Wake event-driven renderers (GPUI). Per-tick
                // renderers (TUI) observe the store on their
                // next tick.
                n.notify_one();
            }
            // Coalesce future bursts into ~60Hz batches.
            std::thread::sleep(REFRESH_WINDOW);
        }
        // Final publish so the renderer sees the very last
        // bytes even when the loop exited mid-window.
        let snap = {
            let term = term.lock();
            let s = seq.fetch_add(1, Ordering::Relaxed) + 1;
            term_to_snapshot(&*term, s)
        };
        snapshot.store(Arc::new(snap));
        if let Some(n) = paint_request.as_ref() {
            n.notify_one();
        }
    });
    shared
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a fresh term + processor pair and advance
    /// the given bytes through it. The test asserts on the
    /// resulting snapshot.
    fn run(bytes: &[u8], rows: u16, cols: u16) -> TerminalSnapshot {
        run_with_scrollback(bytes, rows, cols, 0)
    }

    fn run_with_scrollback(
        bytes: &[u8],
        rows: u16,
        cols: u16,
        scrollback: u32,
    ) -> TerminalSnapshot {
        let mut term = build_term(rows, cols, scrollback);
        let mut processor: Processor = Processor::new();
        processor.advance(&mut term, bytes);
        term_to_snapshot(&term, 1)
    }

    #[test]
    fn plain_ascii_lands_in_cells() {
        let s = run(b"hi", 3, 10);
        assert_eq!(s.cell_at(0, 0).ch, 'h');
        assert_eq!(s.cell_at(0, 1).ch, 'i');
        assert_eq!(s.cursor_col, 2);
        assert_eq!(s.cursor_row, 0);
    }

    #[test]
    fn newline_advances_row() {
        let s = run(b"a\r\nb", 3, 10);
        assert_eq!(s.cell_at(0, 0).ch, 'a');
        assert_eq!(s.cell_at(1, 0).ch, 'b');
        assert_eq!(s.cursor_row, 1);
        assert_eq!(s.cursor_col, 1);
    }

    /// `<C-u>` repro carried forward from the T1 reader: the
    /// shell sends `\r\x1b[K` to clear the line and rewrite the
    /// prompt. After the swap the alacritty parser handles
    /// `\x1b[K` natively, so the leftover characters from
    /// `cargo test` are gone before the prompt redraws.
    #[test]
    fn erase_in_line_clears_to_end_of_line() {
        let s = run(b"cargo test\r\x1b[K", 2, 20);
        for c in 0..20 {
            assert_eq!(
                s.cell_at(0, c).ch,
                ' ',
                "col {c} should be cleared after \\r ESC[K",
            );
        }
        assert_eq!(s.cursor_col, 0);
        assert_eq!(s.cursor_row, 0);
    }

    #[test]
    fn sgr_colors_apply_to_following_cells() {
        // Red foreground; alacritty parses `\x1b[31m`, paints
        // `hello` red, then `\x1b[0m` resets back to default
        // for the following space + chars.
        let s = run(b"\x1b[31mhello\x1b[0m world", 2, 20);
        for c in 0..5 {
            let cell = s.cell_at(0, c);
            assert_eq!(
                cell.fg,
                TerminalColor::Named(NamedColor::Red),
                "cell `{}` at col {c} should be red",
                cell.ch,
            );
        }
        // After the reset, cells should fall back to default fg.
        assert_eq!(s.cell_at(0, 5).fg, TerminalColor::Default);
        assert_eq!(s.cell_at(0, 6).fg, TerminalColor::Default);
        assert_eq!(s.cell_at(0, 6).ch, 'w');
    }

    #[test]
    fn truecolor_sgr_lands_as_rgb_cell() {
        // 24-bit color: `\x1b[38;2;R;G;Bm`.
        let s = run(b"\x1b[38;2;100;150;200mhi", 2, 5);
        assert_eq!(s.cell_at(0, 0).fg, TerminalColor::Rgb(100, 150, 200));
        assert_eq!(s.cell_at(0, 1).fg, TerminalColor::Rgb(100, 150, 200));
    }

    #[test]
    fn bold_attribute_survives_the_swap() {
        let s = run(b"\x1b[1mbold\x1b[22m", 2, 10);
        for c in 0..4 {
            assert!(s.cell_at(0, c).attrs.bold, "col {c} should be bold");
        }
    }

    #[test]
    fn cursor_visible_defaults_true_and_hides_on_civis() {
        let visible = run(b"hi", 2, 5);
        assert!(visible.cursor_visible);
        // `\x1b[?25l` = DECTCEM hide cursor.
        let hidden = run(b"\x1b[?25lhi", 2, 5);
        assert!(!hidden.cursor_visible);
    }

    #[test]
    fn alt_screen_flag_flips_on_smcup() {
        // `\x1b[?1049h` enters alt-screen (smcup).
        let s = run(b"\x1b[?1049h", 2, 5);
        assert!(s.alt_screen);
    }

    // ---- T3 (2026-05-25): scrollback ring + viewport ----

    #[test]
    fn scrollback_disabled_when_lines_zero() {
        let s = run_with_scrollback(b"", 4, 10, 0);
        assert_eq!(s.scrollback_rows, 0);
        assert_eq!(s.scroll_offset, 0);
    }

    #[test]
    fn scrollback_rows_grow_as_history_accumulates() {
        // 3-row screen with capacity for 32 history rows. Push
        // 6 rows of content; 3 land on-screen, the other 3 roll
        // into scrollback.
        let s = run_with_scrollback(
            b"r0\r\nr1\r\nr2\r\nr3\r\nr4\r\nr5",
            3,
            10,
            32,
        );
        assert_eq!(s.scroll_offset, 0);
        // 5 newlines on a 3-row screen ⇒ 3 history rows
        // populated (r0 / r1 / r2 rolled off above the live
        // window).
        assert!(
            s.scrollback_rows >= 3,
            "expected ≥3 scrollback rows, got {}",
            s.scrollback_rows,
        );
    }

    #[test]
    fn snapshot_window_shifts_when_term_scrolled_back() {
        // Push 8 rows of content into a 3-row screen with 16
        // lines of scrollback. After the burst, the live screen
        // shows the last 3 rows; older rows live in scrollback.
        let mut term = build_term(3, 10, 16);
        let mut processor: Processor = Processor::new();
        let burst = b"r0\r\nr1\r\nr2\r\nr3\r\nr4\r\nr5\r\nr6\r\nr7";
        processor.advance(&mut term, burst);
        // Live edge — visible window shows r5/r6/r7.
        let live = term_to_snapshot(&term, 1);
        assert_eq!(live.scroll_offset, 0);
        assert_eq!(live.cell_at(0, 0).ch, 'r');
        assert_eq!(live.cell_at(0, 1).ch, '5');
        assert_eq!(live.cell_at(2, 1).ch, '7');
        // Scroll up by 2 lines via the public Term method.
        term.scroll_display(Scroll::Delta(2));
        let scrolled = term_to_snapshot(&term, 2);
        assert_eq!(scrolled.scroll_offset, 2);
        // Window now shows r3/r4/r5 (rows shifted up by 2).
        assert_eq!(scrolled.cell_at(0, 0).ch, 'r');
        assert_eq!(scrolled.cell_at(0, 1).ch, '3');
        assert_eq!(scrolled.cell_at(2, 1).ch, '5');
    }

    #[test]
    fn cursor_hidden_when_scrolled_past_live_cursor() {
        // Cursor lives on the live edge; scrolling back beyond
        // its row hides the cursor in the rendered snapshot.
        let mut term = build_term(3, 10, 16);
        let mut processor: Processor = Processor::new();
        processor.advance(
            &mut term,
            b"a\r\nb\r\nc\r\nd\r\ne\r\nf\r\ng",
        );
        let live = term_to_snapshot(&term, 1);
        assert!(live.cursor_visible);
        term.scroll_display(Scroll::Top);
        let top = term_to_snapshot(&term, 2);
        assert!(top.scroll_offset > 0);
        assert!(
            !top.cursor_visible,
            "cursor should be hidden when scrolled past it",
        );
    }

    /// T3: simulates the snap-to-live-edge step that
    /// `Editor::do_enter_terminal_insert` performs before
    /// activating the minor mode. After scrolling back into
    /// history then issuing `Bottom`, the published snapshot
    /// reflects the live edge (`scroll_offset == 0`).
    #[test]
    fn scroll_bottom_snaps_back_to_live_edge() {
        let term = Arc::new(Mutex::new(build_term(3, 10, 16)));
        let snapshot = Arc::new(ArcSwap::from_pointee(TerminalSnapshot::empty()));
        let seq = Arc::new(AtomicU64::new(0));
        let shared = SharedTerm {
            inner: Arc::clone(&term),
            snapshot: Arc::clone(&snapshot),
            seq: Arc::clone(&seq),
            paint_request: None,
        };
        {
            let mut t = term.lock();
            let mut p: Processor = Processor::new();
            p.advance(&mut *t, b"r0\r\nr1\r\nr2\r\nr3\r\nr4\r\nr5\r\nr6");
        }
        // Scroll into history.
        shared.scroll(TerminalScrollKind::Top);
        assert!(
            snapshot.load().scroll_offset > 0,
            "expected non-zero scroll_offset after Top",
        );
        // Snap to bottom (mirroring `do_enter_terminal_insert`'s
        // pre-activation step).
        shared.scroll(TerminalScrollKind::Bottom);
        assert_eq!(
            snapshot.load().scroll_offset,
            0,
            "Bottom should reset the viewport to the live edge",
        );
    }

    /// T3.b: forward search walks top-to-bottom and returns
    /// the first match. The hit's `line` field is in alacritty
    /// grid coordinates (negative = history; positive = live
    /// screen).
    #[test]
    fn find_match_forward_returns_oldest_hit() {
        let mut term = build_term(3, 20, 16);
        let mut processor: Processor = Processor::new();
        // Six rows pushed; "needle" appears on the first row
        // (which rolls into scrollback) and the fifth.
        processor.advance(
            &mut term,
            b"needle one\r\nrow1\r\nrow2\r\nrow3\r\nneedle two\r\nrow5",
        );
        let snapshot = Arc::new(ArcSwap::from_pointee(TerminalSnapshot::empty()));
        let seq = Arc::new(AtomicU64::new(0));
        let shared = SharedTerm {
            inner: Arc::new(Mutex::new(term)),
            snapshot,
            seq,
            paint_request: None,
        };
        let regex = fancy_regex::Regex::new("needle").unwrap();
        let hit = shared.find_match(&regex, SearchDir::Forward).unwrap();
        // The first (oldest) match should win. With 6 lines on
        // a 3-row screen, the first `needle one` lives at
        // alacritty line -3 (3 rows back); the second `needle
        // two` at line 1.
        assert!(
            hit.line < 0,
            "forward search should find scrollback first, got line {}",
            hit.line,
        );
        assert_eq!(hit.column, 0);
        assert_eq!(hit.len, 6);
    }

    /// T3.b: backward search walks bottom-to-top and returns
    /// the newest match.
    #[test]
    fn find_match_backward_returns_newest_hit() {
        let mut term = build_term(3, 20, 16);
        let mut processor: Processor = Processor::new();
        processor.advance(
            &mut term,
            b"needle one\r\nrow1\r\nrow2\r\nrow3\r\nneedle two\r\nrow5",
        );
        let snapshot = Arc::new(ArcSwap::from_pointee(TerminalSnapshot::empty()));
        let seq = Arc::new(AtomicU64::new(0));
        let shared = SharedTerm {
            inner: Arc::new(Mutex::new(term)),
            snapshot,
            seq,
            paint_request: None,
        };
        let regex = fancy_regex::Regex::new("needle").unwrap();
        let hit = shared.find_match(&regex, SearchDir::Backward).unwrap();
        // Backward search should find the second (newer)
        // occurrence first; that one is on the live screen so
        // line >= 0.
        assert!(
            hit.line >= 0,
            "backward search should find live-edge match first, got line {}",
            hit.line,
        );
    }

    /// T3.b: scroll_to_line snaps the viewport so the target
    /// row becomes the top of the visible window. Used by
    /// search to bring the match into view.
    #[test]
    fn scroll_to_line_moves_viewport_to_target() {
        let mut term = build_term(3, 10, 16);
        let mut processor: Processor = Processor::new();
        processor.advance(&mut term, b"r0\r\nr1\r\nr2\r\nr3\r\nr4\r\nr5");
        let snapshot = Arc::new(ArcSwap::from_pointee(TerminalSnapshot::empty()));
        let seq = Arc::new(AtomicU64::new(0));
        let shared = SharedTerm {
            inner: Arc::new(Mutex::new(term)),
            snapshot: Arc::clone(&snapshot),
            seq,
            paint_request: None,
        };
        // Scroll to line -3 (3 rows back in history).
        shared.scroll_to_line(-3);
        let snap = snapshot.load();
        assert_eq!(snap.scroll_offset, 3);
    }

    /// T3.b.2: linewise yank — extract cell text for a grid
    /// line range. Trailing space padding is trimmed so the
    /// register doesn't end up with column-wide whitespace
    /// blobs. Result always ends in `\n` so it round-trips as a
    /// linewise yank.
    #[test]
    fn line_range_text_extracts_trimmed_rows() {
        let mut term = build_term(3, 20, 16);
        let mut processor: Processor = Processor::new();
        processor.advance(
            &mut term,
            b"first line\r\nsecond line\r\nthird line",
        );
        let snapshot = Arc::new(ArcSwap::from_pointee(TerminalSnapshot::empty()));
        let seq = Arc::new(AtomicU64::new(0));
        let shared = SharedTerm {
            inner: Arc::new(Mutex::new(term)),
            snapshot,
            seq,
            paint_request: None,
        };
        // Live screen lines are 0, 1, 2 — yank all three.
        let text = shared.line_range_text(0, 2);
        assert_eq!(text, "first line\nsecond line\nthird line\n");
    }

    /// T3.b.2.b: charwise extraction across two rows — first
    /// row picks up the tail from `start_col`, last row picks
    /// up the head through `end_col` inclusive. Intermediate
    /// rows aren't present here (s_line + 1 == e_line).
    #[test]
    fn char_range_text_spans_two_rows() {
        let mut term = build_term(3, 20, 16);
        let mut processor: Processor = Processor::new();
        processor.advance(&mut term, b"hello world\r\nfoo bar baz");
        let snapshot = Arc::new(ArcSwap::from_pointee(TerminalSnapshot::empty()));
        let seq = Arc::new(AtomicU64::new(0));
        let shared = SharedTerm {
            inner: Arc::new(Mutex::new(term)),
            snapshot,
            seq,
            paint_request: None,
        };
        // Select from (row 0, col 6) "world" through (row 1, col 2) "foo".
        let text = shared.char_range_text(0, 6, 1, 2);
        assert_eq!(text, "world\nfoo");
    }

    /// T3.b.2.b: charwise extraction on a single row keeps
    /// exact cell text in the inclusive [start_col, end_col]
    /// window (including any embedded spaces).
    #[test]
    fn char_range_text_single_row_inclusive() {
        let mut term = build_term(2, 20, 4);
        let mut processor: Processor = Processor::new();
        processor.advance(&mut term, b"hello world");
        let snapshot = Arc::new(ArcSwap::from_pointee(TerminalSnapshot::empty()));
        let seq = Arc::new(AtomicU64::new(0));
        let shared = SharedTerm {
            inner: Arc::new(Mutex::new(term)),
            snapshot,
            seq,
            paint_request: None,
        };
        // Cols 6..=10 = "world".
        let text = shared.char_range_text(0, 6, 0, 10);
        assert_eq!(text, "world");
    }

    /// T3.b.2.b: blockwise extraction is a rectangle. Each row
    /// contributes its slice; trailing pad inside the rectangle
    /// is preserved to keep column alignment (matches vim's
    /// `<C-v>` → `y` behaviour).
    #[test]
    fn block_range_text_extracts_rectangle() {
        let mut term = build_term(3, 20, 4);
        let mut processor: Processor = Processor::new();
        processor.advance(&mut term, b"abc def ghi\r\njkl mno pqr\r\nstu vwx yz1");
        let snapshot = Arc::new(ArcSwap::from_pointee(TerminalSnapshot::empty()));
        let seq = Arc::new(AtomicU64::new(0));
        let shared = SharedTerm {
            inner: Arc::new(Mutex::new(term)),
            snapshot,
            seq,
            paint_request: None,
        };
        // Cols 4..=6 (the "def" / "mno" / "vwx" middle column).
        let text = shared.block_range_text(0, 2, 4, 6);
        assert_eq!(text, "def\nmno\nvwx\n");
    }

    /// T3.b.3: `find_all_matches` returns every occurrence of
    /// the pattern across history + live screen, bounded by
    /// the safety cap. Used by the hlsearch overlay so all
    /// hits in the visible window paint.
    #[test]
    fn find_all_matches_collects_every_occurrence() {
        let mut term = build_term(3, 30, 16);
        let mut processor: Processor = Processor::new();
        processor.advance(
            &mut term,
            b"error: one\r\nok\r\nerror: two\r\nerror: three\r\nok",
        );
        let snapshot = Arc::new(ArcSwap::from_pointee(TerminalSnapshot::empty()));
        let seq = Arc::new(AtomicU64::new(0));
        let shared = SharedTerm {
            inner: Arc::new(Mutex::new(term)),
            snapshot,
            seq,
            paint_request: None,
        };
        let regex = fancy_regex::Regex::new("error").unwrap();
        let hits = shared.find_all_matches(&regex);
        assert_eq!(hits.len(), 3, "expected 3 `error` hits, got {hits:?}");
        for h in &hits {
            assert_eq!(h.len, 5);
            assert_eq!(h.column, 0);
        }
    }

    /// T3.b.2: yanking outside the available grid bounds clamps
    /// rather than panicking; an empty / no-overlap range
    /// returns an empty string.
    #[test]
    fn line_range_text_clamps_out_of_bounds() {
        let term = build_term(3, 10, 16);
        let snapshot = Arc::new(ArcSwap::from_pointee(TerminalSnapshot::empty()));
        let seq = Arc::new(AtomicU64::new(0));
        let shared = SharedTerm {
            inner: Arc::new(Mutex::new(term)),
            snapshot,
            seq,
            paint_request: None,
        };
        // 9999..=10000 is entirely past the live edge.
        assert_eq!(shared.line_range_text(9999, 10000), "");
        // start > end → empty.
        assert_eq!(shared.line_range_text(2, 1), "");
    }

    /// T3.b: searching when the pattern doesn't match returns
    /// None without panicking.
    #[test]
    fn find_match_returns_none_for_unmatched_pattern() {
        let term = build_term(3, 10, 16);
        let snapshot = Arc::new(ArcSwap::from_pointee(TerminalSnapshot::empty()));
        let seq = Arc::new(AtomicU64::new(0));
        let shared = SharedTerm {
            inner: Arc::new(Mutex::new(term)),
            snapshot,
            seq,
            paint_request: None,
        };
        let regex = fancy_regex::Regex::new("nothing-here").unwrap();
        assert!(shared.find_match(&regex, SearchDir::Forward).is_none());
    }

    #[test]
    fn shared_term_scroll_publishes_fresh_snapshot() {
        // Constructs a SharedTerm + writes some rows into it,
        // then drives a scroll via the public handle. The
        // snapshot Arc should update with the new scroll
        // offset.
        let term = Arc::new(Mutex::new(build_term(3, 10, 16)));
        let snapshot = Arc::new(ArcSwap::from_pointee(TerminalSnapshot::empty()));
        let seq = Arc::new(AtomicU64::new(0));
        let shared = SharedTerm {
            inner: Arc::clone(&term),
            snapshot: Arc::clone(&snapshot),
            seq: Arc::clone(&seq),
            paint_request: None,
        };
        {
            let mut t = term.lock();
            let mut p: Processor = Processor::new();
            p.advance(&mut *t, b"r0\r\nr1\r\nr2\r\nr3\r\nr4\r\nr5");
            // Publish a baseline snapshot at live edge.
            let s = term_to_snapshot(&*t, 1);
            snapshot.store(Arc::new(s));
            seq.store(1, Ordering::Relaxed);
        }
        assert_eq!(snapshot.load().scroll_offset, 0);
        shared.scroll(TerminalScrollKind::PageUp);
        let scrolled = snapshot.load();
        assert!(scrolled.scroll_offset > 0);
        assert!(scrolled.seq > 1);
    }
}
