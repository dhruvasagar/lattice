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

use lattice_grammar::ModalState;

use lattice_protocol::Event;

use super::{
    App, BufferKind, CodeActionOutcome, CodeActionRow, CompletionItemRow, CompletionOutcome,
    EchoLevel, FormatOutcome, HoverOutcome, LSP_COMPLETION_KIND_ID, LspCompletionMeta,
    LspNavKind, ReferencesOutcome, RenameOutcome, SignatureHelpOutcome, SymbolRow,
    SymbolsOutcome, TagStackEntry, app_to_lsp_position, code_action_kind_glyph,
    completion_kind_glyph, definition_response_to_locations,
    flatten_document_symbol_response, flatten_workspace_edit, hover_contents_to_markdown,
    is_word_char_byte, last_addressable_line, line_byte_len, lsp_position_to_app_byte,
    prepare_rename_placeholder, signature_help_to_markdown, symbol_information_to_row,
    word_under_cursor, workspace_symbol_to_row,
};
use crate::buffers::BufferId;
use crate::help::HelpBuffer;
use lattice_protocol::edit::Edit;

impl App {
    /// `K` (Phase 4.2.b). Send `textDocument/hover` to every LSP
    /// server attached to the active document; the spawned task
    /// awaits the actor's response on the LSP runtime, so the
    /// keystroke handler returns instantly. The markdown body
    /// arrives back through `pending_hover_rx` and the next
    /// frame's `drain_pending_hover` feeds it into the popup.
    ///
    /// **Multi-server merge** is "first non-empty wins" for
    /// 4.2.b. **Cancellation**: any prior in-flight hover's
    /// token is flipped before the new request fires, so a slow
    /// server can't drop a stale popup over the new cursor
    /// position.
    pub(super) fn do_lsp_hover_request(&mut self) {
        // Already focused into the popup (State B) -- K is a
        // no-op. To get a fresh hover the user dismisses with
        // Esc / q, repositions in the doc, then presses K.
        if matches!(self.active_buffer, BufferKind::Help) {
            return;
        }
        // Popup shown but focus still on main buffer (State A) --
        // second K transfers focus into the popup. No new LSP
        // request fires; we just promote.
        if self.help_buffer.is_some() {
            self.focus_help_popup();
            return;
        }
        // First K -- fire a fresh hover request. Cancel any
        // in-flight first.
        if let Some(token) = self.pending_hover_token.take() {
            token.cancel();
        }

        // Resolve the active buffer's URI. No URI = no LSP for
        // this buffer (e.g. unsaved scratch); echo + bail.
        let Some(uri) = self
            .buffer_uris
            .get(&self.document_buffer_id)
            .cloned()
        else {
            self.set_message(
                EchoLevel::Info,
                "no LSP server attached to current buffer".to_string(),
            );
            return;
        };

        // Build the LSP-side cursor position. App's cursor is
        // (line, col_byte) in utf-8; LSP wants utf-16 columns.
        let snapshot = self.document.snapshot();
        let lsp_position = match app_to_lsp_position(&snapshot.buffer, self.cursor) {
            Some(p) => p,
            None => {
                self.set_message(EchoLevel::Error, "hover: cursor out of buffer".to_string());
                return;
            }
        };

        // Fresh channel + token for this request.
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<HoverOutcome>();
        let token = lattice_protocol::CancellationToken::new();
        self.pending_hover_rx = Some(rx);
        self.pending_hover_token = Some(token.clone());

        let lsp = self.lsp.clone();
        let logger = self.lsp_logger.clone();
        let request_started = std::time::Instant::now();
        let request_uri = uri.as_str().to_string();
        crate::runtime::spawn_on_lsp_runtime(async move {
            // Snapshot the attached handles under the supervisor
            // lock, then drop it before awaiting any per-server
            // response.
            let handles: Vec<lattice_lsp::ServerHandle> =
                { lsp.servers_for(&uri) };
            if handles.is_empty() {
                let _ = tx.send(HoverOutcome::NoServers);
                return;
            }
            let mut tried = 0usize;
            for handle in handles {
                if token.is_cancelled() {
                    return;
                }
                tried += 1;
                let params = lsp_types::HoverParams {
                    text_document_position_params: lsp_types::TextDocumentPositionParams {
                        text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                        position: lsp_position,
                    },
                    work_done_progress_params: Default::default(),
                };
                let server_id_arc: std::sync::Arc<str> =
                    std::sync::Arc::from(handle.server_id());
                logger.log(
                    Some(&server_id_arc),
                    lattice_lsp::LogLevel::Debug,
                    lattice_lsp::LogSource::Client,
                    format!(
                        "hover requested @ {request_uri} line {} character {}",
                        lsp_position.line, lsp_position.character
                    ),
                );
                match handle.hover(params, token.clone()).await {
                    Ok(Some(hover)) => {
                        let body = hover_contents_to_markdown(&hover.contents);
                        if !body.trim().is_empty() {
                            logger.log(
                                Some(&server_id_arc),
                                lattice_lsp::LogLevel::Debug,
                                lattice_lsp::LogSource::Client,
                                format!(
                                    "hover reply: {} bytes after {:?}",
                                    body.len(),
                                    request_started.elapsed()
                                ),
                            );
                            let _ = tx.send(HoverOutcome::Body(body));
                            return;
                        }
                        // Server replied but the body's empty.
                        logger.log(
                            Some(&server_id_arc),
                            lattice_lsp::LogLevel::Debug,
                            lattice_lsp::LogSource::Client,
                            "hover reply: empty body (server still indexing?)".to_string(),
                        );
                    }
                    Ok(None) => {
                        logger.log(
                            Some(&server_id_arc),
                            lattice_lsp::LogLevel::Debug,
                            lattice_lsp::LogSource::Client,
                            "hover reply: null (cursor not on a known symbol, or server still indexing)"
                                .to_string(),
                        );
                    }
                    Err(e) => {
                        logger.log(
                            Some(&server_id_arc),
                            lattice_lsp::LogLevel::Warn,
                            lattice_lsp::LogSource::Client,
                            format!("hover error: {e}"),
                        );
                    }
                }
            }
            // Walked every server, none had a non-empty body.
            let _ = tx.send(HoverOutcome::NoBody {
                servers_tried: tried,
            });
        });
    }

    /// Drain the channel populated by `do_lsp_hover_request` and
    /// act on every pending `HoverOutcome`: open the popup for
    /// `Body`, echo a clear message for `NoBody` / `NoServers` so
    /// the user always knows their `K` press was processed.
    /// Called once per main_loop iteration before draw; cheap
    /// when the channel is empty (the common case).
    pub fn drain_pending_hover(&mut self) {
        let Some(mut rx) = self.pending_hover_rx.take() else {
            return;
        };
        // Last-writer-wins -- if a stale outcome and a fresh one
        // both queued, surface the latest.
        let mut latest: Option<HoverOutcome> = None;
        while let Ok(outcome) = rx.try_recv() {
            latest = Some(outcome);
        }
        if let Some(outcome) = latest {
            match outcome {
                HoverOutcome::Body(body) => {
                    self.do_open_hover(&body);
                }
                HoverOutcome::NoBody { servers_tried } => {
                    self.set_message(
                        EchoLevel::Info,
                        format!(
                            "no hover info at cursor ({servers_tried} server{} replied)",
                            if servers_tried == 1 { "" } else { "s" }
                        ),
                    );
                }
                HoverOutcome::NoServers => {
                    self.set_message(
                        EchoLevel::Warn,
                        "hover: no LSP servers attached for this buffer (\
                         check :lsp-status / :lsp-log)"
                            .to_string(),
                    );
                }
            }
            // Outcome delivered: clear the in-flight token so a
            // subsequent motion doesn't try to flip a stale token.
            self.pending_hover_token = None;
        }
        self.pending_hover_rx = Some(rx);
    }

    /// Single canonical hook for "this buffer was just opened":
    /// register `BufferId → Uri` eagerly (path-bearing only),
    /// then publish `Event::DocumentOpened` on the bus. Both the
    /// initial-document path (`App::new`) and the follow-up
    /// `:e <path>` path (`App::do_edit`) call this helper.
    ///
    /// Idempotent against the supervisor: re-publishing the same
    /// URI is a no-op because `LspSupervisorHandle::open_buffer`
    /// short-circuits already-attached URIs.
    pub(super) fn publish_document_opened_for_active(&mut self) {
        let snap = self.document.snapshot();
        let path_opt = snap.path().map(std::path::Path::to_path_buf);
        let version = snap.text_version;
        let text = snap.buffer.as_string();
        let buffer_id = self.document_buffer_id;
        drop(snap);

        if let Some(ref path) = path_opt {
            let uri = lattice_lsp::actor::uri_from_path(path);
            self.buffer_uris.insert(buffer_id, uri);
        }

        self.event_bus.publish(Event::DocumentOpened {
            id: lattice_protocol::ids::DocumentId::new(buffer_id.0 as u64),
            path: path_opt,
            version,
            text,
        });
    }

