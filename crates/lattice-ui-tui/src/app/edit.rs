//! Insert / Replace edits, paste, register store + read,
//! ex-command edit bodies (`:d` / `:g` / `:v`), and the
//! block-visual replicator. The cohesive home for any App
//! method that mutates rope state through `apply_edit_*` or
//! that reads / writes the register store.
//!
//! Methods that live here:
//! - `do_join_lines` (`J` / `gJ`).
//! - `do_toggle_case_at_cursor` (`~`).
//! - `do_paste_text`, `do_paste`, `do_paste_blockwise`
//!   (vim's `p` / `P` family + blockwise paste).
//! - `do_enter_append`, `do_enter_block_visual_insert`,
//!   `do_open_line_below`, `do_open_line_above` (Insert-
//!   mode entries that adjust cursor before mode change).
//! - `do_delete_line` (`:d`), `do_global` (`:g` / `:v`).
//! - `do_overwrite_char`, `do_replace_undo_last`,
//!   `do_insert_text`, `do_delete_char_backward` (Insert /
//!   Replace primitives).
//! - `store_yank`, `read_register` (register store
//!   read/write; the register map itself lives on `App`).
//! - `replicate_block_insert` (commit a block-visual
//!   `I` / `A` session as one undo unit).
//!
//! Stays in app.rs (deferred):
//! - `do_repeat_last_change` (`.`) -- entangled with the
//!   dispatch / dot-record path.
//! - `apply_text_edit` (the LSP-edit applier reused by
//!   substitute, formatting, code actions).
//! - `apply_edit_blocking` / `apply_edit_batch_blocking` /
//!   `undo_blocking` / `redo_blocking` -- synchronous
//!   wrappers over `Document` that pair with the App
//!   Action dispatch path; will move with the document-
//!   mutation slice.
//!
//! What does NOT live here: the rope itself (ropey,
//! wrapped by `Document`), the undo tree, the register
//! store -- those are owned by `crate::document` /
//! `crate::registers`.

use lattice_core::buffer::AppliedEdit;
use lattice_grammar::CommandInvocation;
use lattice_grammar::ModalState;
use lattice_grammar::VisualKind;
use lattice_grammar::YankKind;
use lattice_grammar::register::Register;
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range as ProtoRange};
use lattice_runtime::{RuntimeError, block_on};

use super::{
    App, EchoLevel, PendingBlockInsert, ReplaceEntry, UnnamedRegister, last_addressable_line,
    line_byte_len, previous_position,
};

impl App {
    // ---- Blocking bridges to the document actor ----
    //
    // Per DESIGN.md §5.2.1 every mutating call returns a
    // `Pending<T>`. The TUI input loop runs on a blocking thread
    // (crossterm's poll model) so it forwards each Pending to
    // [`lattice_runtime::block_on`]. These helpers concentrate the
    // bridging in one place; the rest of `App` reads as if it
    // owned `Document` directly.
    //
    // Returns are pre-flattened: callers that only care about
    // success use `.ok()`; callers that need to inspect the error
    // can match on `RuntimeError::Core(_)` for invalid edits vs.
    // `Busy` / `ActorGone` for actor-protocol failures.

    /// Block_on `apply_edit` and return the `AppliedEdit` (or
    /// `RuntimeError`). Snapshot republishes inside the actor
    /// before this returns. On success, publishes a
    /// [`Event::DocumentChanged`] to the App's event bus and
    /// records the edit with the LSP supervisor (Phase
    /// 4.1.i.2) so attached servers see `didChange`.
    ///
    /// Oil-buffer routing: when `active_buffer == Oil` the edit
    /// lands on `oil.content` (the in-memory rope owned by the
    /// `OilBuffer`) instead of the document actor's rope. The
    /// document actor is the wrong destination for oil edits --
    /// oil's content is intentionally separate so `:w` can diff
    /// against a snapshot and translate into filesystem
    /// operations. LSP `didChange` is intentionally not fired
    /// for oil edits (oil isn't an LSP-tracked buffer).
    pub(super) fn apply_edit_blocking(&mut self, edit: Edit) -> Result<AppliedEdit, RuntimeError> {
        if matches!(self.editor.active_buffer, super::BufferKind::Oil) {
            return self.apply_edit_to_oil(edit);
        }
        let result = block_on(self.editor.document.apply_edit(edit));
        if let Ok(applied) = result.as_ref() {
            self.publish_document_changed(std::slice::from_ref(applied));
        }
        result
    }

    /// Block_on `apply_edit_batch`. The batch lands as one undo
    /// unit on the document's undo stack. Each edit in the
    /// batch is also fed to the LSP supervisor in order
    /// (Phase 4.1.i.2).
    ///
    /// Oil-buffer routing matches `apply_edit_blocking`: when
    /// `active_buffer == Oil` the batch lands on `oil.content`
    /// edit-by-edit. The "one undo unit" semantics are weaker
    /// for oil (its content has no undo stack); v1 oil falls
    /// back to `:e!` reload for "undo all my changes."
    pub(super) fn apply_edit_batch_blocking(
        &mut self,
        edits: Vec<Edit>,
    ) -> Result<Vec<AppliedEdit>, RuntimeError> {
        if matches!(self.editor.active_buffer, super::BufferKind::Oil) {
            let mut applied = Vec::with_capacity(edits.len());
            for edit in edits {
                applied.push(self.apply_edit_to_oil(edit)?);
            }
            return Ok(applied);
        }
        let result = block_on(self.editor.document.apply_edit_batch(edits));
        if let Ok(applied) = result.as_ref() {
            self.publish_document_changed(applied);
        }
        result
    }

    /// Apply a single `Edit` to the active oil buffer's rope
    /// (`oil.content`). Returns the `AppliedEdit` with the
    /// inserted-range / removed-text fields populated, same
    /// shape as the document path. Used by
    /// `apply_edit_blocking` and `apply_edit_batch_blocking`'s
    /// oil routing.
    fn apply_edit_to_oil(&mut self, edit: Edit) -> Result<AppliedEdit, RuntimeError> {
        let oil_id = self.active_pane_buffer_id();
        // Use the callback variant so the registry lock is held
        // only for the apply_edit call. The closure runs the
        // mutation; the outer Option unwraps to either the
        // inner Result or the "no oil entry" Cancelled error.
        self.editor.buffers
            .with_oil_mut(oil_id, |oil| oil.content.apply_edit(&edit))
            .ok_or(RuntimeError::Core(lattice_core::CoreError::Cancelled))?
            .map_err(RuntimeError::Core)
    }

    pub(super) fn undo_blocking(&mut self) -> Result<Vec<AppliedEdit>, RuntimeError> {
        let result = block_on(self.editor.document.undo());
        if let Ok(applied) = result.as_ref() {
            self.publish_document_changed(applied);
        }
        result
    }

    pub(super) fn redo_blocking(&mut self) -> Result<Vec<AppliedEdit>, RuntimeError> {
        let result = block_on(self.editor.document.redo());
        if let Ok(applied) = result.as_ref() {
            self.publish_document_changed(applied);
        }
        result
    }

    /// Vim's `J` / `gJ`: join the current line with the next. With
    /// `with_space = true` (J), the joining newline becomes one space
    /// (and any leading whitespace on the next line is trimmed). With
    /// `with_space = false` (gJ), no replacement -- pure concat.
    pub(super) fn do_join_lines(&mut self, with_space: bool) {
        let last = last_addressable_line(&self.editor.document.snapshot().buffer);
        if self.editor.cursor.line >= last {
            return;
        }
        let line = self.editor.cursor.line;
        let next_line = line + 1;
        let cur_len = line_byte_len(&self.editor.document.snapshot().buffer, line);
        // Compute how many leading whitespace bytes to trim from the
        // next line's content (only for J, not gJ).
        let trim = if with_space {
            let text = self.editor.document.text();
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
            self.editor.cursor = applied.original_range.start;
        }
    }

