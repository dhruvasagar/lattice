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

use lattice_grammar::CommandInvocation;
use lattice_grammar::ModalState;
use lattice_grammar::VisualKind;
use lattice_grammar::YankKind;
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range as ProtoRange};

use super::{App, EchoLevel, PendingBlockInsert, last_addressable_line, line_byte_len};

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

    /// Bracketed-paste handler. Routes the payload to the right target
    /// based on the current modal state -- cursor for editing modes,
    /// command line for `:`, search line for `/` `?`. Always one undo
    /// unit. The terminal already stripped the bracketed-paste markers
    /// before crossterm handed us the string.
    pub(super) fn do_paste_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        match self.modal {
            ModalState::Command => {
                self.command_line.push_str(text);
                self.command_history_cursor = None;
            }
            ModalState::Search(_) => {
                if let Some(line) = self.search_line.as_mut() {
                    line.pattern.push_str(text);
                }
            }
            // Insert / Replace / Normal / Visual / OperatorPending all
            // land at the cursor as a single edit. We deliberately don't
            // transition modes -- the user's mode is preserved across
            // the paste, matching Vim's `paste` option behaviour.
            _ => {
                if let Ok(applied) = self.apply_edit_blocking(Edit::insert(self.cursor, text)) {
                    self.cursor = applied.inserted_range.end;
                    if matches!(self.modal, ModalState::Insert)
                        && let Some(rec) = self.recording_insert.as_mut()
                    {
                        rec.push_str(text);
                    }
                }
            }
        }
    }

    /// Paste from the chosen register (`pending_register` if set, else
    /// the unnamed register). `before = true` for `P` (paste before
    /// cursor / above current line), `false` for `p` (paste after
    /// cursor / below current line). Linewise yanks insert on a new
    /// line; charwise yanks splice at the cursor.
    pub(super) fn do_paste(&mut self, before: bool) {
        let chosen = self.pending_register.take();
        let Some(reg) = self.read_register(chosen) else {
            self.set_message(EchoLevel::Error, "register empty".to_string());
            return;
        };
        match reg.kind {
            YankKind::Charwise => {
                // `p` inserts after the cursor's byte; `P` at the cursor.
                let line_len = line_byte_len(&self.document.snapshot().buffer, self.cursor.line);
                let insert_at = if before {
                    self.cursor
                } else if self.cursor.byte < line_len {
                    Position::new(self.cursor.line, self.cursor.byte + 1)
                } else {
                    self.cursor
                };
                if let Ok(applied) = self.apply_edit_blocking(Edit::insert(insert_at, &reg.content))
                {
                    // Vim leaves the cursor on the last char of the pasted text.
                    let end = applied.inserted_range.end;
                    self.cursor = if end.byte > 0 {
                        Position::new(end.line, end.byte - 1)
                    } else {
                        end
                    };
                }
            }
            YankKind::Linewise => {
                // Linewise content is inserted as a whole new line. We
                // normalise by ensuring exactly one trailing newline on the
                // payload before splicing at the appropriate line boundary.
                let mut payload = reg.content.clone();
                if !payload.ends_with('\n') {
                    payload.push('\n');
                }
                let insert_at = if before {
                    Position::new(self.cursor.line, 0)
                } else {
                    let len = line_byte_len(&self.document.snapshot().buffer, self.cursor.line);
                    // Insert at end of current line then a newline -- but
                    // vim's `p` puts the line BELOW. So insert at start of
                    // the next line. If we're on the last line and there's
                    // no trailing newline, insert "\n<payload-without-tail>".
                    if self.cursor.line + 1 < self.document.snapshot().buffer.line_count() {
                        Position::new(self.cursor.line + 1, 0)
                    } else {
                        // Append at EOL of last line; payload starts with \n
                        // implicit in being on a "new" line.
                        let _ = self.apply_edit_blocking(Edit::insert(
                            Position::new(self.cursor.line, len),
                            "\n",
                        ));
                        Position::new(self.cursor.line + 1, 0)
                    }
                };
                if let Ok(applied) = self.apply_edit_blocking(Edit::insert(insert_at, &payload)) {
                    // Cursor lands at the start of the pasted block.
                    self.cursor = applied.inserted_range.start;
                }
            }
            YankKind::Blockwise => self.do_paste_blockwise(&reg.content, before),
        }
    }

    /// Vim's blockwise paste: each `\n`-separated row is inserted on
    /// consecutive lines starting at the same column. `p` (after)
    /// inserts at `cursor.byte + 1`, `P` (before) at `cursor.byte`.
    /// Rows wider than a target line's existing length are appended
    /// after end-of-line; missing rows below the buffer extend it
    /// with new lines. Cursor lands at the top-left of the pasted
    /// block.
    pub(super) fn do_paste_blockwise(&mut self, content: &str, before: bool) {
        if content.is_empty() {
            return;
        }
        let rows: Vec<&str> = content.split('\n').collect();
        let start_line = self.cursor.line;
        let line_len = line_byte_len(&self.document.snapshot().buffer, start_line);
        let start_col = if before {
            self.cursor.byte
        } else if self.cursor.byte < line_len {
            self.cursor.byte + 1
        } else {
            self.cursor.byte
        };

        for (i, row) in rows.iter().enumerate() {
            let target_line = start_line + i as u32;
            let total_lines = self.document.snapshot().buffer.line_count();
            if target_line >= total_lines {
                // Need a new line at the bottom of the buffer. Append
                // a newline at the end of the current last line.
                let last = total_lines.saturating_sub(1);
                let last_len = line_byte_len(&self.document.snapshot().buffer, last);
                let _ = self.apply_edit_blocking(Edit::insert(Position::new(last, last_len), "\n"));
            }
            let target_len = line_byte_len(&self.document.snapshot().buffer, target_line);
            let insert_col = start_col.min(target_len);
            let pos = Position::new(target_line, insert_col);
            // Pad with spaces if the target line is shorter than the
            // start column (vim's behaviour: don't extend the rectangle
            // to the left). With `target_len <= start_col`, append at
            // end-of-line instead.
            let _ = self.apply_edit_blocking(Edit::insert(pos, *row));
        }
        self.cursor = Position::new(start_line, start_col);
    }

    /// Vim's `a` -- step the cursor one byte forward (clamped to
    /// EOL) and switch to Insert.
    pub(super) fn do_enter_append(&mut self) {
        let len = line_byte_len(&self.document.snapshot().buffer, self.cursor.line);
        if self.cursor.byte < len {
            self.cursor.byte += 1;
        }
        self.modal = ModalState::Insert;
    }

    /// Vim's blockwise-visual `I` (`append=false`) and `A`
    /// (`append=true`). Captures the block extents from the active
    /// selection, parks them in `pending_block_insert`, moves the
    /// cursor to the top-row insert column, and switches to Insert.
    /// The replication onto rows 2..N happens when Insert exits.
    ///
    /// No-op if the modal is not blockwise visual; called only
    /// from translate_visual which guards on the mode.
    pub(super) fn do_enter_block_visual_insert(&mut self, append: bool) {
        if !matches!(self.modal, ModalState::Visual(VisualKind::Blockwise)) {
            return;
        }
        let sels = self.document.selections();
        let sel = sels.primary();
        let start_line = sel.anchor.line.min(sel.head.line);
        let end_line = sel.anchor.line.max(sel.head.line);
        let left_col = sel.anchor.byte.min(sel.head.byte);
        let right_col = sel.anchor.byte.max(sel.head.byte);
        let insert_col = if append { right_col + 1 } else { left_col };

        self.pending_block_insert = Some(PendingBlockInsert {
            start_line,
            end_line,
            insert_col,
            live_edits: 0,
        });

        // Move cursor to the top row's insert column. If the line
        // is shorter than insert_col (e.g. `A` on a short line),
        // clamp -- the user's edits land at end-of-line and the
        // replay handles short lines per-row.
        let line_len = line_byte_len(&self.document.snapshot().buffer, start_line);
        let cursor_col = insert_col.min(line_len);
        self.cursor = Position::new(start_line, cursor_col);

        // Drop visual mode and enter Insert. enter_mode handles
        // recording_insert so the typed prefix is captured.
        self.visual_anchor = None;
        self.enter_mode(ModalState::Insert);
    }

    /// Vim's `o` -- open a new line below the cursor, splice a
    /// newline at end-of-line, drop the cursor on the new empty
    /// line, switch to Insert.
    pub(super) fn do_open_line_below(&mut self) {
        let len = line_byte_len(&self.document.snapshot().buffer, self.cursor.line);
        let eol = Position::new(self.cursor.line, len);
        if self.apply_edit_blocking(Edit::insert(eol, "\n")).is_ok() {
            self.cursor = Position::new(self.cursor.line + 1, 0);
        }
        self.modal = ModalState::Insert;
    }

    /// Vim's `O` -- open a new line above the cursor; mirror of
    /// `do_open_line_below` but inserts at start-of-line and
    /// keeps the cursor on the inserted (now upper) row.
    pub(super) fn do_open_line_above(&mut self) {
        let bol = Position::new(self.cursor.line, 0);
        if self.apply_edit_blocking(Edit::insert(bol, "\n")).is_ok() {
            self.cursor = bol;
        }
        self.modal = ModalState::Insert;
    }

    /// Delete the cursor's whole line including its trailing newline
    /// (vim's `:d`). The standard delete operator's CurrentLine range
    /// preserves the newline, which leaves an empty line behind -- that's
    /// fine for `dd` (cursor stays put on a now-empty line) but wrong
    /// for `:d` and `:g/.../d`. Here we explicitly include the newline.
    pub(super) fn do_delete_line(&mut self) {
        let line = self.cursor.line;
        let last = last_addressable_line(&self.document.snapshot().buffer);
        let len = line_byte_len(&self.document.snapshot().buffer, line);
        let r = if line < last {
            // Include the trailing newline by extending into the next line.
            ProtoRange::new(Position::new(line, 0), Position::new(line + 1, 0))
        } else if line > 0 {
            // Last line: include the previous line's newline by reaching
            // back to the end of `line - 1`.
            let prev = line - 1;
            let prev_len = line_byte_len(&self.document.snapshot().buffer, prev);
            ProtoRange::new(Position::new(prev, prev_len), Position::new(line, len))
        } else {
            // Single-line buffer: just delete the content.
            ProtoRange::new(Position::new(line, 0), Position::new(line, len))
        };
        if self.apply_edit_blocking(Edit::delete(r)).is_ok() {
            self.cursor = Position::new(
                line.min(last_addressable_line(&self.document.snapshot().buffer)),
                0,
            );
        }
    }

    /// Vim's :g / :v -- execute `body` on every line matching (or NOT
    /// matching, when inverted) the literal pattern. Operates bottom-up
    /// so deletions don't shift the upcoming target lines. v1: `body`
    /// is parsed as a single ex-command.
    pub(super) fn do_global(&mut self, pattern: &str, inverted: bool, body: &CommandInvocation) {
        if pattern.is_empty() {
            self.set_message(EchoLevel::Error, "empty pattern".to_string());
            return;
        }
        let last = last_addressable_line(&self.document.snapshot().buffer);
        // Build the list of target line numbers from the current snapshot
        // (so subsequent edits don't shift our intent).
        let mut targets = Vec::new();
        {
            let text = self.document.text();
            for (i, line) in text.split_inclusive('\n').enumerate() {
                if i as u32 > last {
                    break;
                }
                let stripped = line.trim_end_matches('\n');
                let matches = stripped.contains(pattern);
                if matches != inverted {
                    targets.push(i as u32);
                }
            }
        }
        if targets.is_empty() {
            self.set_message(
                EchoLevel::Error,
                format!(
                    "no lines {} pattern: {pattern}",
                    if inverted { "lacking" } else { "matching" }
                ),
            );
            return;
        }
        // Run bottom-up so deletions and edits on later lines don't
        // shift the line numbers we plan to operate on. The body is
        // already parsed -- the cmdline's `:g/pat/body` parser
        // compiled it once at submit time, so we just clone the
        // invocation per match.
        for &line in targets.iter().rev() {
            self.cursor = Position::new(line, 0);
            match self.dispatch_blocking(body.clone()) {
                Ok(eff) => self.apply_effect(eff),
                Err(e) => {
                    self.set_message(EchoLevel::Error, format!("g: {e}"));
                    return;
                }
            }
        }
    }
}