    /// Look up the LSP metadata for a candidate via its
    /// `CandidateData::Extension` payload. Returns `None` for
    /// non-LSP candidates (buffer-words / future sync sources)
    /// or when the index is out of range.
    pub(crate) fn lsp_completion_meta_for(
        &self,
        candidate: &lattice_completion::RenderedCandidate,
    ) -> Option<&LspCompletionMeta> {
        let lattice_completion::CandidateData::Extension {
            kind_id,
            payload,
        } = &candidate.raw.data
        else {
            return None;
        };
        if *kind_id != LSP_COMPLETION_KIND_ID {
            return None;
        }
        if payload.len() != 4 {
            return None;
        }
        let idx = u32::from_le_bytes([
            payload[0],
            payload[1],
            payload[2],
            payload[3],
        ]) as usize;
        self.insert_completion_lsp_meta.get(idx)
    }

    /// Look up the current URI of a buffer. None for buffers
    /// that have no on-disk path yet (new unsaved scratch
    /// buffers).
    pub fn buffer_uri(&self, id: BufferId) -> Option<&lattice_lsp::Uri> {
        self.buffer_uris.get(&id)
    }

    /// Flush queued didChange events for a buffer immediately.
    /// Used by will-save hooks (4.3) so the server's view is
    /// caught up before pre-save requests fire. Fire-and-forget
    /// against the supervisor mailbox.
    pub fn lsp_flush(&self, buffer_id: BufferId) {
        let Some(uri) = self.buffer_uris.get(&buffer_id).cloned() else {
            return;
        };
        self.lsp.flush(uri);
    }

    /// Detach a buffer from every attached LSP server. Called
    /// from the bdelete path. Sends `didClose` per server +
    /// clears the URI's diagnostics. Fire-and-forget against
    /// the supervisor mailbox.
    pub fn lsp_close_buffer(&mut self, buffer_id: BufferId) {
        let Some(uri) = self.buffer_uris.remove(&buffer_id) else {
            return;
        };
        self.lsp.close_buffer(uri);
    }

    /// Apply editor-side LSP options that the user configured
    /// under the top-level `[lsp]` TOML table (as distinct from
    /// server-namespaced subtables like `[lsp.rust-analyzer]`,
    /// which are served back to servers via
    /// `workspace/configuration`).
    ///
    /// Today this handles:
    /// - `lsp.log-level` -- string, one of
    ///   `error`/`warn`/`info`/`debug`/`trace`. Sets the
    ///   subsystem-wide default min level (same effect as
    ///   `:lsp-log-level <level>`).
    ///
    /// Unknown / mistyped values surface a warn echo and the
    /// option is skipped. Missing keys are silent.
    pub(super) fn apply_persistent_lsp_editor_options(&mut self) {
        if let Some(toml::Value::String(level)) =
            lattice_config::lookup_dotted_path(&self.lsp_config_tree, "lsp.log-level")
        {
            match lattice_lsp::LogLevel::parse(level) {
                Some(parsed) => self.lsp_logger.set_default_level(parsed),
                None => self.set_message(
                    EchoLevel::Warn,
                    format!(
                        "config: lsp.log-level: unknown level {level:?}; expected error/warn/info/debug/trace"
                    ),
                ),
            }
        }
    }

    /// Apply a `Vec<TextEdit>` (LSP utf-16 ranges) to the active
    /// buffer as one undo unit. TextEdits are sorted in reverse
    /// by start position so each application doesn't shift the
    /// positions of the later ones (LSP convention: edits are
    /// non-overlapping and reference the original document).
    pub(super) fn apply_lsp_text_edits(
        &mut self,
        mut edits: Vec<lsp_types::TextEdit>,
    ) -> Result<(), String> {
        edits.sort_by(|a, b| {
            b.range
                .start
                .line
                .cmp(&a.range.start.line)
                .then_with(|| b.range.start.character.cmp(&a.range.start.character))
        });
        let snap = self.document.snapshot();
        let mut lattice_edits: Vec<Edit> = Vec::with_capacity(edits.len());
        for te in edits {
            let start_line = te.range.start.line;
            let end_line = te.range.end.line;
            let start_byte = lsp_position_to_app_byte(
                &snap.buffer,
                start_line,
                te.range.start.character,
            );
            let end_byte =
                lsp_position_to_app_byte(&snap.buffer, end_line, te.range.end.character);
            let range = lattice_protocol::position::Range::new(
                lattice_protocol::position::Position::new(start_line, start_byte),
                lattice_protocol::position::Position::new(end_line, end_byte),
            );
            lattice_edits.push(Edit::replace(range, te.new_text));
        }
        self.apply_edit_batch_blocking(lattice_edits)
            .map(|_| ())
            .map_err(|e| format!("{e:?}"))
    }

    /// Union of onTypeFormatting trigger characters across LSP
    /// servers attached to the active document.
    pub(super) fn on_type_formatting_trigger_chars(&self) -> Vec<char> {
        let Some(uri) = self.buffer_uris.get(&self.document_buffer_id) else {
            return Vec::new();
        };
        let handles = self.lsp.servers_for(uri);
        let mut chars: Vec<char> = Vec::new();
        for h in handles {
            for c in h.capabilities().on_type_formatting_trigger_chars() {
                if !chars.contains(&c) {
                    chars.push(c);
                }
            }
        }
        chars
    }

    /// Union of signature-help trigger characters across every
    /// LSP server attached to the active document. Empty when
    /// no server advertises the provider.
    pub(super) fn signature_help_trigger_chars(&self) -> Vec<char> {
        let Some(uri) = self.buffer_uris.get(&self.document_buffer_id) else {
            return Vec::new();
        };
        let handles = self.lsp.servers_for(uri);
        let mut chars: Vec<char> = Vec::new();
        for h in handles {
            for c in h.capabilities().signature_help_trigger_chars() {
                if !chars.contains(&c) {
                    chars.push(c);
                }
            }
        }
        chars
    }

    /// Fire `textDocument/onTypeFormatting` to the highest-
    /// priority server advertising the trigger; apply the returned
    /// edits as one undo unit.
    pub(super) fn do_lsp_on_type_formatting_request(&mut self, trigger: char) {
        let Some(uri) = self
            .buffer_uris
            .get(&self.document_buffer_id)
            .cloned()
        else {
            return;
        };
        let snapshot = self.document.snapshot();
        let pos = match app_to_lsp_position(&snapshot.buffer, self.cursor) {
            Some(p) => p,
            None => return,
        };
        let lsp = self.lsp.clone();
        let trigger_str = trigger.to_string();
        let options = lsp_types::FormattingOptions {
            tab_size: 4,
            insert_spaces: true,
            properties: Default::default(),
            trim_trailing_whitespace: Some(true),
            insert_final_newline: Some(true),
            trim_final_newlines: Some(true),
        };
        // OnTypeFormatting fires per-character; apply the result
        // via the same async drain path the format request uses.
        // Reuse `pending_format_*` since onType and `:format` are
        // mutually exclusive in time.
        if let Some(token) = self.pending_format_token.take() {
            token.cancel();
        }
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<FormatOutcome>();
        let token = lattice_protocol::CancellationToken::new();
        self.pending_format_rx = Some(rx);
        self.pending_format_token = Some(token.clone());
        crate::runtime::spawn_on_lsp_runtime(async move {
            let handles: Vec<lattice_lsp::ServerHandle> =
                { lsp.servers_for(&uri) };
            let chosen = handles
                .into_iter()
                .find(|h| h.capabilities().supports_on_type_formatting());
            let Some(handle) = chosen else {
                let _ = tx.send(FormatOutcome::NoProvider { is_range: false });
                return;
            };
            let params = lsp_types::DocumentOnTypeFormattingParams {
                text_document_position: lsp_types::TextDocumentPositionParams {
                    text_document: lsp_types::TextDocumentIdentifier { uri },
                    position: pos,
                },
                ch: trigger_str,
                options,
            };
            let edits = handle
                .on_type_formatting(params, token)
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            let _ = tx.send(FormatOutcome::Edits(edits));
        });
    }

