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

use super::{App, EchoLevel};
use crate::help::HelpBuffer;

impl App {
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
