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
//!
//! Stays in app.rs (deferred):
//! - The remaining motion-class methods (`do_jump_history`,
//!   `do_walk_history`, `do_jump_mark`, etc.) entangle with
//!   position-history and tag-stack state; they migrate
//!   with a future motions.history slice.
//!
//! What does NOT live here: the motion *grammar* (rules and
//! parser) -- that lives in `lattice-grammar`. This module
//! is the App-side glue for motions that need richer state
//! than the grammar provides.

use super::{App, BufferKind, EchoLevel, PositionEntry, PositionSource};

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
}