    /// `:rename <new-name>` (Phase 4.3). Fires
    /// `textDocument/prepareRename` (when the server advertises
    /// the prepare provider) to validate the cursor and pick up
    /// the placeholder; then `textDocument/rename` to compute
    /// the WorkspaceEdit; the App applies the edits per-file as
    /// one undo unit per affected buffer (cross-file atomic
    /// rollback is a follow-up).
    ///
    /// `new_name` empty falls back to `prepareRename`'s
    /// placeholder (when available). When prepareRename returns
    /// nothing AND `new_name` is empty, we error.
    pub(super) fn do_lsp_rename_request(&mut self, new_name: &str) {
        if let Some(token) = self.pending_rename_token.take() {
            token.cancel();
        }
        let Some(uri) = self
            .buffer_uris
            .get(&self.document_buffer_id)
            .cloned()
        else {
            self.set_message(
                EchoLevel::Info,
                "no LSP server attached to current buffer".to_string(),
            );
            return;
        };
        let snapshot = self.document.snapshot();
        let lsp_position = match app_to_lsp_position(&snapshot.buffer, self.cursor) {
            Some(p) => p,
            None => {
                self.set_message(EchoLevel::Error, "rename: cursor out of buffer");
                return;
            }
        };
        let new_name = new_name.to_string();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<RenameOutcome>();
        let token = lattice_protocol::CancellationToken::new();
        self.pending_rename_rx = Some(rx);
        self.pending_rename_token = Some(token.clone());
        let lsp = self.lsp.clone();
        crate::runtime::spawn_on_lsp_runtime(async move {
            let handles: Vec<lattice_lsp::ServerHandle> =
                { lsp.servers_for(&uri) };
            let chosen = handles
                .into_iter()
                .find(|h| h.capabilities().supports_rename());
            let Some(handle) = chosen else {
                let _ = tx.send(RenameOutcome::NoProvider);
                return;
            };
            // Optional prepareRename. If the server advertises
            // prepare and refuses, surface the reason; if it
            // accepts, also use the placeholder when the user
            // didn't supply a name.
            let mut effective_name = new_name.clone();
            if handle.capabilities().supports_prepare_rename() {
                if token.is_cancelled() {
                    return;
                }
                let pos = lsp_types::TextDocumentPositionParams {
                    text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                    position: lsp_position,
                };
                match handle.prepare_rename(pos, token.clone()).await {
                    Ok(Some(prep)) => {
                        if effective_name.is_empty() {
                            effective_name = prepare_rename_placeholder(&prep)
                                .unwrap_or_default();
                        }
                    }
                    Ok(None) => {
                        let _ = tx.send(RenameOutcome::NotRenameable {
                            reason: "server refused rename at this position".into(),
                        });
                        return;
                    }
                    Err(_) => {
                        // Fall through to rename.
                    }
                }
            }
            if effective_name.is_empty() {
                let _ = tx.send(RenameOutcome::NotRenameable {
                    reason: "rename requires a new name".into(),
                });
                return;
            }
            let params = lsp_types::RenameParams {
                text_document_position: lsp_types::TextDocumentPositionParams {
                    text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                    position: lsp_position,
                },
                new_name: effective_name.clone(),
                work_done_progress_params: Default::default(),
            };
            match handle.rename(params, token.clone()).await {
                Ok(Some(workspace_edit)) => {
                    let per_file = flatten_workspace_edit(workspace_edit);
                    if per_file.is_empty() {
                        let _ = tx.send(RenameOutcome::Empty);
                    } else {
                        let _ = tx.send(RenameOutcome::Edits {
                            per_file,
                            new_name: effective_name,
                        });
                    }
                }
                _ => {
                    let _ = tx.send(RenameOutcome::Empty);
                }
            }
        });
    }

    /// Drain queued `:rename` responses; apply the WorkspaceEdit.
    /// v1: per-file edits land as one undo unit in each affected
    /// buffer.
    pub fn drain_pending_rename(&mut self) {
        let Some(mut rx) = self.pending_rename_rx.take() else {
            return;
        };
        let mut latest: Option<RenameOutcome> = None;
        while let Ok(o) = rx.try_recv() {
            latest = Some(o);
        }
        self.pending_rename_rx = Some(rx);
        let outcome = match latest {
            Some(o) => o,
            None => return,
        };
        self.pending_rename_token = None;
        match outcome {
            RenameOutcome::NoProvider => self.set_message(
                EchoLevel::Info,
                "no server with renameProvider",
            ),
            RenameOutcome::NotRenameable { reason } => {
                self.set_message(EchoLevel::Error, format!("rename: {reason}"))
            }
            RenameOutcome::Empty => self.set_message(
                EchoLevel::Info,
                "rename: no changes",
            ),
            RenameOutcome::Edits { per_file, new_name } => {
                self.apply_rename_workspace_edit(per_file, new_name);
            }
        }
    }

    /// Apply a per-file WorkspaceEdit returned by `:rename`. The
    /// active buffer's edits land directly via apply_lsp_text_edits;
    /// cross-file edits open the file via `:e` and apply.
    pub(super) fn apply_rename_workspace_edit(
        &mut self,
        per_file: Vec<(lsp_types::Uri, Vec<lsp_types::TextEdit>)>,
        new_name: String,
    ) {
        let mut applied_files = 0usize;
        let mut total_edits = 0usize;
        let mut deferred_files: Vec<String> = Vec::new();
        for (uri, edits) in per_file {
            let target_path = match lattice_lsp::actor::uri_to_path(&uri) {
                Some(p) => p,
                None => continue,
            };
            let edit_count = edits.len();
            if self
                .document
                .path()
                .map(|p| p == target_path)
                .unwrap_or(false)
            {
                if let Err(e) = self.apply_lsp_text_edits(edits) {
                    self.set_message(
                        EchoLevel::Error,
                        format!("rename: apply failed for active buffer: {e}"),
                    );
                    return;
                }
                applied_files += 1;
                total_edits += edit_count;
            } else {
                // Cross-file edits: open the file via :e and apply.
                self.do_edit(Some(target_path.clone()), false);
                if matches!(
                    self.last_message.as_ref().map(|m| m.level),
                    Some(EchoLevel::Error)
                ) {
                    deferred_files.push(target_path.display().to_string());
                    continue;
                }
                if let Err(e) = self.apply_lsp_text_edits(edits) {
                    self.set_message(
                        EchoLevel::Error,
                        format!(
                            "rename: apply failed for {}: {e}",
                            target_path.display()
                        ),
                    );
                    return;
                }
                applied_files += 1;
                total_edits += edit_count;
            }
        }
        let mut summary = format!(
            "rename -> {new_name}: {total_edits} edit{} across {applied_files} file{}",
            if total_edits == 1 { "" } else { "s" },
            if applied_files == 1 { "" } else { "s" },
        );
        if !deferred_files.is_empty() {
            summary.push_str(&format!(
                " (skipped {}: open the file then re-run)",
                deferred_files.join(", ")
            ));
        }
        self.set_message(EchoLevel::Info, summary);
    }

    /// Apply a chosen code-action. The action may carry an
    /// inline `WorkspaceEdit`, a `Command`, both, or neither
    /// (resolve required). The `handle` is the server that
    /// produced the action -- resolve / executeCommand routes
    /// back to it.
    pub(super) fn apply_lsp_code_action(
        &mut self,
        row: CodeActionRow,
        handle: Option<lattice_lsp::ServerHandle>,
    ) {
        let action = match row.action {
            // Bare command -- skip resolve, route through executeCommand.
            lsp_types::CodeActionOrCommand::Command(cmd) => {
                self.execute_lsp_command(handle, cmd);
                return;
            }
            lsp_types::CodeActionOrCommand::CodeAction(ca) => ca,
        };
        // Resolve when the action arrived without `edit` AND a
        // handle is available.
        let needs_resolve = action.edit.is_none() && action.command.is_none();
        if needs_resolve {
            let Some(handle) = handle else {
                self.set_message(
                    EchoLevel::Error,
                    "code-action: cannot resolve (no server handle)".to_string(),
                );
                return;
            };
            self.spawn_code_action_resolve_apply(handle, action);
            return;
        }
        self.apply_resolved_code_action(handle, action);
    }

    /// Async path for codeAction/resolve. Spawns a task that
    /// resolves the action then queues the resolved version
    /// back to the App for apply via the same channel the
    /// initial code-action request used.
    fn spawn_code_action_resolve_apply(
        &mut self,
        handle: lattice_lsp::ServerHandle,
        action: lsp_types::CodeAction,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<CodeActionOutcome>();
        let token = lattice_protocol::CancellationToken::new();
        // Stash the original handle so the post-resolve dispatch
        // can route back to the same server.
        self.pending_code_action_rx = Some(rx);
        self.pending_code_action_token = Some(token.clone());
        self.pending_code_action_handle = Some(handle.clone());
        crate::runtime::spawn_on_lsp_runtime(async move {
            if token.is_cancelled() {
                return;
            }
            let resolved = match handle.code_action_resolve(action.clone(), token).await {
                Ok(r) => r,
                Err(_) => action,
            };
            let _ = tx.send(CodeActionOutcome::Resolved(resolved));
        });
    }

