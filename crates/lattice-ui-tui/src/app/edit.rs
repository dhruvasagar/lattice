//! Insert-mode edits, undo/redo, register paste, and the
//! low-level rope-mutation helpers App calls. R.1.15 lands a
//! small focused subset; the bulk migrates with follow-up
//! slices.
//!
//! Methods that live here:
//! - `do_join_lines` (`J` / `gJ` -- join current line with
//!   next; `J` collapses joining newline to a single space
//!   and trims leading whitespace, `gJ` is pure concat).
//! - `do_toggle_case_at_cursor` (`~` -- toggle the case of
//!   the char at cursor and advance; non-letters pass
//!   through, EOL stops the cursor).
//!
//! Stays in app.rs (deferred to follow-up slices):
//! - Insert-mode commit path (`do_insert_*`), undo / redo,
//!   register paste (`do_paste`, `do_paste_blockwise`,
//!   `do_paste_text`), `do_repeat_last_change` (`.`).
//! - apply_text_edit (the LSP-edit applier reused by
//!   substitute, formatting, code actions).
//!
//! What does NOT live here: the rope itself (ropey,
//! wrapped by `Document`), the undo tree, the register
//! store -- those are owned by `crate::document` /
//! `crate::registers`.

use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range as ProtoRange};

use super::{App, last_addressable_line, line_byte_len};

impl App {
    /// Vim's `J` / `gJ`: join the current line with the next. With
    /// `with_space = true` (J), the joining newline becomes one space
    /// (and any leading whitespace on the next line is trimmed). With
    /// `with_space = false` (gJ), no replacement -- pure concat.
    pub(super) fn do_join_lines(&mut self, with_space: bool) {
        let last = last_addressable_line(&self.document.snapshot().buffer);
        if self.cursor.line >= last {
            return;
        }
        let line = self.cursor.line;
        let next_line = line + 1;
        let cur_len = line_byte_len(&self.document.snapshot().buffer, line);
        // Compute how many leading whitespace bytes to trim from the
        // next line's content (only for J, not gJ).
        let trim = if with_space {
            let text = self.document.text();
            let next_text = text
                .split_inclusive('\n')
                .nth(next_line as usize)
                .map(|l| l.trim_end_matches('\n'))
                .unwrap_or("");
            let mut t = 0usize;
            let bytes = next_text.as_bytes();
            while t < bytes.len() && (bytes[t] == b' ' || bytes[t] == b'\t') {
                t += 1;
            }
            t as u32
        } else {
            0
        };
        // Range to replace covers `\n` + (optional) leading whitespace.
        let range = ProtoRange::new(Position::new(line, cur_len), Position::new(next_line, trim));
        let replacement = if with_space { " " } else { "" };
        if let Ok(applied) = self.apply_edit_blocking(Edit::replace(range, replacement)) {
            // Cursor lands at the end of the original first line (vim's
            // standard J behavior puts cursor on the first space).
            self.cursor = applied.original_range.start;
        }
    }

    /// Vim's `~`: toggle the case of the char at cursor and advance.
    /// Non-letter chars are unchanged; cursor still advances. At EOL
    /// the cursor stops (no wrap).
    pub(super) fn do_toggle_case_at_cursor(&mut self) {
        let line_len = line_byte_len(&self.document.snapshot().buffer, self.cursor.line);
        if self.cursor.byte >= line_len {
            return;
        }
        let r = ProtoRange::new(
            self.cursor,
            Position::new(self.cursor.line, self.cursor.byte + 1),
        );
        let original = match self.document.snapshot().buffer.slice(r) {
            Ok(s) => s,
            Err(_) => return,
        };
        let toggled: String = original
            .as_bytes()
            .iter()
            .map(|&b| match b {
                b'a'..=b'z' => (b - 32) as char,
                b'A'..=b'Z' => (b + 32) as char,
                other => other as char,
            })
            .collect();
        if let Ok(applied) = self.apply_edit_blocking(Edit::replace(r, &toggled)) {
            self.cursor = applied.inserted_range.end;
        }
    }
}
