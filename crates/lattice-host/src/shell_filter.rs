//! `:{range}!cmd` — filtering lines through a shell command.
//!
//! The lines are the command's stdin; its stdout replaces them. vim blocks
//! the editor while the command runs. Here it runs off-thread, because a
//! command the user typed can take as long as it likes and nothing may block
//! the UI (paramount goal #4): the request starts it and returns, and the
//! lines are replaced when the result lands.
//!
//! That gap is the one thing to be careful about. The result describes the
//! lines as they were when the command started, so it is applied only if
//! the buffer has not changed since — otherwise it would overwrite whatever
//! was typed meanwhile, at line numbers that may no longer mean the same
//! text. A filter that loses that race is dropped, and says so.

use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range};

use crate::action::EchoLevel;
use crate::editor::Editor;

/// What a finished filter run hands back to the editor.
pub struct FilterOutcome {
    /// The buffer the lines came from, and its text version at launch.
    buffer: lattice_core::BufferId,
    version: u64,
    /// The inclusive 0-based lines that were piped in.
    first: u32,
    last: u32,
    command: String,
    /// The command's stdout, or the one-line reason there is none.
    result: Result<String, String>,
}

impl Editor {
    /// Start `command` over `lines` (`None` is the whole buffer).
    pub fn do_filter_lines(&mut self, lines: Option<(u32, u32)>, command: String) {
        // One at a time. vim blocks the editor for the length of a filter,
        // so there is never a second; here the first is still out, and
        // starting another would leave two results racing to describe the
        // same lines. Refused out loud rather than queued or dropped.
        self.drain_pending_filter();
        if self.pending_filter_rx.is_some() {
            self.set_message(
                EchoLevel::Error,
                format!("!{command}: another filter is still running"),
            );
            return;
        }
        let snapshot = self.document.snapshot();
        let buffer = &snapshot.buffer;
        let end = buffer.content_line_count().saturating_sub(1);
        let (first, last) = match lines {
            Some((first, last)) => (first.min(end), last.min(end)),
            None => (0, end),
        };
        let mut input = String::new();
        for line in first..=last {
            input.push_str(&buffer.line(line).unwrap_or_default());
            input.push('\n');
        }
        let buffer_id = self.active_buffer_id();
        let version = self.document.text_version();
        // The buffer's own directory when it has one (a file's project, a
        // magit view's repository); otherwise the editor's.
        let cwd = self
            .buffer_scope_dir(buffer_id)
            .or_else(|| {
                self.document
                    .path()
                    .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            })
            .filter(|d| d.is_dir());

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<FilterOutcome>();
        self.pending_filter_rx = Some(rx);
        // The wake. Without it the result sits until the next keypress
        // (boot-composition.md §3).
        let async_landed = self.async_landed.clone();
        self.set_message(EchoLevel::Info, format!("!{command} …"));
        lattice_runtime::runtime::spawn_blocking_on_lsp_runtime(move || {
            let result = lattice_format::run_shell(
                &command,
                &input,
                cwd.as_deref(),
                lattice_format::FILTER_TIMEOUT,
            )
            .map_err(|e| e.message());
            let _ = tx.send(FilterOutcome {
                buffer: buffer_id,
                version,
                first,
                last,
                command,
                result,
            });
            async_landed.notify_one();
        });
    }

    /// Apply a filter's result once it lands. Returns whether one was
    /// consumed, so a test can tell "not yet" from "arrived and refused".
    pub fn drain_pending_filter(&mut self) -> bool {
        let Some(mut rx) = self.pending_filter_rx.take() else {
            return false;
        };
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                // Still running.
                self.pending_filter_rx = Some(rx);
                return false;
            }
            // Its thread went away without answering. Nothing will ever
            // arrive; forget it, or it blocks every later filter.
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return false,
        };
        let FilterOutcome {
            buffer,
            version,
            first,
            last,
            command,
            result,
        } = outcome;
        let output = match result {
            Ok(output) => output,
            Err(message) => {
                self.set_message(EchoLevel::Error, format!("!{message}"));
                return true;
            }
        };
        if buffer != self.active_buffer_id() || version != self.document.text_version() {
            self.set_message(
                EchoLevel::Error,
                format!("!{command}: the buffer changed while it ran; its output was not applied"),
            );
            return true;
        }

        let snapshot = self.document.snapshot();
        let text = &snapshot.buffer;
        let end = text.content_line_count().saturating_sub(1);
        let replaced = last - first + 1;
        let produced = output.lines().count() as u32;
        // Whole lines out, whole lines in. Below the last line the range
        // takes the trailing newline with it and the output brings its own;
        // at the end of the buffer there is no newline to take, so the
        // output loses one instead — and when there is no output at all,
        // the newline before the range goes, or an empty line is left
        // where lines were deleted.
        let (range, replacement) = if last < end {
            let mut replacement = output;
            if !replacement.is_empty() && !replacement.ends_with('\n') {
                replacement.push('\n');
            }
            (
                Range::new(Position::new(first, 0), Position::new(last + 1, 0)),
                replacement,
            )
        } else {
            let tail = Position::new(last, text.line_byte_len(last));
            if output.is_empty() && first > 0 {
                let before = Position::new(first - 1, text.line_byte_len(first - 1));
                (Range::new(before, tail), String::new())
            } else {
                let trimmed = output.strip_suffix('\n').unwrap_or(&output).to_string();
                (Range::new(Position::new(first, 0), tail), trimmed)
            }
        };
        drop(snapshot);
        match self.apply_edit_blocking(Edit::replace(range, replacement)) {
            Ok(_) => {
                self.cursor = Position::new(first, 0);
                self.clamp_cursor_to_active_buffer();
                self.set_message(
                    EchoLevel::Info,
                    format!(
                        "{replaced} line{} filtered, {produced} line{} out",
                        if replaced == 1 { "" } else { "s" },
                        if produced == 1 { "" } else { "s" },
                    ),
                );
            }
            Err(e) => self.set_message(EchoLevel::Error, format!("!{command}: {e}")),
        }
        true
    }
}