    /// Apply a fully-resolved code-action: WorkspaceEdit (when
    /// present) lands as one undo unit per affected buffer;
    /// `Command` (when present) routes through
    /// `workspace/executeCommand`. Both can fire for the same
    /// action -- LSP spec allows it.
    fn apply_resolved_code_action(
        &mut self,
        handle: Option<lattice_lsp::ServerHandle>,
        action: lsp_types::CodeAction,
    ) {
        if let Some(edit) = action.edit {
            let per_file = flatten_workspace_edit(edit);
            if !per_file.is_empty() {
                self.apply_rename_workspace_edit(per_file, action.title.clone());
            }
        }
        if let Some(cmd) = action.command {
            self.execute_lsp_command(handle, cmd);
        }
    }

    /// Fire `workspace/executeCommand` for a code-action's
    /// command payload. Server response is opaque.
    pub(super) fn execute_lsp_command(
        &mut self,
        handle: Option<lattice_lsp::ServerHandle>,
        cmd: lsp_types::Command,
    ) {
        let Some(handle) = handle else {
            self.set_message(
                EchoLevel::Error,
                format!("execute_command: no server handle for `{}`", cmd.command),
            );
            return;
        };
        if !handle.capabilities().supports_execute_command() {
            self.set_message(
                EchoLevel::Error,
                format!(
                    "execute_command: server doesn't advertise executeCommandProvider for `{}`",
                    cmd.command
                ),
            );
            return;
        }
        let params = lsp_types::ExecuteCommandParams {
            command: cmd.command.clone(),
            arguments: cmd.arguments.unwrap_or_default(),
            work_done_progress_params: Default::default(),
        };
        let title = cmd.title.clone();
        let token = lattice_protocol::CancellationToken::new();
        crate::runtime::spawn_on_lsp_runtime(async move {
            // Fire-and-forget; the response is rarely useful
            // beyond error logging.
            let _ = handle.execute_command(params, token).await;
        });
        self.set_message(EchoLevel::Info, format!("dispatched: {title}"));
    }

    /// Splice a chosen completion item into the buffer at its
    /// captured replace range. Plain text only -- snippet
    /// expansion lands with the buffer-level Insert-mode
    /// completion shell.
    pub(super) fn apply_lsp_completion_item(&mut self, item: &CompletionItemRow) {
        let (start_byte, end_byte) = item.replace_range;
        let range = lattice_protocol::position::Range::new(
            Position::new(item.line, start_byte),
            Position::new(item.line, end_byte),
        );
        let edit = Edit::replace(range, item.insert_text.clone());
        match self.apply_edit_blocking(edit) {
            Ok(applied) => {
                self.cursor = applied.inserted_range.end;
            }
            Err(e) => {
                self.set_message(
                    EchoLevel::Error,
                    format!("complete: apply failed: {e:?}"),
                );
            }
        }
    }

    /// `:code-actions` (Phase 4.3). Run textDocument/codeAction
    /// at the cursor (or active Visual selection); open the
    /// merged item list as a vertico picker. v1 picks the first
    /// server with `codeActionProvider`.
    pub(super) fn do_lsp_code_action_request(&mut self) {
        if let Some(token) = self.pending_code_action_token.take() {
            token.cancel();
        }
        // Browse-style; not a tag-intent drill-down.
        self.pending_tag_origin = None;
        let Some(uri) = self
            .buffer_uris
            .get(&self.document_buffer_id)
            .cloned()
        else {
            self.set_message(
                EchoLevel::Info,
                "no LSP server attached to current buffer".to_string(),
            );
            return;
        };
        let snapshot = self.document.snapshot();
        let range = self.code_action_range(&snapshot.buffer);
        let context = lsp_types::CodeActionContext {
            diagnostics: self.diagnostics_for_range(&uri, &range),
            only: None,
            trigger_kind: Some(lsp_types::CodeActionTriggerKind::INVOKED),
        };
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<CodeActionOutcome>();
        let token = lattice_protocol::CancellationToken::new();
        self.pending_code_action_rx = Some(rx);
        self.pending_code_action_token = Some(token.clone());
        let lsp = self.lsp.clone();
        let stash = std::sync::Arc::new(std::sync::Mutex::new(
            None::<lattice_lsp::ServerHandle>,
        ));
        let stash_for_task = stash.clone();
        crate::runtime::spawn_on_lsp_runtime(async move {
            let handles: Vec<lattice_lsp::ServerHandle> =
                { lsp.servers_for(&uri) };
            let chosen = handles
                .into_iter()
                .find(|h| h.capabilities().supports_code_action());
            let Some(handle) = chosen else {
                let _ = tx.send(CodeActionOutcome::NoProvider);
                return;
            };
            *stash_for_task.lock().unwrap() = Some(handle.clone());
            let params = lsp_types::CodeActionParams {
                text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                range,
                context,
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            };
            if let Ok(Some(resp)) = handle.code_action(params, token.clone()).await {
                let rows: Vec<CodeActionRow> = resp
                    .into_iter()
                    .map(|act| {
                        let (title, kind_glyph) = match &act {
                            lsp_types::CodeActionOrCommand::Command(c) => {
                                (c.title.clone(), code_action_kind_glyph(None))
                            }
                            lsp_types::CodeActionOrCommand::CodeAction(ca) => (
                                ca.title.clone(),
                                code_action_kind_glyph(ca.kind.as_ref()),
                            ),
                        };
                        CodeActionRow {
                            title,
                            kind_glyph,
                            action: act,
                        }
                    })
                    .collect();
                let _ = tx.send(CodeActionOutcome::Items(rows));
            } else {
                let _ = tx.send(CodeActionOutcome::Items(Vec::new()));
            }
        });
        let _ = stash;
    }

    /// LSP-shape range for the current code-action request.
    /// Visual selection when active; point range at cursor otherwise.
    fn code_action_range(
        &self,
        buffer: &lattice_core::Buffer,
    ) -> lsp_types::Range {
        if let lattice_grammar::ModalState::Visual(_) = self.modal {
            let anchor = self.visual_anchor.unwrap_or(self.cursor);
            let head = self.cursor;
            let (start_pos, end_pos) =
                if (anchor.line, anchor.byte) <= (head.line, head.byte) {
                    (anchor, head)
                } else {
                    (head, anchor)
                };
            let start = app_to_lsp_position(buffer, start_pos)
                .unwrap_or(lsp_types::Position {
                    line: 0,
                    character: 0,
                });
            let end = app_to_lsp_position(buffer, end_pos).unwrap_or(start);
            lsp_types::Range { start, end }
        } else {
            let p = app_to_lsp_position(buffer, self.cursor).unwrap_or(
                lsp_types::Position {
                    line: 0,
                    character: 0,
                },
            );
            lsp_types::Range { start: p, end: p }
        }
    }

    /// Diagnostics overlapping `range` in `uri`, converted to
    /// the LSP shape codeAction servers expect in
    /// `CodeActionContext`. Servers use these to emit quick-fix
    /// actions tied to specific diagnostics.
    fn diagnostics_for_range(
        &self,
        uri: &lattice_lsp::Uri,
        range: &lsp_types::Range,
    ) -> Vec<lattice_lsp::Diagnostic> {
        self.lsp_diagnostics
            .diagnostics_for(uri)
            .into_iter()
            .filter(|d| {
                d.range.end.line > range.start.line
                    || (d.range.end.line == range.start.line
                        && d.range.end.character > range.start.character)
            })
            .filter(|d| {
                d.range.start.line < range.end.line
                    || (d.range.start.line == range.end.line
                        && d.range.start.character <= range.end.character)
            })
            .collect()
    }

    /// Drain queued code-action responses. Items pin to App + open
    /// a picker. Resolve responses (single-row outcomes seeded with
    /// the resolved action) apply directly when the original handle
    /// is still pinned.
    pub fn drain_pending_code_actions(&mut self) {
        let Some(mut rx) = self.pending_code_action_rx.take() else {
            return;
        };
        let mut latest: Option<CodeActionOutcome> = None;
        while let Ok(o) = rx.try_recv() {
            latest = Some(o);
        }
        self.pending_code_action_rx = Some(rx);
        let outcome = match latest {
            Some(o) => o,
            None => return,
        };
        self.pending_code_action_token = None;
        match outcome {
            CodeActionOutcome::NoProvider => {
                self.set_message(
                    EchoLevel::Info,
                    "no server with codeActionProvider".to_string(),
                );
            }
            CodeActionOutcome::Resolved(action) => {
                let handle = self.pending_code_action_handle.take();
                self.apply_resolved_code_action(handle, action);
            }
            CodeActionOutcome::Items(items) => {
                if items.is_empty() {
                    self.set_message(EchoLevel::Info, "no code actions".to_string());
                    return;
                }
                let total = items.len();
                let pairs: Vec<(
                    lattice_completion::RawCandidate,
                    crate::picker::RoutingPayload,
                )> = items
                    .iter()
                    .enumerate()
                    .map(|(i, item)| {
                        let mut c = lattice_completion::RawCandidate::plain(
                            item.title.clone(),
                            lattice_completion::CandidateKind::Plain,
                        );
                        c.display = format!("{} {}", item.kind_glyph, item.title);
                        (
                            c,
                            crate::picker::RoutingPayload::LspCodeAction {
                                index: i as u32,
                            },
                        )
                    })
                    .collect();
                let handle = self.first_code_action_handle();
                self.pending_code_action_items = Some(items);
                self.pending_code_action_handle = handle;
                let mut p = crate::picker::Picker::new(
                    format!("code-actions ({total})"),
                    crate::picker::PickerSource::LspLocations,
                    crate::picker::PickerAction::AcceptLspCodeAction,
                );
                p.set_raw_candidates_with_routing(pairs);
                self.picker = Some(p);
            }
        }
    }