    /// Vim's `~`: toggle the case of the char at cursor and advance.
    /// Non-letter chars are unchanged; cursor still advances. At EOL
    /// the cursor stops (no wrap).
    pub(super) fn do_toggle_case_at_cursor(&mut self) {
        let line_len = line_byte_len(&self.editor.document.snapshot().buffer, self.editor.cursor.line);
        if self.editor.cursor.byte >= line_len {
            return;
        }
        let r = ProtoRange::new(
            self.editor.cursor,
            Position::new(self.editor.cursor.line, self.editor.cursor.byte + 1),
        );
        let original = match self.editor.document.snapshot().buffer.slice(r) {
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
            self.editor.cursor = applied.inserted_range.end;
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
        match self.editor.modal {
            ModalState::Command => {
                self.editor.command_line.push_str(text);
                self.editor.command_history_cursor = None;
            }
            ModalState::Search(_) => {
                if let Some(line) = self.editor.search_line.as_mut() {
                    line.pattern.push_str(text);
                }
            }
            // Insert / Replace / Normal / Visual / OperatorPending all
            // land at the cursor as a single edit. We deliberately don't
            // transition modes -- the user's mode is preserved across
            // the paste, matching Vim's `paste` option behaviour.
            _ => {
                if let Ok(applied) = self.apply_edit_blocking(Edit::insert(self.editor.cursor, text)) {
                    self.editor.cursor = applied.inserted_range.end;
                    if matches!(self.editor.modal, ModalState::Insert)
                        && let Some(rec) = self.editor.recording_insert.as_mut()
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
        let chosen = self.editor.pending_register.take();
        let Some(reg) = self.read_register(chosen) else {
            self.set_message(EchoLevel::Error, "register empty".to_string());
            return;
        };
        match reg.kind {
            YankKind::Charwise => {
                // `p` inserts after the cursor's byte; `P` at the cursor.
                let line_len = line_byte_len(&self.editor.document.snapshot().buffer, self.editor.cursor.line);
                let insert_at = if before {
                    self.editor.cursor
                } else if self.editor.cursor.byte < line_len {
                    Position::new(self.editor.cursor.line, self.editor.cursor.byte + 1)
                } else {
                    self.editor.cursor
                };
                if let Ok(applied) = self.apply_edit_blocking(Edit::insert(insert_at, &reg.content))
                {
                    // Vim leaves the cursor on the last char of the pasted text.
                    let end = applied.inserted_range.end;
                    self.editor.cursor = if end.byte > 0 {
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
                    Position::new(self.editor.cursor.line, 0)
                } else {
                    let len = line_byte_len(&self.editor.document.snapshot().buffer, self.editor.cursor.line);
                    // Insert at end of current line then a newline -- but
                    // vim's `p` puts the line BELOW. So insert at start of
                    // the next line. If we're on the last line and there's
                    // no trailing newline, insert "\n<payload-without-tail>".
                    if self.editor.cursor.line + 1 < self.editor.document.snapshot().buffer.line_count() {
                        Position::new(self.editor.cursor.line + 1, 0)
                    } else {
                        // Append at EOL of last line; payload starts with \n
                        // implicit in being on a "new" line.
                        let _ = self.apply_edit_blocking(Edit::insert(
                            Position::new(self.editor.cursor.line, len),
                            "\n",
                        ));
                        Position::new(self.editor.cursor.line + 1, 0)
                    }
                };
                if let Ok(applied) = self.apply_edit_blocking(Edit::insert(insert_at, &payload)) {
                    // Cursor lands at the start of the pasted block.
                    self.editor.cursor = applied.inserted_range.start;
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
        let start_line = self.editor.cursor.line;
        let line_len = line_byte_len(&self.editor.document.snapshot().buffer, start_line);
        let start_col = if before {
            self.editor.cursor.byte
        } else if self.editor.cursor.byte < line_len {
            self.editor.cursor.byte + 1
        } else {
            self.editor.cursor.byte
        };

        for (i, row) in rows.iter().enumerate() {
            let target_line = start_line + i as u32;
            let total_lines = self.editor.document.snapshot().buffer.line_count();
            if target_line >= total_lines {
                // Need a new line at the bottom of the buffer. Append
                // a newline at the end of the current last line.
                let last = total_lines.saturating_sub(1);
                let last_len = line_byte_len(&self.editor.document.snapshot().buffer, last);
                let _ = self.apply_edit_blocking(Edit::insert(Position::new(last, last_len), "\n"));
            }
            let target_len = line_byte_len(&self.editor.document.snapshot().buffer, target_line);
            let insert_col = start_col.min(target_len);
            let pos = Position::new(target_line, insert_col);
            // Pad with spaces if the target line is shorter than the
            // start column (vim's behaviour: don't extend the rectangle
            // to the left). With `target_len <= start_col`, append at
            // end-of-line instead.
            let _ = self.apply_edit_blocking(Edit::insert(pos, *row));
        }
        self.editor.cursor = Position::new(start_line, start_col);
    }

    /// Vim's `a` -- step the cursor one byte forward (clamped to
    /// EOL) and switch to Insert.
    pub(super) fn do_enter_append(&mut self) {
        let len = line_byte_len(&self.editor.document.snapshot().buffer, self.editor.cursor.line);
        if self.editor.cursor.byte < len {
            self.editor.cursor.byte += 1;
        }
        self.editor.modal = ModalState::Insert;
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
        if !matches!(self.editor.modal, ModalState::Visual(VisualKind::Blockwise)) {
            return;
        }
        let sels = self.editor.document.selections();
        let sel = sels.primary();
        let start_line = sel.anchor.line.min(sel.head.line);
        let end_line = sel.anchor.line.max(sel.head.line);
        let left_col = sel.anchor.byte.min(sel.head.byte);
        let right_col = sel.anchor.byte.max(sel.head.byte);
        let insert_col = if append { right_col + 1 } else { left_col };

        self.editor.pending_block_insert = Some(PendingBlockInsert {
            start_line,
            end_line,
            insert_col,
            live_edits: 0,
        });

        // Move cursor to the top row's insert column. If the line
        // is shorter than insert_col (e.g. `A` on a short line),
        // clamp -- the user's edits land at end-of-line and the
        // replay handles short lines per-row.
        let line_len = line_byte_len(&self.editor.document.snapshot().buffer, start_line);
        let cursor_col = insert_col.min(line_len);
        self.editor.cursor = Position::new(start_line, cursor_col);

        // Drop visual mode and enter Insert. enter_mode handles
        // recording_insert so the typed prefix is captured.
        self.editor.visual_anchor = None;
        self.enter_mode(ModalState::Insert);
    }

    /// Vim's `o` -- open a new line below the cursor, splice a
    /// newline at end-of-line, drop the cursor on the new empty
    /// line, switch to Insert.
    ///
    /// Reads line length from `active_text()` so the path works
    /// uniformly across Document / Oil / etc. -- without that,
    /// `o` in an oil buffer reads the wrong (document) rope's
    /// line length and inserts mid-row.
    pub(super) fn do_open_line_below(&mut self) {
        let buf = self.active_text();
        let len = line_byte_len(&buf, self.editor.cursor.line);
        let eol = Position::new(self.editor.cursor.line, len);
        if self.apply_edit_blocking(Edit::insert(eol, "\n")).is_ok() {
            self.editor.cursor = Position::new(self.editor.cursor.line + 1, 0);
        }
        self.editor.modal = ModalState::Insert;
    }

    /// Vim's `O` -- open a new line above the cursor; mirror of
    /// `do_open_line_below` but inserts at start-of-line and
    /// keeps the cursor on the inserted (now upper) row.
    pub(super) fn do_open_line_above(&mut self) {
        let bol = Position::new(self.editor.cursor.line, 0);
        if self.apply_edit_blocking(Edit::insert(bol, "\n")).is_ok() {
            self.editor.cursor = bol;
        }
        self.editor.modal = ModalState::Insert;
    }

    /// Delete the cursor's whole line including its trailing newline
    /// (vim's `:d`). The standard delete operator's CurrentLine range
    /// preserves the newline, which leaves an empty line behind -- that's
    /// fine for `dd` (cursor stays put on a now-empty line) but wrong
    /// for `:d` and `:g/.../d`. Here we explicitly include the newline.
    pub(super) fn do_delete_line(&mut self) {
        let line = self.editor.cursor.line;
        let last = last_addressable_line(&self.editor.document.snapshot().buffer);
        let len = line_byte_len(&self.editor.document.snapshot().buffer, line);
        let r = if line < last {
            // Include the trailing newline by extending into the next line.
            ProtoRange::new(Position::new(line, 0), Position::new(line + 1, 0))
        } else if line > 0 {
            // Last line: include the previous line's newline by reaching
            // back to the end of `line - 1`.
            let prev = line - 1;
            let prev_len = line_byte_len(&self.editor.document.snapshot().buffer, prev);
            ProtoRange::new(Position::new(prev, prev_len), Position::new(line, len))
        } else {
            // Single-line buffer: just delete the content.
            ProtoRange::new(Position::new(line, 0), Position::new(line, len))
        };
        if self.apply_edit_blocking(Edit::delete(r)).is_ok() {
            self.editor.cursor = Position::new(
                line.min(last_addressable_line(&self.editor.document.snapshot().buffer)),
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
        let last = last_addressable_line(&self.editor.document.snapshot().buffer);
        // Build the list of target line numbers from the current snapshot
        // (so subsequent edits don't shift our intent).
        let mut targets = Vec::new();
        {
            let text = self.editor.document.text();
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
            self.editor.cursor = Position::new(line, 0);
            match self.dispatch_blocking(body.clone()) {
                Ok(eff) => self.apply_effect(eff),
                Err(e) => {
                    self.set_message(EchoLevel::Error, format!("g: {e}"));
                    return;
                }
            }
        }
    }

    /// Overstrike one char at the cursor: if the cursor is mid-line,
    /// replace `[cursor, cursor+1)` with `c`; if past EOL, just insert
    /// (vim's R extends the line). Either way the cursor advances by
    /// one byte. The original byte (or `None` if past EOL) is pushed
    /// onto `replace_history` so backspace can restore it.
    pub(super) fn do_overwrite_char(&mut self, c: char) {
        let len = line_byte_len(&self.editor.document.snapshot().buffer, self.editor.cursor.line);
        let s = c.to_string();
        let entry_pos = self.editor.cursor;
        if self.editor.cursor.byte < len {
            let r = ProtoRange::new(
                self.editor.cursor,
                Position::new(self.editor.cursor.line, self.editor.cursor.byte + 1),
            );
            // Capture the original byte before the replace lands.
            let original = self.editor.document.snapshot().buffer.slice(r).ok();
            if let Ok(applied) = self.apply_edit_blocking(Edit::replace(r, &s)) {
                self.editor.cursor = applied.inserted_range.end;
                self.editor.replace_history.push(ReplaceEntry {
                    at: entry_pos,
                    original,
                });
            }
        } else {
            // Past end of line: extend. Original is None.
            if let Ok(applied) = self.apply_edit_blocking(Edit::insert(self.editor.cursor, &s)) {
                self.editor.cursor = applied.inserted_range.end;
                self.editor.replace_history.push(ReplaceEntry {
                    at: entry_pos,
                    original: None,
                });
            }
        }
    }

    /// Pop the latest replace_history entry and restore. If the entry
    /// recorded an original byte, replace the byte at the entry's
    /// position with it. If it didn't (line-extension case), delete
    /// the byte. Either way the cursor moves back to the entry's
    /// position.
    pub(super) fn do_replace_undo_last(&mut self) {
        let Some(entry) = self.editor.replace_history.pop() else {
            return;
        };
        let after = Position::new(entry.at.line, entry.at.byte + 1);
        let r = ProtoRange::new(entry.at, after);
        match entry.original {
            Some(orig) => {
                let _ = self.apply_edit_blocking(Edit::replace(r, &orig));
            }
            None => {
                let _ = self.apply_edit_blocking(Edit::delete(r));
            }
        }
        self.editor.cursor = entry.at;
    }

    /// Splice `s` at the cursor as the canonical Insert-mode insertion
    /// path: applies the edit, advances the cursor, captures the text
    /// for dot-repeat (when an Insert recording is in flight), bumps
    /// the block-visual `I` / `A` live-edit counter, refilters any
    /// open completion popup, and fires the LSP signature-help /
    /// on-type-formatting trigger autopilot when a single-char insert
    /// matches a server-advertised trigger char.
    pub(super) fn do_insert_text(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        if let Ok(applied) = self.apply_edit_blocking(Edit::insert(self.editor.cursor, s)) {
            self.editor.cursor = applied.inserted_range.end;
            // Capture into the in-flight Insert recording for dot-repeat.
            if let Some(rec) = self.editor.recording_insert.as_mut() {
                rec.push_str(s);
            }
            // Block-visual I/A: count this edit so the Esc handler
            // can rewind the whole session and re-emit it as a
            // single batched undo unit.
            if let Some(spec) = self.editor.pending_block_insert.as_mut() {
                spec.live_edits = spec.live_edits.saturating_add(1);
            }
            // Insert-mode completion live-refresh (Phase 4.2.g.1).
            // While the popup is open, every keystroke either
            // refilters the candidate set against the new query
            // or dismisses the popup (if the user moved past the
            // word boundary).
            // State-content read (not a gate read): we're inside
            // an `apply` body; `sync_keymap_overlays` runs at the
            // tail, so the mode-active state isn't yet in sync
            // with the popup state field. The refresh check
            // genuinely wants "is there popup state to refresh,"
            // which is exactly what the field tracks.
            if self.editor.insert_completion.is_some() {
                self.maybe_refresh_insert_completion_after_edit();
            }
            // SignatureHelp trigger autopilot (Phase 4.3). When
            // the user types a server-advertised trigger char in
            // Insert mode, fire `textDocument/signatureHelp`
            // automatically. Common triggers: `(` (call site),
            // `,` (next argument). Skipped silently when no
            // attached server advertises any triggers, and when
            // the inserted text is multi-character (paste, snippet
            // expansion -- those land via different paths).
            if matches!(self.editor.modal, ModalState::Insert) && s.chars().count() == 1 {
                let inserted_char = s.chars().next().unwrap_or('\0');
                if self.signature_help_trigger_chars().contains(&inserted_char) {
                    self.do_lsp_signature_help_request();
                }
                // OnTypeFormatting trigger autopilot (Phase
                // 4.3). C-family servers commonly advertise
                // `;` / `}` / `\n`; the server returns small
                // text edits adjusting the surrounding
                // indentation. Skipped when no server
                // advertises any triggers.
                if self
                    .on_type_formatting_trigger_chars()
                    .contains(&inserted_char)
                {
                    self.do_lsp_on_type_formatting_request(inserted_char);
                }
            }
        }
    }

    /// Vim's `<BS>` in Insert / Replace -- delete the byte before the
    /// cursor (Unicode-aware step via previous_position). No-op at the
    /// start of the buffer. Bumps the block-visual `I` / `A`
    /// live-edit counter so the Esc replay accounts for the deletion.
    pub(super) fn do_delete_char_backward(&mut self) {
        let prev = previous_position(&self.editor.document.snapshot().buffer, self.editor.cursor);
        if prev == self.editor.cursor {
            return;
        }
        let range = ProtoRange::new(prev, self.editor.cursor);
        if self.apply_edit_blocking(Edit::delete(range)).is_ok() {
            self.editor.cursor = prev;
            if let Some(spec) = self.editor.pending_block_insert.as_mut() {
                spec.live_edits = spec.live_edits.saturating_add(1);
            }
        }
    }

    // 5.5.E.3: `store_yank` moved to
    // [`lattice_host::dispatch::Editor::store_yank`] alongside the
    // [`Effect::Yank`] arm. Renderer-side call sites (the oil-buffer
    // narrow re-implementation of `apply_effect`) now invoke
    // `self.editor.store_yank(...)` directly.

    /// Read the register slot for paste / inspection. Falls back to
    /// `unnamed_register`.
    pub(super) fn read_register(&self, register: Option<Register>) -> Option<UnnamedRegister> {
        match register {
            None | Some(Register::Unnamed) => self.editor.unnamed_register.clone(),
            Some(Register::BlackHole) => None,
            Some(r) => self
                .editor.registers
                .get(&r)
                .cloned()
                .or_else(|| self.editor.unnamed_register.clone()),
        }
    }

    /// Commit a block-visual `I` / `A` session as a single undo unit.
    ///
    /// Vim's behavior: the typed prefix on the top row plus the
    /// replicated text on the other rows land as one atomic
    /// change. To honour that without restructuring Insert mode
    /// to defer edits, we:
    ///
    /// 1. Roll back the live-typed edits via `undo_blocking` --
    ///    `spec.live_edits` counts how many `apply_edit` calls
    ///    happened on the top row during the Insert session.
    /// 2. Build a batch: top-row insert at `insert_col` plus an
    ///    insert at the same column on every line in
    ///    `start_line+1..=end_line` whose length is at least
    ///    `insert_col` (lines too short to hold the column are
    ///    skipped, matching vim's behavior).
    /// 3. Apply the batch via `apply_edit_batch_blocking` so the
    ///    whole session is one undo / redo unit.
    pub(super) fn replicate_block_insert(&mut self, spec: PendingBlockInsert, text: &str) {
        // Rewind the live-typed edits. Each call decrements the
        // top-row state by one; after `live_edits` calls the
        // buffer is back to the pre-Insert state and we can
        // build the batched edit list against it.
        for _ in 0..spec.live_edits {
            let _ = self.undo_blocking();
        }

        let buffer = self.editor.document.snapshot().buffer.clone();
        let mut edits = Vec::with_capacity((spec.end_line - spec.start_line + 1) as usize);

        // Top row first. Note: we don't skip the top row even if
        // its length is below insert_col (the user did type there
        // live, so the buffer already has at least one valid
        // insertion point at the line-end position they reached).
        let top_len = line_byte_len(&buffer, spec.start_line);
        let top_col = spec.insert_col.min(top_len);
        edits.push(Edit::insert(Position::new(spec.start_line, top_col), text));

        for line in (spec.start_line + 1)..=spec.end_line {
            let line_len = line_byte_len(&buffer, line);
            if line_len < spec.insert_col {
                continue;
            }
            edits.push(Edit::insert(Position::new(line, spec.insert_col), text));
        }

        let _ = self.apply_edit_batch_blocking(edits);
        // Cursor settles on the start of the inserted prefix on
        // the top row -- vim's behavior. The previous cursor pos
        // (one past the typed text on top row) is no longer
        // accurate after the rewind.
        self.editor.cursor = Position::new(spec.start_line, top_col);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use crate::app::test_helpers::{
        app_in_command_mode, app_with, attach_test_syntax, invoke_motion,
    };
    use crate::app::*;
    use lattice_protocol::edit::Edit;
    use lattice_protocol::selection::VisualMode;

    #[test]
    fn undo_redo_accumulate_inverse_deltas() {
        // Forward edit + undo + redo each push a delta. The
        // undo's delta is the inverse of the forward (start_byte
        // unchanged; old_end / new_end swapped).
        let mut a = app_with("a", 5);
        attach_test_syntax(&mut a, lattice_syntax::Lang::Rust);
        a.apply_edit_blocking(Edit::insert(Position::new(0, 1), "b"))
            .unwrap();
        assert_eq!(a.editor.pending_syntax_edits.len(), 1);
        let forward = a.editor.pending_syntax_edits[0];
        a.undo_blocking().unwrap();
        assert_eq!(a.editor.pending_syntax_edits.len(), 2);
        let undo_delta = a.editor.pending_syntax_edits[1];
        // Undo's old_end/new_end are swapped relative to
        // forward.
        assert_eq!(undo_delta.start_byte, forward.start_byte);
        assert_eq!(undo_delta.old_end_byte, forward.new_end_byte);
        assert_eq!(undo_delta.new_end_byte, forward.old_end_byte);
        a.redo_blocking().unwrap();
        assert_eq!(a.editor.pending_syntax_edits.len(), 3);
    }

    #[test]
    fn insert_mode_inserts_text_and_advances_cursor() {
        let mut a = app_with("", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("h".into()));
        a.apply(Action::Insert("i".into()));
        assert_eq!(a.editor.document.text(), "hi");
        assert_eq!(a.editor.cursor, Position::new(0, 2));
    }

    #[test]
    fn insert_then_normal_pulls_cursor_back_one() {
        let mut a = app_with("", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("hi".into()));
        assert_eq!(a.editor.cursor, Position::new(0, 2));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert_eq!(a.editor.cursor, Position::new(0, 1));
    }

    #[test]
    fn backspace_deletes_char_before_cursor_in_insert() {
        let mut a = app_with("hi", 10);
        a.editor.cursor.byte = 2;
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::DeleteCharBackward);
        assert_eq!(a.editor.document.text(), "h");
        assert_eq!(a.editor.cursor, Position::new(0, 1));
    }

    #[test]
    fn backspace_at_origin_is_a_no_op() {
        let mut a = app_with("hi", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::DeleteCharBackward);
        assert_eq!(a.editor.document.text(), "hi");
        assert_eq!(a.editor.cursor, Position::ZERO);
    }

    #[test]
    fn backspace_across_line_boundary_joins_lines() {
        let mut a = app_with("a\nb", 10);
        a.editor.cursor = Position::new(1, 0);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::DeleteCharBackward);
        assert_eq!(a.editor.document.text(), "ab");
        assert_eq!(a.editor.cursor, Position::new(0, 1));
    }

    #[test]
    fn enter_append_advances_cursor_one_byte_then_inserts() {
        let mut a = app_with("ab", 10);
        a.apply(Action::EnterAppend);
        assert_eq!(a.editor.modal, ModalState::Insert);
        assert_eq!(a.editor.cursor, Position::new(0, 1));
    }

    #[test]
    fn open_line_below_creates_new_line_and_drops_cursor_to_it() {
        let mut a = app_with("first", 10);
        a.apply(Action::OpenLineBelow);
        assert_eq!(a.editor.modal, ModalState::Insert);
        assert_eq!(a.editor.document.text(), "first\n");
        assert_eq!(a.editor.cursor, Position::new(1, 0));
    }

    #[test]
    fn open_line_above_creates_new_line_above() {
        let mut a = app_with("second", 10);
        a.apply(Action::OpenLineAbove);
        assert_eq!(a.editor.modal, ModalState::Insert);
        assert_eq!(a.editor.document.text(), "\nsecond");
        assert_eq!(a.editor.cursor, Position::new(0, 0));
    }

    #[test]
    fn delete_with_word_forward_target_dw_in_app() {
        let mut a = app_with("hello world", 10);
        let inv = CommandInvocation::of(a.editor.builtins.delete.0).with_target(
            lattice_grammar::Target::Motion(a.editor.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        assert_eq!(a.editor.document.text(), "world");
        assert_eq!(a.editor.cursor, Position::ZERO);
    }

    #[test]
    fn delete_char_under_cursor_x_in_app() {
        let mut a = app_with("abc", 10);
        let inv = CommandInvocation::of(a.editor.builtins.delete.0).with_target(
            lattice_grammar::Target::Motion(a.editor.builtins.char_right, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        assert_eq!(a.editor.document.text(), "bc");
        assert_eq!(a.editor.cursor, Position::ZERO);
    }

    #[test]
    fn undo_after_insert_restores_buffer() {
        let mut a = app_with("", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("hi".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert_eq!(a.editor.document.text(), "hi");
        a.apply(Action::Undo);
        assert_eq!(a.editor.document.text(), "");
    }

    #[test]
    fn redo_replays_undone_edit() {
        let mut a = app_with("", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("hi".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        a.apply(Action::Undo);
        a.apply(Action::Redo);
        assert_eq!(a.editor.document.text(), "hi");
    }

    #[test]
    fn cw_deletes_word_and_enters_insert_mode() {
        let mut a = app_with("hello world", 10);
        let inv = CommandInvocation::of(a.editor.builtins.change.0).with_target(
            lattice_grammar::Target::Motion(a.editor.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        assert_eq!(a.editor.document.text(), "world");
        assert_eq!(a.editor.modal, ModalState::Insert);
        assert_eq!(a.editor.cursor, Position::ZERO);
    }

    #[test]
    fn cc_clears_current_line_and_enters_insert_mode() {
        let mut a = app_with("aaa\nBBB\nccc", 10);
        a.editor.cursor = Position::new(1, 0);
        let inv = CommandInvocation::of(a.editor.builtins.change.0)
            .with_range(lattice_grammar::Range::CurrentLine);
        a.apply(Action::Invoke(inv));
        assert_eq!(a.editor.document.text(), "aaa\n\nccc");
        assert_eq!(a.editor.modal, ModalState::Insert);
    }

    #[test]
    fn join_lines_with_space_combines_two_lines_with_one_space() {
        let mut a = app_with("hello\nworld", 10);
        a.apply(Action::JoinLines { with_space: true });
        assert_eq!(a.editor.document.text(), "hello world");
        // Cursor lands at the join point (end of original first line).
        assert_eq!(a.editor.cursor, Position::new(0, 5));
    }

    #[test]
    fn join_lines_without_space_concatenates_directly() {
        let mut a = app_with("hello\nworld", 10);
        a.apply(Action::JoinLines { with_space: false });
        assert_eq!(a.editor.document.text(), "helloworld");
    }

    #[test]
    fn join_lines_trims_leading_whitespace_on_next_line() {
        let mut a = app_with("hello\n   world", 10);
        a.apply(Action::JoinLines { with_space: true });
        assert_eq!(a.editor.document.text(), "hello world");
    }

    #[test]
    fn join_lines_at_last_line_is_no_op() {
        let mut a = app_with("only", 10);
        a.apply(Action::JoinLines { with_space: true });
        assert_eq!(a.editor.document.text(), "only");
    }

    #[test]
    fn paste_from_named_register_uses_named_content() {
        let mut a = app_with("hello", 10);
        // Manually populate "a with custom content.
        a.editor.registers.insert(
            Register::Named('a'),
            UnnamedRegister {
                content: "X".into(),
                kind: YankKind::Charwise,
            },
        );
        a.apply(Action::SelectRegister(Register::Named('a')));
        a.apply(Action::PasteAfter);
        assert_eq!(a.editor.document.text(), "hXello");
    }

    #[test]
    fn delete_into_black_hole_does_not_overwrite_unnamed() {
        let mut a = app_with("hello world", 10);
        // First yank into unnamed.
        let yank = CommandInvocation::of(a.editor.builtins.yank.0).with_target(
            lattice_grammar::Target::Motion(a.editor.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(yank));
        let pre_delete_unnamed = a.editor.unnamed_register.as_ref().unwrap().content.clone();
        // Now delete into black hole; unnamed should be untouched.
        a.apply(Action::SelectRegister(Register::BlackHole));
        let inv = CommandInvocation::of(a.editor.builtins.delete.0).with_target(
            lattice_grammar::Target::Motion(a.editor.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        assert_eq!(
            a.editor.unnamed_register.as_ref().unwrap().content,
            pre_delete_unnamed
        );
    }

    #[test]
    fn delete_does_not_populate_zero_register() {
        let mut a = app_with("hello world", 10);
        let inv = CommandInvocation::of(a.editor.builtins.delete.0).with_target(
            lattice_grammar::Target::Motion(a.editor.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        // Delete populates unnamed but NOT "0.
        assert!(!a.editor.registers.contains_key(&Register::Numbered(0)));
        assert!(a.editor.unnamed_register.is_some());
    }

    #[test]
    fn paste_from_unset_named_register_falls_back_to_unnamed() {
        let mut a = app_with("hello", 10);
        a.editor.unnamed_register = Some(UnnamedRegister {
            content: "X".into(),
            kind: YankKind::Charwise,
        });
        a.apply(Action::SelectRegister(Register::Named('z')));
        a.apply(Action::PasteAfter);
        // 'z' is empty -> fall back to unnamed.
        assert_eq!(a.editor.document.text(), "hXello");
    }

    #[test]
    fn toggle_case_at_cursor_inverts_letter_and_advances() {
        let mut a = app_with("hello", 10);
        a.apply(Action::ToggleCaseAtCursor);
        assert_eq!(a.editor.document.text(), "Hello");
        assert_eq!(a.editor.cursor, Position::new(0, 1));
    }

    #[test]
    fn toggle_case_advances_through_non_letters() {
        let mut a = app_with("a 1 b", 10);
        a.apply(Action::ToggleCaseAtCursor);
        assert_eq!(a.editor.document.text(), "A 1 b");
        a.apply(Action::ToggleCaseAtCursor);
        // Space at byte 1 -> unchanged but cursor advances.
        assert_eq!(a.editor.document.text(), "A 1 b");
        assert_eq!(a.editor.cursor, Position::new(0, 2));
    }

    #[test]
    fn toggle_case_at_eol_is_no_op() {
        let mut a = app_with("hi", 10);
        a.editor.cursor = Position::new(0, 2);
        a.apply(Action::ToggleCaseAtCursor);
        assert_eq!(a.editor.document.text(), "hi");
        assert_eq!(a.editor.cursor, Position::new(0, 2));
    }

    #[test]
    fn overwrite_char_replaces_byte_at_cursor() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterMode(ModalState::Replace));
        a.apply(Action::OverwriteChar('H'));
        assert_eq!(a.editor.document.text(), "Hello");
        assert_eq!(a.editor.cursor, Position::new(0, 1));
    }

    #[test]
    fn overwrite_chain_replaces_consecutively() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterMode(ModalState::Replace));
        for c in "WORL".chars() {
            a.apply(Action::OverwriteChar(c));
        }
        assert_eq!(a.editor.document.text(), "WORLo");
        assert_eq!(a.editor.cursor, Position::new(0, 4));
    }

    #[test]
    fn overwrite_at_eol_extends_line() {
        let mut a = app_with("hi", 10);
        a.editor.cursor = Position::new(0, 2);
        a.apply(Action::EnterMode(ModalState::Replace));
        a.apply(Action::OverwriteChar('!'));
        assert_eq!(a.editor.document.text(), "hi!");
        assert_eq!(a.editor.cursor, Position::new(0, 3));
    }

    #[test]
    fn replace_undo_last_restores_overwritten_char() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterMode(ModalState::Replace));
        a.apply(Action::OverwriteChar('H'));
        assert_eq!(a.editor.document.text(), "Hello");
        assert_eq!(a.editor.cursor, Position::new(0, 1));
        // Backspace: should restore 'h' and step cursor back.
        a.apply(Action::ReplaceUndoLast);
        assert_eq!(a.editor.document.text(), "hello");
        assert_eq!(a.editor.cursor, Position::ZERO);
    }

    #[test]
    fn replace_undo_after_eol_extension_deletes_extension() {
        let mut a = app_with("hi", 10);
        a.editor.cursor = Position::new(0, 2);
        a.apply(Action::EnterMode(ModalState::Replace));
        a.apply(Action::OverwriteChar('!'));
        assert_eq!(a.editor.document.text(), "hi!");
        a.apply(Action::ReplaceUndoLast);
        assert_eq!(a.editor.document.text(), "hi");
        assert_eq!(a.editor.cursor, Position::new(0, 2));
    }

    #[test]
    fn replace_undo_with_empty_history_is_no_op() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterMode(ModalState::Replace));
        a.apply(Action::ReplaceUndoLast);
        assert_eq!(a.editor.document.text(), "hello");
        assert_eq!(a.editor.cursor, Position::ZERO);
    }

    #[test]
    fn replace_undo_chain_restores_in_reverse_order() {
        let mut a = app_with("abcde", 10);
        a.apply(Action::EnterMode(ModalState::Replace));
        a.apply(Action::OverwriteChar('A'));
        a.apply(Action::OverwriteChar('B'));
        a.apply(Action::OverwriteChar('C'));
        assert_eq!(a.editor.document.text(), "ABCde");
        a.apply(Action::ReplaceUndoLast);
        assert_eq!(a.editor.document.text(), "ABcde");
        a.apply(Action::ReplaceUndoLast);
        assert_eq!(a.editor.document.text(), "Abcde");
        a.apply(Action::ReplaceUndoLast);
        assert_eq!(a.editor.document.text(), "abcde");
    }

    #[test]
    fn delete_records_last_change_and_dot_replays_it() {
        let mut a = app_with("foo bar foo bar", 10);
        let inv = CommandInvocation::of(a.editor.builtins.delete.0).with_target(
            lattice_grammar::Target::Motion(a.editor.builtins.word_forward, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        // After dw: "bar foo bar".
        assert_eq!(a.editor.document.text(), "bar foo bar");
        assert!(a.editor.last_change.is_some());
        // `.` replays the same dw at the new cursor position.
        a.apply(Action::RepeatLastChange);
        assert_eq!(a.editor.document.text(), "foo bar");
    }

    #[test]
    fn insert_session_captures_typed_text_into_last_insert() {
        let mut a = app_with("hello", 10);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::Insert("X".into()));
        a.apply(Action::Insert("Y".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert_eq!(a.editor.last_insert.as_deref(), Some("XY"));
    }

    #[test]
    fn paste_after_charwise_inserts_after_cursor() {
        let mut a = app_with("hello", 10);
        a.editor.unnamed_register = Some(UnnamedRegister {
            content: "X".into(),
            kind: YankKind::Charwise,
        });
        a.editor.cursor = Position::new(0, 0); // on 'h'
        a.apply(Action::PasteAfter);
        assert_eq!(a.editor.document.text(), "hXello");
        // Cursor lands on the last char of the pasted text (still 'X').
        assert_eq!(a.editor.cursor, Position::new(0, 1));
    }

    #[test]
    fn paste_before_charwise_inserts_at_cursor() {
        let mut a = app_with("hello", 10);
        a.editor.unnamed_register = Some(UnnamedRegister {
            content: "X".into(),
            kind: YankKind::Charwise,
        });
        a.editor.cursor = Position::new(0, 2); // on 'l'
        a.apply(Action::PasteBefore);
        assert_eq!(a.editor.document.text(), "heXllo");
        assert_eq!(a.editor.cursor, Position::new(0, 2));
    }

    #[test]
    fn paste_after_linewise_inserts_below_current_line() {
        let mut a = app_with("aaa\nBBB\nccc", 10);
        a.editor.unnamed_register = Some(UnnamedRegister {
            content: "XXX\n".into(),
            kind: YankKind::Linewise,
        });
        a.editor.cursor = Position::new(1, 0); // on 'B' line
        a.apply(Action::PasteAfter);
        assert_eq!(a.editor.document.text(), "aaa\nBBB\nXXX\nccc");
        assert_eq!(a.editor.cursor, Position::new(2, 0));
    }

    #[test]
    fn paste_before_linewise_inserts_above_current_line() {
        let mut a = app_with("aaa\nBBB\nccc", 10);
        a.editor.unnamed_register = Some(UnnamedRegister {
            content: "XXX\n".into(),
            kind: YankKind::Linewise,
        });
        a.editor.cursor = Position::new(1, 0);
        a.apply(Action::PasteBefore);
        assert_eq!(a.editor.document.text(), "aaa\nXXX\nBBB\nccc");
        assert_eq!(a.editor.cursor, Position::new(1, 0));
    }

    #[test]
    fn paste_with_empty_register_emits_error_message() {
        let mut a = app_with("hello", 10);
        assert!(a.editor.unnamed_register.is_none());
        a.apply(Action::PasteAfter);
        let msg = a.editor.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        assert_eq!(a.editor.document.text(), "hello");
    }

    #[test]
    fn paste_text_in_normal_inserts_at_cursor_one_undo_unit() {
        let mut a = app_with("hello", 10);
        a.editor.cursor = Position::new(0, 5);
        a.apply(Action::PasteText(" world".into()));
        assert_eq!(a.editor.document.text(), "hello world");
        assert_eq!(a.editor.cursor, Position::new(0, 11));
        // One bracketed-paste = one undo unit.
        a.apply(Action::Undo);
        assert_eq!(a.editor.document.text(), "hello");
    }

    #[test]
    fn paste_text_in_insert_inserts_and_records_for_dot_repeat() {
        let mut a = app_with("a", 10);
        a.editor.cursor = Position::new(0, 1);
        a.apply(Action::EnterMode(ModalState::Insert));
        a.apply(Action::PasteText("bcd".into()));
        assert_eq!(a.editor.document.text(), "abcd");
        assert_eq!(a.editor.cursor, Position::new(0, 4));
        assert!(matches!(a.editor.modal, ModalState::Insert));
        // Dot-repeat insert recording captured the pasted text.
        let rec = a.editor.recording_insert.as_ref().unwrap();
        assert_eq!(rec, "bcd");
    }

    #[test]
    fn paste_text_in_command_appends_to_command_line() {
        let mut a = app_with("xx", 10);
        a.apply(Action::EnterMode(ModalState::Command));
        a.editor.command_line = "w ".into();
        a.apply(Action::PasteText("foo.rs".into()));
        assert_eq!(a.editor.command_line, "w foo.rs");
        // Document untouched.
        assert_eq!(a.editor.document.text(), "xx");
    }

    #[test]
    fn paste_text_in_search_appends_to_search_pattern() {
        let mut a = app_with("xx", 10);
        a.apply(Action::EnterSearch(
            lattice_grammar::SearchDirection::Forward,
        ));
        a.apply(Action::SearchAppend('a'));
        a.apply(Action::PasteText("bcd".into()));
        let line = a.editor.search_line.as_ref().unwrap();
        assert_eq!(line.pattern, "abcd");
    }

    #[test]
    fn paste_text_empty_is_a_noop() {
        let mut a = app_with("hello", 10);
        let before = a.editor.document.text();
        a.apply(Action::PasteText(String::new()));
        assert_eq!(a.editor.document.text(), before);
    }

    #[test]
    fn paste_text_with_newlines_lands_as_single_edit() {
        let mut a = app_with("a", 10);
        a.editor.cursor = Position::new(0, 1);
        a.apply(Action::PasteText("\nb\nc".into()));
        assert_eq!(a.editor.document.text(), "a\nb\nc");
        assert_eq!(a.editor.cursor, Position::new(2, 1));
    }

    fn enter_block_visual(text: &str, anchor: Position, head: Position) -> App {
        let mut a = app_with(text, 10);
        a.editor.cursor = anchor;
        a.apply(Action::EnterVisual(VisualKind::Blockwise));
        a.editor.cursor = head;
        a.editor.visual_anchor = Some(anchor);
        let sel = Selection {
            anchor,
            head,
            visual: Some(VisualMode::Blockwise),
        };
        a.editor.set_selections_blocking(SelectionSet::single(sel));
        a
    }

    #[test]
    fn block_delete_removes_each_rows_column_slice() {
        // Three rows, columns 1..=2 deleted from each.
        // Initial:    "abcd\n1234\nWXYZ"
        // After d :   "ad\n14\nWZ"
        let mut a =
            enter_block_visual("abcd\n1234\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        let inv = CommandInvocation::of(a.editor.builtins.delete.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        assert_eq!(a.editor.document.text(), "ad\n14\nWZ");
    }

    #[test]
    fn block_delete_lands_cursor_at_top_left_of_block() {
        // Vim's behavior: after a rectangle delete, the cursor sits
        // at the block's top-left column, not at column 0.
        let mut a =
            enter_block_visual("abcd\n1234\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        let inv = CommandInvocation::of(a.editor.builtins.delete.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        // Top-left of block was (0, 1); after the delete column 1
        // is the new content's start on the top row.
        assert_eq!(a.editor.cursor, Position::new(0, 1));
    }

    #[test]
    fn block_delete_lands_as_single_undo_unit() {
        // The whole rectangle delete must collapse into one undo
        // entry -- the dispatcher coalesces the per-row AppliedEdits
        // by snapshotting pre/post and replaying as one Edit::replace.
        let mut a =
            enter_block_visual("abcd\n1234\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        let inv = CommandInvocation::of(a.editor.builtins.delete.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        assert_eq!(a.editor.document.text(), "ad\n14\nWZ");
        let _ = a.undo_blocking();
        assert_eq!(a.editor.document.text(), "abcd\n1234\nWXYZ");
    }

    #[test]
    fn block_change_lands_as_single_undo_unit() {
        // Block-visual `c` deletes each row's column slice and enters
        // Insert. The deletion piece must be one undo unit; future
        // typed text would be batched separately by the I/A path.
        let mut a =
            enter_block_visual("abcd\n1234\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        let inv = CommandInvocation::of(a.editor.builtins.change.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        assert_eq!(a.editor.document.text(), "ad\n14\nWZ");
        assert!(matches!(a.editor.modal, ModalState::Insert));
        // Exit Insert without typing anything to isolate the deletion.
        a.apply(Action::EnterMode(ModalState::Normal));
        let _ = a.undo_blocking();
        assert_eq!(a.editor.document.text(), "abcd\n1234\nWXYZ");
    }

    #[test]
    fn block_yank_stores_blockwise_content_in_unnamed_register() {
        // Yank a 3x2 rectangle: cols 1..=2 across three rows of "abcd\n1234\nWXYZ".
        let mut a =
            enter_block_visual("abcd\n1234\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        let inv =
            CommandInvocation::of(a.editor.builtins.yank.0).with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        // Document untouched.
        assert_eq!(a.editor.document.text(), "abcd\n1234\nWXYZ");
        // Unnamed register has the 3 column slices joined by newline,
        // tagged Blockwise.
        let reg = a.editor.unnamed_register.as_ref().expect("yank stored");
        assert_eq!(reg.content, "bc\n23\nXY");
        assert_eq!(reg.kind, YankKind::Blockwise);
    }

    #[test]
    fn block_yank_clamps_short_rows_to_intersection() {
        // Middle row "12" partially overlaps the rectangle: cols 1..=2,
        // line len 2, intersection is `[1, 2)` = "2".
        let mut a = enter_block_visual("abcd\n12\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        let inv =
            CommandInvocation::of(a.editor.builtins.yank.0).with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        let reg = a.editor.unnamed_register.as_ref().unwrap();
        assert_eq!(reg.content, "bc\n2\nXY");
        assert_eq!(reg.kind, YankKind::Blockwise);
    }

    #[test]
    fn block_yank_with_row_entirely_left_of_rectangle_yields_empty_slice() {
        // Middle row is "" (empty). Visual cols 1..=2 fully outside;
        // intersection is empty.
        let mut a = enter_block_visual("abcd\n\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        let inv =
            CommandInvocation::of(a.editor.builtins.yank.0).with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        let reg = a.editor.unnamed_register.as_ref().unwrap();
        assert_eq!(reg.content, "bc\n\nXY");
        assert_eq!(reg.kind, YankKind::Blockwise);
    }

    #[test]
    fn block_visual_indent_right_indents_each_row_in_block() {
        // Indent operates on lines covered by the block. The
        // insertion goes at column 0 of each line (vim's behavior),
        // not at the block's left column. Whole change must be one
        // undo unit (operator opts out of per-row blockwise dispatch
        // via blockwise_per_row=false; the indent operator's
        // apply_edit_batch makes the multi-line indent atomic).
        let mut a = enter_block_visual("abc\n123\nWXY", Position::new(0, 1), Position::new(2, 1));
        let inv = CommandInvocation::of(a.editor.builtins.indent_right.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        assert_eq!(a.editor.document.text(), "    abc\n    123\n    WXY");
        let _ = a.undo_blocking();
        assert_eq!(a.editor.document.text(), "abc\n123\nWXY");
    }

    #[test]
    fn block_visual_capital_i_via_real_motions_not_explicit_selection() {
        // Reproduces the path the actual user takes: Ctrl-V to enter
        // blockwise, motions to extend the selection, capital I.
        // No manual set_selections_blocking -- selections must be
        // maintained by the SelectionChange effect from motions.
        let mut a = app_with("abcd\n1234\nWXYZ", 10);
        a.editor.cursor = Position::new(0, 1);
        a.apply(Action::EnterVisual(VisualKind::Blockwise));
        // Move down 2 rows + right 1 column via motions.
        a.apply(invoke_motion(a.editor.builtins.line_down));
        a.apply(invoke_motion(a.editor.builtins.line_down));
        a.apply(invoke_motion(a.editor.builtins.char_right));
        // Cursor should now be at (2, 2). visual_anchor was (0, 1).
        assert_eq!(a.editor.cursor, Position::new(2, 2));
        assert_eq!(a.editor.visual_anchor, Some(Position::new(0, 1)));

        a.apply(Action::EnterBlockVisualInsert);
        assert!(matches!(a.editor.modal, ModalState::Insert));
        // I should land at column 1 (block's left col) on the top row.
        assert_eq!(a.editor.cursor, Position::new(0, 1));
        a.apply(Action::Insert("X".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert_eq!(a.editor.document.text(), "aXbcd\n1X234\nWXXYZ");
    }

    #[test]
    fn block_visual_capital_i_inserts_at_block_left_column_on_each_row() {
        // 3 rows, block at column 1. `I` enters Insert at (top_row, 1).
        // Type "X", Esc -> "X" lands at column 1 on every row.
        let mut a =
            enter_block_visual("abcd\n1234\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        a.apply(Action::EnterBlockVisualInsert);
        assert!(matches!(a.editor.modal, ModalState::Insert));
        assert_eq!(a.editor.cursor, Position::new(0, 1));
        a.apply(Action::Insert("X".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert_eq!(a.editor.document.text(), "aXbcd\n1X234\nWXXYZ");
    }

    #[test]
    fn block_visual_capital_a_appends_after_block_right_column() {
        // Block cols 1..=2 across 3 rows; `A` lands at col 3 on each row.
        let mut a =
            enter_block_visual("abcd\n1234\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        a.apply(Action::EnterBlockVisualAppend);
        assert!(matches!(a.editor.modal, ModalState::Insert));
        assert_eq!(a.editor.cursor, Position::new(0, 3));
        a.apply(Action::Insert("@".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert_eq!(a.editor.document.text(), "abc@d\n123@4\nWXY@Z");
    }

    #[test]
    fn block_visual_capital_i_lands_as_single_undo_unit() {
        // Type 3 chars during the I session, replicate to 2 other rows,
        // then `u` once -- the buffer should fully revert. Without the
        // batched-commit fix, undo would only roll back the last char
        // on one row.
        let mut a =
            enter_block_visual("abcd\n1234\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        a.apply(Action::EnterBlockVisualInsert);
        a.apply(Action::Insert("X".into()));
        a.apply(Action::Insert("Y".into()));
        a.apply(Action::Insert("Z".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert_eq!(a.editor.document.text(), "aXYZbcd\n1XYZ234\nWXYZXYZ");

        // One undo should restore the original buffer.
        let _ = a.undo_blocking();
        assert_eq!(a.editor.document.text(), "abcd\n1234\nWXYZ");
    }

    #[test]
    fn block_visual_capital_a_lands_as_single_undo_unit() {
        let mut a =
            enter_block_visual("abcd\n1234\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        a.apply(Action::EnterBlockVisualAppend);
        a.apply(Action::Insert("@".into()));
        a.apply(Action::Insert("@".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        assert_eq!(a.editor.document.text(), "abc@@d\n123@@4\nWXY@@Z");
        let _ = a.undo_blocking();
        assert_eq!(a.editor.document.text(), "abcd\n1234\nWXYZ");
    }

    #[test]
    fn block_visual_capital_i_skips_lines_shorter_than_insert_col() {
        // Middle row "12" is too short for col 3 (insert_col). Vim skips it.
        let mut a = enter_block_visual("abcd\n12\nWXYZ", Position::new(0, 3), Position::new(2, 3));
        a.apply(Action::EnterBlockVisualInsert);
        a.apply(Action::Insert("Q".into()));
        a.apply(Action::EnterMode(ModalState::Normal));
        // Top row gets the live edit; bottom row replays at col 3;
        // middle row is too short and is left untouched.
        assert_eq!(a.editor.document.text(), "abcQd\n12\nWXYQZ");
    }

    #[test]
    fn block_visual_indent_left_dedents_each_row_in_block() {
        let mut a = enter_block_visual(
            "    abc\n    123\n    WXY",
            Position::new(0, 0),
            Position::new(2, 0),
        );
        let inv = CommandInvocation::of(a.editor.builtins.indent_left.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        assert_eq!(a.editor.document.text(), "abc\n123\nWXY");
    }

    #[test]
    fn block_change_deletes_rectangle_and_enters_insert() {
        let mut a =
            enter_block_visual("abcd\n1234\nWXYZ", Position::new(0, 1), Position::new(2, 2));
        let inv = CommandInvocation::of(a.editor.builtins.change.0)
            .with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(inv));
        assert_eq!(a.editor.document.text(), "ad\n14\nWZ");
        assert!(matches!(a.editor.modal, ModalState::Insert));
    }

    #[test]
    fn block_paste_after_replays_rectangle_on_consecutive_lines() {
        // Yank a 2x2 rectangle from the top, paste it at column 0 of
        // line 2. Each row of the yanked content lands on a successive
        // line at the paste column.
        let mut a = enter_block_visual(
            "abcd\n1234\nWXYZ\n----",
            Position::new(0, 1),
            Position::new(1, 2),
        );
        let yank =
            CommandInvocation::of(a.editor.builtins.yank.0).with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(yank));
        // Exit visual and move to a fresh paste site.
        a.apply(Action::ExitVisual);
        a.editor.cursor = Position::new(2, 0);
        // `p` (after-cursor) -> insert at col 1 on line 2 and line 3.
        a.apply(Action::PasteAfter);
        // Line 2: "WXYZ" -> "WbcXYZ"; Line 3: "----" -> "-23---"
        assert_eq!(a.editor.document.text(), "abcd\n1234\nWbcXYZ\n-23---");
    }

    #[test]
    fn backspace_after_popup_open_live_refilters() {
        let mut a = app_in_command_mode("describ");
        a.apply(Action::CommandLineCompleteOrAdvance);
        let narrow_count = a.editor.completion_state.as_ref().unwrap().candidates.len();
        a.apply(Action::CommandLineBackspace);
        assert!(a.editor.completion_state.is_some());
        assert_eq!(a.editor.command_line, "descri");
        // Shorter prefix -> at least as many candidates.
        let widened = a.editor.completion_state.as_ref().unwrap().candidates.len();
        assert!(widened >= narrow_count);
    }

    #[test]
    fn delete_word_backward_with_open_popup_refilters() {
        let mut a = app_in_command_mode("describ");
        a.apply(Action::CommandLineCompleteOrAdvance);
        a.apply(Action::CommandLineDeleteWordBackward);
        // Word-delete leaves us with an empty cmdline -> Empty slot
        // -> all commands; popup stays open.
        assert!(a.editor.completion_state.is_some());
        assert_eq!(a.editor.command_line, "");
    }

    #[test]
    fn delete_chord_pops_one_full_token() {
        let mut a = app_in_command_mode("describe-key <C-c>");
        a.apply(Action::CommandLineDeleteChord);
        // The whole `<C-c>` token (5 bytes) gets removed in one
        // delete -- not a single byte.
        assert_eq!(a.editor.command_line, "describe-key ");
    }

    #[test]
    fn delete_chord_on_plain_char_pops_one_char() {
        let mut a = app_in_command_mode("describe-key gg");
        a.apply(Action::CommandLineDeleteChord);
        assert_eq!(a.editor.command_line, "describe-key g");
    }

    #[test]
    fn delete_chord_on_empty_cmdline_exits_command_mode() {
        let mut a = app_with("xx", 10);
        a.editor.modal = ModalState::Command;
        a.editor.command_line = String::new();
        a.apply(Action::CommandLineDeleteChord);
        assert!(matches!(a.editor.modal, ModalState::Normal));
    }

    #[test]
    fn insert_completion_trigger_outside_insert_is_noop() {
        let mut a = app_with("foo bar baz", 10);
        // Normal mode by default -- trigger should no-op.
        a.do_completion_trigger();
        assert!(a.editor.insert_completion.is_none());
    }

    #[test]
    fn insert_completion_trigger_with_no_matches_echoes_no_completions() {
        let mut a = app_with("hello world hello\nfoo bar baz qux", 10);
        a.editor.modal = ModalState::Insert;
        // Cursor at end of `hello` on line 0 -- prefix "hello".
        // BufferWordsSource skips the cursor's own word, and
        // none of the remaining buffer words fuzzy-match
        // "hello", so the popup auto-closes with the
        // "no completions" echo.
        a.editor.cursor = Position::new(0, 5);
        a.do_completion_trigger();
        assert!(a.editor.insert_completion.is_none());
        let msg = a.editor.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no completions"));
    }

    #[test]
    fn insert_completion_open_with_matching_query_keeps_popup() {
        let mut a = app_with("hello world helper helmet hi", 10);
        a.editor.modal = ModalState::Insert;
        // Cursor right after `hel` -- prefix "hel". Buffer words:
        // "hello", "world", "helper", "helmet", "hi".
        a.editor.cursor = Position::new(0, 3);
        // Place hel at the cursor: rewrite content via cursor
        // positioning (the buffer already has "hel" as part of
        // hello). For the test, just place cursor on a different
        // line.
        let _ = a.apply_edit_blocking(Edit::insert(Position::new(0, 28), "\nhel"));
        a.editor.cursor = Position::new(1, 3);
        a.do_completion_trigger();
        let state = a.editor.insert_completion.as_ref().expect("popup opened");
        assert_eq!(state.query, "hel");
        // hello / helper / helmet all start with "hel" -- prefix
        // tier (score 800) matches. Order may vary by stable
        // sort over insertion order.
        let labels: Vec<String> = state.rendered.iter().map(|c| c.raw.text.clone()).collect();
        assert!(labels.contains(&"hello".to_string()));
        assert!(labels.contains(&"helper".to_string()));
        assert!(labels.contains(&"helmet".to_string()));
        // "hi" doesn't fuzzy-match "hel", "world" doesn't either.
        assert!(!labels.contains(&"hi".to_string()));
        assert!(!labels.contains(&"world".to_string()));
    }

    #[test]
    fn insert_completion_next_prev_navigates_with_wrap() {
        let mut a = app_with("alpha alphabet alligator", 10);
        a.editor.modal = ModalState::Insert;
        let _ = a.apply_edit_blocking(Edit::insert(Position::new(0, 24), "\nal"));
        a.editor.cursor = Position::new(1, 2);
        a.do_completion_trigger();
        let total = a.editor.insert_completion.as_ref().expect("popup").rendered.len();
        assert!(total >= 2, "need ≥ 2 candidates for wrap test");
        assert_eq!(a.editor.insert_completion.as_ref().unwrap().selected, 0);
        a.do_completion_next();
        assert_eq!(a.editor.insert_completion.as_ref().unwrap().selected, 1);
        // Wrap to last via prev from 1 -> 0 -> total-1.
        a.do_completion_prev();
        a.do_completion_prev();
        assert_eq!(a.editor.insert_completion.as_ref().unwrap().selected, total - 1);
    }

    #[test]
    fn insert_completion_accept_replaces_prefix_and_closes() {
        let mut a = app_with("alphabet alligator\nal", 10);
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(1, 2);
        a.do_completion_trigger();
        // Pick the first candidate.
        let first_text = a.editor
            .insert_completion
            .as_ref()
            .and_then(|s| s.selected_candidate())
            .map(|c| c.raw.text.clone())
            .expect("at least one candidate");
        a.do_completion_accept();
        assert!(a.editor.insert_completion.is_none());
        // Buffer line 1 should now be the chosen word.
        let snap = a.editor.document.snapshot();
        let line1 = snap.buffer.line(1).unwrap_or_default();
        assert_eq!(line1.trim_end(), first_text);
    }

    #[test]
    fn insert_completion_cancel_drops_popup() {
        let mut a = app_with("alpha alphabet\nal", 10);
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(1, 2);
        a.do_completion_trigger();
        assert!(a.editor.insert_completion.is_some());
        a.do_completion_cancel();
        assert!(a.editor.insert_completion.is_none());
        // Modal stays Insert.
        assert!(matches!(a.editor.modal, ModalState::Insert));
    }

    #[test]
    fn insert_completion_cancel_and_exit_insert_drops_popup_and_exits() {
        let mut a = app_with("alpha alphabet\nal", 10);
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(1, 2);
        a.do_completion_trigger();
        assert!(a.editor.insert_completion.is_some());
        a.apply(Action::CompletionCancelAndExitInsert);
        assert!(a.editor.insert_completion.is_none());
        assert!(matches!(a.editor.modal, ModalState::Normal));
    }

    #[test]
    fn insert_completion_toggle_docs_flips_state() {
        let mut a = app_with("alpha alphabet\nal", 10);
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(1, 2);
        a.do_completion_trigger();
        assert!(
            a.editor.insert_completion
                .as_ref()
                .map(|s| s.doc_popup.is_none())
                .unwrap_or(false)
        );
        a.do_completion_toggle_docs();
        assert!(a.editor.insert_completion.as_ref().unwrap().doc_popup.is_some());
        a.do_completion_toggle_docs();
        assert!(a.editor.insert_completion.as_ref().unwrap().doc_popup.is_none());
    }

    #[test]
    fn insert_completion_refilters_on_keystroke() {
        let mut a = app_with("alpha alphabet alligator\nal", 10);
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(1, 2);
        a.do_completion_trigger();
        let pre_count = a.editor.insert_completion.as_ref().expect("popup").rendered.len();
        // Type 'p' -- query becomes "alp"; only "alpha" /
        // "alphabet" survive (alligator drops out).
        a.apply(Action::Insert("p".into()));
        let state = a.editor.insert_completion.as_ref().expect("popup still open");
        assert_eq!(state.query, "alp");
        let labels: Vec<String> = state.rendered.iter().map(|c| c.raw.text.clone()).collect();
        assert!(labels.contains(&"alpha".to_string()));
        assert!(labels.contains(&"alphabet".to_string()));
        assert!(!labels.contains(&"alligator".to_string()));
        assert!(state.rendered.len() < pre_count);
    }

    #[test]
    fn insert_completion_closes_when_query_leaves_word_boundary() {
        let mut a = app_with("alpha alphabet\nal", 10);
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(1, 2);
        a.do_completion_trigger();
        assert!(a.editor.insert_completion.is_some());
        // Type a space -- pushes the cursor past the word.
        a.apply(Action::Insert(" ".into()));
        assert!(a.editor.insert_completion.is_none());
    }

    #[test]
    fn block_paste_extends_buffer_when_below_eof() {
        // Yank 2 rows then paste at the bottom -- the missing row is
        // appended as a fresh line.
        let mut a = enter_block_visual("abcd\n1234", Position::new(0, 1), Position::new(1, 2));
        let yank =
            CommandInvocation::of(a.editor.builtins.yank.0).with_range(lattice_grammar::Range::Selection);
        a.apply(Action::Invoke(yank));
        a.apply(Action::ExitVisual);
        // Move to last line and paste with `P` (before-cursor) at col 0.
        a.editor.cursor = Position::new(1, 0);
        a.apply(Action::PasteBefore);
        // Line 1 becomes "bc1234"; new line 2 holds "23".
        assert_eq!(a.editor.document.text(), "abcd\nbc1234\n23");
    }

    #[test]
    fn delete_then_paste_after_emulates_xp_swap() {
        // Vim trick: cursor on 'a' of "abc"; `xp` swaps 'a' and 'b' -> "bac".
        let mut a = app_with("abc", 10);
        a.editor.cursor = Position::ZERO;
        // x: delete char-right
        let inv = CommandInvocation::of(a.editor.builtins.delete.0).with_target(
            lattice_grammar::Target::Motion(a.editor.builtins.char_right, lattice_grammar::Args::None),
        );
        a.apply(Action::Invoke(inv));
        assert_eq!(a.editor.document.text(), "bc");
        // p: paste after cursor (cursor at 0 on 'b'; paste after -> "bac").
        a.apply(Action::PasteAfter);
        assert_eq!(a.editor.document.text(), "bac");
    }
}
