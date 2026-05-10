//! LSP feature surface -- App methods for the various
//! `:lsp-*` ex commands (admin / log / trace / status /
//! restart) plus the request-driven LSP feature methods
//! (hover, definition, references, completion, format,
//! rename, code action, document / workspace symbols).
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
//! - LSP request handlers + their drain pumps:
//!   - hover, nav (`gd` / `gD` / `gy` / `gI`),
//!     references, signature help, completion (palette +
//!     Insert-mode popup), document / workspace symbols,
//!     format / format-range, rename, code action.
//!   - drain_pending_lsp_*, drain_pending_completion_resolve,
//!     drain_pending_insert_completion_lsp, etc.
//! - apply_lsp_text_edits / apply_lsp_workspace_edit and
//!   the per-feature outcome appliers
//!   (apply_lsp_completion_accept, apply_lsp_format_outcome,
//!   apply_lsp_rename_outcome, apply_code_action_outcome,
//!   ...).
//! - LSP completion meta sidecar + helpers
//!   (lsp_completion_meta_for, dedup_rendered_by_text,
//!   docs_body_for_selected, selected_needs_resolve).
//! - apply_persistent_lsp_editor_options (lifecycle bridge)
//!   and execute_lsp_command.
//! - resolve_server_id / running_server_ids (pub(super);
//!   shared with picker.rs).
//!
//! What does NOT live here: the LSP wire layer / actor /
//! supervisor (those live in `lattice-lsp`). This module is
//! about App's *consumption* of that layer.

use lattice_protocol::position::Position;

use lattice_grammar::ModalState;

use lattice_protocol::Event;

use super::{
    App, BufferKind, CodeActionOutcome, CodeActionRow, CompletionItemRow, CompletionOutcome,
    CompletionResolveOutcome, EchoLevel, FormatOutcome, HoverOutcome, InsertCompletionLspOutcome,
    LSP_COMPLETION_KIND_ID, LspCompletionMeta, LspNavKind, ReferencesOutcome, RenameOutcome,
    SignatureHelpOutcome, SymbolRow, SymbolsOutcome, TagStackEntry, app_to_lsp_position,
    code_action_kind_glyph, completion_kind_glyph, dedup_rendered_by_text,
    definition_response_to_locations, flatten_document_symbol_response, flatten_workspace_edit,
    hover_contents_to_markdown, is_word_char_byte, last_addressable_line, line_byte_len,
    lsp_position_to_app_byte, prepare_rename_placeholder, signature_help_to_markdown,
    symbol_information_to_row, word_under_cursor, workspace_symbol_to_row,
};
use crate::buffers::BufferId;
use lattice_protocol::edit::Edit;

impl App {
    /// M.5.0: is `lsp-mode` active for `buffer_id`? Every LSP
    /// entry point (hover, completion, diagnostics-render,
    /// document-sync, ...) gates on this in subsequent slices;
    /// returns `true` when the minor is in
    /// `active_modes[buffer_id]`'s minor list, `false`
    /// otherwise (no buffer registered, or mode not active on
    /// it).
    ///
    /// In M.5.0 nothing reads this -- the surface is here so
    /// M.5.2 (auto-activation hook), M.5.3 (lifecycle), and
    /// M.5.4+ (gates) have a single accessor to consume.
    pub fn lsp_mode_enabled_for(&self, buffer_id: BufferId) -> bool {
        self.active_modes
            .get(&buffer_id)
            .map(|modes| modes.has_minor(lattice_lsp::modes::LspMode::mode_id()))
            .unwrap_or(false)
    }

    /// M.6.0: is `mode_id` active on `buffer_id`? Generic minor-
    /// mode accessor used by every M.6 sub-mode reader. Always
    /// returns `false` when no entry exists for `buffer_id` --
    /// matches the umbrella accessor's shape.
    fn minor_mode_enabled_for(
        &self,
        buffer_id: BufferId,
        mode_id: lattice_mode::ModeId,
    ) -> bool {
        self.active_modes
            .get(&buffer_id)
            .map(|modes| modes.has_minor(mode_id))
            .unwrap_or(false)
    }

    /// M.6.0: is `lsp-completion-mode` active on `buffer_id`? Read
    /// by `do_lsp_completion_request` /
    /// `do_lsp_insert_completion_request` and the LSP completion
    /// source filter once M.6.2 / M.6.3 wire the gates.
    pub fn lsp_completion_mode_enabled_for(&self, buffer_id: BufferId) -> bool {
        self.minor_mode_enabled_for(
            buffer_id,
            lattice_lsp::modes::LspCompletionMode::mode_id(),
        )
    }

    /// M.6.0: is `lsp-diagnostics-mode` active on `buffer_id`?
    /// Read by the publish-diagnostics paint pipeline and
    /// `:diag-next` / `:diag-prev` once M.6.3 wires the gate.
    pub fn lsp_diagnostics_mode_enabled_for(&self, buffer_id: BufferId) -> bool {
        self.minor_mode_enabled_for(
            buffer_id,
            lattice_lsp::modes::LspDiagnosticsMode::mode_id(),
        )
    }

    /// M.6.0: is `lsp-hover-mode` active on `buffer_id`? Read by
    /// `do_lsp_hover_request` once M.6.2 wires the gate.
    pub fn lsp_hover_mode_enabled_for(&self, buffer_id: BufferId) -> bool {
        self.minor_mode_enabled_for(
            buffer_id,
            lattice_lsp::modes::LspHoverMode::mode_id(),
        )
    }

    /// M.6.0: is `lsp-signature-mode` active on `buffer_id`?
    pub fn lsp_signature_mode_enabled_for(&self, buffer_id: BufferId) -> bool {
        self.minor_mode_enabled_for(
            buffer_id,
            lattice_lsp::modes::LspSignatureMode::mode_id(),
        )
    }

    /// M.6.0: is `lsp-format-mode` active on `buffer_id`? Gates
    /// `:lsp-format` / `:lsp-format-range` and `onTypeFormatting`.
    pub fn lsp_format_mode_enabled_for(&self, buffer_id: BufferId) -> bool {
        self.minor_mode_enabled_for(
            buffer_id,
            lattice_lsp::modes::LspFormatMode::mode_id(),
        )
    }

    /// M.6.0: is `lsp-rename-mode` active on `buffer_id`?
    pub fn lsp_rename_mode_enabled_for(&self, buffer_id: BufferId) -> bool {
        self.minor_mode_enabled_for(
            buffer_id,
            lattice_lsp::modes::LspRenameMode::mode_id(),
        )
    }

    /// M.6.0: is `lsp-symbols-mode` active on `buffer_id`? Gates
    /// `:lsp-symbols` and `:lsp-workspace-symbol`.
    pub fn lsp_symbols_mode_enabled_for(&self, buffer_id: BufferId) -> bool {
        self.minor_mode_enabled_for(
            buffer_id,
            lattice_lsp::modes::LspSymbolsMode::mode_id(),
        )
    }

    /// M.6.0: is `lsp-code-action-mode` active on `buffer_id`?
    pub fn lsp_code_action_mode_enabled_for(&self, buffer_id: BufferId) -> bool {
        self.minor_mode_enabled_for(
            buffer_id,
            lattice_lsp::modes::LspCodeActionMode::mode_id(),
        )
    }

    /// M.6.0: is `lsp-nav-mode` active on `buffer_id`? Gates
    /// definition / declaration / type-def / impl + references.
    pub fn lsp_nav_mode_enabled_for(&self, buffer_id: BufferId) -> bool {
        self.minor_mode_enabled_for(
            buffer_id,
            lattice_lsp::modes::LspNavMode::mode_id(),
        )
    }

    /// M.5.4: shared gate for every LSP request entry point
    /// (hover / definition / completion / format / rename /
    /// code-action / symbols / signature / references). Returns
    /// `true` when `lsp-mode` is active on the current document;
    /// callers early-return on `false`. A single echo surfaces
    /// the gate state so users discover the mode -- silent gates
    /// are a documented anti-pattern when an editor's defaults
    /// the user expects (`K`, `gd`) suddenly do nothing.
    ///
    /// The echo level is `Info` (not `Warn`) -- gated state is
    /// expected user-controlled, not a misconfiguration.
    pub(super) fn check_lsp_mode_gate(&mut self) -> bool {
        if self.lsp_mode_enabled_for(self.document_buffer_id) {
            return true;
        }
        self.set_message(
            EchoLevel::Info,
            "lsp-mode disabled for this buffer (`:lsp-mode` to enable)".to_string(),
        );
        false
    }