    /// Pick the first attached server that advertises
    /// `codeActionProvider` -- mirrors the choice the spawn task
    /// made when firing the original request.
    fn first_code_action_handle(&self) -> Option<lattice_lsp::ServerHandle> {
        let uri = self.buffer_uris.get(&self.document_buffer_id)?;
        self.lsp
            .servers_for(uri)
            .into_iter()
            .find(|h| h.capabilities().supports_code_action())
    }

    /// `:complete` (Phase 4.2.g). Fires
    /// `textDocument/completion` at the cursor; the merged item
    /// list opens as a vertico picker. Multi-server union;
    /// dedup by `(label, kind)`.
    pub(super) fn do_lsp_completion_request(&mut self) {
        if let Some(token) = self.pending_completion_token.take() {
            token.cancel();
        }
        // Browse-style; not a tag-intent drill-down.
        self.pending_tag_origin = None;
        let Some(uri) = self
            .buffer_uris
            .get(&self.document_buffer_id)
            .cloned()
        else {
            self.set_message(
                EchoLevel::Info,
                "no LSP server attached to current buffer".to_string(),
            );
            return;
        };
        let snapshot = self.document.snapshot();
        let lsp_position = match app_to_lsp_position(&snapshot.buffer, self.cursor) {
            Some(p) => p,
            None => return,
        };
        // Compute the prefix replace range: walk back from the
        // cursor over word characters. The server may override
        // via `text_edit` per-item; this is the fallback.
        let line_text = snapshot.buffer.line(self.cursor.line).unwrap_or_default();
        let cursor_byte = self.cursor.byte as usize;
        let mut start = cursor_byte;
        let bytes = line_text.as_bytes();
        while start > 0 && start <= bytes.len() && is_word_char_byte(bytes[start - 1]) {
            start -= 1;
        }
        let prefix_start = start as u32;
        let cursor_line = self.cursor.line;
        let cursor_col = self.cursor.byte;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<CompletionOutcome>();
        let token = lattice_protocol::CancellationToken::new();
        self.pending_completion_rx = Some(rx);
        self.pending_completion_token = Some(token.clone());
        let lsp = self.lsp.clone();
        crate::runtime::spawn_on_lsp_runtime(async move {
            let handles: Vec<lattice_lsp::ServerHandle> =
                { lsp.servers_for(&uri) };
            if handles.is_empty() {
                let _ = tx.send(CompletionOutcome::NoServers);
                return;
            }
            let mut all: Vec<CompletionItemRow> = Vec::new();
            for handle in handles {
                if token.is_cancelled() {
                    return;
                }
                if !handle.capabilities().supports_completion() {
                    continue;
                }
                let params = lsp_types::CompletionParams {
                    text_document_position: lsp_types::TextDocumentPositionParams {
                        text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                        position: lsp_position,
                    },
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                    context: None,
                };
                if let Ok(Some(resp)) = handle.completion(params, token.clone()).await {
                    let items = match resp {
                        lsp_types::CompletionResponse::Array(items) => items,
                        lsp_types::CompletionResponse::List(list) => list.items,
                    };
                    for ci in items {
                        let label = ci.label;
                        let kind_glyph = completion_kind_glyph(ci.kind);
                        let detail = ci.detail.clone();
                        let insert_text = ci
                            .insert_text
                            .clone()
                            .unwrap_or_else(|| label.clone());
                        all.push(CompletionItemRow {
                            label,
                            kind_glyph,
                            detail,
                            insert_text,
                            replace_range: (prefix_start, cursor_col),
                            line: cursor_line,
                        });
                    }
                }
            }
            // Dedup by (label, kind glyph) -- avoid two servers
            // emitting the same name twice.
            all.sort_by(|a, b| a.label.cmp(&b.label).then_with(|| a.kind_glyph.cmp(b.kind_glyph)));
            all.dedup_by(|a, b| a.label == b.label && a.kind_glyph == b.kind_glyph);
            let _ = tx.send(CompletionOutcome::Items(all));
        });
    }

    /// Drain queued LSP completion responses and open a picker.
    /// `NoServers` echoes; empty list echoes.
    pub fn drain_pending_completion(&mut self) {
        let Some(mut rx) = self.pending_completion_rx.take() else {
            return;
        };
        let mut latest: Option<CompletionOutcome> = None;
        while let Ok(o) = rx.try_recv() {
            latest = Some(o);
        }
        self.pending_completion_rx = Some(rx);
        let outcome = match latest {
            Some(o) => o,
            None => return,
        };
        self.pending_completion_token = None;
        match outcome {
            CompletionOutcome::NoServers => {
                self.set_message(
                    EchoLevel::Info,
                    "no LSP server attached".to_string(),
                );
            }
            CompletionOutcome::Items(items) => {
                if items.is_empty() {
                    self.set_message(EchoLevel::Info, "no completions".to_string());
                    return;
                }
                let total = items.len();
                let pairs: Vec<(
                    lattice_completion::RawCandidate,
                    crate::picker::RoutingPayload,
                )> = items
                    .iter()
                    .enumerate()
                    .map(|(i, item)| {
                        let mut c = lattice_completion::RawCandidate::plain(
                            item.label.clone(),
                            lattice_completion::CandidateKind::Plain,
                        );
                        c.display = match &item.detail {
                            Some(d) => format!("{} {}  {d}", item.kind_glyph, item.label),
                            None => format!("{} {}", item.kind_glyph, item.label),
                        };
                        (
                            c,
                            crate::picker::RoutingPayload::LspCompletion {
                                index: i as u32,
                            },
                        )
                    })
                    .collect();
                self.pending_completion_items = Some(items);
                let mut p = crate::picker::Picker::new(
                    format!("complete ({total})"),
                    crate::picker::PickerSource::LspLocations,
                    crate::picker::PickerAction::AcceptLspCompletion,
                );
                p.set_raw_candidates_with_routing(pairs);
                self.picker = Some(p);
            }
        }
    }

    /// `:format` / `:format-range` (Phase 4.3). Picks the
    /// highest-priority server with `documentFormattingProvider`
    /// (or `documentRangeFormattingProvider` when `is_range`),
    /// fires the request, applies the returned edits as one
    /// undo unit.
    ///
    /// Single-server strategy per docs/lsp-architecture.md §7b:
    /// "Two formatters can't agree on whitespace." -- so unlike
    /// nav we don't fan out / merge.
    ///
    /// Range source for `is_range`: active Visual selection (if
    /// in Visual mode), else the whole buffer.
    pub(super) fn do_lsp_format_request(&mut self, is_range: bool) {
        if let Some(token) = self.pending_format_token.take() {
            token.cancel();
        }
        let Some(uri) = self
            .buffer_uris
            .get(&self.document_buffer_id)
            .cloned()
        else {
            self.set_message(
                EchoLevel::Info,
                "no LSP server attached to current buffer".to_string(),
            );
            return;
        };
        let snapshot = self.document.snapshot();
        let last_line = last_addressable_line(&snapshot.buffer);
        // Range resolution.
        let range_lines: Option<(u32, u32)> = if is_range {
            // Use the active Visual selection if any, else the whole buffer.
            if let ModalState::Visual(_) = self.modal {
                let anchor = self.visual_anchor.unwrap_or(self.cursor);
                let head = self.cursor;
                let (s, e): (u32, u32) = if anchor.line <= head.line {
                    (anchor.line, head.line)
                } else {
                    (head.line, anchor.line)
                };
                Some((s, e.min(last_line)))
            } else {
                Some((0u32, last_line))
            }
        } else {
            None
        };
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<FormatOutcome>();
        let token = lattice_protocol::CancellationToken::new();
        self.pending_format_rx = Some(rx);
        self.pending_format_token = Some(token.clone());
        let lsp = self.lsp.clone();
        // Compute the LSP range parameters when needed.
        let lsp_range = range_lines.map(|(s, e)| {
            let end_line_text_len = line_byte_len(&snapshot.buffer, e);
            let line_text = snapshot.buffer.line(e).unwrap_or_default();
            let end_char =
                lattice_lsp::position::utf8_byte_to_utf16_column(&line_text, end_line_text_len);
            lsp_types::Range {
                start: lsp_types::Position {
                    line: s,
                    character: 0,
                },
                end: lsp_types::Position {
                    line: e,
                    character: end_char,
                },
            }
        });
        // Conservative formatting options.
        let options = lsp_types::FormattingOptions {
            tab_size: 4,
            insert_spaces: true,
            properties: Default::default(),
            trim_trailing_whitespace: Some(true),
            insert_final_newline: Some(true),
            trim_final_newlines: Some(true),
        };
        crate::runtime::spawn_on_lsp_runtime(async move {
            let handles: Vec<lattice_lsp::ServerHandle> =
                { lsp.servers_for(&uri) };
            // Pick the first server advertising the right provider.
            let chosen: Option<lattice_lsp::ServerHandle> = handles
                .into_iter()
                .find(|h| {
                    let caps = h.capabilities();
                    if lsp_range.is_some() {
                        caps.supports_range_formatting()
                    } else {
                        caps.supports_formatting()
                    }
                });
            let Some(handle) = chosen else {
                let _ = tx.send(FormatOutcome::NoProvider {
                    is_range: lsp_range.is_some(),
                });
                return;
            };
            let edits: Option<Vec<lsp_types::TextEdit>> = if let Some(range) = lsp_range {
                let params = lsp_types::DocumentRangeFormattingParams {
                    text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                    range,
                    options: options.clone(),
                    work_done_progress_params: Default::default(),
                };
                handle
                    .range_formatting(params, token.clone())
                    .await
                    .ok()
                    .flatten()
            } else {
                let params = lsp_types::DocumentFormattingParams {
                    text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                    options,
                    work_done_progress_params: Default::default(),
                };
                handle
                    .formatting(params, token.clone())
                    .await
                    .ok()
                    .flatten()
            };
            let edits = edits.unwrap_or_default();
            let _ = tx.send(FormatOutcome::Edits(edits));
        });
    }

