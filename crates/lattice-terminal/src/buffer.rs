use std::{path::PathBuf, sync::Arc};

use arc_swap::ArcSwap;
use lattice_core::BufferId;

use crate::reader::{GridSearchHit, SharedTerm};
use crate::{PtyHandle, TerminalSnapshot};

/// PTY-backed terminal buffer entry held in the host's
/// buffer registry. Owns the writer handle, the published
/// snapshot cell, and the reader task's `AbortHandle` so
/// dropping the buffer kills the reader (and, transitively,
/// the child via PTY close on SIGHUP).
///
/// Construct via [`TerminalBuffer::from_spawn`] — the host
/// never names the internal field shape directly so adding
/// fields stays non-breaking.
#[derive(Debug)]
pub struct TerminalBuffer {
    pub id: BufferId,
    pub pty: Arc<PtyHandle>,
    pub cwd: Option<PathBuf>,
    pub label: String,
    pub snapshot: Arc<ArcSwap<TerminalSnapshot>>,
    /// T3 (2026-05-25): shared handle to the alacritty `Term`.
    /// Dispatch-side scroll / resize ops lock the inner Mutex
    /// and republish the snapshot. Same handle the reader task
    /// holds — cheap to clone (Arc + ArcSwap + Notify).
    pub term: SharedTerm,
    /// T3.b.3 (2026-05-25): the most recent search hit on this
    /// terminal, set by `submit_search` / `repeat_search` for
    /// Terminal buffers and cleared by `cancel_search`.
    /// Renderers read this in their per-frame paint and overlay
    /// the matched cells with the search-highlight style.
    /// `None` when no active search is in flight.
    pub current_match: Option<GridSearchHit>,
    /// T3.b.2 (2026-05-25): linewise Visual-mode selection state
    /// on this terminal buffer. `None` outside Visual. Both rows
    /// are alacritty grid lines (negative = history). Renderers
    /// paint the inclusive `min(anchor,head)..=max(anchor,head)`
    /// row range with the selection bg; `run_terminal_invocation`
    /// extends `head_line` on `j` / `k` while Visual is active.
    pub visual: Option<TerminalVisualState>,
    /// T3.b.3 (2026-05-25): captured prior-Visual state on
    /// terminal — restored by `gv`. Same shape as `visual`.
    /// `None` when no Visual session has been completed yet.
    pub last_visual: Option<TerminalVisualState>,
    /// T2.c (2026-05-25): `true` after the user has pressed
    /// `<C-\>` in Terminal-Insert and we're waiting for the
    /// second key of the exit chord. Cleared by either the
    /// confirm key (`<C-n>` → exit) or any other key (which
    /// emits `\x1c` plus that key's PTY bytes).
    pub insert_exit_pending: bool,
    /// T3.b.3 (2026-05-25): every regex match across the grid
    /// for the current search pattern. Populated alongside
    /// `current_match` in submit_search / repeat_search and
    /// cleared by cancel_search / enter-Insert. Renderers
    /// paint these with the softer hlsearch overlay; the
    /// distinguished `current_match` keeps the stronger
    /// current-hit style.
    pub all_matches: Vec<GridSearchHit>,
    pub created_at: std::time::SystemTime,
    /// Abort handle for the reader task. Held to keep the task
    /// linked to the buffer's lifetime; on Drop the abort fires
    /// so a removed terminal stops draining its PTY.
    reader_abort: tokio::task::AbortHandle,
}

impl Drop for TerminalBuffer {
    fn drop(&mut self) {
        self.reader_abort.abort();
    }
}

pub struct ScrollbackView {
    // TODO: add lifetime if exposing references
    pub total_rows: u32,
    /// 0 = bottom (live); N = N rows up
    pub viewport_row: u32,
}

/// T3.b.2 (2026-05-25): Visual-mode flavour on a terminal
/// buffer. Mirrors `lattice_grammar::VisualKind` without taking
/// a dep on the grammar crate from the substrate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisualKind {
    /// `v` — character-wise selection from anchor to head.
    Char,
    /// `V` — full-row selection between anchor_line and head_line.
    Line,
    /// `<C-v>` — rectangular selection [min_col..=max_col] ×
    /// [min_line..=max_line].
    Block,
}

/// T3.b.2 (2026-05-25, extended T3.b.2.b): Visual-mode
/// selection over a terminal's cell grid. Lines are alacritty
/// grid coords (negative = scrollback history; positive = live
/// screen); cols are cell columns. Stored on
/// [`TerminalBuffer::visual`] while the user is in
/// Terminal-Visual mode; `None` otherwise.
///
/// Linewise entries leave the columns at 0; renderers and the
/// yank text extractor ignore them. Charwise / blockwise track
/// both axes — see `VisualKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalVisualState {
    pub kind: VisualKind,
    /// Where the selection started — set on `v` / `V` / `<C-v>`
    /// entry.
    pub anchor_line: i32,
    pub anchor_col: u16,
    /// Where the head is — moves with `h` / `j` / `k` / `l`,
    /// scroll, `gg` / `G`, etc.
    pub head_line: i32,
    pub head_col: u16,
}

impl TerminalVisualState {
    /// Inclusive `[min, max]` of the two endpoint lines so
    /// renderers can iterate the selected rows regardless of
    /// which direction the user dragged.
    pub fn line_range(self) -> (i32, i32) {
        (
            self.anchor_line.min(self.head_line),
            self.anchor_line.max(self.head_line),
        )
    }

    /// Inclusive `[min, max]` of the two endpoint columns —
    /// only meaningful for `Block` selections (chars and lines
    /// don't use a rectangular column window).
    pub fn block_col_range(self) -> (u16, u16) {
        (
            self.anchor_col.min(self.head_col),
            self.anchor_col.max(self.head_col),
        )
    }

    /// Sorted `(start, end)` endpoints for character-wise
    /// selections, where `start <= end` in (line, col) order.
    /// Used by the yank extractor to walk the selection in
    /// reading order.
    pub fn char_endpoints(self) -> ((i32, u16), (i32, u16)) {
        let a = (self.anchor_line, self.anchor_col);
        let h = (self.head_line, self.head_col);
        if a <= h { (a, h) } else { (h, a) }
    }
}

impl TerminalBuffer {
    /// Build a buffer entry from freshly-spawned PTY handles +
    /// the host-assigned identity. Centralises the
    /// `TerminalBuffer` field list so the host stays insulated
    /// from substrate-internal field changes.
    pub fn from_spawn(
        id: BufferId,
        label: String,
        cwd: Option<PathBuf>,
        handles: crate::spawner::SpawnHandles,
    ) -> Self {
        let crate::spawner::SpawnHandles {
            pty,
            snapshot,
            term,
            reader_task,
        } = handles;
        Self {
            id,
            pty: Arc::new(pty),
            cwd,
            label,
            snapshot,
            term,
            current_match: None,
            visual: None,
            last_visual: None,
            all_matches: Vec::new(),
            insert_exit_pending: false,
            created_at: std::time::SystemTime::now(),
            reader_abort: reader_task.abort_handle(),
        }
    }

    pub fn scrollback_view(&self) -> ScrollbackView {
        // T1 stub: scrollback not yet implemented
        ScrollbackView {
            total_rows: 0,
            viewport_row: 0,
        }
    }
}
