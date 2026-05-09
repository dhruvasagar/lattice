//! Motion-feature App methods that aren't already
//! implemented by the grammar layer in `lattice-grammar`.
//! Most motions evaluate via the grammar's `Motion` type;
//! this module is for the App-side cases that need state
//! beyond the buffer (history pushes, fold auto-open,
//! multi-step scans).
//!
//! Methods that live here:
//! - `do_match_bracket` (`%` -- scan from cursor for the
//!   first bracket on the current line, then walk to its
//!   match; pushes position history and auto-opens folds).
//! - `scan_forward_for_match` /
//!   `scan_backward_for_match` (file-private helpers for
//!   the bracket walk).
//! - `push_position_history` (the write side of the
//!   position-history ring; pairs with `do_walk_history`
//!   here). `POSITION_HISTORY_CAP` lives alongside it.
//! - `clamp_cursor_to_buffer` /
//!   `clamp_cursor_to_active_buffer` /
//!   `ensure_cursor_visible` -- foundational primitives the
//!   viewport / scroll family depends on.
//!
//! What does NOT live here: the motion *grammar* (rules and
//! parser) -- that lives in `lattice-grammar`. This module
//! is the App-side glue for motions that need richer state
//! than the grammar provides.

use lattice_grammar::{ScrollPos, ViewportPos};
use lattice_protocol::position::Position;

use super::{
    App, BufferKind, EchoLevel, PositionEntry, PositionSource, is_valid_mark_name,
    last_addressable_line, line_byte_len,
};

/// Cap on entries in the position-history ring. The write side
/// (`push_position_history`) drops the oldest entry when this
/// is exceeded; the walkers (`do_walk_history` etc.) clamp
/// against the live length.
const POSITION_HISTORY_CAP: usize = 100;

impl App {
    /// Vim's `%`: jump to the matching `()[]{}`. Behavior: scan
    /// the current line from `cursor.byte` for the first bracket
    /// char; that bracket and its match define the jump. If the
    /// cursor is past every bracket on the line, do nothing.
    pub(super) fn do_match_bracket(&mut self) {
        let text = self.document.text();
        let bytes = text.as_bytes();
        let cursor_byte = match self
            .document
            .snapshot()
            .buffer
            .position_to_byte(self.cursor)
        {
            Ok(b) => b,
            Err(_) => return,
        };
        let mut idx = cursor_byte;
        let mut bracket = None;
        while idx < bytes.len() && bytes[idx] != b'\n' {
            if matches!(bytes[idx], b'(' | b')' | b'[' | b']' | b'{' | b'}') {
                bracket = Some((idx, bytes[idx]));
                break;
            }
            idx += 1;
        }
        let Some((start, b)) = bracket else {
            self.set_message(EchoLevel::Error, "no bracket on this line".to_string());
            return;
        };
        let (open, close, forward) = match b {
            b'(' => (b'(', b')', true),
            b')' => (b'(', b')', false),
            b'[' => (b'[', b']', true),
            b']' => (b'[', b']', false),
            b'{' => (b'{', b'}', true),
            b'}' => (b'{', b'}', false),
            _ => return,
        };
        let pre_jump = self.cursor;
        let target = if forward {
            scan_forward_for_match(bytes, start, open, close)
        } else {
            scan_backward_for_match(bytes, start, open, close)
        };
        match target {
            Some(t) => {
                if let Ok(pos) = self.document.snapshot().buffer.byte_to_position(t) {
                    self.push_position_history(pre_jump, PositionSource::AutoJump);
                    self.cursor = pos;
                    self.auto_open_folds_at_cursor();
                }
            }
            None => {
                self.set_message(EchoLevel::Error, "unmatched bracket".to_string());
            }
        }
    }
}

