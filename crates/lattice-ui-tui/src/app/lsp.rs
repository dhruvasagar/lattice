//! LSP feature surface -- App methods for the various
//! `:lsp-*` ex commands (admin / log / trace / status /
//! restart) plus the request-driven LSP feature methods
//! (hover, definition, references, completion, etc.).
//!
//! Methods that live here:
//! - LSP admin / log / trace ex-commands:
//!   - do_open_lsp_log (`:lsp-log [server]`),
//!   - do_open_lsp_trace_log (`:lsp-trace-log [server]`),
//!   - do_toggle_lsp_trace (`:lsp-trace <name>`),
//!   - do_lsp_status (`:lsp-status`),
//!   - do_lsp_server_log_listing (`:lsp-server-log`),
//!   - do_lsp_restart (`:lsp-restart <server>`),
//!   - do_set_lsp_log_level
//!     (`:lsp-log-level [server] <level>`),
//!   - do_lsp_log_clear (`:lsp-log-clear [server]`).
//!
//! Stays in app.rs (deferred to follow-up LSP slices):
//! - LSP request handlers: do_lsp_hover_request,
//!   do_lsp_nav_request, do_lsp_references_request,
//!   do_lsp_signature_help_request,
//!   do_lsp_completion_request,
//!   do_lsp_insert_completion_request,
//!   do_lsp_document_symbol_request,
//!   do_lsp_workspace_symbol_request, do_lsp_format,
//!   do_lsp_format_range, do_lsp_rename_request,
//!   do_lsp_code_action_request.
//! - Event-bus drains and apply-edit handlers.
//! - LSP completion meta + completion-result helpers.
//! - apply_persistent_lsp_editor_options (lifecycle path).
//! - resolve_server_id / running_server_ids (already
//!   pub(super); used by both lsp.rs and picker.rs).
//!
//! What does NOT live here: the LSP wire layer / actor /
//! supervisor (those live in `lattice-lsp`). This module is
//! about App's *consumption* of that layer.

use lattice_protocol::position::Position;

use super::{App, EchoLevel};
use crate::help::HelpBuffer;

impl App {
    /// `:diagnostics` -- open every published diagnostic across
    /// every attached server in a vertico-style picker. Severity
    /// glyph in the marginalia (`[E]` / `[W]` / `[I]` / `[H]`)
    /// and the diagnostic message as the preview text.
    pub fn do_list_diagnostics(&mut self) {
        // `:diagnostics` is a browse-style picker, not a tag-
        // intent drill-down -- clear any stale nav origin so a
        // later JumpToLspLocation accept doesn't push a phantom
        // tag stack entry.
        self.pending_tag_origin = None;
        let snapshot = self.lsp_diagnostics.snapshot();
        if snapshot.is_empty() {
            self.set_message(EchoLevel::Info, "no diagnostics".to_string());
            return;
        }
        let mut rows: Vec<crate::picker::LspLocationRow> = Vec::new();
        for (uri, diags) in snapshot {
            let path = match lattice_lsp::actor::uri_to_path(&uri) {
                Some(p) => p,
                None => continue,
            };
            for d in diags {
                let sev = match d.severity {
                    Some(lattice_lsp::DiagnosticSeverity::ERROR) => "[E]",
                    Some(lattice_lsp::DiagnosticSeverity::WARNING) => "[W]",
                    Some(lattice_lsp::DiagnosticSeverity::INFORMATION) => "[I]",
                    Some(lattice_lsp::DiagnosticSeverity::HINT) => "[H]",
                    _ => "[?]",
                };
                rows.push(crate::picker::LspLocationRow {
                    path: path.clone(),
                    line: d.range.start.line,
                    col: d.range.start.character,
                    preview: crate::help::one_line(&d.message),
                    marginalia: sev.to_string(),
                });
            }
        }
        if rows.is_empty() {
            self.set_message(EchoLevel::Info, "no diagnostics".to_string());
            return;
        }
        let total = rows.len();
        let mut p = crate::picker::Picker::new(
            format!("diagnostics ({total})"),
            crate::picker::PickerSource::LspLocations,
            crate::picker::PickerAction::JumpToLspLocation,
        );
        p.set_lsp_locations(rows);
        self.picker = Some(p);
    }

