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

use lattice_core::Buffer;
use lattice_grammar::{ScrollPos, ViewportPos};
use lattice_protocol::position::Position;

use super::{
    App, BufferId, BufferKind, EchoLevel, PositionEntry, PositionSource, is_valid_mark_name,
    last_addressable_line, line_byte_len,
};

/// Cap on entries in the position-history ring. The write side
/// (`push_position_history`) drops the oldest entry when this
/// is exceeded; the walkers (`do_walk_history` etc.) clamp
/// against the live length.
// 5.5.F.4.2: `POSITION_HISTORY_CAP` relocated to
// `lattice_host::dispatch::POSITION_HISTORY_CAP` alongside
// `Editor::push_position_history`. No App-side consumer remains.

impl App {
    /// Vim's `%`: jump to the matching `()[]{}`. Behavior: scan
    /// the current line from `cursor.byte` for the first bracket
    /// char; that bracket and its match define the jump. If the
    /// cursor is past every bracket on the line, do nothing.
    pub(super) fn do_match_bracket(&mut self) {
        let text = self.editor.document.text();
        let bytes = text.as_bytes();
        let cursor_byte = match self
            .editor.document
            .snapshot()
            .buffer
            .position_to_byte(self.editor.cursor)
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
        let pre_jump = self.editor.cursor;
        let target = if forward {
            scan_forward_for_match(bytes, start, open, close)
        } else {
            scan_backward_for_match(bytes, start, open, close)
        };
        match target {
            Some(t) => {
                if let Ok(pos) = self.editor.document.snapshot().buffer.byte_to_position(t) {
                    self.push_position_history(pre_jump, PositionSource::AutoJump);
                    self.editor.cursor = pos;
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
        // In-popup `<C-o>` first walks the popup's back-stack so the
        // user stays within the popup chain (`:describe-buffer` →
        // mode link → `:describe-mode foo` → `<C-o>` returns to
        // `:describe-buffer` without flipping out of Help). Only
        // after the back-stack is empty does `<C-o>` fall through
        // to the outer position-history walk.
        if delta < 0
            && matches!(self.editor.active_buffer, BufferKind::Help)
            && self.editor.popup_buffer.is_some()
            && !self.editor.popup_back_stack.is_empty()
            && self.pop_popup_back()
        {
            return;
        }
        if delta < 0
            && self.editor.position_history_cursor == self.editor.position_history.len()
            && self.editor.position_history.iter().any(|e| e.is_jump())
        {
            let cur = self.active_cursor();
            let already_there = self
                .editor.position_history
                .last()
                .map(|e| e.position == cur && e.buffer == self.editor.active_buffer)
                .unwrap_or(false);
            if !already_there {
                self.push_position_history(cur, PositionSource::AutoJump);
                // After push the cursor==len. Step it one back so the
                // walk finds the entry preceding our snapshot rather
                // than the snapshot itself.
                self.editor.position_history_cursor = self.editor.position_history.len().saturating_sub(1);
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
        if !self.editor.position_history.iter().any(&pred) {
            self.set_message(EchoLevel::Error, format!("no {empty_label}"));
            return;
        }
        // Reachable: the registry still holds an entry for the
        // recorded buffer id (in-pane Help / Document / FileTree
        // all live in `self.editor.buffers`); the transient popup-mode
        // Help overlay's id is checked separately.
        let popup_help_id = self.editor.popup_buffer;
        let reachable = |e: &PositionEntry| -> bool {
            match e.buffer {
                BufferKind::Document | BufferKind::FileTree => self.editor.buffers.contains(e.buffer_id),
                BufferKind::Help => {
                    self.editor.buffers.contains_help(e.buffer_id) || popup_help_id == Some(e.buffer_id)
                }
                BufferKind::Oil => self.editor.buffers.contains(e.buffer_id),
            }
        };
        let combined = |e: &PositionEntry| pred(e) && reachable(e);
        let target_idx = if delta < 0 {
            self.editor.position_history[..self.editor.position_history_cursor]
                .iter()
                .rposition(&combined)
        } else {
            let from = self
                .editor.position_history_cursor
                .saturating_add(1)
                .min(self.editor.position_history.len());
            self.editor.position_history[from..]
                .iter()
                .position(&combined)
                .map(|i| i + from)
        };
        let Some(idx) = target_idx else {
            let bound = if delta < 0 { "start" } else { "end" };
            self.set_message(EchoLevel::Error, format!("at {bound} of {bound_label}"));
            return;
        };
        self.editor.position_history_cursor = idx;
        let entry = self.editor.position_history[idx];
        // Cross-buffer landing: switch active_buffer and write the
        // cursor onto the right buffer's tracking field.
        match entry.buffer {
            BufferKind::Document => {
                // Pane + document state must follow the entry. Cross-buffer
                // walks (Help/FileTree/Oil -> Document) need `pane.buffer_id`
                // updated so the renderer + modeline read the right buffer;
                // cross-document walks need the document handle swapped via
                // `activate_document` so motion / search / save target the
                // recorded buffer. The reachable check above already verified
                // the registry entry; activate_document re-checks for safety.
                if self.editor.buffers.contains_document(entry.buffer_id) {
                    self.activate_document(entry.buffer_id);
                    self.editor.cursor = entry.position;
                    self.clamp_cursor_to_buffer();
                    self.auto_open_folds_at_cursor();
                }
            }
            BufferKind::Help => {
                self.editor.active_buffer = BufferKind::Help;
                // Prefer an in-pane help buffer with the recorded id;
                // fall back to the transient popup. Either way the
                // live cursor lands on `self.editor.cursor` (unified).
                let buffer_present = self.editor.buffers.contains_help(entry.buffer_id)
                    || self.editor.popup_buffer == Some(entry.buffer_id);
                if buffer_present {
                    self.editor.cursor = entry.position;
                    self.editor.pane_tree.active_mut().buffer = BufferKind::Help;
                    self.editor.pane_tree.active_mut().buffer_id = entry.buffer_id;
                    self.clamp_cursor_to_active_buffer();
                }
            }
            BufferKind::FileTree => {
                if self.editor.buffers.contains_file_tree(entry.buffer_id) {
                    self.editor.active_buffer = BufferKind::FileTree;
                    self.editor.cursor = entry.position;
                    self.editor.pane_tree.active_mut().buffer = BufferKind::FileTree;
                    self.editor.pane_tree.active_mut().buffer_id = entry.buffer_id;
                    self.clamp_cursor_to_active_buffer();
                }
            }
            BufferKind::Oil => {
                if self.editor.buffers.contains_oil(entry.buffer_id) {
                    self.editor.active_buffer = BufferKind::Oil;
                    self.editor.cursor = entry.position;
                    self.editor.pane_tree.active_mut().buffer = BufferKind::Oil;
                    self.editor.pane_tree.active_mut().buffer_id = entry.buffer_id;
                    self.clamp_cursor_to_active_buffer();
                }
            }
        }
    }

    /// 5.5.D: cursor clamp moved to
    /// [`lattice_host::editor::Editor::clamp_cursor_to_active_buffer`].
    /// Renderer call sites keep the thin wrapper for now; 5.5.G
    /// removes it when App's match collapses.
    pub(super) fn clamp_cursor_to_buffer(&mut self) {
        self.editor.clamp_cursor_to_active_buffer();
    }

    /// 5.5.D: see [`Self::clamp_cursor_to_buffer`]. Delegates to the
    /// host-side implementation.
    pub(super) fn clamp_cursor_to_active_buffer(&mut self) {
        self.editor.clamp_cursor_to_active_buffer();
    }

    /// 5.5.D: viewport-scroll-to-cursor logic moved to
    /// [`lattice_host::editor::Editor::ensure_cursor_visible`].
    pub(super) fn ensure_cursor_visible(&mut self) {
        self.editor.ensure_cursor_visible();
    }

    pub fn set_viewport_height(&mut self, height: u32) {
        self.editor.viewport_height = height.max(1);
        self.editor.ensure_cursor_visible();
    }

    /// Compute the active pane's *content* height inside a buffer
    /// area of `buffer_height` rows. Mirrors the renderer's per-pane
    /// layout: the pane tree splits the area evenly; with more than
    /// one pane, the bottom row of each pane is reserved for the
    /// status line. Returns at least 1 so callers can multiply / use
    /// without checking for zero.
    ///
    /// Used by the runtime to feed `set_viewport_height` the
    /// **active pane's** content height -- not the full buffer area
    /// -- so motions, scroll, fold-aware ensure_cursor_visible all
    /// agree with what's actually drawn. Without this, a horizontal
    /// split clips the lower half of the upper pane: the App thinks
    /// it has the whole screen, the renderer only paints half.
    ///
    /// **Help-popup overlay (State B).** When the focus has moved
    /// into a hover/help popup that paints as a centred overlay
    /// (active_buffer == Help, but the active pane still shows a
    /// Document underneath), the popup -- not the pane -- is the
    /// surface receiving motion. Returning the *popup's inner
    /// height* here keeps `ensure_cursor_visible` and the renderer
    /// in sync: without it, `j` past the last *visible* popup row
    /// silently advanced `cursor.line` (the pane viewport is much
    /// taller than the popup, so the App thought the cursor was
    /// fine) and the renderer pinned the cursor visually to the
    /// last drawn row -- so subsequent `k` had to "unwind" the
    /// phantom overshoot before any visible motion. Help-as-buffer
    /// (in-pane help, where pane.buffer == Help) doesn't take this
    /// branch -- the pane content height is the right answer.
    pub fn active_pane_content_height(&self, buffer_height: u32) -> u32 {
        if let Some(h) = self.help_popup_inner_height(buffer_height) {
            return h;
        }
        let area = crate::pane::PaneRect {
            x: 0,
            y: 0,
            width: 1,
            height: buffer_height as u16,
        };
        let rects = self.editor.pane_tree.compute_rects(area);
        let active_idx = self.editor.pane_tree.active_index();
        let multi = rects.len() > 1;
        let pane_h = rects
            .iter()
            .find(|(idx, _)| *idx == active_idx)
            .map(|(_, r)| r.height)
            .unwrap_or(buffer_height as u16);
        let content_h = if multi && pane_h >= 2 {
            pane_h - 1 // reserve the per-pane status row
        } else {
            pane_h
        };
        u32::from(content_h).max(1)
    }

    /// Inner height of the hover/help popup overlay when one is
    /// active in State B (focused popup, doc still showing in the
    /// pane below). `None` when no overlay is active or help fills
    /// the pane (in which case the regular pane-content-height
    /// path applies).
    ///
    /// Sizing matches `render::position_help_popup` exactly so the
    /// motion engine and the renderer agree on the popup viewport.
    /// Border rows (top + bottom) are subtracted; the result is
    /// the row count `Paragraph` actually paints into.
    pub fn help_popup_inner_height(&self, buffer_height: u32) -> Option<u32> {
        if !matches!(self.editor.active_buffer, BufferKind::Help) {
            return None;
        }
        if self.editor.pane_tree.active().buffer == BufferKind::Help {
            return None;
        }
        let help = self.popup_help()?;
        // Single source of truth for popup sizing -- keeps the
        // renderer's painted viewport and the motion engine's
        // scroll bounds in lockstep regardless of placement.
        // Buffer width is unknown here (the caller passes height
        // only); pass a generous synthetic width since the
        // helper's height calc doesn't depend on width.
        let line_count = u16::try_from(help.line_count().max(1)).unwrap_or(u16::MAX);
        let buffer_h = u16::try_from(buffer_height.max(1)).unwrap_or(u16::MAX);
        let (_w, height) = lattice_core::ui::popup::popup_outer_size(
            u16::MAX,
            buffer_h,
            line_count,
            self.editor.popup_placement,
        );
        Some(u32::from(height).saturating_sub(2).max(1))
    }

    /// Jump the cursor to a viewport-relative line. `H` -> top of view,
    /// `M` -> middle, `L` -> bottom. Column is preserved (clamped to the
    /// destination line's length).
    pub(super) fn do_jump_viewport(&mut self, vpos: ViewportPos) {
        let height = self.editor.viewport_height.max(1);
        let line = match vpos {
            ViewportPos::Top => self.editor.scroll,
            ViewportPos::Middle => self.editor.scroll + height / 2,
            ViewportPos::Bottom => self.editor.scroll + height.saturating_sub(1),
        };
        let buffer = self.active_text();
        let last = last_addressable_line(&buffer);
        let line = line.min(last);
        let len = line_byte_len(&buffer, line);
        let byte = self.editor.cursor.byte.min(len);
        self.editor.cursor = Position::new(line, byte);
        // Folds only apply to documents.
        if matches!(self.editor.active_buffer, BufferKind::Document) {
            self.auto_open_folds_at_cursor();
        }
    }

    /// Adjust scroll so the cursor lands at the requested viewport row.
    /// Cursor itself doesn't move (vim's `zt`/`zz`/`zb`).
    pub(super) fn do_scroll_cursor_to(&mut self, spos: ScrollPos) {
        let height = self.editor.viewport_height.max(1);
        self.editor.scroll = match spos {
            ScrollPos::Top => self.editor.cursor.line,
            ScrollPos::Center => self.editor.cursor.line.saturating_sub(height / 2),
            ScrollPos::Bottom => self.editor.cursor.line.saturating_sub(height.saturating_sub(1)),
        };
    }

    /// Move cursor by one viewport-height (vim's Ctrl-F / Ctrl-B). Vim
    /// leaves a 1-line overlap; we mirror that by stepping
    /// `viewport_height - 2` lines and letting `ensure_cursor_visible`
    /// handle the scroll.
    pub(super) fn do_page(&mut self, down: bool) {
        let height = self.editor.viewport_height.max(1);
        let step = height.saturating_sub(2).max(1);
        let buffer = self.active_text();
        let last = last_addressable_line(&buffer);
        let new_line = if down {
            self.editor.cursor.line.saturating_add(step).min(last)
        } else {
            self.editor.cursor.line.saturating_sub(step)
        };
        let len = line_byte_len(&buffer, new_line);
        let byte = self.editor.cursor.byte.min(len);
        self.editor.cursor = Position::new(new_line, byte);
    }

    /// Scroll one line. `down = true` -> Ctrl-E (scroll content up,
    /// pulling the next line into view); `down = false` -> Ctrl-Y.
    /// Cursor follows so it stays on-screen.
    pub(super) fn do_scroll_line(&mut self, down: bool) {
        let height = self.editor.viewport_height.max(1);
        let buffer = self.active_text();
        if down {
            let last = last_addressable_line(&buffer);
            self.editor.scroll = self.editor.scroll.saturating_add(1).min(last);
            // Pull cursor down if it's now off the top of the viewport.
            if self.editor.cursor.line < self.editor.scroll {
                self.editor.cursor.line = self.editor.scroll;
            }
        } else {
            self.editor.scroll = self.editor.scroll.saturating_sub(1);
            // Push cursor up if it's now off the bottom.
            let bottom = self.editor.scroll + height.saturating_sub(1);
            if self.editor.cursor.line > bottom {
                self.editor.cursor.line = bottom;
            }
        }
        let len = line_byte_len(&buffer, self.editor.cursor.line);
        if self.editor.cursor.byte > len {
            self.editor.cursor.byte = len;
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
        let Some(entry) = self.editor.tag_stack.pop() else {
            self.set_message(EchoLevel::Info, "tag stack empty".to_string());
            return;
        };
        // Push the *current* cursor onto the jump list before
        // walking back so `<C-i>` returns to the post-pop spot.
        self.push_position_history(self.editor.cursor, PositionSource::PluginPush);
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
            if self.editor.buffers.contains(entry.buffer_id) {
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
        self.editor.cursor = Position::new(line, col);
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
        let Some(&pos) = self.editor.marks.get(&name) else {
            self.set_message(EchoLevel::Error, format!("mark not set: {name}"));
            return;
        };
        // Push pre-jump position so Ctrl-O can return.
        let cur = self.editor.cursor;
        self.push_position_history(cur, PositionSource::AutoJump);
        if exact {
            self.editor.cursor = pos;
        } else {
            // Line-only jump: snap byte to first non-blank on that line.
            let text = self.editor.document.text();
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
            self.editor.cursor = Position::new(pos.line, col as u32);
        }
        self.clamp_cursor_to_buffer();
        self.auto_open_folds_at_cursor();
    }

    /// 5.5.F.4.2: body relocated to
    /// [`lattice_host::dispatch::Editor::push_position_history`].
    /// Delegate retained so the ~30 ui-tui call sites compile
    /// unchanged across the wider `motions.rs` migration window.
    pub(super) fn push_position_history(&mut self, pos: Position, source: PositionSource) {
        self.editor.push_position_history(pos, source);
    }

    /// Id of whichever buffer is currently active. The active
    /// pane's `buffer_id` is the source of truth -- documents and
    /// trees both live in [`Self::buffers`] under one id space.
    /// Help still lives outside the registry as a transient
    /// overlay; while help is active we return its id, otherwise
    /// the active pane's id.
    pub fn active_buffer_id(&self) -> BufferId {
        self.editor.active_buffer_id()
    }

    /// 5.5.D: see [`lattice_host::editor::Editor::active_cursor`].
    pub fn active_cursor(&self) -> Position {
        self.editor.active_cursor()
    }

    /// 5.5.D: see [`lattice_host::editor::Editor::active_text`].
    pub fn active_text(&self) -> Buffer {
        self.editor.active_text()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use crate::app::TagStackEntry;
    use crate::app::test_helpers::{app_with, invoke_motion};
    use crate::app::*;

    #[test]
    fn invoke_char_right_advances_cursor() {
        let mut a = app_with("abc", 10);
        let id = a.editor.builtins.char_right;
        a.apply(invoke_motion(id));
        assert_eq!(a.editor.cursor, Position::new(0, 1));
    }

    #[test]
    fn invoke_char_left_at_origin_does_not_underflow() {
        let mut a = app_with("abc", 10);
        let id = a.editor.builtins.char_left;
        a.apply(invoke_motion(id));
        assert_eq!(a.editor.cursor, Position::ZERO);
    }

    #[test]
    fn invoke_line_down_then_line_up() {
        let mut a = app_with("hello\nworld", 10);
        let down = a.editor.builtins.line_down;
        let up = a.editor.builtins.line_up;
        a.apply(invoke_motion(down));
        assert_eq!(a.editor.cursor.line, 1);
        a.apply(invoke_motion(up));
        assert_eq!(a.editor.cursor.line, 0);
    }

    #[test]
    fn invoke_goto_last_line_jumps_to_last_line() {
        let mut a = app_with("a\nb\nc", 10);
        let id = a.editor.builtins.goto_last_line;
        a.apply(invoke_motion(id));
        assert_eq!(a.editor.cursor.line, 2);
    }

    #[test]
    fn invoke_goto_first_line_returns_to_origin() {
        let mut a = app_with("a\nb\nc", 10);
        let last = a.editor.builtins.goto_last_line;
        let first = a.editor.builtins.goto_first_line;
        a.apply(invoke_motion(last));
        a.apply(invoke_motion(first));
        assert_eq!(a.editor.cursor, Position::ZERO);
    }

    #[test]
    fn invoke_line_end_moves_to_eol() {
        let mut a = app_with("hello world", 10);
        let id = a.editor.builtins.line_end;
        a.apply(invoke_motion(id));
        assert_eq!(a.editor.cursor, Position::new(0, 11));
    }

    #[test]
    fn ensure_visible_scrolls_when_cursor_goes_off_bottom() {
        let mut a = app_with("0\n1\n2\n3\n4\n5\n6\n7\n8\n9", 3);
        let id = a.editor.builtins.goto_last_line;
        a.apply(invoke_motion(id));
        assert_eq!(a.editor.cursor.line, 9);
        assert_eq!(a.editor.scroll, 9 - 3 + 1);
    }

    #[test]
    fn ensure_visible_scrolls_back_to_top_on_goto_first() {
        let mut a = app_with("0\n1\n2\n3\n4", 2);
        let last = a.editor.builtins.goto_last_line;
        let first = a.editor.builtins.goto_first_line;
        a.apply(invoke_motion(last));
        a.apply(invoke_motion(first));
        assert_eq!(a.editor.scroll, 0);
    }

    #[test]
    fn set_mark_clears_partial_chord() {
        let mut a = app_with("hello", 10);
        a.apply(Action::AbsorbPartialChord(crate::chord::KeyChord::char(
            'm',
        )));
        a.apply(Action::SetMark('a'));
        assert!(a.editor.partial_chord.is_empty());
    }

    #[test]
    fn jump_to_mark_clears_partial_chord() {
        let mut a = app_with("hello\nworld", 10);
        a.apply(Action::SetMark('a'));
        a.apply(Action::AbsorbPartialChord(crate::chord::KeyChord::char(
            '`',
        )));
        a.apply(Action::JumpToMarkExact('a'));
        assert!(a.editor.partial_chord.is_empty());
    }

    #[test]
    fn jump_history_with_no_jumps_emits_error() {
        let mut a = app_with("hello", 10);
        a.apply(Action::JumpHistoryBack);
        let msg = a.editor.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
    }

    #[test]
    fn gg_pushes_jump_history_and_ctrl_o_returns() {
        let mut a = app_with("a\nb\nc\nd\ne", 10);
        a.editor.cursor = Position::new(3, 0); // line 3 ('d')
        a.apply(invoke_motion(a.editor.builtins.goto_first_line));
        assert_eq!(a.editor.cursor, Position::ZERO);
        a.apply(Action::JumpHistoryBack);
        assert_eq!(a.editor.cursor, Position::new(3, 0));
    }

    #[test]
    fn star_pushes_position_history() {
        let mut a = app_with("foo bar foo", 10);
        a.editor.cursor = Position::new(0, 1); // on 'o' of first "foo"
        a.apply(Action::SearchWordUnderCursor(SearchDirection::Forward));
        // Cursor now on second "foo" at byte 8.
        assert_eq!(a.editor.cursor, Position::new(0, 8));
        a.apply(Action::JumpHistoryBack);
        assert_eq!(a.editor.cursor, Position::new(0, 1));
    }

    #[test]
    fn percent_pushes_position_history() {
        let mut a = app_with("call(arg)", 10);
        a.editor.cursor = Position::new(0, 4); // on '('
        a.apply(Action::MatchBracket);
        assert_eq!(a.editor.cursor, Position::new(0, 8)); // ')'
        a.apply(Action::JumpHistoryBack);
        assert_eq!(a.editor.cursor, Position::new(0, 4));
    }

    #[test]
    fn mark_jump_pushes_position_history() {
        let mut a = app_with("hello\nworld", 10);
        a.editor.cursor = Position::new(1, 2);
        a.apply(Action::SetMark('a'));
        a.editor.cursor = Position::ZERO;
        a.apply(Action::JumpToMarkExact('a'));
        assert_eq!(a.editor.cursor, Position::new(1, 2));
        a.apply(Action::JumpHistoryBack);
        assert_eq!(a.editor.cursor, Position::ZERO);
    }

    #[test]
    fn set_mark_pushes_named_mark_into_position_history() {
        let mut a = app_with("hello\nworld", 10);
        a.editor.cursor = Position::new(1, 2);
        a.apply(Action::SetMark('a'));
        // Last entry is a NamedMark.
        let last = a.editor.position_history.last().unwrap();
        assert_eq!(last.position, Position::new(1, 2));
        assert!(matches!(last.source, PositionSource::NamedMark('a')));
    }

    #[test]
    fn jump_history_filters_to_jump_class_only() {
        let mut a = app_with("aaa\nbbb\nccc\nddd", 10);
        // mX (NamedMark) followed by gg (AutoJump). Ctrl-O should walk
        // to the AutoJump entry, NOT the NamedMark.
        a.editor.cursor = Position::new(1, 0);
        a.apply(Action::SetMark('a'));
        // Position history now has [NamedMark('a') at (1,0)].
        a.editor.cursor = Position::new(3, 0);
        a.apply(invoke_motion(a.editor.builtins.goto_first_line));
        // Now history: [NamedMark('a'), AutoJump (3,0)].
        a.apply(Action::JumpHistoryBack);
        // Ctrl-O lands on the AutoJump entry, not the named mark.
        assert_eq!(a.editor.cursor, Position::new(3, 0));
    }

    #[test]
    fn jump_and_mark_walks_share_the_same_ring_cursor() {
        // After Ctrl-O moves cursor through the ring, g; should pick
        // up from the new cursor position when scanning for marks.
        let mut a = app_with("a\nb\nc\nd\ne", 10);
        a.editor.cursor = Position::new(1, 0);
        a.apply(Action::SetMark('a')); // ring [NamedMark a@(1,0)] cursor=1
        a.editor.cursor = Position::new(3, 0);
        a.apply(invoke_motion(a.editor.builtins.goto_first_line));
        // ring [NamedMark a, AutoJump (3,0)] cursor=2
        // Ctrl-O jumps to AutoJump (3,0). Snapshot of (0,0) pushed.
        // Actually: with snapshot pre-step, ring [a, (3,0), (0,0)],
        // cursor walks from 3 backward to find jump -> index 1 ((3,0)).
        a.apply(Action::JumpHistoryBack);
        assert_eq!(a.editor.cursor, Position::new(3, 0));
        // g; from current ring cursor (1) walks back to find NamedMark
        // at index 0.
        a.apply(Action::WalkMarkHistoryBack);
        assert_eq!(a.editor.cursor, Position::new(1, 0));
    }

    #[test]
    fn position_history_dedups_consecutive_same() {
        let mut a = app_with("a\nb\nc", 10);
        a.push_position_history(Position::new(2, 0), PositionSource::AutoJump);
        a.push_position_history(Position::new(2, 0), PositionSource::AutoJump);
        // Pushing the same position-and-source twice in a row -> single entry.
        assert_eq!(a.editor.position_history.len(), 1);
    }

    #[test]
    fn position_history_capped_at_max() {
        let mut a = app_with("a\nb\nc", 10);
        for i in 0..200 {
            a.push_position_history(Position::new(i % 3, 0), PositionSource::AutoJump);
        }
        assert!(a.editor.position_history.len() <= 100);
    }

    #[test]
    fn star_finds_next_occurrence_of_word_under_cursor() {
        let mut a = app_with("foo bar foo bar", 10);
        a.editor.cursor = Position::new(0, 1); // on 'o' of first "foo"
        a.apply(Action::SearchWordUnderCursor(SearchDirection::Forward));
        assert_eq!(a.editor.cursor, Position::new(0, 8)); // start of second "foo"
        let last = a.editor.last_search.as_ref().unwrap();
        assert_eq!(last.pattern, "foo");
    }

    #[test]
    fn star_when_cursor_not_on_word_scans_forward() {
        let mut a = app_with("  hello world", 10);
        a.editor.cursor = Position::new(0, 0); // on space
        a.apply(Action::SearchWordUnderCursor(SearchDirection::Forward));
        // The first word "hello" appears once in the buffer; pattern is
        // recorded but no match is found beyond it (no second "hello").
        let last = a.editor.last_search.as_ref().unwrap();
        assert_eq!(last.pattern, "hello");
    }

    #[test]
    fn star_with_no_word_on_line_emits_error() {
        let mut a = app_with("   ", 10);
        a.apply(Action::SearchWordUnderCursor(SearchDirection::Forward));
        let msg = a.editor.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
    }

    #[test]
    fn star_records_pattern_even_on_no_other_match() {
        let mut a = app_with("only hello", 10);
        a.editor.cursor = Position::new(0, 5); // on 'h'
        a.apply(Action::SearchWordUnderCursor(SearchDirection::Forward));
        // Only one occurrence; wrap puts us at the same place.
        let last = a.editor.last_search.as_ref().unwrap();
        assert_eq!(last.pattern, "hello");
    }

    #[test]
    fn percent_jumps_from_open_to_close_paren() {
        let mut a = app_with("call(arg1, arg2)", 10);
        a.editor.cursor = Position::new(0, 4); // on '('
        a.apply(Action::MatchBracket);
        assert_eq!(a.editor.cursor, Position::new(0, 15));
    }

    #[test]
    fn percent_jumps_from_close_to_open_paren() {
        let mut a = app_with("call(arg1, arg2)", 10);
        a.editor.cursor = Position::new(0, 15); // on ')'
        a.apply(Action::MatchBracket);
        assert_eq!(a.editor.cursor, Position::new(0, 4));
    }

    #[test]
    fn percent_with_nested_picks_correct_match() {
        let mut a = app_with("a(b(c)d)e", 10);
        a.editor.cursor = Position::new(0, 1); // on outer '('
        a.apply(Action::MatchBracket);
        assert_eq!(a.editor.cursor, Position::new(0, 7)); // outer ')'
    }

    #[test]
    fn percent_searches_forward_for_first_bracket_when_cursor_off() {
        let mut a = app_with("call(arg)", 10);
        a.editor.cursor = Position::ZERO; // 'c'; first bracket on line is '(' at byte 4
        a.apply(Action::MatchBracket);
        assert_eq!(a.editor.cursor, Position::new(0, 8)); // ')'
    }

    #[test]
    fn percent_with_no_bracket_on_line_emits_error() {
        let mut a = app_with("plain text only", 10);
        a.apply(Action::MatchBracket);
        let msg = a.editor.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
    }

    #[test]
    fn percent_with_unmatched_bracket_emits_error() {
        let mut a = app_with("foo(bar", 10);
        a.editor.cursor = Position::new(0, 3);
        a.apply(Action::MatchBracket);
        let msg = a.editor.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
    }

    #[test]
    fn percent_works_for_brackets_and_braces() {
        let mut a = app_with("[a, b, c]", 10);
        a.editor.cursor = Position::ZERO;
        a.apply(Action::MatchBracket);
        assert_eq!(a.editor.cursor, Position::new(0, 8));
    }

    #[test]
    fn jump_viewport_top_lands_on_scroll_line() {
        let mut a = app_with("0\n1\n2\n3\n4\n5\n6\n7\n8\n9", 5);
        a.editor.scroll = 3;
        a.editor.cursor = Position::new(7, 0);
        a.apply(Action::JumpViewport(ViewportPos::Top));
        assert_eq!(a.editor.cursor.line, 3);
    }

    #[test]
    fn jump_viewport_middle_lands_at_half_height() {
        let mut a = app_with("0\n1\n2\n3\n4\n5\n6\n7\n8\n9", 6);
        a.editor.scroll = 0;
        a.apply(Action::JumpViewport(ViewportPos::Middle));
        // height/2 = 3, so cursor goes to line 3.
        assert_eq!(a.editor.cursor.line, 3);
    }

    #[test]
    fn jump_viewport_bottom_lands_at_height_minus_one() {
        let mut a = app_with("0\n1\n2\n3\n4\n5\n6\n7\n8\n9", 5);
        a.editor.scroll = 2;
        a.apply(Action::JumpViewport(ViewportPos::Bottom));
        // 2 + 5 - 1 = 6.
        assert_eq!(a.editor.cursor.line, 6);
    }

    #[test]
    fn jump_viewport_clamps_to_last_addressable_line() {
        let mut a = app_with("a\nb", 50);
        a.apply(Action::JumpViewport(ViewportPos::Bottom));
        assert_eq!(a.editor.cursor.line, 1);
    }

    #[test]
    fn scroll_cursor_to_center_centers_cursor() {
        let mut a = app_with("0\n1\n2\n3\n4\n5\n6\n7\n8\n9", 5);
        a.editor.cursor = Position::new(6, 0);
        a.apply(Action::ScrollCursorTo(ScrollPos::Center));
        // cursor.line - height/2 = 6 - 2 = 4.
        assert_eq!(a.editor.scroll, 4);
        // Cursor itself unchanged.
        assert_eq!(a.editor.cursor.line, 6);
    }

    #[test]
    fn scroll_cursor_to_top_aligns_scroll_with_cursor() {
        let mut a = app_with("0\n1\n2\n3\n4\n5\n6\n7\n8\n9", 5);
        a.editor.cursor = Position::new(6, 0);
        a.apply(Action::ScrollCursorTo(ScrollPos::Top));
        assert_eq!(a.editor.scroll, 6);
    }

    #[test]
    fn scroll_cursor_to_bottom_pulls_scroll_up_by_height_minus_one() {
        let mut a = app_with("0\n1\n2\n3\n4\n5\n6\n7\n8\n9", 5);
        a.editor.cursor = Position::new(8, 0);
        a.apply(Action::ScrollCursorTo(ScrollPos::Bottom));
        // 8 - (5 - 1) = 4.
        assert_eq!(a.editor.scroll, 4);
    }

    #[test]
    fn page_down_advances_by_viewport_height_minus_two() {
        let mut a = app_with("0\n1\n2\n3\n4\n5\n6\n7\n8\n9", 5);
        a.editor.cursor = Position::ZERO;
        a.apply(Action::PageDown);
        assert_eq!(a.editor.cursor.line, 3);
    }

    #[test]
    fn page_down_clamps_to_last_addressable_line() {
        let mut a = app_with("0\n1\n2\n3\n4\n5\n6\n7\n8\n9", 5);
        a.editor.cursor = Position::new(8, 0);
        a.apply(Action::PageDown);
        assert_eq!(a.editor.cursor.line, 9);
    }

    #[test]
    fn page_up_steps_back_by_viewport_height_minus_two() {
        let mut a = app_with("0\n1\n2\n3\n4\n5\n6\n7\n8\n9", 5);
        a.editor.cursor = Position::new(7, 0);
        a.apply(Action::PageUp);
        assert_eq!(a.editor.cursor.line, 4);
    }

    #[test]
    fn page_up_at_top_stays_at_top() {
        let mut a = app_with("0\n1\n2", 5);
        a.apply(Action::PageUp);
        assert_eq!(a.editor.cursor.line, 0);
    }

    #[test]
    fn scroll_line_down_advances_scroll_and_pulls_cursor_if_off_top() {
        let mut a = app_with("0\n1\n2\n3\n4\n5\n6", 3);
        a.editor.cursor = Position::ZERO;
        a.editor.scroll = 0;
        a.apply(Action::ScrollLineDown);
        assert_eq!(a.editor.scroll, 1);
        // Cursor was at line 0; now it's off the top, so it follows.
        assert_eq!(a.editor.cursor.line, 1);
    }

    #[test]
    fn scroll_line_up_decreases_scroll_and_pushes_cursor_if_off_bottom() {
        let mut a = app_with("0\n1\n2\n3\n4\n5\n6", 3);
        a.editor.cursor = Position::new(4, 0);
        a.editor.scroll = 2; // viewport covers lines 2,3,4.
        a.apply(Action::ScrollLineUp);
        assert_eq!(a.editor.scroll, 1);
        // Bottom of new viewport is line 3; cursor was at 4, gets pushed up.
        assert_eq!(a.editor.cursor.line, 3);
    }

    #[test]
    fn set_mark_records_cursor_position() {
        let mut a = app_with("hello\nworld", 10);
        a.editor.cursor = Position::new(1, 2);
        a.apply(Action::SetMark('a'));
        assert_eq!(a.editor.marks.get(&'a'), Some(&Position::new(1, 2)));
    }

    #[test]
    fn jump_mark_exact_restores_cursor_position() {
        let mut a = app_with("hello\nworld\nfoo", 10);
        a.editor.cursor = Position::new(0, 3);
        a.apply(Action::SetMark('m'));
        a.editor.cursor = Position::new(2, 0);
        a.apply(Action::JumpToMarkExact('m'));
        assert_eq!(a.editor.cursor, Position::new(0, 3));
    }

    #[test]
    fn jump_mark_line_lands_on_first_non_blank() {
        let mut a = app_with("hello\n    indented\nfoo", 10);
        a.editor.cursor = Position::new(1, 8); // mid-word on the indented line
        a.apply(Action::SetMark('a'));
        a.editor.cursor = Position::ZERO;
        a.apply(Action::JumpToMarkLine('a'));
        // Line 1, byte 4 = 'i' (after 4 leading spaces).
        assert_eq!(a.editor.cursor, Position::new(1, 4));
    }

    #[test]
    fn jump_to_unset_mark_emits_error() {
        let mut a = app_with("hello", 10);
        a.apply(Action::JumpToMarkExact('z'));
        let msg = a.editor.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
    }

    #[test]
    fn count_with_line_motion_advances_count_lines() {
        let mut a = app_with("0\n1\n2\n3\n4\n5\n6\n7\n8\n9", 20);
        a.apply(Action::Invoke(
            CommandInvocation::of(a.editor.builtins.line_down.0)
                .with_count(lattice_grammar::command::Count(5)),
        ));
        assert_eq!(a.editor.cursor.line, 5);
    }

    #[test]
    fn count_with_dd_deletes_n_lines_as_single_undo() {
        // `2dd`: count=2 expands Range::CurrentLine to span 2 lines.
        // The whole deletion MUST land as a single undo unit -- a
        // single `u` should restore the original buffer.
        let mut a = app_with("one\ntwo\nthree\nfour", 10);
        a.editor.cursor = Position::new(0, 0);
        a.editor.op_count = 2;
        let inv = CommandInvocation::of(a.editor.builtins.delete.0)
            .with_range(lattice_grammar::Range::CurrentLine)
            .with_count(lattice_grammar::command::Count(2));
        a.apply(Action::Invoke(inv));
        // Lines 0 and 1 ("one" and "two") deleted; line 2 ("three") survives.
        let text = a.editor.document.text();
        assert!(!text.contains("one"));
        assert!(!text.contains("two"));
        assert!(text.contains("three"));
        assert!(text.contains("four"));

        // One undo should fully restore.
        let _ = a.undo_blocking();
        assert_eq!(a.editor.document.text(), "one\ntwo\nthree\nfour");
    }

    #[test]
    fn count_with_indent_right_indents_n_lines_as_single_undo() {
        // `2>>`: count=2 expands Range::CurrentLine to span 2 lines.
        // The whole indent MUST land as a single undo unit -- the
        // operator builds the per-line edits up front and commits
        // via apply_edit_batch.
        let mut a = app_with("one\ntwo\nthree\nfour", 10);
        a.editor.cursor = Position::new(0, 0);
        a.editor.op_count = 2;
        let inv = CommandInvocation::of(a.editor.builtins.indent_right.0)
            .with_range(lattice_grammar::Range::CurrentLine)
            .with_count(lattice_grammar::command::Count(2));
        a.apply(Action::Invoke(inv));
        assert_eq!(a.editor.document.text(), "    one\n    two\nthree\nfour");
        // Single undo restores the original buffer.
        let _ = a.undo_blocking();
        assert_eq!(a.editor.document.text(), "one\ntwo\nthree\nfour");
    }

    #[test]
    fn count_with_indent_left_dedents_n_lines_as_single_undo() {
        let mut a = app_with("    one\n    two\nthree\nfour", 10);
        a.editor.cursor = Position::new(0, 0);
        a.editor.op_count = 2;
        let inv = CommandInvocation::of(a.editor.builtins.indent_left.0)
            .with_range(lattice_grammar::Range::CurrentLine)
            .with_count(lattice_grammar::command::Count(2));
        a.apply(Action::Invoke(inv));
        assert_eq!(a.editor.document.text(), "one\ntwo\nthree\nfour");
        let _ = a.undo_blocking();
        assert_eq!(a.editor.document.text(), "    one\n    two\nthree\nfour");
    }

    #[test]
    fn count_zero_through_pending_count_is_ignored_by_motion() {
        // pending_count remains 0 after no digit; motion uses default 1.
        let mut a = app_with("hello world", 10);
        let id = a.editor.builtins.word_forward;
        a.apply(invoke_motion(id));
        assert_eq!(a.editor.cursor, Position::new(0, 6));
    }

    #[test]
    fn next_pane_cycles_active() {
        let mut a = app_with("first\nsecond\nthird", 10);
        a.editor.cursor = Position::new(2, 0);
        a.apply(Action::SplitPaneVertical);
        // After split: 2 panes, both seeded with cursor (2, 0).
        // Move cursor in the active pane.
        a.editor.cursor = Position::new(0, 0);
        a.apply(Action::NextPane);
        assert_eq!(a.editor.pane_tree.active_index(), 1);
        // Pane 1 should still hold its stashed cursor (2, 0).
        assert_eq!(a.editor.cursor, Position::new(2, 0));
        // Cycle back -- pane 0 holds (0, 0) per the in-active mutation.
        a.apply(Action::NextPane);
        assert_eq!(a.editor.pane_tree.active_index(), 0);
        assert_eq!(a.editor.cursor, Position::new(0, 0));
    }

    #[test]
    fn navigate_pane_walks_to_spatial_neighbour() {
        let mut a = app_with("xx", 10);
        a.editor.terminal_width = Some(80);
        a.apply(Action::SplitPaneVertical);
        // Active=0 (left). Navigate Right -> active=1.
        a.apply(Action::NavigatePane(PaneDirection::Right));
        assert_eq!(a.editor.pane_tree.active_index(), 1);
        // Navigate Left -> active=0.
        a.apply(Action::NavigatePane(PaneDirection::Left));
        assert_eq!(a.editor.pane_tree.active_index(), 0);
    }

    #[test]
    fn tag_stack_pop_on_empty_echoes_message() {
        let mut a = app_with("xx", 10);
        a.apply(Action::TagStackPop);
        let msg = a.editor.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("tag stack empty"));
    }

    #[test]
    fn ctrl_o_from_help_buffer_swaps_pane_back_to_document() {
        // Regression: do_walk_history's Document arm previously only
        // updated `active_buffer` + `self.editor.cursor` and forgot to update
        // `pane.buffer` / `pane.buffer_id`. The renderer + modeline read
        // pane.buffer_id, so <C-o> from a help-in-pane buffer back to a
        // document position left the renderer painting the help buffer.
        let mut a = app_with("alpha\nbeta\ngamma\n", 10);
        let doc_id = a.editor.document_buffer_id;
        a.editor.cursor = Position::new(1, 2);
        // Open a help buffer in-pane; activate_help_in_pane pushes an
        // AutoJump entry recording the pre-activation Document cursor.
        let help =
            crate::help::HelpContent::from_lines("regression", vec!["help body".to_string()]);
        a.open_help_in_pane(help);
        assert_eq!(a.editor.active_buffer, BufferKind::Help);
        let active_pane = a.editor.pane_tree.active();
        assert_eq!(active_pane.buffer, BufferKind::Help);
        assert_ne!(active_pane.buffer_id, doc_id);
        // <C-o> walks back to the AutoJump entry pointing at the doc.
        a.apply(Action::JumpHistoryBack);
        assert_eq!(a.editor.active_buffer, BufferKind::Document);
        let active_pane = a.editor.pane_tree.active();
        assert_eq!(
            active_pane.buffer,
            BufferKind::Document,
            "pane.buffer must follow active_buffer back to Document"
        );
        assert_eq!(
            active_pane.buffer_id, doc_id,
            "pane.buffer_id must point at the original document so the renderer paints it"
        );
        assert_eq!(a.editor.cursor, Position::new(1, 2));
    }

    #[test]
    fn tag_stack_drives_pop_back_to_origin() {
        let mut a = app_with("alpha\nbeta\ngamma\ndelta\n", 10);
        // Pretend we drilled down from line 0 col 2 to line 3
        // col 1 (the gd-like `do_lsp_nav_request` -> drain
        // single-result path normally pushes; we synthesise
        // the entry directly to keep the test free of LSP wire).
        a.editor.tag_stack.push(TagStackEntry {
            buffer: a.editor.active_buffer,
            buffer_id: a.active_pane_buffer_id(),
            position: Position::new(0, 2),
            label: "foo".into(),
        });
        a.editor.cursor = Position::new(3, 1);
        a.apply(Action::TagStackPop);
        assert_eq!(a.editor.cursor, Position::new(0, 2));
        assert!(a.editor.tag_stack.is_empty());
        // Pop pushes the post-pop cursor onto position history
        // (PluginPush) so a follow-up `<C-i>` returns to (3, 1).
        let last = a.editor.position_history.last().expect("history entry");
        assert!(matches!(last.source, PositionSource::PluginPush));
        assert_eq!(last.position, Position::new(3, 1));
    }
}