    /// M.6.2: shared gate for a per-feature LSP sub-mode. Checks
    /// the umbrella first (so the user gets one consistent
    /// message-source-of-truth: enable `lsp-mode` first, then
    /// the sub-mode); returns `true` only when both are active.
    /// Echoes at `Info` matching the umbrella's level.
    ///
    /// Used by `do_lsp_*_request` methods that want a
    /// user-discoverable bail message. Insert-mode auto-triggers
    /// (insert completion, signature help, on-type formatting)
    /// skip the echo path entirely and check the bool directly
    /// -- a typed character that doesn't fire isn't a moment to
    /// surface mode state.
    fn check_lsp_sub_mode_gate(
        &mut self,
        sub_mode_id: lattice_mode::ModeId,
        sub_mode_name: &str,
    ) -> bool {
        if !self.check_lsp_mode_gate() {
            return false;
        }
        if self.minor_mode_enabled_for(self.document_buffer_id, sub_mode_id) {
            return true;
        }
        self.set_message(
            EchoLevel::Info,
            format!("{sub_mode_name} disabled for this buffer (`:{sub_mode_name}` to enable)"),
        );
        false
    }

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
        if self.popup_buffer.is_some() {
            self.focus_help_popup();
            return;
        }
        // First K -- fire a fresh hover request. Cancel any
        // in-flight first. (Cancel-stale-work runs before the
        // M.5.4 gate so the prior request's relay loop sees the
        // flip even when the gate is now closed.)
        if let Some(token) = self.pending_hover_token.take() {
            token.cancel();
        }
        // M.6.2: lsp-hover-mode gate (umbrella check inside).
        if !self.check_lsp_sub_mode_gate(
            lattice_lsp::modes::LspHoverMode::mode_id(),
            "lsp-hover-mode",
        ) {
            return;
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

    /// Apply an accepted LSP completion item. Routes the main
    /// insert (with `textEdit.range` honoured when present) plus
    /// `additionalTextEdits` through `apply_lsp_text_edits` so
    /// the whole set lands as one undo unit. Snippet-flavoured
    /// items currently splice the literal body -- placeholder
    /// navigation is in 4.2.g.4 with `lattice-snippet`.
    pub(super) fn apply_lsp_completion_accept(
        &mut self,
        meta: LspCompletionMeta,
        anchor: lattice_protocol::position::Position,
    ) {
        // Main edit: prefer the server-supplied range when
        // present; else replace `[anchor, cursor]`.
        let main_range = match meta.replace_range {
            Some(r) => r,
            None => {
                let start = lsp_types::Position {
                    line: anchor.line,
                    character: lattice_lsp::position::utf8_byte_to_utf16_column(
                        &self
                            .document
                            .snapshot()
                            .buffer
                            .line(anchor.line)
                            .unwrap_or_default(),
                        anchor.byte,
                    ),
                };
                let end = lsp_types::Position {
                    line: self.cursor.line,
                    character: lattice_lsp::position::utf8_byte_to_utf16_column(
                        &self
                            .document
                            .snapshot()
                            .buffer
                            .line(self.cursor.line)
                            .unwrap_or_default(),
                        self.cursor.byte,
                    ),
                };
                lsp_types::Range { start, end }
            }
        };
        // Apply additionalTextEdits + main as one batch via the
        // existing path. Sort + reverse-apply is handled there;
        // pass everything together so undo is atomic.
        let mut edits: Vec<lsp_types::TextEdit> = meta
            .additional_text_edits
            .clone();
        edits.push(lsp_types::TextEdit {
            range: main_range,
            new_text: meta.insert_text.clone(),
        });
        if let Err(e) = self.apply_lsp_text_edits(edits) {
            self.set_message(
                EchoLevel::Error,
                format!("completion: apply failed: {e}"),
            );
            return;
        }
        // Position the cursor at the end of the just-inserted
        // text. Compute it from the inserted text length.
        let inserted_lines: Vec<&str> = meta.insert_text.split('\n').collect();
        if inserted_lines.len() == 1 {
            self.cursor = lattice_protocol::position::Position::new(
                main_range.start.line,
                lattice_lsp::position::utf16_column_to_utf8_byte(
                    &self
                        .document
                        .snapshot()
                        .buffer
                        .line(main_range.start.line)
                        .unwrap_or_default(),
                    main_range.start.character + inserted_lines[0].len() as u32,
                ),
            );
        } else {
            // Multi-line insert (rare for plain completions;
            // common for snippets once 4.2.g.4 lands).
            let last_line_idx =
                main_range.start.line + (inserted_lines.len() as u32 - 1);
            let last_line_text = inserted_lines.last().unwrap_or(&"");
            self.cursor = lattice_protocol::position::Position::new(
                last_line_idx,
                last_line_text.len() as u32,
            );
        }
        // Optional: fire the LSP `command` payload (e.g. server-
        // side post-accept hooks).
        if let Some(cmd) = meta.command.clone() {
            let uri = self
                .buffer_uris
                .get(&self.document_buffer_id)
                .cloned();
            if let Some(uri) = uri {
                let handle = self
                    .lsp
                    .servers_for(&uri)
                    .into_iter()
                    .find(|h| h.capabilities().supports_execute_command());
                self.execute_lsp_command(handle, cmd);
            }
        }
    }

    /// Fire `completionItem/resolve` for the focused candidate
    /// (Phase 4.2.g.3). The original CompletionItem is round-
    /// tripped to the originating server; the response fills in
    /// `documentation` / `additionalTextEdits` / `detail` per
    /// the LSP spec. Drain updates the meta + the docs popup
    /// body in place.
    pub(super) fn do_completion_resolve_focused(&mut self) {
        // Cancel any prior in-flight resolve -- the focus moved
        // to a different candidate.
        if let Some(token) = self.pending_completion_resolve_token.take() {
            token.cancel();
        }
        let Some(state) = self.insert_completion.as_ref() else {
            return;
        };
        let Some(cand) = state.rendered.get(state.selected) else {
            return;
        };
        let Some(meta) = self.lsp_completion_meta_for(cand) else {
            return;
        };
        if meta.resolved {
            return;
        }
        let original = meta.original_item.clone();
        let server_id = meta.server_id.clone();
        // Index of this meta entry, computed by walking the
        // sidecar.
        let lattice_completion::CandidateData::Extension { payload, .. } =
            &cand.raw.data
        else {
            return;
        };
        if payload.len() != 4 {
            return;
        }
        let meta_index = u32::from_le_bytes([
            payload[0],
            payload[1],
            payload[2],
            payload[3],
        ]) as usize;
        // Resolve URI to find the originating server handle.
        let Some(uri) = self
            .buffer_uris
            .get(&self.document_buffer_id)
            .cloned()
        else {
            return;
        };
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<CompletionResolveOutcome>();
        let token = lattice_protocol::CancellationToken::new();
        self.pending_completion_resolve_rx = Some(rx);
        self.pending_completion_resolve_token = Some(token.clone());
        let lsp = self.lsp.clone();
        crate::runtime::spawn_on_lsp_runtime(async move {
            let handle = lsp
                .servers_for(&uri)
                .into_iter()
                .find(|h| h.server_id() == &*server_id);
            let Some(handle) = handle else {
                return;
            };
            if !handle.capabilities().completion_resolve_provider() {
                return;
            }
            if token.is_cancelled() {
                return;
            }
            // `request_with_cancel` takes `&str` method name and
            // a serializable param; the resolved item comes back
            // as `CompletionItem`.
            let pending = handle.request_with_cancel::<
                lsp_types::CompletionItem,
                lsp_types::CompletionItem,
            >(
                "completionItem/resolve",
                original,
                token.clone(),
            );
            let Ok(resolved) = pending.await else {
                return;
            };
            let _ = tx.send(CompletionResolveOutcome {
                meta_index,
                resolved,
            });
        });
    }

    /// Fire `textDocument/completion` for the active Insert-
    /// mode popup (Phase 4.2.g.2). The response merges into
    /// `state.raw` via the per-frame drain. Cancellation token
    /// rides on every keystroke that mutates the query when
    /// `isIncomplete: true`; manual re-triggers always re-fire
    /// fresh.
    ///
    /// Multi-server fan-out + dedup (label + kind) is the
    /// architecture-doc strategy. Items beyond `MAX_LSP_ITEMS`
    /// are dropped.
    pub(super) fn do_lsp_insert_completion_request(&mut self) {
        const MAX_LSP_ITEMS: usize = 500;
        // M.6.2: lsp-completion-mode gate (umbrella implied by
        // M.6.1 cascade: sub-mode can't be on without umbrella).
        // Insert-mode is high-frequency; silent on bail (the user
        // is typing, not invoking a discrete command).
        if !self.lsp_completion_mode_enabled_for(self.document_buffer_id) {
            return;
        }
        if let Some(token) = self.pending_insert_completion_lsp_token.take() {
            token.cancel();
        }
        // Path-completion mode (4.2.g.6 (2/2)) suppresses LSP
        // completion -- the popup is showing filesystem entries.
        if self.completion_in_path_context {
            return;
        }
        // Per-language sources filter (Phase 4.2.g.5 (3b/3)).
        let language = self.active_language_id();
        let effective = self.effective_completion_for(&language);
        let lsp_id = lattice_completion::SourceId::new(
            lattice_completion::LSP_COMPLETION_SOURCE_ID,
        );
        if !effective.source_enabled(&lsp_id) {
            return;
        }
        let Some(uri) = self
            .buffer_uris
            .get(&self.document_buffer_id)
            .cloned()
        else {
            // No URI -- no LSP. Sync sources still populate the
            // popup; just skip the LSP request silently.
            return;
        };
        let snapshot = self.document.snapshot();
        let lsp_position = match app_to_lsp_position(&snapshot.buffer, self.cursor) {
            Some(p) => p,
            None => return,
        };
        // Pull the trigger context out of the popup state so the
        // LSP request faithfully reports `triggerKind`.
        let (lsp_trigger_kind, lsp_trigger_char) = match self
            .insert_completion
            .as_ref()
            .map(|s| s.trigger.clone())
        {
            Some(lattice_completion::CompletionTrigger::TriggerChar(c)) => (
                lsp_types::CompletionTriggerKind::TRIGGER_CHARACTER,
                Some(c.to_string()),
            ),
            Some(lattice_completion::CompletionTrigger::IncompleteRefresh) => (
                lsp_types::CompletionTriggerKind::TRIGGER_FOR_INCOMPLETE_COMPLETIONS,
                None,
            ),
            _ => (lsp_types::CompletionTriggerKind::INVOKED, None),
        };
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<InsertCompletionLspOutcome>();
        let token = lattice_protocol::CancellationToken::new();
        self.pending_insert_completion_lsp_rx = Some(rx);
        self.pending_insert_completion_lsp_token = Some(token.clone());
        let lsp = self.lsp.clone();
        crate::runtime::spawn_on_lsp_runtime(async move {
            let handles: Vec<lattice_lsp::ServerHandle> =
                { lsp.servers_for(&uri) };
            if handles.is_empty() {
                let _ = tx.send(InsertCompletionLspOutcome::NoServers);
                return;
            }
            let mut all: Vec<LspCompletionMeta> = Vec::new();
            let mut any_incomplete = false;
            let mut seen_keys: std::collections::HashSet<(String, String)> =
                std::collections::HashSet::new();
            for handle in handles {
                if token.is_cancelled() {
                    return;
                }
                if !handle.capabilities().supports_completion() {
                    continue;
                }
                let params = lsp_types::CompletionParams {
                    text_document_position: lsp_types::TextDocumentPositionParams {
                        text_document: lsp_types::TextDocumentIdentifier {
                            uri: uri.clone(),
                        },
                        position: lsp_position,
                    },
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                    context: Some(lsp_types::CompletionContext {
                        trigger_kind: lsp_trigger_kind,
                        trigger_character: lsp_trigger_char.clone(),
                    }),
                };
                let Ok(Some(resp)) = handle.completion(params, token.clone()).await
                else {
                    continue;
                };
                let (items, is_incomplete) = match resp {
                    lsp_types::CompletionResponse::Array(items) => (items, false),
                    lsp_types::CompletionResponse::List(list) => {
                        (list.items, list.is_incomplete)
                    }
                };
                if is_incomplete {
                    any_incomplete = true;
                }
                for ci in items {
                    let kind = ci.kind;
                    let label = ci.label.clone();
                    let kind_tag = kind
                        .map(|k| format!("{k:?}"))
                        .unwrap_or_else(|| "none".to_string());
                    let key = (label.clone(), kind_tag);
                    if !seen_keys.insert(key) {
                        continue;
                    }
                    let deprecated = ci
                        .tags
                        .as_ref()
                        .map(|t| t.contains(&lsp_types::CompletionItemTag::DEPRECATED))
                        .unwrap_or(false)
                        || ci.deprecated.unwrap_or(false);
                    // Insert text resolution: textEdit.newText >
                    // insertText > label.
                    let (insert_text, replace_range) = match ci.text_edit.as_ref() {
                        Some(lsp_types::CompletionTextEdit::Edit(te)) => {
                            (te.new_text.clone(), Some(te.range))
                        }
                        Some(lsp_types::CompletionTextEdit::InsertAndReplace(ir)) => {
                            (ir.new_text.clone(), Some(ir.replace))
                        }
                        None => {
                            (ci.insert_text.clone().unwrap_or_else(|| label.clone()), None)
                        }
                    };
                    let documentation = ci.documentation.as_ref().map(|d| match d {
                        lsp_types::Documentation::String(s) => s.clone(),
                        lsp_types::Documentation::MarkupContent(mc) => {
                            mc.value.clone()
                        }
                    });
                    let commit_characters = ci
                        .commit_characters
                        .as_ref()
                        .map(|chars| {
                            chars
                                .iter()
                                .filter_map(|s| s.chars().next())
                                .collect()
                        })
                        .unwrap_or_default();
                    all.push(LspCompletionMeta {
                        label,
                        insert_text,
                        filter_text: ci.filter_text.clone(),
                        sort_text: ci.sort_text.clone(),
                        detail: ci.detail.clone(),
                        documentation,
                        kind,
                        deprecated,
                        preselect: ci.preselect.unwrap_or(false),
                        commit_characters,
                        additional_text_edits: ci
                            .additional_text_edits
                            .clone()
                            .unwrap_or_default(),
                        command: ci.command.clone(),
                        insert_text_format: ci
                            .insert_text_format
                            .unwrap_or(lsp_types::InsertTextFormat::PLAIN_TEXT),
                        replace_range,
                        server_id: std::sync::Arc::from(handle.server_id()),
                        original_item: ci,
                        resolved: false,
                    });
                    if all.len() >= MAX_LSP_ITEMS {
                        break;
                    }
                }
                if all.len() >= MAX_LSP_ITEMS {
                    break;
                }
            }
            let _ = tx.send(InsertCompletionLspOutcome::Items {
                items: all,
                is_incomplete: any_incomplete,
            });
        });
    }

    /// Drain queued `completionItem/resolve` responses -- fill
    /// in the meta entry, update the docs-popup body when the
    /// resolved item is the popup's currently-focused one.
    pub fn drain_pending_completion_resolve(&mut self) {
        let Some(mut rx) = self.pending_completion_resolve_rx.take() else {
            return;
        };
        let mut latest: Option<CompletionResolveOutcome> = None;
        while let Ok(o) = rx.try_recv() {
            latest = Some(o);
        }
        self.pending_completion_resolve_rx = Some(rx);
        let Some(outcome) = latest else {
            return;
        };
        self.pending_completion_resolve_token = None;
        // Fill the meta entry with the resolved fields.
        let Some(meta) = self
            .insert_completion_lsp_meta
            .get_mut(outcome.meta_index)
        else {
            return;
        };
        let resolved = outcome.resolved;
        if let Some(d) = resolved.documentation.as_ref() {
            let body = match d {
                lsp_types::Documentation::String(s) => s.clone(),
                lsp_types::Documentation::MarkupContent(mc) => mc.value.clone(),
            };
            meta.documentation = Some(body);
        }
        if let Some(detail) = resolved.detail.clone() {
            meta.detail = Some(detail);
        }
        if let Some(adds) = resolved.additional_text_edits.clone() {
            meta.additional_text_edits = adds;
        }
        if let Some(cmd) = resolved.command.clone() {
            meta.command = Some(cmd);
        }
        meta.resolved = true;
        // Refresh the docs popup body when this resolve was for
        // the currently-focused candidate.
        let Some(state) = self.insert_completion.as_mut() else {
            return;
        };
        let Some(doc_popup) = state.doc_popup.as_mut() else {
            return;
        };
        let Some(cand) = state.rendered.get(state.selected) else {
            return;
        };
        let payload = match &cand.raw.data {
            lattice_completion::CandidateData::Extension { payload, .. } => payload,
            _ => return,
        };
        if payload.len() != 4 {
            return;
        }
        let active_idx = u32::from_le_bytes([
            payload[0],
            payload[1],
            payload[2],
            payload[3],
        ]) as usize;
        if active_idx != outcome.meta_index {
            return;
        }
        // Build the body from the freshly-resolved meta.
        let detail = self
            .insert_completion_lsp_meta
            .get(outcome.meta_index)
            .and_then(|m| m.detail.clone())
            .filter(|s| !s.is_empty())
            .map(|s| format!("```\n{s}\n```"));
        let docs = self
            .insert_completion_lsp_meta
            .get(outcome.meta_index)
            .and_then(|m| m.documentation.clone())
            .filter(|s| !s.is_empty());
        doc_popup.body = match (detail, docs) {
            (Some(d), Some(b)) => Some(format!("{d}\n\n{b}")),
            (Some(d), None) => Some(d),
            (None, Some(b)) => Some(b),
            (None, None) => Some("(no documentation)".to_string()),
        };
        doc_popup.scroll = 0;
    }

    /// Per-frame drain hook -- merge any LSP completion response
    /// into the active popup's `raw` set, refilter, and update
    /// the `lsp_incomplete` flag.
    pub fn drain_pending_insert_completion_lsp(&mut self) {
        let Some(mut rx) = self.pending_insert_completion_lsp_rx.take() else {
            return;
        };
        let mut latest: Option<InsertCompletionLspOutcome> = None;
        while let Ok(o) = rx.try_recv() {
            latest = Some(o);
        }
        self.pending_insert_completion_lsp_rx = Some(rx);
        let outcome = match latest {
            Some(o) => o,
            None => return,
        };
        self.pending_insert_completion_lsp_token = None;
        let Some(state) = self.insert_completion.as_mut() else {
            // Popup closed before the response arrived; drop it.
            self.insert_completion_lsp_meta.clear();
            return;
        };
        match outcome {
            InsertCompletionLspOutcome::NoServers => {
                // Nothing to merge; sync sources stand alone.
            }
            InsertCompletionLspOutcome::Items {
                items,
                is_incomplete,
            } => {
                // Drop any prior LSP rows from raw + meta.
                state.raw.retain(|c| {
                    !matches!(
                        c.data,
                        lattice_completion::CandidateData::Extension {
                            kind_id: LSP_COMPLETION_KIND_ID,
                            ..
                        }
                    )
                });
                self.insert_completion_lsp_meta.clear();
                // Append fresh items.
                for (i, meta) in items.into_iter().enumerate() {
                    let display = match meta.detail.as_ref() {
                        Some(d) => format!("{}  {}", meta.label, d),
                        None => meta.label.clone(),
                    };
                    let match_text = meta
                        .filter_text
                        .clone()
                        .unwrap_or_else(|| meta.label.clone());
                    let payload = (i as u32).to_le_bytes().to_vec();
                    let mut raw = lattice_completion::RawCandidate::plain(
                        match_text,
                        lattice_completion::CandidateKind::Plain,
                    )
                    .with_source(lattice_completion::SourceId::new(
                        lattice_completion::LSP_COMPLETION_SOURCE_ID,
                    ));
                    raw.display = display;
                    raw.data = lattice_completion::CandidateData::Extension {
                        kind_id: LSP_COMPLETION_KIND_ID,
                        payload,
                    };
                    state.raw.push(raw);
                    self.insert_completion_lsp_meta.push(meta);
                }
                state.lsp_incomplete = is_incomplete;
            }
        }
        // Refilter against the (now-merged) raw set. Inline
        // mirror of refilter_insert_completion's body (we have
        // a mutable borrow on `state` here so calling the
        // helper would re-borrow).
        let matcher = lattice_completion::FuzzyInsertMatcher::new();
        let mut scored: Vec<lattice_completion::ScoredCandidate> = state
            .raw
            .iter()
            .filter_map(|raw| {
                lattice_completion::CandidateMatcher::matches(
                    &matcher,
                    &state.query,
                    raw,
                )
                .map(|(score, ranges)| lattice_completion::ScoredCandidate {
                    raw: raw.clone(),
                    score,
                    match_ranges: ranges,
                })
            })
            .collect();
        let ranker = lattice_completion::InsertRanker::new();
        let freq = &self.completion_accept_freq;
        let config = &self.config;
        ranker.rank_with_bonus(&mut scored, |raw| {
            let priority = match raw.source.as_ref().map(|s| s.as_str()) {
                Some("gen:lsp-completion") => *config
                    .get_typed::<lattice_config::CompletionSourceLspPriority>()
                    .expect("CompletionSourceLspPriority"),
                Some("gen:snippet") => *config
                    .get_typed::<lattice_config::CompletionSourceSnippetPriority>()
                    .expect("CompletionSourceSnippetPriority"),
                Some("gen:buffer-words") => *config
                    .get_typed::<lattice_config::CompletionSourceBufferWordsPriority>()
                    .expect("CompletionSourceBufferWordsPriority"),
                _ => 0,
            }
            .clamp(0, u32::MAX as i64) as u32;
            let freq_bonus = freq
                .get(&(raw.text.clone(), raw.kind))
                .copied()
                .unwrap_or(0)
                .min(lattice_completion::InsertRanker::FREQUENCY_BONUS_CAP);
            priority.saturating_add(freq_bonus)
        });
        state.rendered = scored
            .into_iter()
            .map(lattice_completion::RenderedCandidate::from_scored)
            .collect();
        dedup_rendered_by_text(&mut state.rendered);
        if !state.rendered.is_empty() && state.selected >= state.rendered.len() {
            state.selected = state.rendered.len() - 1;
        }
        if state.rendered.is_empty() {
            // No matches after merge -- close the popup.
            self.insert_completion = None;
            self.insert_completion_lsp_meta.clear();
        }
    }

    /// Drain queued `lattice_lsp::LspLogPushed` events (Phase 4;
    /// M.5.3.b: the event type moved from
    /// `lattice-protocol::Event::LspLogPushed` into
    /// `lattice-lsp::events`). Refreshes any open log / trace
    /// help buffers from the logger snapshot. Called once per
    /// main-loop tick + at the end of any path that pushes a
    /// log record synchronously.
    ///
    /// Cheap when no log buffers are open: the refresh path
    /// short-circuits on `BufferRegistry::help_with_title`
    /// missing the title.
    pub fn drain_lsp_log_events(&mut self) {
        let Some(mut rx) = self.lsp_log_event_rx.take() else {
            return;
        };
        // Coalesce: collect every drained event's scope, then
        // refresh each unique scope at most once.
        let mut subsystem = false;
        let mut server_logs: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut server_traces: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        while let Ok(event) = rx.try_recv() {
            let lattice_lsp::LspLogPushed {
                server_id,
                level,
                source,
                ..
            } = event;
            match server_id {
                None => subsystem = true,
                Some(id) => {
                    let id_owned = id.to_string();
                    if level == "trace" || source == "trace" {
                        server_traces.insert(id_owned);
                    } else {
                        server_logs.insert(id_owned);
                    }
                }
            }
        }
        if subsystem {
            self.refresh_lsp_log_buffer_subsystem();
        }
        for id in server_logs {
            self.refresh_lsp_log_buffer_per_server(&id);
        }
        for id in server_traces {
            self.refresh_lsp_trace_buffer(&id);
        }
        self.lsp_log_event_rx = Some(rx);
    }

    /// Rebuild the `*lsp*` (subsystem-wide) help buffer from the
    /// logger snapshot, preserving cursor + scroll. No-op when
    /// the buffer isn't currently open.
    fn refresh_lsp_log_buffer_subsystem(&mut self) {
        let Some(id) = self.buffers.help_with_title("lsp") else {
            return;
        };
        let new_buf = lattice_lsp::help_views::lsp_global_log_help(&self.lsp_logger)
            .with_markdown_syntax(self.lang_registry.clone());
        self.replace_help_buffer_preserving_cursor(id, new_buf);
    }

    /// Rebuild `*lsp:<server_id>*` from the logger snapshot.
    fn refresh_lsp_log_buffer_per_server(&mut self, server_id: &str) {
        let title = format!("lsp:{server_id}");
        let Some(id) = self.buffers.help_with_title(&title) else {
            return;
        };
        let arc: std::sync::Arc<str> = std::sync::Arc::from(server_id);
        let new_buf = lattice_lsp::help_views::lsp_server_log_help(&self.lsp_logger, &arc)
            .with_markdown_syntax(self.lang_registry.clone());
        self.replace_help_buffer_preserving_cursor(id, new_buf);
    }

    /// Rebuild `*lsp:<server_id>:trace*` from the logger snapshot.
    fn refresh_lsp_trace_buffer(&mut self, server_id: &str) {
        let title = format!("lsp:{server_id}:trace");
        let Some(id) = self.buffers.help_with_title(&title) else {
            return;
        };
        let arc: std::sync::Arc<str> = std::sync::Arc::from(server_id);
        let new_buf = lattice_lsp::help_views::lsp_server_trace_help(&self.lsp_logger, &arc)
            .with_markdown_syntax(self.lang_registry.clone());
        self.replace_help_buffer_preserving_cursor(id, new_buf);
    }

    /// Atomically replace a registry-tracked help buffer's body
    /// with `new_content`, preserving the existing buffer id +
    /// cursor + scroll so the user's view stays put across the
    /// rebuild. Clamps cursor to the new content's line bounds.
    /// Re-seeds the parsed metadata into `buffer_locals[id]` so
    /// live-tail readers (links / anchors / highlights) reflect
    /// the updated parse. Also syncs `App.popup_buffer` (the popup
    /// hot-path mirror) when it points at the same id.
    fn replace_help_buffer_preserving_cursor(
        &mut self,
        id: BufferId,
        new_content: crate::help::HelpContent,
    ) {
        let crate::help::HelpContent {
            buffer: mut new_buf,
            metadata,
        } = new_content;
        let (cur, scr) = match self.buffers.help(id) {
            Some(h) => (h.cursor, h.scroll),
            None => return,
        };
        new_buf.id = id;
        new_buf.cursor = cur;
        new_buf.scroll = scr;
        let line_count = new_buf.line_count() as u32;
        if line_count > 0 && new_buf.cursor.line >= line_count {
            new_buf.cursor.line = line_count - 1;
        }
        if let Some(slot) = self.buffers.help_mut(id) {
            *slot = new_buf;
        }
        // M.3.2.c.5: refresh the locals so the renderer + link/
        // anchor lookups see the updated parse.
        self.seed_help_metadata_locals(id, metadata);
        // M.4 (b): the popup hot-path slot is just the id; the
        // registry copy was updated in place by `*slot = new_buf`
        // above, so there's nothing further to sync.
        let _ = id;
    }

    /// Drain server-initiated `workspace/configuration` requests.
    /// Each request lands as a `lattice_lsp::InboundConfigurationRequest`
    /// carrying section paths + a oneshot for the response.
    /// Server-side keys come in their own namespaces (e.g.
    /// `"rust-analyzer.cargo.features"`); the editor's TOML places
    /// these under an `[lsp.<server>]` umbrella so multiple
    /// servers' keys don't collide. The drain prepends `lsp.` to
    /// the requested section before walking the tree.
    pub fn drain_inbound_configuration_requests(&mut self) {
        let Some(mut rx) = self.pending_configuration_rx.take() else {
            return;
        };
        let mut requests: Vec<lattice_lsp::InboundConfigurationRequest> = Vec::new();
        while let Ok(req) = rx.try_recv() {
            requests.push(req);
        }
        self.pending_configuration_rx = Some(rx);
        for req in requests {
            let values: Vec<serde_json::Value> = req
                .sections
                .iter()
                .map(|section| self.lookup_lsp_config_section(section))
                .collect();
            let _ = req.response.send(values);
        }
    }

    /// Look up a server-supplied `section` path in the cached
    /// TOML tree at `lsp.<section>`. Returns `Value::Null` when
    /// the path is missing or the TOML value can't be converted
    /// to JSON. Empty section ("all") returns the whole `lsp`
    /// sub-tree.
    fn lookup_lsp_config_section(&self, section: &str) -> serde_json::Value {
        let path = if section.is_empty() {
            "lsp".to_string()
        } else {
            format!("lsp.{section}")
        };
        let toml_value =
            match lattice_config::lookup_dotted_path(&self.lsp_config_tree, &path) {
                Some(v) => v,
                None => return serde_json::Value::Null,
            };
        // toml::Value -> serde_json::Value via the round-trip
        // serialiser. Both crates speak serde, so this is the
        // direct path -- no manual variant matching.
        serde_json::to_value(toml_value).unwrap_or(serde_json::Value::Null)
    }

    /// Drain server-initiated `workspace/applyEdit` requests
    /// (Phase 4.3). Each request lands as a
    /// `lattice_lsp::InboundApplyEdit` carrying a typed
    /// `WorkspaceEdit` + a oneshot for the response. We flatten
    /// the edit into per-file `Vec<TextEdit>` batches (same
    /// `flatten_workspace_edit` path the `:rename` drain uses),
    /// apply each, and reply via the oneshot.
    ///
    /// Apply semantics mirror `apply_rename_workspace_edit`:
    /// edits to the active buffer land directly via
    /// `apply_lsp_text_edits`; cross-file edits open the target
    /// via `do_edit` and apply there. Failures on individual
    /// files echo a warning but don't roll back successfully-
    /// applied files.
    pub fn drain_inbound_apply_edits(&mut self) {
        let Some(mut rx) = self.pending_apply_edit_rx.take() else {
            return;
        };
        let mut requests: Vec<lattice_lsp::InboundApplyEdit> = Vec::new();
        while let Ok(req) = rx.try_recv() {
            requests.push(req);
        }
        self.pending_apply_edit_rx = Some(rx);
        for req in requests {
            let outcome = self.apply_inbound_workspace_edit(&req.server_id, req.label.as_deref(), req.edit);
            let _ = req.response.send(outcome);
        }
    }

    /// Apply one server-initiated WorkspaceEdit to the editor's
    /// buffers. Returns the `lattice_lsp::ApplyEditOutcome` the
    /// actor's response task ferries back to the server.
    fn apply_inbound_workspace_edit(
        &mut self,
        server_id: &std::sync::Arc<str>,
        label: Option<&str>,
        edit: lsp_types::WorkspaceEdit,
    ) -> lattice_lsp::ApplyEditOutcome {
        let per_file = flatten_workspace_edit(edit);
        if per_file.is_empty() {
            // Spec: when the edit is empty there's nothing to do;
            // reply applied=true with a clarifying note.
            return lattice_lsp::ApplyEditOutcome {
                applied: true,
                failure_reason: Some("empty workspace edit".into()),
            };
        }
        let mut applied_files = 0usize;
        let mut failed_files: Vec<String> = Vec::new();
        let mut total_edits = 0usize;
        for (uri, edits) in per_file {
            let target_path = match lattice_lsp::actor::uri_to_path(&uri) {
                Some(p) => p,
                None => {
                    failed_files.push(format!("{uri:?} (malformed URI)"));
                    continue;
                }
            };
            let edit_count = edits.len();
            if self
                .document
                .path()
                .map(|p| p == target_path)
                .unwrap_or(false)
            {
                if let Err(e) = self.apply_lsp_text_edits(edits) {
                    failed_files.push(format!("{}: {e}", target_path.display()));
                    continue;
                }
                applied_files += 1;
                total_edits += edit_count;
            } else {
                // Cross-file edits: open via `:e` then apply.
                self.do_edit(Some(target_path.clone()), false);
                if matches!(
                    self.last_message.as_ref().map(|m| m.level),
                    Some(EchoLevel::Error)
                ) {
                    failed_files
                        .push(format!("{}: open failed", target_path.display()));
                    continue;
                }
                if let Err(e) = self.apply_lsp_text_edits(edits) {
                    failed_files.push(format!("{}: {e}", target_path.display()));
                    continue;
                }
                applied_files += 1;
                total_edits += edit_count;
            }
        }
        // Echo a status line for the user.
        let label_text = label.map(|l| format!(" `{l}`")).unwrap_or_default();
        let summary = if failed_files.is_empty() {
            format!(
                "{server_id}: applyEdit{label_text} -> {total_edits} edit{} across {applied_files} file{}",
                if total_edits == 1 { "" } else { "s" },
                if applied_files == 1 { "" } else { "s" },
            )
        } else {
            format!(
                "{server_id}: applyEdit{label_text} partial -- {applied_files} ok, {} failed: {}",
                failed_files.len(),
                failed_files.join("; "),
            )
        };
        let echo_level = if failed_files.is_empty() {
            EchoLevel::Info
        } else {
            EchoLevel::Warn
        };
        self.set_message(echo_level, summary.clone());
        lattice_lsp::ApplyEditOutcome {
            applied: applied_files > 0,
            failure_reason: if failed_files.is_empty() {
                None
            } else {
                Some(format!(
                    "{} file{} failed: {}",
                    failed_files.len(),
                    if failed_files.len() == 1 { "" } else { "s" },
                    failed_files.join("; "),
                ))
            },
        }
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
        // M.6.2: lsp-format-mode gate. Insert-mode trigger; silent
        // (same shape as completion -- typed character that
        // doesn't fire isn't a moment to surface mode state).
        if !self.lsp_format_mode_enabled_for(self.document_buffer_id) {
            return;
        }
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
        // M.6.2: lsp-rename-mode gate (umbrella check inside).
        if !self.check_lsp_sub_mode_gate(
            lattice_lsp::modes::LspRenameMode::mode_id(),
            "lsp-rename-mode",
        ) {
            return;
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
        // M.6.2: lsp-code-action-mode gate (after cancel-stale-work).
        if !self.check_lsp_sub_mode_gate(
            lattice_lsp::modes::LspCodeActionMode::mode_id(),
            "lsp-code-action-mode",
        ) {
            return;
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
        // M.6.2: lsp-completion-mode gate (after cancel-stale-work).
        if !self.check_lsp_sub_mode_gate(
            lattice_lsp::modes::LspCompletionMode::mode_id(),
            "lsp-completion-mode",
        ) {
            return;
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
        // M.6.2: lsp-format-mode gate (after cancel-stale-work).
        if !self.check_lsp_sub_mode_gate(
            lattice_lsp::modes::LspFormatMode::mode_id(),
            "lsp-format-mode",
        ) {
            return;
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
        // M.6.2: lsp-symbols-mode gate (after cancel-stale-work).
        if !self.check_lsp_sub_mode_gate(
            lattice_lsp::modes::LspSymbolsMode::mode_id(),
            "lsp-symbols-mode",
        ) {
            return;
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
        // M.6.2: lsp-symbols-mode gate (after cancel-stale-work).
        if !self.check_lsp_sub_mode_gate(
            lattice_lsp::modes::LspSymbolsMode::mode_id(),
            "lsp-symbols-mode",
        ) {
            return;
        }
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
        // M.6.2: lsp-signature-mode gate (after cancel-stale-work).
        // Insert-mode auto-trigger; silent (matches completion /
        // on-type-format -- typed character that doesn't fire
        // isn't a moment to surface mode state).
        if !self.lsp_signature_mode_enabled_for(self.document_buffer_id) {
            return;
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
        // M.6.2: lsp-nav-mode gate (after cancel-stale-work).
        // Keymap-driven (`gd` / `gD` / `gy` / `gI`); echo so
        // users find out why nothing happened when nav is gated.
        if !self.check_lsp_sub_mode_gate(
            lattice_lsp::modes::LspNavMode::mode_id(),
            "lsp-nav-mode",
        ) {
            return;
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
        // M.6.2: lsp-nav-mode gate (after cancel-stale-work).
        // `gr` is part of the nav family.
        if !self.check_lsp_sub_mode_gate(
            lattice_lsp::modes::LspNavMode::mode_id(),
            "lsp-nav-mode",
        ) {
            return;
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
        let buffer = lattice_lsp::help_views::lsp_status_help(&self.lsp)
            .with_markdown_syntax(self.lang_registry.clone());
        self.display_buffer(
            buffer,
            lattice_core::ui::display::BufferDisplayCategory::LspStatus,
        );
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

    /// Open `*lsp:<server_id>*` in the active pane via the
    /// in-pane help registry path. Used by both the picker
    /// accept dispatcher and the direct ex-command short path
    /// when only one instance matches.
    pub(super) fn open_lsp_log_in_pane(&mut self, server_id: &str) {
        let arc: std::sync::Arc<str> = std::sync::Arc::from(server_id);
        let buffer = lattice_lsp::help_views::lsp_server_log_help(&self.lsp_logger, &arc)
            .with_markdown_syntax(self.lang_registry.clone());
        self.display_buffer(
            buffer,
            lattice_core::ui::display::BufferDisplayCategory::LspLog,
        );
    }

    /// Open `*lsp:<server_id>:trace*` in the active pane. Pure
    /// view -- the trace toggle is `:lsp-trace <server>` and is
    /// independent of opening / closing this buffer.
    pub(super) fn open_lsp_trace_log_in_pane(&mut self, server_id: &str) {
        let arc: std::sync::Arc<str> = std::sync::Arc::from(server_id);
        let buffer = lattice_lsp::help_views::lsp_server_trace_help(&self.lsp_logger, &arc)
            .with_markdown_syntax(self.lang_registry.clone());
        self.display_buffer(
            buffer,
            lattice_core::ui::display::BufferDisplayCategory::LspLog,
        );
    }

    /// Helper: publish a position-only change event. Cheap
    /// stand-in for whatever the rest of the App uses to
    /// signal cursor moves. Currently a no-op since the
    /// renderer reads cursor directly; reserved for future
    /// position-history pushes.
    pub(super) fn publish_position_change(&self) {
        // 4.1.d.iv: position history hook reserved -- a real
        // PluginPush entry lands here when the position-history
        // wiring catches up.
    }

    /// Resolve a user-supplied server name to a canonical server
    /// id. Tries, in order:
    ///
    /// 1. Exact id match against running actors (the common case
    ///    once a buffer has attached).
    /// 2. Exact id match against registered configs (so
    ///    `:lsp-trace rust` works pre-spawn -- e.g. enable trace
    ///    before opening the first .rs file).
    /// 3. Binary file-name (or stem) match against configs (so
    ///    `:lsp-trace rust-analyzer` resolves to the `rust` actor
    ///    id when the user types the binary they recognise).
    ///
    /// Returns `None` when none matches.
    pub(super) fn resolve_server_id(&self, name: &str) -> Option<String> {
        for ((_, sid), _) in self.lsp.running_actors() {
            if sid == name {
                return Some(sid);
            }
        }
        for cfg in self.lsp.configs() {
            if cfg.id == name {
                return Some(cfg.id.clone());
            }
            let file = cfg
                .binary
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            let stem = file.trim_end_matches(".exe");
            if file == name || stem == name {
                return Some(cfg.id.clone());
            }
        }
        None
    }

    /// Distinct server ids of every running actor. Used in echo
    /// messages so the user sees what's available.
    pub(super) fn running_server_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .lsp
            .running_actors()
            .into_iter()
            .map(|((_, sid), _)| sid)
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use crate::app::*;
    use crate::app::test_helpers::{app_with, seed_diags_at_lines};

    #[test]
    fn lsp_mode_gates_document_changed_typed_event_at_publish_site() {
        // M.5.5: the LSP fan-in subscribes to
        // `LspDocumentChanged` (typed bus), and App's
        // `publish_document_changed` gates the publish_typed
        // call on `lsp_mode_enabled_for`. This test verifies
        // the gate at the publish site:
        // - lsp-mode off → no LspDocumentChanged emitted.
        // - lsp-mode on  → LspDocumentChanged emitted on edit.
        let mut a = app_with("xx", 10);
        let (tx, mut rx) =
            tokio::sync::mpsc::unbounded_channel::<lattice_lsp::LspDocumentChanged>();
        a.event_bus.subscribe_typed(tx);
        // Default (no path → lsp-mode off): drive an edit; no
        // typed event should reach the subscriber.
        a.apply(Action::Insert("a".into()));
        assert!(
            rx.try_recv().is_err(),
            "lsp-mode off should suppress LspDocumentChanged"
        );
        // Activate lsp-mode and edit again -- now the typed
        // event should publish.
        a.toggle_mode_by_name("lsp-mode");
        a.apply(Action::Insert("b".into()));
        let received = rx.try_recv();
        assert!(
            received.is_ok(),
            "lsp-mode on should emit LspDocumentChanged on edit"
        );
    }

    #[test]
    fn lsp_mode_round_trip_end_to_end() {
        // M.5.7: end-to-end gate exercise across one buffer's
        // lifetime. Open a *.rs file (auto-activates lsp-mode
        // per M.5.2) -> verify the gate is open. Toggle off ->
        // every observable LSP signal silences (request gate
        // echoes, document-sync typed event suppressed, render
        // gate suppresses diagnostics + modeline segment).
        // Toggle on again -> gate opens, signals resume.
        use crate::app::test_helpers::app_with_path;
        let mut a = app_with_path("fn main() {}", 5, std::path::PathBuf::from("foo.rs"));
        let id = a.pane_tree.active().buffer_id;

        // Auto-activated on file open via M.5.2.
        assert!(a.lsp_mode_enabled_for(id), "M.5.2 auto-activation");

        // Subscribe to LspBufferDetached + LspDocumentChanged so
        // the toggle-off path (M.5.3 detach) and the gated
        // edit path (M.5.5) are observable end-to-end.
        let (detach_tx, mut detach_rx) =
            tokio::sync::mpsc::unbounded_channel::<lattice_lsp::LspBufferDetached>();
        a.event_bus.subscribe_typed(detach_tx);
        let (changed_tx, mut changed_rx) =
            tokio::sync::mpsc::unbounded_channel::<lattice_lsp::LspDocumentChanged>();
        a.event_bus.subscribe_typed(changed_tx);

        // ---- toggle off: every gate closes. ----
        a.toggle_mode_by_name("lsp-mode");
        assert!(!a.lsp_mode_enabled_for(id));
        // M.5.3 detach event fires.
        assert!(
            detach_rx.try_recv().is_ok(),
            "expected LspBufferDetached on toggle off"
        );
        // M.5.4 request gate: hover echoes the gate message.
        a.apply(Action::LspHoverRequest);
        let msg = a.last_message.as_ref().expect("gate echo");
        assert!(
            msg.text.contains("lsp-mode disabled"),
            "expected gate echo, got: {}",
            msg.text
        );
        // M.5.5 sync gate: edits don't publish LspDocumentChanged.
        a.apply(Action::Insert("a".into()));
        assert!(
            changed_rx.try_recv().is_err(),
            "lsp-mode off should suppress LspDocumentChanged"
        );

        // ---- toggle on: gate opens; signals resume. ----
        a.toggle_mode_by_name("lsp-mode");
        assert!(a.lsp_mode_enabled_for(id));
        a.apply(Action::Insert("b".into()));
        assert!(
            changed_rx.try_recv().is_ok(),
            "lsp-mode on should re-emit LspDocumentChanged"
        );
    }

    #[test]
    fn lsp_mode_off_gates_request_entry_points_with_info_echo() {
        // M.5.4: `lsp-mode` off means LSP request entry points
        // bail with a discoverable echo (so users don't think
        // their bindings are broken). Verified for the
        // ex-command-driven path; keymap-driven (insert-mode
        // completion / signature) are silent by design.
        let mut a = app_with("xx", 10);
        // Default: no auto-activation (no path).
        assert!(!a.lsp_mode_enabled_for(a.document_buffer_id));
        a.apply(Action::LspHoverRequest);
        let msg = a.last_message.as_ref().expect("gate echo");
        assert_eq!(msg.level, EchoLevel::Info);
        assert!(
            msg.text.contains("lsp-mode disabled"),
            "expected lsp-mode-disabled echo, got: {}",
            msg.text
        );
    }

    #[test]
    fn lsp_hover_mode_off_with_umbrella_on_echoes_sub_mode_message() {
        // M.6.2: when umbrella is on but sub-mode is off, the
        // gate echoes the *sub-mode's* name -- the user knows
        // exactly which switch to flip.
        let mut a = app_with("xx", 10);
        a.toggle_mode_by_name("lsp-mode");
        // Cascade activated lsp-hover-mode; toggle it off
        // independently.
        a.toggle_mode_by_name("lsp-hover-mode");
        assert!(a.lsp_mode_enabled_for(a.document_buffer_id));
        assert!(!a.lsp_hover_mode_enabled_for(a.document_buffer_id));
        // Hover request now bails with sub-mode echo (umbrella
        // is on, so the umbrella check inside the helper passes).
        a.apply(Action::LspHoverRequest);
        let msg = a.last_message.as_ref().expect("gate echo");
        assert_eq!(msg.level, EchoLevel::Info);
        assert!(
            msg.text.contains("lsp-hover-mode disabled"),
            "expected lsp-hover-mode-disabled echo, got: {}",
            msg.text
        );
    }

    #[test]
    fn lsp_format_mode_off_gates_format_request() {
        // M.6.2: independent disable of `lsp-format-mode`. Format
        // requests echo the sub-mode's name; other LSP requests
        // (hover, nav) keep working in the same buffer.
        let mut a = app_with("xx", 10);
        a.toggle_mode_by_name("lsp-mode");
        a.toggle_mode_by_name("lsp-format-mode");
        // Format gates. (Tested via the do_lsp_format_request
        // method directly -- the dispatch path through
        // ex-commands also routes here.)
        a.do_lsp_format_request(false);
        let msg = a.last_message.as_ref().expect("format gate echo");
        assert!(
            msg.text.contains("lsp-format-mode disabled"),
            "expected lsp-format-mode-disabled echo, got: {}",
            msg.text
        );
    }

    #[test]
    fn lsp_nav_mode_off_gates_definition_request() {
        // M.6.2: nav family (`gd` / `gD` / `gy` / `gI` / `gr`)
        // shares one sub-mode (`lsp-nav-mode`).
        let mut a = app_with("xx", 10);
        a.toggle_mode_by_name("lsp-mode");
        a.toggle_mode_by_name("lsp-nav-mode");
        a.do_lsp_definition_request();
        let msg = a.last_message.as_ref().expect("nav gate echo");
        assert!(
            msg.text.contains("lsp-nav-mode disabled"),
            "expected lsp-nav-mode-disabled echo, got: {}",
            msg.text
        );
    }

    #[test]
    fn umbrella_off_wins_over_sub_mode_state() {
        // M.6.2: when the umbrella is off, the sub-mode message
        // never fires -- the umbrella check is the first thing
        // every gate does. The user sees one consistent message
        // ("enable lsp-mode first") rather than a stack of
        // sub-mode-disabled echoes.
        let mut a = app_with("xx", 10);
        // Activate umbrella + sub-modes via cascade, then turn
        // umbrella off. Sub-modes also flip off via cascade-off
        // — but even hypothetically a stale sub-mode entry
        // wouldn't bypass the umbrella check.
        a.toggle_mode_by_name("lsp-mode");
        a.toggle_mode_by_name("lsp-mode");
        assert!(!a.lsp_mode_enabled_for(a.document_buffer_id));
        a.apply(Action::LspHoverRequest);
        let msg = a.last_message.as_ref().expect("umbrella echo");
        // Umbrella echo, not sub-mode echo.
        assert!(
            msg.text.contains("lsp-mode disabled") &&
            !msg.text.contains("lsp-hover-mode"),
            "expected umbrella echo (not sub-mode echo), got: {}",
            msg.text
        );
    }

    #[test]
    fn lsp_mode_enabled_for_returns_false_by_default() {
        // M.5.0: a freshly-opened buffer has no `lsp-mode`
        // activated yet. Auto-activation lands in M.5.2 via the
        // `MajorEntered` event hook; until then the accessor is
        // false everywhere.
        let a = app_with("fn main() {}", 5);
        let id = a.pane_tree.active().buffer_id;
        assert!(!a.lsp_mode_enabled_for(id));
    }

    #[test]
    fn lsp_mode_enabled_for_tracks_minor_activation() {
        // M.5.0: activating `lsp-mode` through the registry
        // flips the accessor. M.5.3 will wrap this in actual
        // `:lsp-mode` toggle / auto-activation flow; for now
        // we drive the registry directly.
        let mut a = app_with("fn main() {}", 5);
        let id = a.pane_tree.active().buffer_id;
        let proto_id = lattice_protocol::ids::BufferId::new(id.0 as u64);
        let mut active = a.active_modes.remove(&id).unwrap_or_default();
        let mut locals = a.buffer_locals.remove(&id).unwrap_or_default();
        a.mode_registry
            .activate_minor(
                &mut active,
                &mut locals,
                proto_id,
                lattice_lsp::modes::LspMode::mode_id(),
                lattice_mode::CapabilitySet::empty(),
            )
            .expect("activate lsp-mode");
        a.active_modes.insert(id, active);
        a.buffer_locals.insert(id, locals);
        assert!(a.lsp_mode_enabled_for(id));
    }

    #[test]
    fn hover_dismisses_on_document_cursor_motion() {
        // Vim/emacs UX: any motion off the hovered symbol drops
        // the popup. Apply a hover popup directly (skipping the
        // async LSP path), move the cursor, assert dismissal.
        let mut a = app_with("fn main() {}\nlet x = 1;\n", 5);
        a.do_open_hover("hover body");
        assert!(a.popup_buffer.is_some());
        // State A: focus still on doc, prev_pane_for_help is None.
        assert!(a.prev_pane_for_help.is_none());
        assert!(matches!(a.active_buffer, BufferKind::Document));
        // Drive a real motion through `apply` (`l` -- char-right).
        let inv = lattice_grammar::CommandInvocation::of(a.builtins.char_right.0);
        a.apply(Action::Invoke(inv));
        assert!(
            a.popup_buffer.is_none(),
            "hover popup should dismiss on cursor motion in State A"
        );
    }

    #[test]
    fn hover_does_not_dismiss_when_cursor_unchanged() {
        // No-op actions (e.g. setting a no-arg ex command,
        // an out-of-bounds motion that clamps in place) must not
        // dismiss the popup. Use a count-only push (`5`) which
        // doesn't move the cursor.
        let mut a = app_with("fn main() {}\n", 5);
        a.do_open_hover("hover body");
        assert!(a.popup_buffer.is_some());
        a.apply(Action::PushDigit(5));
        assert!(
            a.popup_buffer.is_some(),
            "hover should survive a count-prefix push"
        );
    }

    #[test]
    fn hover_open_populates_help_buffer() {
        let mut a = app_with("alpha\nbeta\ngamma", 10);
        a.cursor = Position::new(1, 2);
        a.command_line = "hover documentation".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.popup_help().expect("hover open");
        assert_eq!(h.title, "hover");
        assert!(h.content.as_string().contains("documentation"));
        // State A: focus stays on doc.
        assert!(matches!(a.active_buffer, BufferKind::Document));
        assert!(a.prev_pane_for_help.is_none());
    }

    #[test]
    fn hover_close_dismisses_popup() {
        let mut a = app_with("xx", 10);
        a.command_line = "hover x".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert!(a.popup_buffer.is_some());
        a.command_line = "HoverClose".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert!(a.popup_buffer.is_none());
    }

    #[test]
    fn hover_with_no_arg_uses_placeholder() {
        let mut a = app_with("xx", 10);
        a.command_line = "hover".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.popup_help().expect("hover open");
        assert!(h.content.as_string().contains("empty"));
    }

    #[test]
    fn hover_contents_scalar_string_renders_verbatim() {
        let m = lsp_types::HoverContents::Scalar(lsp_types::MarkedString::String(
            "fn foo() -> u32".into(),
        ));
        assert_eq!(super::hover_contents_to_markdown(&m), "fn foo() -> u32");
    }

    #[test]
    fn hover_contents_language_string_renders_as_fenced_block() {
        let m = lsp_types::HoverContents::Scalar(lsp_types::MarkedString::LanguageString(
            lsp_types::LanguageString {
                language: "rust".into(),
                value: "let x: u32 = 5;".into(),
            },
        ));
        let md = super::hover_contents_to_markdown(&m);
        assert!(md.contains("```rust"));
        assert!(md.contains("let x: u32 = 5;"));
        assert!(md.ends_with("```"));
    }

    #[test]
    fn hover_contents_array_joins_with_double_newline() {
        let m = lsp_types::HoverContents::Array(vec![
            lsp_types::MarkedString::String("first".into()),
            lsp_types::MarkedString::String("second".into()),
        ]);
        let md = super::hover_contents_to_markdown(&m);
        assert_eq!(md, "first\n\nsecond");
    }

    #[test]
    fn hover_contents_markup_uses_value_as_markdown() {
        let m = lsp_types::HoverContents::Markup(lsp_types::MarkupContent {
            kind: lsp_types::MarkupKind::Markdown,
            value: "# heading\n\nbody".into(),
        });
        assert_eq!(super::hover_contents_to_markdown(&m), "# heading\n\nbody");
    }

    #[test]
    fn lsp_hover_request_with_no_uri_echoes_no_lsp_attached() {
        // Initial document has no path, so no URI mapping; the
        // request should set an info message and not panic.
        // M.5.4: gate is checked first, so we activate lsp-mode
        // explicitly to test the URI-bail path the original test
        // was probing.
        let mut a = app_with("xx", 10);
        a.toggle_mode_by_name("lsp-mode");
        a.apply(Action::LspHoverRequest);
        let msg = a.last_message.as_ref().expect("echo");
        assert_eq!(msg.level, EchoLevel::Info);
        assert!(msg.text.contains("no LSP server"));
    }

    #[test]
    fn lsp_hover_request_pre_cancels_in_flight_token() {
        // Two K presses in a row: the first one's token must be
        // flipped before the second's request fires, so a slow
        // first response gets dropped by the relay's cancel-aware
        // poll loop.
        let mut a = app_with("xx", 10);
        // Manually install an in-flight token.
        let stale = lattice_protocol::CancellationToken::new();
        a.pending_hover_token = Some(stale.clone());
        // Trigger another hover. With no LSP attached the new
        // request bails on the URI lookup, but the cancel of the
        // previous token should still happen first.
        a.apply(Action::LspHoverRequest);
        assert!(
            stale.is_cancelled(),
            "prior in-flight hover token should flip on a new K press"
        );
    }

    #[test]
    fn drain_pending_hover_body_outcome_opens_popup() {
        let mut a = app_with("xx", 10);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<crate::app::HoverOutcome>();
        a.pending_hover_rx = Some(rx);
        a.pending_hover_token = Some(lattice_protocol::CancellationToken::new());
        tx.send(crate::app::HoverOutcome::Body("**bold body**".into()))
            .unwrap();
        a.drain_pending_hover();
        let h = a.popup_help().expect("popup");
        assert!(h.content.as_string().contains("**bold body**"));
        // State A entry: focus still on the doc.
        assert!(matches!(a.active_buffer, BufferKind::Document));
        assert!(a.prev_pane_for_help.is_none());
        assert!(
            a.pending_hover_token.is_none(),
            "delivering the outcome should clear the in-flight token"
        );
    }

    #[test]
    fn drain_pending_hover_no_body_outcome_echoes_no_hover_info() {
        // Regression for the silent-K-press symptom: if every
        // attached server replies with empty contents,
        // `drain_pending_hover` should echo a clear "no hover
        // info" so the user knows their K press was received.
        let mut a = app_with("xx", 10);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<crate::app::HoverOutcome>();
        a.pending_hover_rx = Some(rx);
        a.pending_hover_token = Some(lattice_protocol::CancellationToken::new());
        tx.send(crate::app::HoverOutcome::NoBody { servers_tried: 1 })
            .unwrap();
        a.drain_pending_hover();
        assert!(a.popup_buffer.is_none(), "no popup for empty hover");
        let msg = a.last_message.as_ref().expect("echo on no-hover-info");
        assert_eq!(msg.level, EchoLevel::Info);
        assert!(
            msg.text.contains("no hover info"),
            "expected 'no hover info' echo; got `{}`",
            msg.text
        );
    }

    #[test]
    fn drain_pending_hover_no_servers_outcome_echoes_warn() {
        // Buffer URI maps to no attached servers (e.g. spawn
        // failed at boot). The user gets a Warn echo pointing at
        // :lsp-status / :lsp-log so they can investigate.
        let mut a = app_with("xx", 10);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<crate::app::HoverOutcome>();
        a.pending_hover_rx = Some(rx);
        a.pending_hover_token = Some(lattice_protocol::CancellationToken::new());
        tx.send(crate::app::HoverOutcome::NoServers).unwrap();
        a.drain_pending_hover();
        let msg = a
            .last_message
            .as_ref()
            .expect("echo on no-servers-attached");
        assert_eq!(msg.level, EchoLevel::Warn);
        assert!(
            msg.text.contains("no LSP servers"),
            "expected NoServers warn echo; got `{}`",
            msg.text
        );
    }

    #[test]
    fn drain_pending_hover_idle_channel_is_noop() {
        let mut a = app_with("xx", 10);
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel::<crate::app::HoverOutcome>();
        a.pending_hover_rx = Some(rx);
        a.drain_pending_hover();
        assert!(a.popup_buffer.is_none());
        assert!(a.last_message.is_none());
    }

    #[test]
    fn app_to_lsp_position_converts_utf8_byte_to_utf16_column() {
        let buf = lattice_core::Buffer::from_text("hello\nαβγ\nworld\n");
        // Line 1 (αβγ): 2-byte UTF-8 chars; byte 4 = end of β.
        // utf-16 column at byte 4: α (1 unit) + β (1 unit) = 2.
        let p = super::app_to_lsp_position(&buf, Position::new(1, 4)).expect("in-range");
        assert_eq!(p.line, 1);
        assert_eq!(p.character, 2);
    }

    #[test]
    fn app_to_lsp_position_returns_none_for_out_of_range_line() {
        let buf = lattice_core::Buffer::from_text("only-one-line\n");
        assert!(super::app_to_lsp_position(&buf, Position::new(99, 0)).is_none());
    }

    fn fake_uri(path: &str) -> lsp_types::Uri {
        use std::str::FromStr;
        lsp_types::Uri::from_str(&format!("file://{path}")).unwrap()
    }

    fn loc(path: &str, line: u32, col: u32) -> lsp_types::Location {
        lsp_types::Location {
            uri: fake_uri(path),
            range: lsp_types::Range {
                start: lsp_types::Position {
                    line,
                    character: col,
                },
                end: lsp_types::Position {
                    line,
                    character: col + 1,
                },
            },
        }
    }

    #[test]
    fn definition_response_scalar_flattens_to_one_location() {
        let resp = lsp_types::GotoDefinitionResponse::Scalar(loc("/x.rs", 1, 2));
        let v = super::definition_response_to_locations(resp);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].range.start.line, 1);
    }

    #[test]
    fn definition_response_array_flattens_verbatim() {
        let resp = lsp_types::GotoDefinitionResponse::Array(vec![
            loc("/a.rs", 0, 0),
            loc("/b.rs", 5, 5),
        ]);
        let v = super::definition_response_to_locations(resp);
        assert_eq!(v.len(), 2);
    }

    #[test]
    fn definition_response_link_uses_target_selection_range() {
        // Link variant carries richer per-result info; we use
        // target_selection_range (narrower) for jumps.
        let link = lsp_types::LocationLink {
            origin_selection_range: None,
            target_uri: fake_uri("/x.rs"),
            target_range: lsp_types::Range {
                start: lsp_types::Position {
                    line: 0,
                    character: 0,
                },
                end: lsp_types::Position {
                    line: 10,
                    character: 0,
                },
            },
            target_selection_range: lsp_types::Range {
                start: lsp_types::Position {
                    line: 5,
                    character: 4,
                },
                end: lsp_types::Position {
                    line: 5,
                    character: 7,
                },
            },
        };
        let resp = lsp_types::GotoDefinitionResponse::Link(vec![link]);
        let v = super::definition_response_to_locations(resp);
        assert_eq!(v.len(), 1);
        // Should be the target_selection_range, not target_range.
        assert_eq!(v[0].range.start.line, 5);
        assert_eq!(v[0].range.start.character, 4);
    }

    #[test]
    fn lsp_definition_request_with_no_uri_echoes_no_lsp_attached() {
        let mut a = app_with("xx", 10);
        a.toggle_mode_by_name("lsp-mode");
        a.apply(Action::LspDefinitionRequest);
        let msg = a.last_message.as_ref().expect("echo");
        assert_eq!(msg.level, EchoLevel::Info);
        assert!(msg.text.contains("no LSP server"));
    }

    #[test]
    fn lsp_declaration_request_routes_through_unified_nav_dispatch() {
        let mut a = app_with("xx", 10);
        a.toggle_mode_by_name("lsp-mode");
        a.apply(Action::LspDeclarationRequest);
        // No URI mapped, same "no LSP server" guard fires.
        let msg = a.last_message.as_ref().expect("echo");
        assert_eq!(msg.level, EchoLevel::Info);
        assert!(msg.text.contains("no LSP server"));
    }

    #[test]
    fn lsp_type_definition_request_routes_through_unified_nav_dispatch() {
        let mut a = app_with("xx", 10);
        a.toggle_mode_by_name("lsp-mode");
        a.apply(Action::LspTypeDefinitionRequest);
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no LSP server"));
    }

    #[test]
    fn lsp_implementation_request_routes_through_unified_nav_dispatch() {
        let mut a = app_with("xx", 10);
        a.toggle_mode_by_name("lsp-mode");
        a.apply(Action::LspImplementationRequest);
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no LSP server"));
    }

    #[test]
    fn drain_pending_no_implementations_echoes_kind_specific_message() {
        // Verify the kind drives the verb in the "no X found" echo.
        let mut a = app_with("xx", 10);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Vec<lsp_types::Location>>();
        a.pending_definition_rx = Some(rx);
        a.pending_definition_token = Some(lattice_protocol::CancellationToken::new());
        a.pending_nav_kind = Some(super::LspNavKind::Implementation);
        tx.send(Vec::new()).unwrap();
        a.drain_pending_definitions();
        let msg = a.last_message.as_ref().expect("echo");
        assert!(
            msg.text.contains("no implementations"),
            "expected implementations echo, got: {}",
            msg.text
        );
        assert!(a.pending_nav_kind.is_none());
    }

    #[test]
    fn drain_pending_no_type_definitions_echoes_kind_specific_message() {
        let mut a = app_with("xx", 10);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Vec<lsp_types::Location>>();
        a.pending_definition_rx = Some(rx);
        a.pending_definition_token = Some(lattice_protocol::CancellationToken::new());
        a.pending_nav_kind = Some(super::LspNavKind::TypeDefinition);
        tx.send(Vec::new()).unwrap();
        a.drain_pending_definitions();
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no type definitions"));
    }

    #[test]
    fn drain_pending_no_declarations_echoes_kind_specific_message() {
        let mut a = app_with("xx", 10);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Vec<lsp_types::Location>>();
        a.pending_definition_rx = Some(rx);
        a.pending_definition_token = Some(lattice_protocol::CancellationToken::new());
        a.pending_nav_kind = Some(super::LspNavKind::Declaration);
        tx.send(Vec::new()).unwrap();
        a.drain_pending_definitions();
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no declarations"));
    }

    #[test]
    fn lsp_references_request_with_no_uri_echoes_no_lsp_attached() {
        let mut a = app_with("xx", 10);
        a.toggle_mode_by_name("lsp-mode");
        a.apply(Action::LspReferencesRequest);
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no LSP server"));
    }

    #[test]
    fn lsp_references_request_pre_cancels_in_flight_token() {
        let mut a = app_with("xx", 10);
        let stale = lattice_protocol::CancellationToken::new();
        a.pending_references_token = Some(stale.clone());
        a.apply(Action::LspReferencesRequest);
        assert!(stale.is_cancelled());
    }

    #[test]
    fn drain_pending_references_no_servers_outcome_echoes() {
        let mut a = app_with("xx", 10);
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::ReferencesOutcome>();
        a.pending_references_rx = Some(rx);
        a.pending_references_token = Some(lattice_protocol::CancellationToken::new());
        tx.send(super::ReferencesOutcome::NoServers).unwrap();
        a.drain_pending_references();
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no LSP server"));
        assert!(a.pending_references_token.is_none());
    }

    #[test]
    fn drain_pending_references_found_opens_lsp_locations_picker() {
        let mut a = app_with("xx", 10);
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::ReferencesOutcome>();
        a.pending_references_rx = Some(rx);
        a.pending_references_token = Some(lattice_protocol::CancellationToken::new());
        tx.send(super::ReferencesOutcome::Found {
            symbol: "foo".into(),
            locations: vec![loc("/tmp/notarealfile.rs", 3, 5)],
        })
        .unwrap();
        a.drain_pending_references();
        // Picker opened, NOT a help buffer (the pre-picker shape).
        let picker = a.picker.as_ref().expect("picker");
        assert_eq!(picker.title, "references: foo");
        assert!(matches!(
            picker.source,
            crate::picker::PickerSource::LspLocations
        ));
        assert!(matches!(
            picker.on_accept,
            crate::picker::PickerAction::JumpToLspLocation
        ));
        // The candidate's typed routing payload carries the
        // jump target -- post-4.2.g.7 this replaces the prior
        // tab-encoded `text` parsing.
        let c = picker.selected_candidate().expect("one row");
        let routing = picker.routing_for(c).expect("routing payload set");
        let crate::picker::RoutingPayload::LspLocation { path, line, .. } = routing else {
            panic!("expected LspLocation routing, got {routing:?}");
        };
        assert_eq!(*path, std::path::PathBuf::from("/tmp/notarealfile.rs"));
        assert_eq!(*line, 3);
        // Column round-trips through utf-16→utf-8 conversion that
        // reads from the file's actual line text. For a missing
        // file the preview is empty so the conversion bottoms out
        // at 0; ASCII files round-trip cleanly. We don't assert on
        // col here because the conversion needs the line text and
        // the test fixture's path doesn't exist.
    }

    #[test]
    fn drain_pending_references_empty_echoes_not_found() {
        // After the picker pivot, the empty-Found case echoes
        // rather than opening a buffer with a placeholder. The
        // picker UX expects "show the user a list to choose
        // from" -- showing an empty picker would be worse UX
        // than the echo.
        let mut a = app_with("xx", 10);
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::ReferencesOutcome>();
        a.pending_references_rx = Some(rx);
        a.pending_references_token = Some(lattice_protocol::CancellationToken::new());
        tx.send(super::ReferencesOutcome::Found {
            symbol: "missing".into(),
            locations: Vec::new(),
        })
        .unwrap();
        a.drain_pending_references();
        assert!(a.picker.is_none());
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no references"));
        assert!(msg.text.contains("missing"));
    }

    #[test]
    fn flatten_document_symbol_response_flat_preserves_order() {
        use lsp_types::{Range as LRange, Position as LPos, Location as LLoc};
        let path = std::path::PathBuf::from("/tmp/x.rs");
        #[allow(deprecated)]
        let syms = vec![
            lsp_types::SymbolInformation {
                name: "foo".into(),
                kind: lsp_types::SymbolKind::FUNCTION,
                tags: None,
                deprecated: None,
                location: LLoc {
                    uri: super::tests::fake_uri("/tmp/x.rs"),
                    range: LRange {
                        start: LPos { line: 5, character: 0 },
                        end: LPos { line: 5, character: 3 },
                    },
                },
                container_name: None,
            },
            lsp_types::SymbolInformation {
                name: "bar".into(),
                kind: lsp_types::SymbolKind::METHOD,
                tags: None,
                deprecated: None,
                location: LLoc {
                    uri: super::tests::fake_uri("/tmp/x.rs"),
                    range: LRange {
                        start: LPos { line: 10, character: 4 },
                        end: LPos { line: 10, character: 7 },
                    },
                },
                container_name: Some("Bag".into()),
            },
        ];
        let resp = lsp_types::DocumentSymbolResponse::Flat(syms);
        let mut out = Vec::new();
        super::flatten_document_symbol_response(resp, &path, &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name, "foo");
        assert_eq!(out[0].depth, 0);
        assert_eq!(out[1].name, "bar");
        assert_eq!(out[1].container.as_deref(), Some("Bag"));
    }

    #[test]
    fn flatten_document_symbol_response_nested_assigns_depth_via_dfs() {
        use lsp_types::{Range as LRange, Position as LPos, DocumentSymbol};
        let path = std::path::PathBuf::from("/tmp/x.rs");
        // mod foo { fn bar() {} } -> outer at depth 0, bar at depth 1.
        let inner_range = LRange {
            start: LPos { line: 1, character: 4 },
            end: LPos { line: 3, character: 5 },
        };
        let outer_range = LRange {
            start: LPos { line: 0, character: 0 },
            end: LPos { line: 4, character: 0 },
        };
        #[allow(deprecated)]
        let inner = DocumentSymbol {
            name: "bar".into(),
            detail: None,
            kind: lsp_types::SymbolKind::FUNCTION,
            tags: None,
            deprecated: None,
            range: inner_range,
            selection_range: inner_range,
            children: None,
        };
        #[allow(deprecated)]
        let outer = DocumentSymbol {
            name: "foo".into(),
            detail: None,
            kind: lsp_types::SymbolKind::MODULE,
            tags: None,
            deprecated: None,
            range: outer_range,
            selection_range: outer_range,
            children: Some(vec![inner]),
        };
        let resp = lsp_types::DocumentSymbolResponse::Nested(vec![outer]);
        let mut out = Vec::new();
        super::flatten_document_symbol_response(resp, &path, &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name, "foo");
        assert_eq!(out[0].depth, 0);
        assert_eq!(out[1].name, "bar");
        assert_eq!(out[1].depth, 1);
    }

    #[test]
    fn drain_pending_symbols_no_servers_outcome_echoes() {
        let mut a = app_with("xx", 10);
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::SymbolsOutcome>();
        a.pending_symbols_rx = Some(rx);
        a.pending_symbols_token = Some(lattice_protocol::CancellationToken::new());
        tx.send(super::SymbolsOutcome::NoServers).unwrap();
        a.drain_pending_symbols();
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no LSP server"));
        assert!(a.pending_symbols_token.is_none());
    }

    #[test]
    fn drain_pending_symbols_found_opens_picker() {
        let mut a = app_with("xx", 10);
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::SymbolsOutcome>();
        a.pending_symbols_rx = Some(rx);
        a.pending_symbols_token = Some(lattice_protocol::CancellationToken::new());
        tx.send(super::SymbolsOutcome::Found {
            title: "symbols (2)".into(),
            rows: vec![
                super::SymbolRow {
                    name: "foo".into(),
                    kind_glyph: "ƒ",
                    container: None,
                    depth: 0,
                    path: std::path::PathBuf::from("/tmp/x.rs"),
                    line: 5,
                    col: 0,
                },
                super::SymbolRow {
                    name: "bar".into(),
                    kind_glyph: "v",
                    container: None,
                    depth: 1,
                    path: std::path::PathBuf::from("/tmp/x.rs"),
                    line: 10,
                    col: 4,
                },
            ],
        })
        .unwrap();
        a.drain_pending_symbols();
        let picker = a.picker.as_ref().expect("picker");
        assert_eq!(picker.title, "symbols (2)");
        assert_eq!(picker.candidates.len(), 2);
        // depth-1 row carries indentation in display.
        let display = &picker.candidates[1].raw.display;
        assert!(display.contains("  v bar"), "got: {display}");
    }

    #[test]
    fn drain_pending_symbols_empty_echoes() {
        let mut a = app_with("xx", 10);
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::SymbolsOutcome>();
        a.pending_symbols_rx = Some(rx);
        a.pending_symbols_token = Some(lattice_protocol::CancellationToken::new());
        tx.send(super::SymbolsOutcome::Found {
            title: "symbols (0)".into(),
            rows: Vec::new(),
        })
        .unwrap();
        a.drain_pending_symbols();
        assert!(a.picker.is_none());
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no symbols"));
    }

    #[test]
    fn code_action_kind_glyph_distinct_for_common_kinds() {
        use lsp_types::CodeActionKind as K;
        let qf = super::code_action_kind_glyph(Some(&K::QUICKFIX));
        let rf = super::code_action_kind_glyph(Some(&K::REFACTOR));
        let sr = super::code_action_kind_glyph(Some(&K::SOURCE));
        assert_ne!(qf, rf);
        assert_ne!(qf, sr);
        assert_ne!(rf, sr);
    }

    #[test]
    fn drain_pending_code_actions_no_provider_echoes() {
        let mut a = app_with("xx", 10);
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::CodeActionOutcome>();
        a.pending_code_action_rx = Some(rx);
        a.pending_code_action_token =
            Some(lattice_protocol::CancellationToken::new());
        tx.send(super::CodeActionOutcome::NoProvider).unwrap();
        a.drain_pending_code_actions();
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("codeActionProvider"));
    }

    #[test]
    fn drain_pending_code_actions_empty_echoes_no_actions() {
        let mut a = app_with("xx", 10);
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::CodeActionOutcome>();
        a.pending_code_action_rx = Some(rx);
        a.pending_code_action_token =
            Some(lattice_protocol::CancellationToken::new());
        tx.send(super::CodeActionOutcome::Items(Vec::new())).unwrap();
        a.drain_pending_code_actions();
        assert!(a.picker.is_none());
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no code actions"));
    }

    #[test]
    fn drain_pending_code_actions_items_open_picker() {
        let mut a = app_with("foo\n", 10);
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::CodeActionOutcome>();
        a.pending_code_action_rx = Some(rx);
        a.pending_code_action_token =
            Some(lattice_protocol::CancellationToken::new());
        let act = lsp_types::CodeAction {
            title: "Add `mut` modifier".into(),
            kind: Some(lsp_types::CodeActionKind::QUICKFIX),
            diagnostics: None,
            edit: None,
            command: None,
            is_preferred: None,
            disabled: None,
            data: None,
        };
        tx.send(super::CodeActionOutcome::Items(vec![super::CodeActionRow {
            title: act.title.clone(),
            kind_glyph: "🛠",
            action: lsp_types::CodeActionOrCommand::CodeAction(act),
        }]))
        .unwrap();
        a.drain_pending_code_actions();
        let picker = a.picker.as_ref().expect("picker");
        assert!(picker.title.starts_with("code-actions"));
        assert!(matches!(
            picker.on_accept,
            crate::picker::PickerAction::AcceptLspCodeAction
        ));
        assert_eq!(picker.candidates.len(), 1);
        let display = &picker.candidates[0].raw.display;
        assert!(display.contains("🛠 Add `mut` modifier"));
        // Items pinned for the accept path.
        assert!(a.pending_code_action_items.is_some());
    }

    #[test]
    fn flatten_workspace_edit_collects_legacy_changes_map() {
        use std::collections::HashMap;
        let uri = super::tests::fake_uri("/tmp/x.rs");
        let mut changes: HashMap<lsp_types::Uri, Vec<lsp_types::TextEdit>> = HashMap::new();
        changes.insert(
            uri.clone(),
            vec![lsp_types::TextEdit {
                range: lsp_types::Range {
                    start: lsp_types::Position {
                        line: 0,
                        character: 0,
                    },
                    end: lsp_types::Position {
                        line: 0,
                        character: 3,
                    },
                },
                new_text: "bar".into(),
            }],
        );
        let we = lsp_types::WorkspaceEdit {
            changes: Some(changes),
            document_changes: None,
            change_annotations: None,
        };
        let flat = super::flatten_workspace_edit(we);
        assert_eq!(flat.len(), 1);
        assert_eq!(flat[0].0, uri);
        assert_eq!(flat[0].1[0].new_text, "bar");
    }

    #[test]
    fn drain_pending_rename_no_provider_echoes() {
        let mut a = app_with("xx", 10);
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::RenameOutcome>();
        a.pending_rename_rx = Some(rx);
        a.pending_rename_token = Some(lattice_protocol::CancellationToken::new());
        tx.send(super::RenameOutcome::NoProvider).unwrap();
        a.drain_pending_rename();
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("renameProvider"));
    }

    #[test]
    fn drain_pending_rename_not_renameable_echoes_reason() {
        let mut a = app_with("xx", 10);
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::RenameOutcome>();
        a.pending_rename_rx = Some(rx);
        a.pending_rename_token = Some(lattice_protocol::CancellationToken::new());
        tx.send(super::RenameOutcome::NotRenameable {
            reason: "out of bounds".into(),
        })
        .unwrap();
        a.drain_pending_rename();
        let msg = a.last_message.as_ref().expect("echo");
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(msg.text.contains("out of bounds"));
    }

    #[test]
    fn drain_pending_rename_empty_echoes_no_changes() {
        let mut a = app_with("xx", 10);
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::RenameOutcome>();
        a.pending_rename_rx = Some(rx);
        a.pending_rename_token = Some(lattice_protocol::CancellationToken::new());
        tx.send(super::RenameOutcome::Empty).unwrap();
        a.drain_pending_rename();
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no changes"));
    }

    #[test]
    fn drain_pending_rename_applies_active_buffer_edits_as_one_undo_unit() {
        // End-to-end-ish: load a real document, send a rename
        // outcome targeting it, verify the buffer text changed
        // and a single undo restores.
        let path = std::env::temp_dir()
            .join(format!("lattice-rename-{}.rs", std::process::id()));
        std::fs::write(&path, "let foo = 1;\nlet x = foo + 2;\n").unwrap();
        let doc = Document::open(&path).unwrap();
        let mut a = App::new(doc);
        a.set_viewport_height(10);
        let uri = super::tests::fake_uri(path.to_str().unwrap());
        let edits = vec![
            // Replace `foo` on line 0 col 4..7
            lsp_types::TextEdit {
                range: lsp_types::Range {
                    start: lsp_types::Position {
                        line: 0,
                        character: 4,
                    },
                    end: lsp_types::Position {
                        line: 0,
                        character: 7,
                    },
                },
                new_text: "bar".into(),
            },
            // Replace `foo` on line 1 col 8..11
            lsp_types::TextEdit {
                range: lsp_types::Range {
                    start: lsp_types::Position {
                        line: 1,
                        character: 8,
                    },
                    end: lsp_types::Position {
                        line: 1,
                        character: 11,
                    },
                },
                new_text: "bar".into(),
            },
        ];
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::RenameOutcome>();
        a.pending_rename_rx = Some(rx);
        a.pending_rename_token = Some(lattice_protocol::CancellationToken::new());
        tx.send(super::RenameOutcome::Edits {
            per_file: vec![(uri, edits)],
            new_name: "bar".into(),
        })
        .unwrap();
        a.drain_pending_rename();
        let body = a.document.snapshot().buffer.as_string();
        assert!(body.contains("let bar = 1;"));
        assert!(body.contains("let x = bar + 2;"));
        // One undo restores the pre-rename buffer (apply_lsp_text_edits
        // commits via apply_edit_batch_blocking which is one undo unit).
        let _ = a.undo_blocking();
        let restored = a.document.snapshot().buffer.as_string();
        assert!(restored.contains("let foo = 1;"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn drain_pending_insert_completion_lsp_no_servers_keeps_popup_open_if_sync_had_results() {
        // When sync sources gave us candidates and LSP says
        // NoServers, the popup stays open with the sync set.
        let mut a = app_with("alpha alphabet alligator\nal", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(1, 2);
        a.do_completion_trigger();
        // No URI mapped -> LSP request didn't fire; the popup
        // is open from the sync sources alone. Manually push
        // a NoServers outcome to verify the drain handles it
        // without exploding.
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<
            super::InsertCompletionLspOutcome,
        >();
        a.pending_insert_completion_lsp_rx = Some(rx);
        a.pending_insert_completion_lsp_token =
            Some(lattice_protocol::CancellationToken::new());
        tx.send(super::InsertCompletionLspOutcome::NoServers).unwrap();
        a.drain_pending_insert_completion_lsp();
        // Popup still open from sync sources.
        assert!(a.insert_completion.is_some());
    }

    #[test]
    fn drain_pending_insert_completion_lsp_items_merge_into_popup() {
        let mut a = app_with("\nfo", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(1, 2);
        // Seed the popup state directly -- skip do_completion_trigger
        // so the test doesn't depend on sync sources producing
        // matches first. The drain merges LSP items into
        // whatever raw set is present.
        a.insert_completion = Some(
            lattice_completion::InsertCompletionState::open(
                lattice_completion::CompletionTrigger::Manual,
                Position::new(1, 0),
                Position::new(1, 2),
                "fo".to_string(),
            ),
        );
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<
            super::InsertCompletionLspOutcome,
        >();
        a.pending_insert_completion_lsp_rx = Some(rx);
        a.pending_insert_completion_lsp_token =
            Some(lattice_protocol::CancellationToken::new());
        tx.send(super::InsertCompletionLspOutcome::Items {
            items: vec![
                super::LspCompletionMeta {
                    label: "foo".into(),
                    insert_text: "foo".into(),
                    filter_text: None,
                    sort_text: None,
                    detail: Some("fn() -> i32".into()),
                    documentation: None,
                    kind: Some(lsp_types::CompletionItemKind::FUNCTION),
                    deprecated: false,
                    preselect: false,
                    commit_characters: Vec::new(),
                    additional_text_edits: Vec::new(),
                    command: None,
                    insert_text_format: lsp_types::InsertTextFormat::PLAIN_TEXT,
                    replace_range: None,
                    server_id: std::sync::Arc::from("test-server"),
                    original_item: lsp_types::CompletionItem::default(),
                    resolved: false,
                },
                super::LspCompletionMeta {
                    label: "foobar".into(),
                    insert_text: "foobar".into(),
                    filter_text: None,
                    sort_text: None,
                    detail: None,
                    documentation: None,
                    kind: Some(lsp_types::CompletionItemKind::VARIABLE),
                    deprecated: false,
                    preselect: false,
                    commit_characters: Vec::new(),
                    additional_text_edits: Vec::new(),
                    command: None,
                    insert_text_format: lsp_types::InsertTextFormat::PLAIN_TEXT,
                    replace_range: None,
                    server_id: std::sync::Arc::from("test-server"),
                    original_item: lsp_types::CompletionItem::default(),
                    resolved: false,
                },
            ],
            is_incomplete: false,
        })
        .unwrap();
        a.drain_pending_insert_completion_lsp();
        let state = a.insert_completion.as_ref().expect("popup open");
        // Both items render; "foo" prefix matches both.
        let labels: Vec<String> = state
            .rendered
            .iter()
            .map(|c| c.raw.display.clone())
            .collect();
        assert!(labels.iter().any(|l| l.starts_with("foo")));
        assert!(labels.iter().any(|l| l.starts_with("foobar")));
        // Sidecar meta is populated.
        assert_eq!(a.insert_completion_lsp_meta.len(), 2);
    }

    #[test]
    fn drain_pending_insert_completion_lsp_drops_prior_lsp_rows_on_refresh() {
        // First merge populates LSP rows; second merge with
        // a different item set should REPLACE (not append).
        let mut a = app_with("xx", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::ZERO;
        a.insert_completion = Some(
            lattice_completion::InsertCompletionState::open(
                lattice_completion::CompletionTrigger::Manual,
                Position::ZERO,
                Position::ZERO,
                String::new(),
            ),
        );
        let mk_item = |label: &str| super::LspCompletionMeta {
            label: label.into(),
            insert_text: label.into(),
            filter_text: None,
            sort_text: None,
            detail: None,
            documentation: None,
            kind: None,
            deprecated: false,
            preselect: false,
            commit_characters: Vec::new(),
            additional_text_edits: Vec::new(),
            command: None,
            insert_text_format: lsp_types::InsertTextFormat::PLAIN_TEXT,
            replace_range: None,
            server_id: std::sync::Arc::from("test-server"),
            original_item: lsp_types::CompletionItem::default(),
            resolved: false,
        };
        // First batch.
        let (tx1, rx1) = tokio::sync::mpsc::unbounded_channel::<
            super::InsertCompletionLspOutcome,
        >();
        a.pending_insert_completion_lsp_rx = Some(rx1);
        a.pending_insert_completion_lsp_token =
            Some(lattice_protocol::CancellationToken::new());
        tx1.send(super::InsertCompletionLspOutcome::Items {
            items: vec![mk_item("alpha"), mk_item("alphabet")],
            is_incomplete: false,
        })
        .unwrap();
        a.drain_pending_insert_completion_lsp();
        assert_eq!(a.insert_completion_lsp_meta.len(), 2);
        let pre = a
            .insert_completion
            .as_ref()
            .map(|s| s.raw.len())
            .unwrap_or(0);
        assert_eq!(pre, 2);
        // Second batch -- only one item, "beta". Prior LSP
        // rows should be pruned.
        let (tx2, rx2) = tokio::sync::mpsc::unbounded_channel::<
            super::InsertCompletionLspOutcome,
        >();
        a.pending_insert_completion_lsp_rx = Some(rx2);
        a.pending_insert_completion_lsp_token =
            Some(lattice_protocol::CancellationToken::new());
        tx2.send(super::InsertCompletionLspOutcome::Items {
            items: vec![mk_item("beta")],
            is_incomplete: false,
        })
        .unwrap();
        a.drain_pending_insert_completion_lsp();
        assert_eq!(a.insert_completion_lsp_meta.len(), 1);
        assert_eq!(a.insert_completion_lsp_meta[0].label, "beta");
    }

    #[test]
    fn lsp_completion_meta_for_returns_none_for_sync_sourced_candidates() {
        let a = app_with("xx", 10);
        let raw = lattice_completion::RawCandidate::plain(
            "foo",
            lattice_completion::CandidateKind::Plain,
        );
        let scored = lattice_completion::ScoredCandidate {
            raw,
            score: lattice_completion::MatchScore(100),
            match_ranges: Vec::new(),
        };
        let rendered =
            lattice_completion::RenderedCandidate::from_scored(scored);
        assert!(a.lsp_completion_meta_for(&rendered).is_none());
    }

    #[test]
    fn drain_pending_completion_resolve_fills_metadata_and_body() {
        let mut a = app_with("xx", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::ZERO;
        // Build state with one candidate pointing at meta[0].
        let mut state = lattice_completion::InsertCompletionState::open(
            lattice_completion::CompletionTrigger::Manual,
            Position::ZERO,
            Position::ZERO,
            String::new(),
        );
        let mut raw = lattice_completion::RawCandidate::plain(
            "foo",
            lattice_completion::CandidateKind::Plain,
        );
        raw.data = lattice_completion::CandidateData::Extension {
            kind_id: super::LSP_COMPLETION_KIND_ID,
            payload: 0u32.to_le_bytes().to_vec(),
        };
        state
            .rendered
            .push(lattice_completion::RenderedCandidate::from_scored(
                lattice_completion::ScoredCandidate {
                    raw,
                    score: lattice_completion::MatchScore(100),
                    match_ranges: Vec::new(),
                },
            ));
        // Open the doc popup -- empty body initially because
        // meta has no documentation yet.
        state.doc_popup = Some(lattice_completion::DocPopupState {
            for_index: 0,
            body: None,
            scroll: 5, // verify scroll resets on body refresh
        });
        a.insert_completion = Some(state);
        a.insert_completion_lsp_meta.push(super::LspCompletionMeta {
            label: "foo".into(),
            insert_text: "foo".into(),
            filter_text: None,
            sort_text: None,
            detail: None,
            documentation: None,
            kind: None,
            deprecated: false,
            preselect: false,
            commit_characters: Vec::new(),
            additional_text_edits: Vec::new(),
            command: None,
            insert_text_format: lsp_types::InsertTextFormat::PLAIN_TEXT,
            replace_range: None,
            server_id: std::sync::Arc::from("test-server"),
            original_item: lsp_types::CompletionItem::default(),
            resolved: false,
        });
        // Push a resolve outcome that fills documentation.
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<
            super::CompletionResolveOutcome,
        >();
        a.pending_completion_resolve_rx = Some(rx);
        a.pending_completion_resolve_token =
            Some(lattice_protocol::CancellationToken::new());
        let mut resolved = lsp_types::CompletionItem::default();
        resolved.label = "foo".into();
        resolved.detail = Some("fn foo() -> i32".into());
        resolved.documentation = Some(lsp_types::Documentation::String(
            "Returns 42.".into(),
        ));
        tx.send(super::CompletionResolveOutcome {
            meta_index: 0,
            resolved,
        })
        .unwrap();
        a.drain_pending_completion_resolve();
        // Meta updated.
        let meta = &a.insert_completion_lsp_meta[0];
        assert!(meta.resolved);
        assert_eq!(meta.detail.as_deref(), Some("fn foo() -> i32"));
        assert_eq!(meta.documentation.as_deref(), Some("Returns 42."));
        // Doc popup body refreshed; scroll reset to 0.
        let popup = a
            .insert_completion
            .as_ref()
            .and_then(|s| s.doc_popup.as_ref())
            .expect("popup");
        assert_eq!(popup.scroll, 0);
        let body = popup.body.as_deref().unwrap_or("");
        assert!(body.contains("fn foo() -> i32"));
        assert!(body.contains("Returns 42."));
    }

    #[test]
    fn drain_pending_completion_resolve_drops_stale_index_after_selection_moved() {
        // Resolve arrives for meta[0] but selection has moved
        // to meta[1]. The meta still updates (so a future
        // refocus uses the cached docs) but the doc popup body
        // doesn't change.
        let mut a = app_with("xx", 10);
        let mut state = lattice_completion::InsertCompletionState::open(
            lattice_completion::CompletionTrigger::Manual,
            Position::ZERO,
            Position::ZERO,
            String::new(),
        );
        for i in 0..2u32 {
            let mut raw = lattice_completion::RawCandidate::plain(
                format!("c{i}"),
                lattice_completion::CandidateKind::Plain,
            );
            raw.data = lattice_completion::CandidateData::Extension {
                kind_id: super::LSP_COMPLETION_KIND_ID,
                payload: i.to_le_bytes().to_vec(),
            };
            state.rendered.push(
                lattice_completion::RenderedCandidate::from_scored(
                    lattice_completion::ScoredCandidate {
                        raw,
                        score: lattice_completion::MatchScore(100),
                        match_ranges: Vec::new(),
                    },
                ),
            );
        }
        state.selected = 1; // user moved past meta[0]
        state.doc_popup = Some(lattice_completion::DocPopupState {
            for_index: 1,
            body: Some("for c1".into()),
            scroll: 0,
        });
        a.insert_completion = Some(state);
        for i in 0..2 {
            a.insert_completion_lsp_meta.push(super::LspCompletionMeta {
                label: format!("c{i}"),
                insert_text: format!("c{i}"),
                filter_text: None,
                sort_text: None,
                detail: None,
                documentation: None,
                kind: None,
                deprecated: false,
                preselect: false,
                commit_characters: Vec::new(),
                additional_text_edits: Vec::new(),
                command: None,
                insert_text_format: lsp_types::InsertTextFormat::PLAIN_TEXT,
                replace_range: None,
                server_id: std::sync::Arc::from("test-server"),
                original_item: lsp_types::CompletionItem::default(),
                resolved: false,
            });
        }
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<
            super::CompletionResolveOutcome,
        >();
        a.pending_completion_resolve_rx = Some(rx);
        a.pending_completion_resolve_token =
            Some(lattice_protocol::CancellationToken::new());
        let mut resolved = lsp_types::CompletionItem::default();
        resolved.label = "c0".into();
        resolved.documentation = Some(lsp_types::Documentation::String(
            "stale".into(),
        ));
        tx.send(super::CompletionResolveOutcome {
            meta_index: 0,
            resolved,
        })
        .unwrap();
        a.drain_pending_completion_resolve();
        // Meta[0] updated.
        assert!(a.insert_completion_lsp_meta[0].resolved);
        assert!(
            a.insert_completion_lsp_meta[0]
                .documentation
                .as_deref()
                == Some("stale")
        );
        // Doc popup body unchanged (still pointing at meta[1]).
        let body = a
            .insert_completion
            .as_ref()
            .and_then(|s| s.doc_popup.as_ref())
            .and_then(|d| d.body.clone());
        assert_eq!(body.as_deref(), Some("for c1"));
    }

    #[test]
    fn lsp_completion_meta_for_resolves_extension_payload_index() {
        let mut a = app_with("xx", 10);
        a.insert_completion_lsp_meta.push(super::LspCompletionMeta {
            label: "first".into(),
            insert_text: "first".into(),
            filter_text: None,
            sort_text: None,
            detail: None,
            documentation: None,
            kind: None,
            deprecated: false,
            preselect: false,
            commit_characters: Vec::new(),
            additional_text_edits: Vec::new(),
            command: None,
            insert_text_format: lsp_types::InsertTextFormat::PLAIN_TEXT,
            replace_range: None,
            server_id: std::sync::Arc::from("test-server"),
            original_item: lsp_types::CompletionItem::default(),
            resolved: false,
        });
        a.insert_completion_lsp_meta.push(super::LspCompletionMeta {
            label: "second".into(),
            insert_text: "second".into(),
            filter_text: None,
            sort_text: None,
            detail: None,
            documentation: None,
            kind: None,
            deprecated: false,
            preselect: false,
            commit_characters: Vec::new(),
            additional_text_edits: Vec::new(),
            command: None,
            insert_text_format: lsp_types::InsertTextFormat::PLAIN_TEXT,
            replace_range: None,
            server_id: std::sync::Arc::from("test-server"),
            original_item: lsp_types::CompletionItem::default(),
            resolved: false,
        });
        // Build a candidate pointing at index 1.
        let mut raw = lattice_completion::RawCandidate::plain(
            "second",
            lattice_completion::CandidateKind::Plain,
        );
        raw.data = lattice_completion::CandidateData::Extension {
            kind_id: super::LSP_COMPLETION_KIND_ID,
            payload: 1u32.to_le_bytes().to_vec(),
        };
        let scored = lattice_completion::ScoredCandidate {
            raw,
            score: lattice_completion::MatchScore(100),
            match_ranges: Vec::new(),
        };
        let rendered =
            lattice_completion::RenderedCandidate::from_scored(scored);
        let meta = a
            .lsp_completion_meta_for(&rendered)
            .expect("meta resolves");
        assert_eq!(meta.label, "second");
    }

    #[test]
    fn drain_pending_completion_no_servers_echoes() {
        let mut a = app_with("xx", 10);
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::CompletionOutcome>();
        a.pending_completion_rx = Some(rx);
        a.pending_completion_token = Some(lattice_protocol::CancellationToken::new());
        tx.send(super::CompletionOutcome::NoServers).unwrap();
        a.drain_pending_completion();
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no LSP server"));
    }

    #[test]
    fn drain_pending_completion_items_open_picker_with_indexed_text() {
        let mut a = app_with("foo\n", 10);
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::CompletionOutcome>();
        a.pending_completion_rx = Some(rx);
        a.pending_completion_token = Some(lattice_protocol::CancellationToken::new());
        tx.send(super::CompletionOutcome::Items(vec![
            super::CompletionItemRow {
                label: "foo_bar".into(),
                kind_glyph: "ƒ",
                detail: Some("fn foo_bar()".into()),
                insert_text: "foo_bar()".into(),
                replace_range: (0, 3),
                line: 0,
            },
        ]))
        .unwrap();
        a.drain_pending_completion();
        let picker = a.picker.as_ref().expect("picker");
        assert!(picker.title.starts_with("complete"));
        assert!(matches!(
            picker.on_accept,
            crate::picker::PickerAction::AcceptLspCompletion
        ));
        assert_eq!(picker.candidates.len(), 1);
        // Display carries kind glyph + label + detail.
        let display = &picker.candidates[0].raw.display;
        assert!(display.contains("ƒ foo_bar"));
        assert!(display.contains("fn foo_bar()"));
        // Routing payload carries the typed LspCompletion index
        // (post-4.2.g.7 typed routing replaces the prior `#<idx>`
        // string encoding).
        let routing = picker
            .routing_for(&picker.candidates[0])
            .expect("routing payload set");
        match routing {
            crate::picker::RoutingPayload::LspCompletion { index } => {
                assert_eq!(*index, 0);
            }
            other => panic!("expected LspCompletion routing, got {other:?}"),
        }
        // Items survive on the App for the accept path.
        assert!(a.pending_completion_items.is_some());
    }

    #[test]
    fn drain_pending_completion_empty_echoes_no_completions() {
        let mut a = app_with("xx", 10);
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::CompletionOutcome>();
        a.pending_completion_rx = Some(rx);
        a.pending_completion_token = Some(lattice_protocol::CancellationToken::new());
        tx.send(super::CompletionOutcome::Items(Vec::new())).unwrap();
        a.drain_pending_completion();
        assert!(a.picker.is_none());
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no completions"));
    }

    #[test]
    fn signature_help_to_markdown_renders_active_signature() {
        let sh = lsp_types::SignatureHelp {
            signatures: vec![
                lsp_types::SignatureInformation {
                    label: "fn foo(a: i32, b: &str) -> i32".into(),
                    documentation: Some(lsp_types::Documentation::String(
                        "Adds.".into(),
                    )),
                    parameters: Some(vec![
                        lsp_types::ParameterInformation {
                            label: lsp_types::ParameterLabel::Simple("a: i32".into()),
                            documentation: Some(lsp_types::Documentation::String(
                                "the first.".into(),
                            )),
                        },
                        lsp_types::ParameterInformation {
                            label: lsp_types::ParameterLabel::Simple("b: &str".into()),
                            documentation: None,
                        },
                    ]),
                    active_parameter: Some(0),
                },
            ],
            active_signature: Some(0),
            active_parameter: None,
        };
        let body = super::signature_help_to_markdown(&sh);
        assert!(body.contains("fn foo(a: i32"));
        assert!(body.contains("**param:** `a: i32`"));
        assert!(body.contains("the first."));
        assert!(body.contains("Adds."));
    }

    #[test]
    fn signature_help_to_markdown_empty_when_no_signatures() {
        let sh = lsp_types::SignatureHelp {
            signatures: vec![],
            active_signature: None,
            active_parameter: None,
        };
        assert_eq!(super::signature_help_to_markdown(&sh), "");
    }

    #[test]
    fn drain_pending_signature_help_body_opens_popup() {
        let mut a = app_with("xx", 10);
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::SignatureHelpOutcome>();
        a.pending_signature_help_rx = Some(rx);
        a.pending_signature_help_token =
            Some(lattice_protocol::CancellationToken::new());
        tx.send(super::SignatureHelpOutcome::Body(
            "```text\nfn x()\n```\n".into(),
        ))
        .unwrap();
        a.drain_pending_signature_help();
        let h = a.popup_help().expect("popup");
        assert_eq!(h.title, "hover");
        assert!(a.pending_signature_help_token.is_none());
    }

    #[test]
    fn drain_pending_signature_help_empty_body_echoes_no_signature_info() {
        let mut a = app_with("xx", 10);
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<super::SignatureHelpOutcome>();
        a.pending_signature_help_rx = Some(rx);
        a.pending_signature_help_token =
            Some(lattice_protocol::CancellationToken::new());
        tx.send(super::SignatureHelpOutcome::Body(String::new())).unwrap();
        a.drain_pending_signature_help();
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no signature info"));
        assert!(a.popup_buffer.is_none());
    }

    #[test]
    fn nav_request_captures_tag_origin_for_picker_consumption() {
        // `do_lsp_nav_request` should set `pending_tag_origin`
        // so a subsequent picker accept (multi-result) pushes
        // the right entry onto the tag stack.
        let mut a = app_with("foo bar\nbaz\n", 10);
        // M.5.4: gate fires before tag-origin capture; activate
        // lsp-mode so the request gets that far.
        a.toggle_mode_by_name("lsp-mode");
        a.cursor = Position::new(0, 1);
        // Manually set a uri so do_lsp_nav_request gets past
        // the "no LSP server" guard.
        use std::str::FromStr;
        a.buffer_uris.insert(
            a.document_buffer_id,
            lattice_lsp::Uri::from_str("file:///tmp/x.rs").unwrap(),
        );
        a.apply(Action::LspDefinitionRequest);
        let origin = a.pending_tag_origin.as_ref().expect("origin set");
        assert_eq!(origin.position, Position::new(0, 1));
        assert_eq!(origin.label, "foo");
    }

    #[test]
    fn lsp_nav_request_pre_cancels_prior_token_regardless_of_kind() {
        // A new nav request of any kind must cancel a still-in-flight
        // request of any other kind -- they all share one slot.
        let mut a = app_with("xx", 10);
        let stale = lattice_protocol::CancellationToken::new();
        a.pending_definition_token = Some(stale.clone());
        a.apply(Action::LspImplementationRequest);
        assert!(stale.is_cancelled());
    }

    #[test]
    fn lsp_definition_request_pre_cancels_in_flight_token() {
        let mut a = app_with("xx", 10);
        let stale = lattice_protocol::CancellationToken::new();
        a.pending_definition_token = Some(stale.clone());
        a.apply(Action::LspDefinitionRequest);
        assert!(stale.is_cancelled());
    }

    #[test]
    fn drain_pending_definitions_with_no_results_echoes_not_found() {
        let mut a = app_with("xx", 10);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Vec<lsp_types::Location>>();
        a.pending_definition_rx = Some(rx);
        a.pending_definition_token = Some(lattice_protocol::CancellationToken::new());
        tx.send(Vec::new()).unwrap();
        a.drain_pending_definitions();
        let msg = a.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no definitions"));
        assert!(a.pending_definition_token.is_none());
    }

    #[test]
    fn drain_pending_definitions_with_single_same_buffer_jumps_in_place() {
        // Set up an App whose document path matches the location's
        // uri, so the jump stays in-buffer (no `:e` round-trip).
        let path = std::env::temp_dir()
            .join(format!("lattice-defjump-{}.rs", std::process::id()));
        std::fs::write(&path, "first line\nsecond line\nthird line\n").unwrap();
        let doc = Document::open(&path).unwrap();
        let mut a = App::new(doc);
        a.set_viewport_height(10);
        // Cursor starts at (0, 0). Drain a definition pointing at
        // line 2 col 5 (utf-16 character; same as utf-8 byte for
        // ASCII).
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Vec<lsp_types::Location>>();
        a.pending_definition_rx = Some(rx);
        a.pending_definition_token = Some(lattice_protocol::CancellationToken::new());
        let target = lsp_types::Location {
            uri: super::tests::fake_uri(path.to_str().unwrap()),
            range: lsp_types::Range {
                start: lsp_types::Position {
                    line: 2,
                    character: 5,
                },
                end: lsp_types::Position {
                    line: 2,
                    character: 6,
                },
            },
        };
        tx.send(vec![target]).unwrap();
        a.drain_pending_definitions();
        // Cursor moved to (2, 5).
        assert_eq!(a.cursor.line, 2);
        assert_eq!(a.cursor.byte, 5);
        // Pre-jump position pushed onto history as PluginPush.
        let pushed = a
            .position_history
            .iter()
            .any(|e| e.source == PositionSource::PluginPush && e.position == Position::ZERO);
        assert!(pushed, "expected PluginPush entry for pre-jump cursor");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn drain_pending_definitions_with_multiple_opens_picker() {
        // After the picker pivot, multi-result nav opens the
        // vertico picker rather than auto-jumping to the first
        // result. Single-result jump path is still tested by
        // `drain_pending_definitions_with_single_same_buffer_jumps_in_place`.
        let path = std::env::temp_dir()
            .join(format!("lattice-defmulti-{}.rs", std::process::id()));
        std::fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();
        let doc = Document::open(&path).unwrap();
        let mut a = App::new(doc);
        a.set_viewport_height(10);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Vec<lsp_types::Location>>();
        a.pending_definition_rx = Some(rx);
        a.pending_definition_token = Some(lattice_protocol::CancellationToken::new());
        a.pending_nav_kind = Some(super::LspNavKind::Definition);
        let target_path = path.to_str().unwrap();
        tx.send(vec![
            super::tests::loc(target_path, 1, 0),
            super::tests::loc(target_path, 2, 0),
        ])
        .unwrap();
        a.drain_pending_definitions();
        let picker = a.picker.as_ref().expect("multi-result opens picker");
        assert_eq!(picker.title, "lsp:definitions");
        assert_eq!(picker.candidates.len(), 2);
        assert!(matches!(
            picker.on_accept,
            crate::picker::PickerAction::JumpToLspLocation
        ));
        // Cursor should NOT have moved (no auto-jump).
        assert_eq!(a.cursor.line, 0);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn lsp_supervisor_constructed_with_builtin_configs() {
        let app = App::new(Document::from_text(""));
        // Builtin registry: rust, python, go, typescript, c-cpp,
        // lua. Six entries today.
        assert!(
            app.lsp.configs().len() >= 6,
            "expected at least 6 builtin server configs"
        );
        // Supervisor starts dormant.
        assert_eq!(app.lsp.running_actor_count(), 0);
        assert_eq!(app.lsp.attached_buffer_count(), 0);
        assert!(app.buffer_uris.is_empty());
    }

    #[test]
    fn lsp_close_buffer_removes_uri_mapping_for_unattached_buffer() {
        let mut app = App::new(Document::from_text(""));
        // Seed a fake mapping (as if the attach driver's open
        // had landed for a path-bearing buffer).
        let fake_uri =
            <lattice_lsp::Uri as std::str::FromStr>::from_str("file:///tmp/x.rs").unwrap();
        app.buffer_uris.insert(app.document_buffer_id, fake_uri);
        assert!(app.buffer_uri(app.document_buffer_id).is_some());

        app.lsp_close_buffer(app.document_buffer_id);
        assert!(app.buffer_uri(app.document_buffer_id).is_none());
    }

    #[test]
    fn lsp_close_buffer_is_noop_for_unmapped_id() {
        let mut app = App::new(Document::from_text(""));
        // No mapping exists; close must not panic.
        app.lsp_close_buffer(app.document_buffer_id);
        assert!(app.buffer_uris.is_empty());
    }

    #[test]
    fn lsp_log_with_no_running_servers_echoes_message() {
        // Phase 3: `:lsp-log` (with or without arg) routes through
        // the LSP picker. With zero running actors there's nothing
        // to pick; the user gets a clear echo instead of an empty
        // popup.
        let mut app = app_with("hi\n", 5);
        app.do_open_lsp_log(None);
        let msg = app.last_message.as_ref().expect("echoes a message");
        assert!(
            msg.text.contains("no LSP servers running"),
            "expected 'no LSP servers running' in echo, got {:?}",
            msg.text
        );
        assert!(app.picker.is_none(), "picker should not have opened");
    }

    #[test]
    fn lsp_log_with_arg_no_match_echoes_message() {
        let mut app = app_with("hi\n", 5);
        app.do_open_lsp_log(Some("rust"));
        let msg = app.last_message.as_ref().unwrap();
        assert!(msg.text.contains("no LSP server"));
    }

    #[test]
    fn lsp_log_buffer_refreshes_live_when_record_appended() {
        let mut app = app_with("hi\n", 5);
        let id: std::sync::Arc<str> = std::sync::Arc::from("rust");
        // Open the per-server log buffer in pane.
        app.open_lsp_log_in_pane("rust");
        let help_id = app.buffers.help_with_title("lsp:rust").unwrap();
        let body_before = app.buffers.help(help_id).unwrap().content.as_string();
        assert!(!body_before.contains("fresh-after-open"));
        // Push a new record AFTER the buffer was opened.
        app.lsp_logger.log(
            Some(&id),
            lattice_lsp::LogLevel::Info,
            lattice_lsp::LogSource::Client,
            "fresh-after-open",
        );
        // The publisher fired Event::LspLogPushed; drain hook
        // should refresh the open log buffer.
        app.drain_lsp_log_events();
        let body_after = app.buffers.help(help_id).unwrap().content.as_string();
        assert!(
            body_after.contains("fresh-after-open"),
            "expected new record visible after drain, got body:\n{body_after}"
        );
    }

    #[test]
    fn lsp_log_drain_is_noop_when_no_log_buffer_open() {
        // Pushing log records with no log buffer open should not
        // crash or echo anything; the drain just consumes events
        // and finds no matching titles.
        let mut app = app_with("hi\n", 5);
        let id: std::sync::Arc<str> = std::sync::Arc::from("rust");
        app.lsp_logger.log(
            Some(&id),
            lattice_lsp::LogLevel::Info,
            lattice_lsp::LogSource::Client,
            "no-target",
        );
        app.drain_lsp_log_events();
        // No help buffers should have appeared.
        assert!(app.buffers.help_with_title("lsp:rust").is_none());
        assert!(app.buffers.help_with_title("lsp").is_none());
    }

    #[test]
    fn lsp_trace_buffer_refreshes_live_when_trace_record_appended() {
        let mut app = app_with("hi\n", 5);
        let id: std::sync::Arc<str> = std::sync::Arc::from("rust");
        // Trace gating requires the toggle on for trace records
        // to land in the ring (and fire the publisher).
        app.lsp_logger.enable_trace(std::sync::Arc::clone(&id));
        app.open_lsp_trace_log_in_pane("rust");
        let help_id = app.buffers.help_with_title("lsp:rust:trace").unwrap();
        let before = app.buffers.help(help_id).unwrap().content.as_string();
        assert!(!before.contains("→ NEW"));
        app.lsp_logger.log(
            Some(&id),
            lattice_lsp::LogLevel::Trace,
            lattice_lsp::LogSource::Trace,
            "→ NEW request id=42",
        );
        app.drain_lsp_log_events();
        let after = app.buffers.help(help_id).unwrap().content.as_string();
        assert!(after.contains("→ NEW"));
    }

    #[test]
    fn lsp_log_burst_coalesces_into_one_refresh() {
        // Many records pushed in quick succession should result
        // in at most one buffer rebuild per scope per drain.
        // (We can't observe the rebuild count directly without
        // instrumentation; instead we assert the final body
        // contains every pushed record AND that drain is fast.)
        let mut app = app_with("hi\n", 5);
        let id: std::sync::Arc<str> = std::sync::Arc::from("rust");
        app.open_lsp_log_in_pane("rust");
        for i in 0..50 {
            app.lsp_logger.log(
                Some(&id),
                lattice_lsp::LogLevel::Info,
                lattice_lsp::LogSource::Client,
                format!("msg-{i}"),
            );
        }
        app.drain_lsp_log_events();
        let help_id = app.buffers.help_with_title("lsp:rust").unwrap();
        let body = app.buffers.help(help_id).unwrap().content.as_string();
        // First and last pushed records both visible.
        assert!(body.contains("msg-0"));
        assert!(body.contains("msg-49"));
    }

    #[test]
    fn lsp_trace_toggle_flips_state_without_opening_buffer() {
        let mut app = app_with("hi\n", 5);
        let id: std::sync::Arc<str> = std::sync::Arc::from("rust");
        // Off -> on.
        app.do_toggle_lsp_trace("rust");
        assert!(app.lsp_logger.is_tracing(&id));
        // Pure toggle now -- the trace buffer is opened separately
        // via :lsp-trace-log so peeking doesn't flip the toggle off.
        assert!(app.popup_buffer.is_none());
        let msg = app.last_message.as_ref().unwrap();
        assert!(msg.text.contains("on"));
        assert!(msg.text.contains(":lsp-trace-log"));
        // On -> off.
        app.do_toggle_lsp_trace("rust");
        assert!(!app.lsp_logger.is_tracing(&id));
        assert!(app.popup_buffer.is_none());
    }

    #[test]
    fn lsp_trace_resolves_binary_name_to_canonical_id() {
        // `:lsp-trace rust-analyzer` should resolve to the `rust`
        // config id (the registered binary file_name match) and
        // toggle the trace flag on `rust`, NOT a phantom
        // `rust-analyzer` id that nothing else looks at.
        let mut app = app_with("hi\n", 5);
        let canonical: std::sync::Arc<str> = std::sync::Arc::from("rust");
        let phantom: std::sync::Arc<str> = std::sync::Arc::from("rust-analyzer");
        app.do_toggle_lsp_trace("rust-analyzer");
        assert!(app.lsp_logger.is_tracing(&canonical));
        assert!(!app.lsp_logger.is_tracing(&phantom));
        let msg = app.last_message.as_ref().unwrap();
        assert!(msg.text.contains("resolved"));
    }

    #[test]
    fn lsp_trace_unknown_name_echoes_error_with_running_servers() {
        let mut app = app_with("hi\n", 5);
        app.do_toggle_lsp_trace("totally-fake-server-name");
        let msg = app.last_message.as_ref().unwrap();
        assert!(matches!(msg.level, EchoLevel::Error));
        assert!(msg.text.contains("totally-fake-server-name"));
    }

    #[test]
    fn lsp_status_with_no_servers_renders_placeholder() {
        let mut app = app_with("hi\n", 5);
        app.do_lsp_status();
        let body = app.popup_help().unwrap().content.as_string();
        assert!(body.contains("0 server"));
        assert!(body.contains("no LSP servers running"));
    }

    #[test]
    fn lsp_log_level_subsystem_wide_accepts_known_levels() {
        let mut app = app_with("hi\n", 5);
        for lvl in ["error", "warn", "info", "debug", "trace"] {
            app.do_set_lsp_log_level(None, lvl);
            let msg = app.last_message.as_ref().unwrap();
            assert!(
                msg.text.contains(lvl),
                "echo should mention {lvl}, got {}",
                msg.text
            );
        }
    }

    #[test]
    fn lsp_log_level_rejects_unknown_level() {
        let mut app = app_with("hi\n", 5);
        app.do_set_lsp_log_level(None, "babble");
        let msg = app.last_message.as_ref().unwrap();
        assert!(msg.text.contains("unknown log level"));
    }

    #[test]
    fn lsp_log_level_per_server_override() {
        let mut app = app_with("hi\n", 5);
        app.do_set_lsp_log_level(Some("rust"), "debug");
        // Verify the override actually took: a Debug record on
        // the "rust" server now lands in the ring (the default
        // is Info, so without the override it'd be filtered).
        let id: std::sync::Arc<str> = std::sync::Arc::from("rust");
        app.lsp_logger.log(
            Some(&id),
            lattice_lsp::LogLevel::Debug,
            lattice_lsp::LogSource::Client,
            "debug event",
        );
        let recs = app.lsp_logger.snapshot_server(&id);
        assert!(recs.iter().any(|r| r.message == "debug event"));
    }

    #[test]
    fn lsp_log_clear_drops_global_records() {
        let mut app = app_with("hi\n", 5);
        app.lsp_logger.log(
            None,
            lattice_lsp::LogLevel::Info,
            lattice_lsp::LogSource::Client,
            "x",
        );
        assert_eq!(app.lsp_logger.snapshot_global().len(), 1);
        app.do_lsp_log_clear(None);
        assert_eq!(app.lsp_logger.snapshot_global().len(), 0);
    }

    #[test]
    fn lsp_log_clear_drops_per_server_records() {
        let mut app = app_with("hi\n", 5);
        let id: std::sync::Arc<str> = std::sync::Arc::from("rust");
        app.lsp_logger.log(
            Some(&id),
            lattice_lsp::LogLevel::Info,
            lattice_lsp::LogSource::Client,
            "x",
        );
        assert_eq!(app.lsp_logger.snapshot_server(&id).len(), 1);
        app.do_lsp_log_clear(Some("rust"));
        assert_eq!(app.lsp_logger.snapshot_server(&id).len(), 0);
    }

    #[test]
    fn lsp_restart_currently_echoes_placeholder() {
        let mut app = app_with("hi\n", 5);
        app.do_lsp_restart("rust");
        let msg = app.last_message.as_ref().unwrap();
        assert!(msg.text.contains("4.4"));
    }

    fn app_with_path(
        text: &str,
        viewport: u32,
        path: std::path::PathBuf,
    ) -> App {
        let doc = lattice_core::DocumentBuilder::default()
            .with_text(text)
            .with_path(path)
            .build();
        let mut a = App::new(doc);
        a.set_viewport_height(viewport);
        a
    }

    fn inject_inbound_apply_edit(
        a: &mut App,
        inbound: lattice_lsp::InboundApplyEdit,
    ) {
        let (bus, new_rx) = lattice_lsp::ApplyEditBus::new();
        bus.dispatch(inbound).expect("dispatch");
        a.pending_apply_edit_rx = Some(new_rx);
    }

    #[test]
    fn drain_inbound_apply_edits_applies_active_buffer_edit() {
        // Synthesise an inbound `workspace/applyEdit` against
        // the active buffer. Drain should apply the edit and
        // signal `applied: true` on the oneshot.
        let dir = std::env::temp_dir().join(format!(
            "lattice-applyedit-test-{}",
            std::process::id(),
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("buffer.rs");
        std::fs::write(&path, "fn main() {}\n").unwrap();
        let mut a = app_with_path("fn main() {}\n", 5, path.clone());
        let uri: lsp_types::Uri = format!("file://{}", path.display()).parse().unwrap();
        // Edit replaces `main` (line 0, char 3..7) with `xyz`.
        let edit = lsp_types::TextEdit {
            range: lsp_types::Range {
                start: lsp_types::Position { line: 0, character: 3 },
                end: lsp_types::Position { line: 0, character: 7 },
            },
            new_text: "xyz".into(),
        };
        let mut changes = std::collections::HashMap::new();
        changes.insert(uri, vec![edit]);
        let workspace_edit = lsp_types::WorkspaceEdit {
            changes: Some(changes),
            document_changes: None,
            change_annotations: None,
        };
        let (resp_tx, mut resp_rx) = tokio::sync::oneshot::channel();
        inject_inbound_apply_edit(
            &mut a,
            lattice_lsp::InboundApplyEdit {
                server_id: std::sync::Arc::from("test-server"),
                label: Some("rename main".into()),
                edit: workspace_edit,
                response: resp_tx,
            },
        );
        a.drain_inbound_apply_edits();
        // Drain ran synchronously; the oneshot is already
        // populated -- `try_recv` returns Ok.
        let outcome = resp_rx
            .try_recv()
            .expect("drain replied via oneshot");
        assert!(
            outcome.applied,
            "edit applied: {:?}",
            outcome.failure_reason,
        );
        let after = a.document.snapshot().buffer.as_string();
        assert_eq!(after, "fn xyz() {}\n");
    }

    #[test]
    fn drain_inbound_apply_edits_empty_workspace_edit_replies_applied_true() {
        // An empty WorkspaceEdit (no changes, no
        // document_changes) is a server no-op. Spec: reply
        // applied=true so the server doesn't think we
        // failed -- just nothing to do.
        let mut a = app_with("", 5);
        let workspace_edit = lsp_types::WorkspaceEdit::default();
        let (resp_tx, mut resp_rx) = tokio::sync::oneshot::channel();
        inject_inbound_apply_edit(
            &mut a,
            lattice_lsp::InboundApplyEdit {
                server_id: std::sync::Arc::from("test-server"),
                label: None,
                edit: workspace_edit,
                response: resp_tx,
            },
        );
        a.drain_inbound_apply_edits();
        let outcome = resp_rx.try_recv().expect("drain replied");
        assert!(outcome.applied);
        assert_eq!(
            outcome.failure_reason.as_deref(),
            Some("empty workspace edit"),
        );
    }

    #[test]
    fn drain_inbound_configuration_walks_cached_tree_at_lsp_prefix() {
        // Stash an `[lsp.rust-analyzer.cargo]` block in the
        // App's cached tree (mimics what
        // `load_persistent_config` does after parsing user
        // TOML). Drain receives a request for
        // `"rust-analyzer.cargo.features"` and the
        // `"rust-analyzer.checkOnSave"` -- both surface from
        // the tree.
        let mut a = app_with("", 5);
        let toml_text = "[lsp.rust-analyzer.cargo]\n\
                         features = [\"foo\", \"bar\"]\n\
                         [lsp.rust-analyzer]\n\
                         checkOnSave = true\n";
        a.lsp_config_tree = toml_text.parse().expect("toml parse");
        let (resp_tx, mut resp_rx) = tokio::sync::oneshot::channel();
        let req = lattice_lsp::InboundConfigurationRequest {
            server_id: std::sync::Arc::from("rust-analyzer"),
            sections: vec![
                "rust-analyzer.cargo.features".into(),
                "rust-analyzer.checkOnSave".into(),
            ],
            response: resp_tx,
        };
        let (bus, new_rx) = lattice_lsp::ConfigurationBus::new();
        bus.dispatch(req).expect("dispatch");
        a.pending_configuration_rx = Some(new_rx);
        a.drain_inbound_configuration_requests();
        let values = resp_rx.try_recv().expect("drain replied");
        assert_eq!(values.len(), 2);
        // First: features array.
        let arr = values[0].as_array().expect("array");
        assert_eq!(arr[0].as_str(), Some("foo"));
        assert_eq!(arr[1].as_str(), Some("bar"));
        // Second: bool.
        assert_eq!(values[1].as_bool(), Some(true));
    }

    #[test]
    fn drain_inbound_configuration_returns_null_for_missing_section() {
        let mut a = app_with("", 5);
        // No tree populated -> every lookup is null.
        let (resp_tx, mut resp_rx) = tokio::sync::oneshot::channel();
        let req = lattice_lsp::InboundConfigurationRequest {
            server_id: std::sync::Arc::from("rust-analyzer"),
            sections: vec!["rust-analyzer.cargo.features".into()],
            response: resp_tx,
        };
        let (bus, new_rx) = lattice_lsp::ConfigurationBus::new();
        bus.dispatch(req).expect("dispatch");
        a.pending_configuration_rx = Some(new_rx);
        a.drain_inbound_configuration_requests();
        let values = resp_rx.try_recv().expect("drain replied");
        assert_eq!(values.len(), 1);
        assert!(values[0].is_null());
    }

    #[test]
    fn drain_inbound_configuration_empty_section_returns_whole_lsp_subtree() {
        // A server requesting `section: null` (or empty) wants
        // the whole `lsp` sub-tree -- our convention serves
        // this from the namespaced top.
        let mut a = app_with("", 5);
        let toml_text = "[lsp.rust-analyzer]\nchecker = \"clippy\"\n";
        a.lsp_config_tree = toml_text.parse().unwrap();
        let (resp_tx, mut resp_rx) = tokio::sync::oneshot::channel();
        let req = lattice_lsp::InboundConfigurationRequest {
            server_id: std::sync::Arc::from("rust-analyzer"),
            sections: vec![String::new()],
            response: resp_tx,
        };
        let (bus, new_rx) = lattice_lsp::ConfigurationBus::new();
        bus.dispatch(req).expect("dispatch");
        a.pending_configuration_rx = Some(new_rx);
        a.drain_inbound_configuration_requests();
        let values = resp_rx.try_recv().expect("drain replied");
        // Whole `lsp` sub-tree comes back as a JSON object.
        let obj = values[0].as_object().expect("object");
        assert!(obj.contains_key("rust-analyzer"));
    }

    #[test]
    fn drain_inbound_configuration_no_op_when_channel_empty() {
        let mut a = app_with("", 5);
        a.drain_inbound_configuration_requests();
        assert!(a.pending_configuration_rx.is_some());
    }

    #[test]
    fn drain_inbound_apply_edits_no_op_when_channel_empty() {
        // Idle drain: no requests, no outgoing oneshots, no
        // panic. Cheap path that runs every frame.
        let mut a = app_with("", 5);
        a.drain_inbound_apply_edits();
        // Receiver is restored after the drain (the take + put-back).
        assert!(a.pending_apply_edit_rx.is_some());
    }

    #[test]
    fn lsp_snippet_with_additional_edits_lands_as_one_undo_unit() {
        // Buffer has space for the auto-import on line 0 and
        // the snippet expansion on line 2. The accept path
        // applies BOTH edits in a single batch; one Ctrl-Z
        // reverts both.
        let mut a = app_with("\n\nfor", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(2, 3);
        // Manually install the popup state: one candidate
        // with snippet `insertTextFormat`, an auto-import
        // additionalTextEdit at line 0, and a snippet body
        // that splices `[anchor, cursor]`.
        let mut state = lattice_completion::InsertCompletionState::open(
            lattice_completion::CompletionTrigger::Manual,
            Position::new(2, 0),
            Position::new(2, 3),
            "for".into(),
        );
        let mut raw = lattice_completion::RawCandidate::plain(
            "for",
            lattice_completion::CandidateKind::Plain,
        )
        .with_source(lattice_completion::SourceId::new(
            lattice_completion::LSP_COMPLETION_SOURCE_ID,
        ));
        raw.data = lattice_completion::CandidateData::Extension {
            kind_id: LSP_COMPLETION_KIND_ID,
            payload: 0u32.to_le_bytes().to_vec(),
        };
        state.raw.push(raw.clone());
        state.rendered.push(lattice_completion::RenderedCandidate::from_scored(
            lattice_completion::ScoredCandidate {
                raw,
                score: lattice_completion::MatchScore(100),
                match_ranges: Vec::new(),
            },
        ));
        a.insert_completion = Some(state);
        a.insert_completion_lsp_meta.push(LspCompletionMeta {
            label: "for-loop".into(),
            // Snippet body with one tabstop -- expand_snippet_with_lsp_edits
            // sets up the active snippet, focuses $1.
            insert_text: "for ${1:i} in iter {}".into(),
            filter_text: None,
            sort_text: None,
            detail: None,
            documentation: None,
            kind: Some(lsp_types::CompletionItemKind::SNIPPET),
            deprecated: false,
            preselect: false,
            commit_characters: Vec::new(),
            additional_text_edits: vec![lsp_types::TextEdit {
                range: lsp_types::Range {
                    start: lsp_types::Position { line: 0, character: 0 },
                    end: lsp_types::Position { line: 0, character: 0 },
                },
                new_text: "use std::iter;\n".into(),
            }],
            command: None,
            insert_text_format: lsp_types::InsertTextFormat::SNIPPET,
            replace_range: None,
            server_id: std::sync::Arc::from("test-server"),
            original_item: lsp_types::CompletionItem::default(),
            resolved: true,
        });
        a.do_completion_accept();
        // After accept: line 0 has the auto-import, line 2
        // (now line 3 after the import inserted a newline,
        // wait -- the import is `use std::iter;\n` which adds
        // an extra newline; existing line 0 was empty so the
        // buffer is now: line 0 = "use std::iter;", line 1 = "",
        // line 2 = "", line 3 = "for i in iter {}").
        let after_accept = a.document.snapshot().buffer.as_string();
        assert!(after_accept.contains("use std::iter;"), "auto-import applied: `{after_accept}`");
        assert!(after_accept.contains("for i in iter {}"), "snippet expanded: `{after_accept}`");
        // Active snippet focused on $1 ("i").
        assert!(a.active_snippet.is_some(), "active snippet started");
        // Undo ONCE -> both the auto-import AND the snippet
        // expansion revert.
        a.undo_blocking().expect("undo");
        let after_undo = a.document.snapshot().buffer.as_string();
        assert_eq!(
            after_undo, "\n\nfor",
            "single undo reverted both auto-import and snippet (`{after_undo}`)",
        );
    }


    #[test]
    fn next_diagnostic_advances_cursor() {
        let mut app = app_with("a\nb\nc\nd\ne\n", 10);
        seed_diags_at_lines(&mut app, &[1, 3]);
        app.cursor = Position::new(0, 0);
        app.do_next_diagnostic();
        assert_eq!(app.cursor, Position::new(1, 0));
        app.do_next_diagnostic();
        assert_eq!(app.cursor, Position::new(3, 0));
        // Past the last -> wraps to the first.
        app.do_next_diagnostic();
        assert_eq!(app.cursor, Position::new(1, 0));
    }

    #[test]
    fn prev_diagnostic_walks_backward() {
        let mut app = app_with("a\nb\nc\nd\ne\n", 10);
        seed_diags_at_lines(&mut app, &[1, 3]);
        app.cursor = Position::new(4, 0);
        app.do_prev_diagnostic();
        assert_eq!(app.cursor, Position::new(3, 0));
        app.do_prev_diagnostic();
        assert_eq!(app.cursor, Position::new(1, 0));
        // Past the first -> wraps to the last.
        app.do_prev_diagnostic();
        assert_eq!(app.cursor, Position::new(3, 0));
    }

    #[test]
    fn next_diagnostic_with_no_attachment_echoes_error() {
        let mut app = app_with("hi\n", 5);
        // No buffer_uris mapping -> "no LSP attachment".
        app.do_next_diagnostic();
        let msg = app.last_message.as_ref().expect("expected echo");
        assert!(msg.text.contains("no LSP attachment"), "got: {}", msg.text);
    }

    #[test]
    fn next_diagnostic_with_no_diagnostics_echoes_info() {
        let mut app = app_with("hi\n", 5);
        // Seed an empty layer mapping.
        use std::str::FromStr;
        let uri = lattice_lsp::Uri::from_str("file:///tmp/empty.rs").unwrap();
        app.buffer_uris.insert(app.document_buffer_id, uri);
        app.do_next_diagnostic();
        let msg = app.last_message.as_ref().expect("expected echo");
        assert!(msg.text.contains("no diagnostics"), "got: {}", msg.text);
    }

    #[test]
    fn list_diagnostics_opens_picker() {
        let mut app = app_with("hi\n", 5);
        seed_diags_at_lines(&mut app, &[0, 1]);
        app.do_list_diagnostics();
        let picker = app.picker.as_ref().expect("picker should open");
        assert!(picker.title.starts_with("diagnostics"));
        assert!(matches!(
            picker.source,
            crate::picker::PickerSource::LspLocations
        ));
        assert!(matches!(
            picker.on_accept,
            crate::picker::PickerAction::JumpToLspLocation
        ));
        // Two diagnostic rows.
        assert_eq!(picker.candidates.len(), 2);
        // Severity prefix marginalia in display.
        let display = &picker.candidates[0].raw.display;
        assert!(display.starts_with("[E]"), "got: {display}");
        // Help buffer is NOT opened (the pre-picker shape).
        assert!(app.popup_buffer.is_none());
    }

    #[test]
    fn list_diagnostics_with_empty_layer_echoes() {
        let mut app = app_with("hi\n", 5);
        // No diagnostics seeded.
        app.do_list_diagnostics();
        // Empty diagnostics: no picker, just an echo.
        assert!(app.picker.is_none());
        let msg = app.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no diagnostics"));
    }

}