    /// `]d` / `:diag-next` / `:cnext` -- move the cursor to the
    /// next diagnostic in the active buffer. Wraps to top.
    pub fn do_next_diagnostic(&mut self) {
        let Some(uri) = self.buffer_uris.get(&self.document_buffer_id) else {
            self.set_message(EchoLevel::Error, "no LSP attachment".to_string());
            return;
        };
        let mut diags = self.lsp_diagnostics.diagnostics_for(uri);
        if diags.is_empty() {
            self.set_message(EchoLevel::Info, "no diagnostics in buffer".to_string());
            return;
        }
        diags.sort_by_key(|d| (d.range.start.line, d.range.start.character));
        let cursor = self.cursor;
        let Some(next) = diags
            .iter()
            .find(|d| {
                d.range.start.line > cursor.line
                    || (d.range.start.line == cursor.line
                        && d.range.start.character > cursor.byte)
            })
            .or_else(|| diags.first())
            .map(|d| d.range.start)
        else {
            return;
        };
        self.cursor = Position::new(next.line, next.character);
        self.publish_position_change();
    }

    /// `[d` / `:diag-prev` / `:cprev` -- move the cursor to the
    /// previous diagnostic in the active buffer. Wraps to bottom.
    pub fn do_prev_diagnostic(&mut self) {
        let Some(uri) = self.buffer_uris.get(&self.document_buffer_id) else {
            self.set_message(EchoLevel::Error, "no LSP attachment".to_string());
            return;
        };
        let mut diags = self.lsp_diagnostics.diagnostics_for(uri);
        if diags.is_empty() {
            self.set_message(EchoLevel::Info, "no diagnostics in buffer".to_string());
            return;
        }
        diags.sort_by_key(|d| (d.range.start.line, d.range.start.character));
        let cursor = self.cursor;
        let Some(prev) = diags
            .iter()
            .rev()
            .find(|d| {
                d.range.start.line < cursor.line
                    || (d.range.start.line == cursor.line
                        && d.range.start.character < cursor.byte)
            })
            .or_else(|| diags.last())
            .map(|d| d.range.start)
        else {
            return;
        };
        self.cursor = Position::new(prev.line, prev.character);
        self.publish_position_change();
    }

    /// `:lsp-log [server]` -- open the per-server log buffer
    /// for `server` (or the picker if no arg / multi-match).
    /// Buffer goes through `open_help_in_pane` -- it lives
    /// in `BufferRegistry` and is reachable via `:bn` / `:b N`
    /// / the buffer picker (Phase 1 / Phase 2 wiring).
    pub fn do_open_lsp_log(&mut self, server_id: Option<&str>) {
        self.open_lsp_picker(
            "lsp-log",
            server_id.map(|s| s.to_string()),
            crate::picker::PickerAction::OpenLspLog,
        );
    }

    /// `:lsp-trace-log [server]` -- open the JSON-RPC trace ring
    /// in the active pane. Same dispatch shape as `:lsp-log`:
    /// picker on no-arg or multi-match, direct open on single
    /// match. **Does not toggle tracing** -- pair with
    /// `:lsp-trace <server>` to start / stop the wire trace; this
    /// command only views the records.
    pub fn do_open_lsp_trace_log(&mut self, server_id: Option<&str>) {
        self.open_lsp_picker(
            "lsp-trace-log",
            server_id.map(|s| s.to_string()),
            crate::picker::PickerAction::OpenLspTraceLog,
        );
    }