    /// Drain the format response channel and apply the returned
    /// edits as one undo unit. Echoes when the server returned no
    /// edits ("already formatted") or no provider was available.
    pub fn drain_pending_format(&mut self) {
        let Some(mut rx) = self.pending_format_rx.take() else {
            return;
        };
        let mut latest: Option<FormatOutcome> = None;
        while let Ok(o) = rx.try_recv() {
            latest = Some(o);
        }
        self.pending_format_rx = Some(rx);
        let outcome = match latest {
            Some(o) => o,
            None => return,
        };
        self.pending_format_token = None;
        match outcome {
            FormatOutcome::NoProvider { is_range } => {
                let kind = if is_range { "range " } else { "" };
                self.set_message(
                    EchoLevel::Info,
                    format!("no server with {kind}formatting provider"),
                );
            }
            FormatOutcome::Edits(edits) => {
                if edits.is_empty() {
                    self.set_message(
                        EchoLevel::Info,
                        "format: no changes (already formatted)".to_string(),
                    );
                    return;
                }
                let n = edits.len();
                match self.apply_lsp_text_edits(edits) {
                    Ok(()) => self.set_message(
                        EchoLevel::Info,
                        format!("format: applied {n} edit{}", if n == 1 { "" } else { "s" }),
                    ),
                    Err(e) => self.set_message(
                        EchoLevel::Error,
                        format!("format: apply failed: {e}"),
                    ),
                }
            }
        }
    }

    /// `:lsp-symbols` (Phase 4.2.e). Send
    /// `textDocument/documentSymbol` to every attached server;
    /// flatten the hierarchy + merge across servers; drain on
    /// the next frame opens a picker.
    pub(super) fn do_lsp_document_symbol_request(&mut self) {
        if let Some(token) = self.pending_symbols_token.take() {
            token.cancel();
        }
        // Outline browse; not a tag-intent drill-down.
        self.pending_tag_origin = None;
        let Some(uri) = self
            .buffer_uris
            .get(&self.document_buffer_id)
            .cloned()
        else {
            self.set_message(
                EchoLevel::Info,
                "no LSP server attached to current buffer".to_string(),
            );
            return;
        };
        let path = match lattice_lsp::actor::uri_to_path(&uri) {
            Some(p) => p,
            None => {
                self.set_message(
                    EchoLevel::Error,
                    "documentSymbol: buffer URI is not a file".to_string(),
                );
                return;
            }
        };
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<SymbolsOutcome>();
        let token = lattice_protocol::CancellationToken::new();
        self.pending_symbols_rx = Some(rx);
        self.pending_symbols_token = Some(token.clone());
        let lsp = self.lsp.clone();
        crate::runtime::spawn_on_lsp_runtime(async move {
            let handles: Vec<lattice_lsp::ServerHandle> =
                { lsp.servers_for(&uri) };
            if handles.is_empty() {
                let _ = tx.send(SymbolsOutcome::NoServers);
                return;
            }
            let mut all: Vec<SymbolRow> = Vec::new();
            for handle in handles {
                if token.is_cancelled() {
                    return;
                }
                let params = lsp_types::DocumentSymbolParams {
                    text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                };
                if let Ok(Some(resp)) = handle.document_symbol(params, token.clone()).await {
                    flatten_document_symbol_response(resp, &path, &mut all);
                }
            }
            // Dedup by (path, line, col, name).
            all.sort_by(|a, b| {
                a.path
                    .cmp(&b.path)
                    .then_with(|| a.line.cmp(&b.line))
                    .then_with(|| a.col.cmp(&b.col))
                    .then_with(|| a.name.cmp(&b.name))
            });
            all.dedup_by(|a, b| {
                a.path == b.path && a.line == b.line && a.col == b.col && a.name == b.name
            });
            let title = format!("symbols ({})", all.len());
            let _ = tx.send(SymbolsOutcome::Found { title, rows: all });
        });
    }

    /// `:lsp-workspace-symbol [query]` (Phase 4.2.f).
    pub(super) fn do_lsp_workspace_symbol_request(&mut self, query: &str) {
        if let Some(token) = self.pending_symbols_token.take() {
            token.cancel();
        }
        // Workspace search browse; not a tag-intent drill-down.
        self.pending_tag_origin = None;
        // Workspace symbol is workspace-scoped, so we fan out
        // over EVERY server the supervisor has running -- not
        // just servers attached to the current buffer.
        let lsp = self.lsp.clone();
        let query = query.to_string();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<SymbolsOutcome>();
        let token = lattice_protocol::CancellationToken::new();
        self.pending_symbols_rx = Some(rx);
        self.pending_symbols_token = Some(token.clone());
        crate::runtime::spawn_on_lsp_runtime(async move {
            let handles: Vec<lattice_lsp::ServerHandle> = lsp.all_running_handles();
            if handles.is_empty() {
                let _ = tx.send(SymbolsOutcome::NoServers);
                return;
            }
            let mut all: Vec<SymbolRow> = Vec::new();
            for handle in handles {
                if token.is_cancelled() {
                    return;
                }
                let params = lsp_types::WorkspaceSymbolParams {
                    query: query.clone(),
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                };
                let Ok(Some(resp)) = handle.workspace_symbol(params, token.clone()).await
                else {
                    continue;
                };
                match resp {
                    // Legacy `Vec<SymbolInformation>` shape.
                    lsp_types::WorkspaceSymbolResponse::Flat(syms) => {
                        for sym in syms {
                            if let Some(row) = symbol_information_to_row(&sym) {
                                all.push(row);
                            }
                        }
                    }
                    // Modern `Vec<WorkspaceSymbol>` shape (LSP 3.17+).
                    lsp_types::WorkspaceSymbolResponse::Nested(syms) => {
                        for sym in syms {
                            if let Some(row) =
                                workspace_symbol_to_row(&handle, sym, &token).await
                            {
                                all.push(row);
                            }
                        }
                    }
                }
            }
            all.sort_by(|a, b| {
                a.path
                    .cmp(&b.path)
                    .then_with(|| a.line.cmp(&b.line))
                    .then_with(|| a.col.cmp(&b.col))
                    .then_with(|| a.name.cmp(&b.name))
            });
            all.dedup_by(|a, b| {
                a.path == b.path && a.line == b.line && a.col == b.col && a.name == b.name
            });
            let title = if query.is_empty() {
                format!("workspace-symbols ({})", all.len())
            } else {
                format!("workspace-symbols {query:?} ({})", all.len())
            };
            let _ = tx.send(SymbolsOutcome::Found { title, rows: all });
        });
    }