fn scan_forward_for_match(bytes: &[u8], from: usize, open: u8, close: u8) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = from;
    loop {
        if i >= bytes.len() {
            return None;
        }
        let b = bytes[i];
        if b == open {
            depth += 1;
        } else if b == close {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
}

fn scan_backward_for_match(bytes: &[u8], from: usize, open: u8, close: u8) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = from;
    loop {
        let b = bytes[i];
        if b == close {
            depth += 1;
        } else if b == open {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        if i == 0 {
            return None;
        }
        i -= 1;
    }
}

impl App {
    /// Step through the position history filtered to jump-class entries
    /// (AutoJump | PluginPush). `delta = -1` for Ctrl-O, `+1` for Ctrl-I.
    /// On the first Ctrl-O from end-of-ring, also snapshot the current
    /// cursor as AutoJump so a subsequent Ctrl-I can return to it.
    pub(super) fn do_jump_history(&mut self, delta: i32) {
        if delta < 0
            && self.position_history_cursor == self.position_history.len()
            && self.position_history.iter().any(|e| e.is_jump())
        {
            let cur = self.active_cursor();
            let already_there = self
                .position_history
                .last()
                .map(|e| e.position == cur && e.buffer == self.active_buffer)
                .unwrap_or(false);
            if !already_there {
                self.push_position_history(cur, PositionSource::AutoJump);
                // After push the cursor==len. Step it one back so the
                // walk finds the entry preceding our snapshot rather
                // than the snapshot itself.
                self.position_history_cursor = self.position_history.len().saturating_sub(1);
            }
        }
        self.do_walk_history(delta, |e| e.is_jump(), "jumps", "jump list");
    }

    /// Step through named-mark entries -- vim's `g;` (back) / `g,`
    /// (forward) per §5.1.1's interpretation. No "snapshot current
    /// pos" pre-step: mark navigation is exploratory and shouldn't
    /// pollute the jump-list ring.
    pub(super) fn do_mark_history(&mut self, delta: i32) {
        self.do_walk_history(delta, |e| e.is_named_mark(), "marks", "mark history");
    }

    /// Generic walk over the unified ring filtered by `pred`. Mirrors
    /// vim's "save current pos on first step back so the forward step
    /// can return to it" behavior, but only when the current position
    /// itself qualifies for the filter (so jumping back over named
    /// marks doesn't pollute the ring with AutoJump entries and vice
    /// versa).
    ///
    /// When the target entry was recorded in a different buffer
    /// (e.g. the user pressed `<C-o>` from a help overlay back to a
    /// document position), the walk also flips
    /// [`Self::active_buffer`] and lands the cursor on the correct
    /// buffer. Stale entries pointing at a closed Help buffer
    /// (matching kind but different id) are skipped -- the history
    /// outlives any one Help session.
    pub(super) fn do_walk_history<F: Fn(&PositionEntry) -> bool>(
        &mut self,
        delta: i32,
        pred: F,
        empty_label: &str,
        bound_label: &str,
    ) {
        if !self.position_history.iter().any(&pred) {
            self.set_message(EchoLevel::Error, format!("no {empty_label}"));
            return;
        }
        // Reachable: the registry still holds an entry for the
        // recorded buffer id (in-pane Help / Document / FileTree
        // all live in `self.buffers`); the transient popup-mode
        // Help overlay's id is checked separately.
        let popup_help_id = self.help_buffer.as_ref().map(|h| h.id);
        let reachable = |e: &PositionEntry| -> bool {
            match e.buffer {
                BufferKind::Document | BufferKind::FileTree => self.buffers.contains(e.buffer_id),
                BufferKind::Help => {
                    self.buffers.help(e.buffer_id).is_some()
                        || popup_help_id == Some(e.buffer_id)
                }
                BufferKind::Oil => self.buffers.contains(e.buffer_id),
            }
        };
        let combined = |e: &PositionEntry| pred(e) && reachable(e);
        let target_idx = if delta < 0 {
            self.position_history[..self.position_history_cursor]
                .iter()
                .rposition(&combined)
        } else {
            let from = self
                .position_history_cursor
                .saturating_add(1)
                .min(self.position_history.len());
            self.position_history[from..]
                .iter()
                .position(&combined)
                .map(|i| i + from)
        };
        let Some(idx) = target_idx else {
            let bound = if delta < 0 { "start" } else { "end" };
            self.set_message(EchoLevel::Error, format!("at {bound} of {bound_label}"));
            return;
        };
        self.position_history_cursor = idx;
        let entry = self.position_history[idx];
        // Cross-buffer landing: switch active_buffer and write the
        // cursor onto the right buffer's tracking field.
        match entry.buffer {
            BufferKind::Document => {
                self.active_buffer = BufferKind::Document;
                self.cursor = entry.position;
                self.clamp_cursor_to_buffer();
                self.auto_open_folds_at_cursor();
            }
            BufferKind::Help => {
                self.active_buffer = BufferKind::Help;
                // Prefer an in-pane help buffer with the recorded id;
                // fall back to the transient popup. Either way the
                // live cursor lands on `self.cursor` (unified).
                let buffer_present = self.buffers.help(entry.buffer_id).is_some()
                    || self
                        .help_buffer
                        .as_ref()
                        .map(|h| h.id == entry.buffer_id)
                        .unwrap_or(false);
                if buffer_present {
                    self.cursor = entry.position;
                    self.pane_tree.active_mut().buffer = BufferKind::Help;
                    self.pane_tree.active_mut().buffer_id = entry.buffer_id;
                    self.clamp_cursor_to_active_buffer();
                }
            }
            BufferKind::FileTree => {
                if self.buffers.file_tree(entry.buffer_id).is_some() {
                    self.active_buffer = BufferKind::FileTree;
                    self.cursor = entry.position;
                    self.pane_tree.active_mut().buffer = BufferKind::FileTree;
                    self.pane_tree.active_mut().buffer_id = entry.buffer_id;
                    self.clamp_cursor_to_active_buffer();
                }
            }
            BufferKind::Oil => {
                if self.buffers.oil(entry.buffer_id).is_some() {
                    self.active_buffer = BufferKind::Oil;
                    self.cursor = entry.position;
                    self.pane_tree.active_mut().buffer = BufferKind::Oil;
                    self.pane_tree.active_mut().buffer_id = entry.buffer_id;
                    self.clamp_cursor_to_active_buffer();
                }
            }
        }
    }

    pub(super) fn clamp_cursor_to_buffer(&mut self) {
        self.clamp_cursor_to_active_buffer();
    }

    /// Clamp `self.cursor` to the active buffer's bounds. Same as
    /// `clamp_cursor_to_buffer` but reads from `active_text()` so
    /// it works for help / file-tree / document uniformly.
    pub(super) fn clamp_cursor_to_active_buffer(&mut self) {
        let buffer = self.active_text();
        let last_line = last_addressable_line(&buffer);
        if self.cursor.line > last_line {
            self.cursor.line = last_line;
        }
        let len = line_byte_len(&buffer, self.cursor.line);
        if self.cursor.byte > len {
            self.cursor.byte = len;
        }
    }

    pub(super) fn ensure_cursor_visible(&mut self) {
        if self.viewport_height == 0 {
            return;
        }
        if self.cursor.line < self.scroll {
            self.scroll = self.cursor.line;
        }
        let bottom = self.scroll + self.viewport_height - 1;
        if self.cursor.line > bottom {
            self.scroll = self.cursor.line + 1 - self.viewport_height;
        }
    }

    /// Jump the cursor to a viewport-relative line. `H` -> top of view,
    /// `M` -> middle, `L` -> bottom. Column is preserved (clamped to the
    /// destination line's length).
    pub(super) fn do_jump_viewport(&mut self, vpos: ViewportPos) {
        let height = self.viewport_height.max(1);
        let line = match vpos {
            ViewportPos::Top => self.scroll,
            ViewportPos::Middle => self.scroll + height / 2,
            ViewportPos::Bottom => self.scroll + height.saturating_sub(1),
        };
        let buffer = self.active_text();
        let last = last_addressable_line(&buffer);
        let line = line.min(last);
        let len = line_byte_len(&buffer, line);
        let byte = self.cursor.byte.min(len);
        self.cursor = Position::new(line, byte);
        // Folds only apply to documents.
        if matches!(self.active_buffer, BufferKind::Document) {
            self.auto_open_folds_at_cursor();
        }
    }

    /// Adjust scroll so the cursor lands at the requested viewport row.
    /// Cursor itself doesn't move (vim's `zt`/`zz`/`zb`).
    pub(super) fn do_scroll_cursor_to(&mut self, spos: ScrollPos) {
        let height = self.viewport_height.max(1);
        self.scroll = match spos {
            ScrollPos::Top => self.cursor.line,
            ScrollPos::Center => self.cursor.line.saturating_sub(height / 2),
            ScrollPos::Bottom => self.cursor.line.saturating_sub(height.saturating_sub(1)),
        };
    }

    /// Move cursor by one viewport-height (vim's Ctrl-F / Ctrl-B). Vim
    /// leaves a 1-line overlap; we mirror that by stepping
    /// `viewport_height - 2` lines and letting `ensure_cursor_visible`
    /// handle the scroll.
    pub(super) fn do_page(&mut self, down: bool) {
        let height = self.viewport_height.max(1);
        let step = height.saturating_sub(2).max(1);
        let buffer = self.active_text();
        let last = last_addressable_line(&buffer);
        let new_line = if down {
            self.cursor.line.saturating_add(step).min(last)
        } else {
            self.cursor.line.saturating_sub(step)
        };
        let len = line_byte_len(&buffer, new_line);
        let byte = self.cursor.byte.min(len);
        self.cursor = Position::new(new_line, byte);
    }

    /// Scroll one line. `down = true` -> Ctrl-E (scroll content up,
    /// pulling the next line into view); `down = false` -> Ctrl-Y.
    /// Cursor follows so it stays on-screen.
    pub(super) fn do_scroll_line(&mut self, down: bool) {
        let height = self.viewport_height.max(1);
        let buffer = self.active_text();
        if down {
            let last = last_addressable_line(&buffer);
            self.scroll = self.scroll.saturating_add(1).min(last);
            // Pull cursor down if it's now off the top of the viewport.
            if self.cursor.line < self.scroll {
                self.cursor.line = self.scroll;
            }
        } else {
            self.scroll = self.scroll.saturating_sub(1);
            // Push cursor up if it's now off the bottom.
            let bottom = self.scroll + height.saturating_sub(1);
            if self.cursor.line > bottom {
                self.cursor.line = bottom;
            }
        }
        let len = line_byte_len(&buffer, self.cursor.line);
        if self.cursor.byte > len {
            self.cursor.byte = len;
        }
    }

    /// `<C-t>` -- pop the tag stack (vim's `:pop`). LIFO walk
    /// back through the chain of `gd` / `gD` / `gy` / `gI`
    /// drill-downs. Echoes "tag stack empty" with no entries.
    /// Restores the recorded buffer (when cross-file) and
    /// position; pushes the post-pop cursor onto the unified
    /// position-history ring (PluginPush) so `<C-o>` continues
    /// to walk the chronological jump record after a `<C-t>`
    /// step.
    pub(super) fn do_tag_stack_pop(&mut self) {
        let Some(entry) = self.tag_stack.pop() else {
            self.set_message(EchoLevel::Info, "tag stack empty".to_string());
            return;
        };
        // Push the *current* cursor onto the jump list before
        // walking back so `<C-i>` returns to the post-pop spot.
        self.push_position_history(self.cursor, PositionSource::PluginPush);
        // If the recorded buffer differs from the active one,
        // switch to it. The match is structural: prefer the
        // exact `buffer_id` if it still exists in the registry,
        // else any buffer of the recorded `buffer` kind.
        let active_id = self.active_pane_buffer_id();
        if entry.buffer_id != active_id {
            // Best-effort activate; if the original buffer is
            // gone (closed) the cursor still moves on whatever
            // buffer is active. Acceptable v1 behaviour --
            // future passes can echo "tag origin buffer gone"
            // or hop to the alternate of the same kind.
            if self.buffers.get(entry.buffer_id).is_some() {
                self.activate_buffer(entry.buffer_id);
            }
        }
        // Clamp to current buffer extents in case the doc was
        // edited after the tag-stack push.
        let buffer = self.active_text();
        let last = last_addressable_line(&buffer);
        let line = entry.position.line.min(last);
        let len = line_byte_len(&buffer, line);
        let col = entry.position.byte.min(len);
        self.cursor = Position::new(line, col);
        let label = if entry.label.is_empty() {
            format!("tag pop -> ({},{})", line + 1, col + 1)
        } else {
            format!("tag pop -> {} ({},{})", entry.label, line + 1, col + 1)
        };
        self.set_message(EchoLevel::Info, label);
    }

    /// Jump to a recorded mark. `exact = true` puts the cursor at the
    /// stored byte; `exact = false` jumps to the line and column = first
    /// non-blank (vim's `'<letter>` semantics).
    pub(super) fn do_jump_mark(&mut self, name: char, exact: bool) {
        if !is_valid_mark_name(name) {
            self.set_message(EchoLevel::Error, format!("invalid mark: {name}"));
            return;
        }
        let Some(&pos) = self.marks.get(&name) else {
            self.set_message(EchoLevel::Error, format!("mark not set: {name}"));
            return;
        };
        // Push pre-jump position so Ctrl-O can return.
        let cur = self.cursor;
        self.push_position_history(cur, PositionSource::AutoJump);
        if exact {
            self.cursor = pos;
        } else {
            // Line-only jump: snap byte to first non-blank on that line.
            let text = self.document.text();
            let line_text = text
                .split_inclusive('\n')
                .nth(pos.line as usize)
                .map(|l| l.trim_end_matches('\n'))
                .unwrap_or("");
            let bytes = line_text.as_bytes();
            let mut col = 0usize;
            while col < bytes.len() && (bytes[col] == b' ' || bytes[col] == b'\t') {
                col += 1;
            }
            self.cursor = Position::new(pos.line, col as u32);
        }
        self.clamp_cursor_to_buffer();
        self.auto_open_folds_at_cursor();
    }

    /// Push a tagged entry onto the history ring. If the history-cursor
    /// is not at the end (the user has been walking back), truncate
    /// forward entries before pushing -- standard "modify-from-middle"
    /// semantics. Capped at POSITION_HISTORY_CAP entries; oldest dropped.
    /// Adjacent same-position-and-source duplicates are coalesced.
    pub(super) fn push_position_history(&mut self, pos: Position, source: PositionSource) {
        let buffer = self.active_buffer;
        let buffer_id = self.active_buffer_id();
        if let Some(last) = self.position_history.last()
            && last.position == pos
            && last.source == source
            && last.buffer == buffer
            && last.buffer_id == buffer_id
        {
            return;
        }
        if self.position_history_cursor < self.position_history.len() {
            self.position_history.truncate(self.position_history_cursor);
        }
        self.position_history.push(PositionEntry {
            position: pos,
            source,
            buffer,
            buffer_id,
        });
        if self.position_history.len() > POSITION_HISTORY_CAP {
            self.position_history.remove(0);
            // Truncating from the front shifts the cursor too; clamp
            // before we re-anchor it.
            self.position_history_cursor = self.position_history_cursor.saturating_sub(1);
        }
        self.position_history_cursor = self.position_history.len();
    }
}