    /// `:lsp-trace <name>` -- toggle JSON-RPC trace for the
    /// server. Pure toggle: the trace buffer is opened by the
    /// separate `:lsp-trace-log [server]` command so peeking
    /// mid-stream doesn't flip the toggle off.
    pub fn do_toggle_lsp_trace(&mut self, name: &str) {
        let resolved = self.resolve_server_id(name);
        let Some(server_id) = resolved else {
            let running = self.running_server_ids();
            let listing = if running.is_empty() {
                "no LSP servers running".to_string()
            } else {
                format!("running: {}", running.join(", "))
            };
            self.set_message(
                EchoLevel::Error,
                format!("lsp-trace: no server matches {name:?} ({listing})"),
            );
            return;
        };
        let id: std::sync::Arc<str> = std::sync::Arc::from(server_id.as_str());
        let now_on = self.lsp_logger.toggle_trace(id);
        let label = if now_on { "on" } else { "off" };
        let alias_note = if server_id != name {
            format!(" (resolved {name:?} -> {server_id:?})")
        } else {
            String::new()
        };
        self.set_message(
            EchoLevel::Info,
            format!(
                "lsp-trace {server_id}: {label}{alias_note} (use :lsp-trace-log {server_id} to view)"
            ),
        );
    }

    /// `:lsp-status` -- render every running server in a
    /// help-style buffer.
    pub fn do_lsp_status(&mut self) {
        let buffer = HelpBuffer::lsp_status(&self.lsp);
        self.open_help(buffer.with_markdown_syntax(self.lang_registry.clone()));
    }

    /// `:lsp-server-log` -- vertico picker over every running
    /// `(workspace, server_id)` LSP actor. `<CR>` opens the
    /// per-server log (`*lsp:<server>*`) for the chosen row.
    pub fn do_lsp_server_log_listing(&mut self) {
        self.open_lsp_picker(
            "lsp-server-log",
            None,
            crate::picker::PickerAction::OpenLspLog,
        );
    }

    /// `:lsp-restart <server>` -- supervisor restart hook.
    /// Currently emits an info message; full restart-with-
    /// backoff lands in 4.4.
    pub fn do_lsp_restart(&mut self, server_id: &str) {
        self.set_message(
            EchoLevel::Info,
            format!(
                "lsp-restart {}: supervisor restart wiring lands in 4.4",
                server_id
            ),
        );
    }

    /// `:lsp-log-level [server] <level>` -- set the subsystem
    /// default min level (when no server) or a per-server
    /// override.
    pub fn do_set_lsp_log_level(&mut self, server_id: Option<&str>, level: &str) {
        let Some(parsed) = lattice_lsp::LogLevel::parse(level) else {
            self.set_message(
                EchoLevel::Error,
                format!(
                    "unknown log level {level:?}; expected error/warn/info/debug/trace"
                ),
            );
            return;
        };
        match server_id {
            None => {
                self.lsp_logger.set_default_level(parsed);
                self.set_message(
                    EchoLevel::Info,
                    format!("lsp default log level: {level}"),
                );
            }
            Some(id) => {
                let arc: std::sync::Arc<str> = std::sync::Arc::from(id);
                self.lsp_logger.set_server_level(arc, Some(parsed));
                self.set_message(
                    EchoLevel::Info,
                    format!("lsp log level for {id}: {level}"),
                );
            }
        }
    }

    /// `:lsp-log-clear [server]` -- drop ring contents.
    pub fn do_lsp_log_clear(&mut self, server_id: Option<&str>) {
        match server_id {
            None => {
                self.lsp_logger.clear_global();
                self.set_message(EchoLevel::Info, "*lsp* cleared".to_string());
            }
            Some(id) => {
                let arc: std::sync::Arc<str> = std::sync::Arc::from(id);
                self.lsp_logger.clear_server(&arc);
                self.set_message(
                    EchoLevel::Info,
                    format!("*lsp:{id}* cleared"),
                );
            }
        }
    }
}