    /// Drain queued document-symbol / workspace-symbol responses
    /// and open the picker.
    pub fn drain_pending_symbols(&mut self) {
        let Some(mut rx) = self.pending_symbols_rx.take() else {
            return;
        };
        let mut latest: Option<SymbolsOutcome> = None;
        while let Ok(o) = rx.try_recv() {
            latest = Some(o);
        }
        self.pending_symbols_rx = Some(rx);
        let outcome = match latest {
            Some(o) => o,
            None => return,
        };
        self.pending_symbols_token = None;
        match outcome {
            SymbolsOutcome::NoServers => {
                self.set_message(
                    EchoLevel::Info,
                    "no LSP server attached".to_string(),
                );
            }
            SymbolsOutcome::Found { title, rows } => {
                if rows.is_empty() {
                    self.set_message(EchoLevel::Info, "no symbols".to_string());
                    return;
                }
                let picker_rows: Vec<crate::picker::LspLocationRow> = rows
                    .into_iter()
                    .map(|r| {
                        let indent = "  ".repeat(r.depth as usize);
                        let preview = if let Some(c) = r.container {
                            format!("{indent}{} {}  ({c})", r.kind_glyph, r.name)
                        } else {
                            format!("{indent}{} {}", r.kind_glyph, r.name)
                        };
                        crate::picker::LspLocationRow {
                            path: r.path,
                            line: r.line,
                            col: r.col,
                            preview,
                            marginalia: String::new(),
                        }
                    })
                    .collect();
                let mut p = crate::picker::Picker::new(
                    title,
                    crate::picker::PickerSource::LspLocations,
                    crate::picker::PickerAction::JumpToLspLocation,
                );
                p.set_lsp_locations(picker_rows);
                self.picker = Some(p);
            }
        }
    }

    /// `:lsp-signature-help` (Phase 4.3). Fan-out across attached
    /// servers; first non-empty `SignatureHelp` response wins
    /// (per docs/lsp-architecture.md §7b "First non-empty wins.
    /// Signatures are usually language-specific; merging rarely
    /// useful.").
    pub(super) fn do_lsp_signature_help_request(&mut self) {
        if let Some(token) = self.pending_signature_help_token.take() {
            token.cancel();
        }
        let Some(uri) = self
            .buffer_uris
            .get(&self.document_buffer_id)
            .cloned()
        else {
            self.set_message(
                EchoLevel::Info,
                "no LSP server attached to current buffer".to_string(),
            );
            return;
        };
        let snapshot = self.document.snapshot();
        let lsp_position = match app_to_lsp_position(&snapshot.buffer, self.cursor) {
            Some(p) => p,
            None => return,
        };
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<SignatureHelpOutcome>();
        let token = lattice_protocol::CancellationToken::new();
        self.pending_signature_help_rx = Some(rx);
        self.pending_signature_help_token = Some(token.clone());
        let lsp = self.lsp.clone();
        crate::runtime::spawn_on_lsp_runtime(async move {
            let handles: Vec<lattice_lsp::ServerHandle> =
                { lsp.servers_for(&uri) };
            if handles.is_empty() {
                let _ = tx.send(SignatureHelpOutcome::NoServers);
                return;
            }
            for handle in handles {
                if token.is_cancelled() {
                    return;
                }
                if !handle.capabilities().supports_signature_help() {
                    continue;
                }
                let params = lsp_types::SignatureHelpParams {
                    text_document_position_params: lsp_types::TextDocumentPositionParams {
                        text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                        position: lsp_position,
                    },
                    work_done_progress_params: Default::default(),
                    context: None,
                };
                if let Ok(Some(sh)) = handle.signature_help(params, token.clone()).await {
                    let body = signature_help_to_markdown(&sh);
                    if !body.is_empty() {
                        let _ = tx.send(SignatureHelpOutcome::Body(body));
                        return;
                    }
                }
            }
            let _ = tx.send(SignatureHelpOutcome::Body(String::new()));
        });
    }

    /// Drain queued signature-help responses. A non-empty body
    /// renders into the popup; empty echoes "no signature info";
    /// `NoServers` echoes the standard "no LSP server" message.
    pub fn drain_pending_signature_help(&mut self) {
        let Some(mut rx) = self.pending_signature_help_rx.take() else {
            return;
        };
        let mut latest: Option<SignatureHelpOutcome> = None;
        while let Ok(o) = rx.try_recv() {
            latest = Some(o);
        }
        self.pending_signature_help_rx = Some(rx);
        let outcome = match latest {
            Some(o) => o,
            None => return,
        };
        self.pending_signature_help_token = None;
        match outcome {
            SignatureHelpOutcome::NoServers => {
                self.set_message(
                    EchoLevel::Info,
                    "no LSP server attached to current buffer".to_string(),
                );
            }
            SignatureHelpOutcome::Body(body) if body.is_empty() => {
                self.set_message(EchoLevel::Info, "no signature info".to_string());
            }
            SignatureHelpOutcome::Body(body) => {
                self.do_open_hover(&body);
            }
        }
    }

    /// Generic dispatch for the four navigation flavours
    /// (definition / declaration / typeDefinition / implementation
    /// -- DESIGN.md §5.4 / docs/lsp-features.md). All share the
    /// same `Vec<Location>` shape, so dispatch is parameterised
    /// by `LspNavKind`; the kind selects the LSP method and
    /// drives the user-facing echo from `drain_pending_definitions`.
    ///
    /// Multi-server merge: every server's response is flattened
    /// to `Vec<Location>`; the union is deduplicated by
    /// `(uri, range.start)`. A single result jumps; multiple
    /// results echo a count and open the LSP-locations picker.
    pub(super) fn do_lsp_nav_request(&mut self, kind: LspNavKind) {
        if let Some(token) = self.pending_definition_token.take() {
            token.cancel();
        }
        let Some(uri) = self
            .buffer_uris
            .get(&self.document_buffer_id)
            .cloned()
        else {
            self.set_message(
                EchoLevel::Info,
                "no LSP server attached to current buffer".to_string(),
            );
            return;
        };
        let snapshot = self.document.snapshot();
        let lsp_position = match app_to_lsp_position(&snapshot.buffer, self.cursor) {
            Some(p) => p,
            None => {
                self.set_message(
                    EchoLevel::Error,
                    format!("{}: cursor out of buffer", kind.noun_singular()),
                );
                return;
            }
        };
        // Capture the pre-jump origin for the tag stack -- the
        // gd family is "drill down" navigation, so users expect
        // <C-t> to walk back even after navigating through a
        // chain of definitions.
        let label = word_under_cursor(&snapshot.buffer, self.cursor).unwrap_or_default();
        self.pending_tag_origin = Some(TagStackEntry {
            buffer: self.active_buffer,
            buffer_id: self.active_pane_buffer_id(),
            position: self.cursor,
            label,
        });
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<Vec<lsp_types::Location>>();
        let token = lattice_protocol::CancellationToken::new();
        self.pending_definition_rx = Some(rx);
        self.pending_definition_token = Some(token.clone());
        self.pending_nav_kind = Some(kind);
        let lsp = self.lsp.clone();
        crate::runtime::spawn_on_lsp_runtime(async move {
            let handles: Vec<lattice_lsp::ServerHandle> =
                { lsp.servers_for(&uri) };
            let mut all: Vec<lsp_types::Location> = Vec::new();
            for handle in handles {
                if token.is_cancelled() {
                    return;
                }
                let pos_params = lsp_types::TextDocumentPositionParams {
                    text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                    position: lsp_position,
                };
                let resp_locs = match kind {
                    LspNavKind::Definition => {
                        let params = lsp_types::GotoDefinitionParams {
                            text_document_position_params: pos_params,
                            work_done_progress_params: Default::default(),
                            partial_result_params: Default::default(),
                        };
                        handle
                            .goto_definition(params, token.clone())
                            .await
                            .ok()
                            .flatten()
                            .map(definition_response_to_locations)
                            .unwrap_or_default()
                    }
                    LspNavKind::Declaration => {
                        let params = lsp_types::request::GotoDeclarationParams {
                            text_document_position_params: pos_params,
                            work_done_progress_params: Default::default(),
                            partial_result_params: Default::default(),
                        };
                        handle
                            .goto_declaration(params, token.clone())
                            .await
                            .ok()
                            .flatten()
                            .map(definition_response_to_locations)
                            .unwrap_or_default()
                    }
                    LspNavKind::TypeDefinition => {
                        let params = lsp_types::request::GotoTypeDefinitionParams {
                            text_document_position_params: pos_params,
                            work_done_progress_params: Default::default(),
                            partial_result_params: Default::default(),
                        };
                        handle
                            .goto_type_definition(params, token.clone())
                            .await
                            .ok()
                            .flatten()
                            .map(definition_response_to_locations)
                            .unwrap_or_default()
                    }
                    LspNavKind::Implementation => {
                        let params = lsp_types::request::GotoImplementationParams {
                            text_document_position_params: pos_params,
                            work_done_progress_params: Default::default(),
                            partial_result_params: Default::default(),
                        };
                        handle
                            .goto_implementation(params, token.clone())
                            .await
                            .ok()
                            .flatten()
                            .map(definition_response_to_locations)
                            .unwrap_or_default()
                    }
                };
                all.extend(resp_locs);
            }
            // Dedup by (uri, range.start).
            all.sort_by(|a, b| {
                let au = a.uri.as_str();
                let bu = b.uri.as_str();
                au.cmp(bu)
                    .then_with(|| a.range.start.line.cmp(&b.range.start.line))
                    .then_with(|| a.range.start.character.cmp(&b.range.start.character))
            });
            all.dedup_by(|a, b| {
                a.uri.as_str() == b.uri.as_str() && a.range.start == b.range.start
            });
            let _ = tx.send(all);
        });
    }

