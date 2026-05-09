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

use super::{App, EchoLevel, PositionSource};

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