    /// Backwards-compat wrapper. Tests + plugin contributions may
    /// reach for the named `do_lsp_definition_request`; this keeps
    /// the public surface intact while the unified `do_lsp_nav_request`
    /// handles the actual work.
    pub fn do_lsp_definition_request(&mut self) {
        self.do_lsp_nav_request(LspNavKind::Definition)
    }

    /// Drain queued nav (definition / declaration / typeDef /
    /// impl) results and act on them: 0 -> echo, 1 -> jump, N>1
    /// -> echo count + open picker. Pushes the pre-jump cursor
    /// onto the position history so `<C-o>` walks back. The verb
    /// in echoes (`definitions` vs `implementations` etc.) reads
    /// from `pending_nav_kind`.
    pub fn drain_pending_definitions(&mut self) {
        let Some(mut rx) = self.pending_definition_rx.take() else {
            return;
        };
        let mut latest: Option<Vec<lsp_types::Location>> = None;
        while let Ok(locs) = rx.try_recv() {
            latest = Some(locs);
        }
        self.pending_definition_rx = Some(rx);
        let locs = match latest {
            Some(l) => l,
            None => return,
        };
        // Result delivered; clear the in-flight token.
        self.pending_definition_token = None;
        let kind = self.pending_nav_kind.take().unwrap_or(LspNavKind::Definition);
        let noun = kind.noun_plural();

        match locs.len() {
            0 => {
                // No drill-down happened; drop the captured tag
                // origin so a follow-up nav doesn't see a stale
                // value.
                self.pending_tag_origin = None;
                self.set_message(EchoLevel::Info, format!("no {noun} found"));
            }
            1 => {
                // Vim-style "do what I mean" -- a single-result
                // nav request still jumps directly. Single-result
                // jump pushes the tag stack now.
                if let Some(origin) = self.pending_tag_origin.take() {
                    self.tag_stack.push(origin);
                }
                self.jump_to_lsp_location(&locs[0]);
            }
            _ => {
                // Multi-result -- the picker will consume the
                // pending tag origin on accept.
                self.open_lsp_locations_picker(format!("lsp:{noun}"), &locs);
            }
        }
    }

    /// `gr` (Phase 4.2.d). Send `textDocument/references` to
    /// every attached LSP server with `include_declaration: true`
    /// (vim convention -- `gr` includes the symbol's own
    /// declaration in the list). Spawn the per-server walk on
    /// the LSP runtime; drain on the next frame opens a buffer-
    /// backed `*lsp:references*` view in the active pane.
    pub(super) fn do_lsp_references_request(&mut self) {
        if let Some(token) = self.pending_references_token.take() {
            token.cancel();
        }
        // Browse-style; not a tag-intent drill-down.
        self.pending_tag_origin = None;
        let Some(uri) = self
            .buffer_uris
            .get(&self.document_buffer_id)
            .cloned()
        else {
            self.set_message(
                EchoLevel::Info,
                "no LSP server attached to current buffer".to_string(),
            );
            return;
        };
        let snapshot = self.document.snapshot();
        let lsp_position = match app_to_lsp_position(&snapshot.buffer, self.cursor) {
            Some(p) => p,
            None => {
                self.set_message(
                    EchoLevel::Error,
                    "references: cursor out of buffer".to_string(),
                );
                return;
            }
        };
        let symbol = word_under_cursor(&snapshot.buffer, self.cursor).unwrap_or_default();
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<ReferencesOutcome>();
        let token = lattice_protocol::CancellationToken::new();
        self.pending_references_rx = Some(rx);
        self.pending_references_token = Some(token.clone());
        let lsp = self.lsp.clone();
        crate::runtime::spawn_on_lsp_runtime(async move {
            let handles: Vec<lattice_lsp::ServerHandle> =
                { lsp.servers_for(&uri) };
            if handles.is_empty() {
                let _ = tx.send(ReferencesOutcome::NoServers);
                return;
            }
            let mut all: Vec<lsp_types::Location> = Vec::new();
            for handle in handles {
                if token.is_cancelled() {
                    return;
                }
                let params = lsp_types::ReferenceParams {
                    text_document_position: lsp_types::TextDocumentPositionParams {
                        text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                        position: lsp_position,
                    },
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                    context: lsp_types::ReferenceContext {
                        include_declaration: true,
                    },
                };
                if let Ok(Some(locs)) = handle.references(params, token.clone()).await {
                    all.extend(locs);
                }
            }
            // Sort + dedup by (uri, range.start).
            all.sort_by(|a, b| {
                let au = a.uri.as_str();
                let bu = b.uri.as_str();
                au.cmp(bu)
                    .then_with(|| a.range.start.line.cmp(&b.range.start.line))
                    .then_with(|| a.range.start.character.cmp(&b.range.start.character))
            });
            all.dedup_by(|a, b| {
                a.uri.as_str() == b.uri.as_str() && a.range.start == b.range.start
            });
            let _ = tx.send(ReferencesOutcome::Found {
                symbol,
                locations: all,
            });
        });
    }

    /// Drain queued references results. The merged list is
    /// rendered as a `*lsp:references*` help buffer and opened
    /// in-pane via the LSP-locations picker; existing follow-
    /// link machinery (`<CR>` on a Source link) handles jumps.
    /// `NoServers` echoes "no LSP server attached"; an empty
    /// `Found(_, [])` echoes "no references for X".
    pub fn drain_pending_references(&mut self) {
        let Some(mut rx) = self.pending_references_rx.take() else {
            return;
        };
        let mut latest: Option<ReferencesOutcome> = None;
        while let Ok(o) = rx.try_recv() {
            latest = Some(o);
        }
        self.pending_references_rx = Some(rx);
        let outcome = match latest {
            Some(o) => o,
            None => return,
        };
        // Delivered; clear the in-flight token regardless of
        // shape so a follow-up gr fires fresh.
        self.pending_references_token = None;
        match outcome {
            ReferencesOutcome::NoServers => {
                self.set_message(
                    EchoLevel::Info,
                    "no LSP server attached to current buffer".to_string(),
                );
            }
            ReferencesOutcome::Found { symbol, locations } => {
                if locations.is_empty() {
                    let label = if symbol.is_empty() {
                        "(symbol)".to_string()
                    } else {
                        format!("\"{symbol}\"")
                    };
                    self.set_message(
                        EchoLevel::Info,
                        format!("no references for {label}"),
                    );
                    return;
                }
                let title = if symbol.is_empty() {
                    "lsp:references".to_string()
                } else {
                    format!("references: {symbol}")
                };
                self.open_lsp_locations_picker(title, &locations);
            }
        }
    }

    /// Jump to an LSP `Location`. If the target is the current
    /// buffer, just move the cursor + push history. If
    /// cross-file, route through `do_edit` so the `:e` machinery
    /// (LSP attach, buffer registry) handles the open; then move
    /// cursor.
    ///
    /// Pushes the *pre-jump* cursor onto position history with
    /// source `PositionSource::PluginPush` so `<C-o>` walks back.
    /// Tagging it as PluginPush (not AutoJump) reflects that the
    /// jump came from an external dispatch (LSP) rather than a
    /// vim-style motion.
    pub(super) fn jump_to_lsp_location(&mut self, loc: &lsp_types::Location) {
        let target_path = match lattice_lsp::actor::uri_to_path(&loc.uri) {
            Some(p) => p,
            None => {
                self.set_message(
                    EchoLevel::Error,
                    format!("definition target uri is not a file: {}", loc.uri.as_str()),
                );
                return;
            }
        };
        // Push pre-jump cursor before doing anything else so a
        // subsequent <C-o> walks back to where we started, not
        // to the target.
        self.push_position_history(self.cursor, super::PositionSource::PluginPush);

        // Same buffer? Just update the cursor.
        let same_buffer = self
            .document
            .path()
            .map(|p| p == target_path)
            .unwrap_or(false);
        if !same_buffer {
            self.do_edit(Some(target_path), false);
        }
        // Convert LSP target position back to App (line, byte).
        let snap = self.document.snapshot();
        let line_text = snap.buffer.line(loc.range.start.line).unwrap_or_default();
        // utf-16 -> utf-8 byte.
        let byte = lattice_lsp::position::utf16_column_to_utf8_byte(
            &line_text,
            loc.range.start.character,
        );
        self.cursor = lattice_protocol::position::Position::new(loc.range.start.line, byte);
    }

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
