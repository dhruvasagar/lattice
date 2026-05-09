//! Insert-mode completion popup state machine -- the
//! in-buffer completion UI's selection / cancel / docs-scroll
//! surface. The popup is a minor mode whose chord layer
//! (`<C-n>` / `<C-p>` / `<C-d>` / `<C-f>` / `<C-b>`) is the
//! main actor here.
//!
//! Methods that live here:
//! - `do_completion_next` / `do_completion_prev` -- popup
//!   selection navigation; both hook through the docs-popup
//!   refresh when documentation is open.
//! - `do_completion_docs_scroll_down` /
//!   `do_completion_docs_scroll_up` -- page the side docs
//!   panel.
//! - `do_completion_cancel` -- close the popup, clear the
//!   path-context flag.
//! - `refresh_docs_popup_for_selection` (private helper) --
//!   re-targets the docs popup when the selection changes
//!   (fires `completionItem/resolve` when the new
//!   candidate has no cached body).
//!
//! What does NOT live here yet (deferred to a later slice):
//! - `do_completion_trigger` (entry point to opening the
//!   popup; couples with LSP completion-request).
//! - `do_completion_accept` / `do_completion_accept_then_insert`
//!   (apply paths -- LSP textEdit, snippet expansion, freq
//!   bump).
//! - `do_completion_toggle_docs` (entangled with the resolve
//!   request flow).
//! - LSP completion request / drain / apply, the
//!   `populate_*` and `refilter_*` helpers, snippet expansion.
//!
//! What does NOT live here at all: the completion provider
//! registry, source plugins, snippet parser -- those live
//! in `crate::completion` / `crate::snippet`.

use lattice_grammar::ModalState;
use lattice_protocol::position::Position;

use super::{App, EchoLevel, is_path_byte, is_word_char_byte};

impl App {
    /// `<C-x><C-s>` -- direct snippet expansion (Phase 4.2.g.4).
    /// Looks up the word at the cursor in the per-language
    /// snippet registry; expands the matching snippet directly
    /// without surfacing the popup.
    pub fn do_snippet_expand_at_cursor(&mut self) {
        if !matches!(self.modal, ModalState::Insert) {
            return;
        }
        let snap = self.document.snapshot();
        // Walk back from cursor over word chars to compute the
        // prefix. Same heuristic as `do_completion_trigger`.
        let line_text = snap.buffer.line(self.cursor.line).unwrap_or_default();
        let bytes = line_text.as_bytes();
        let cursor_byte = self.cursor.byte as usize;
        let mut start = cursor_byte;
        while start > 0
            && start <= bytes.len()
            && is_word_char_byte(bytes[start - 1])
        {
            start -= 1;
        }
        let anchor = Position::new(self.cursor.line, start as u32);
        let prefix: String = line_text
            .get(start..cursor_byte.min(line_text.len()))
            .unwrap_or("")
            .to_string();
        if prefix.is_empty() {
            self.set_message(EchoLevel::Info, "no snippet prefix at cursor");
            return;
        }
        let language = self.active_language_id();
        let snippet = self
            .snippet_registry
            .lookup(&language, &prefix)
            .first()
            .cloned()
            .or_else(|| self.snippet_registry.lookup("*", &prefix).first().cloned())
            .cloned();
        let Some(snippet) = snippet else {
            self.set_message(
                EchoLevel::Info,
                format!("no snippet for prefix `{prefix}`"),
            );
            return;
        };
        self.expand_snippet(&snippet.body, anchor);
    }

    /// `<Tab>` while a snippet is active -- jump to the next
    /// placeholder. Exits the snippet on `$0`.
    pub fn do_snippet_next_placeholder(&mut self) {
        let Some(active) = self.active_snippet.as_mut() else {
            return;
        };
        let next = active.next().cloned();
        match next {
            Some(group) => {
                self.move_cursor_to_snippet_group(&group);
            }
            None => {
                self.active_snippet = None;
            }
        }
    }

    /// `<S-Tab>` -- jump to the previous placeholder.
    pub fn do_snippet_prev_placeholder(&mut self) {
        let Some(active) = self.active_snippet.as_mut() else {
            return;
        };
        if let Some(group) = active.prev().cloned() {
            self.move_cursor_to_snippet_group(&group);
        }
    }

    /// `:reload-snippets` (Phase 4.2.g.4) -- re-read every
    /// configured snippet directory and rebuild the per-language
    /// registry. The previous registry is replaced atomically; if
    /// no directories are configured the user gets a clear "no
    /// snippet sources configured" echo so the no-op doesn't look
    /// like a silent failure.
    pub fn do_reload_snippets(&mut self) {
        if self.snippet_dirs.is_empty() {
            self.set_message(
                EchoLevel::Info,
                "no snippet sources configured (set App::snippet_dirs)",
            );
            return;
        }
        let mut next = lattice_snippet::SnippetRegistry::new();
        let mut total = 0usize;
        let mut errors: Vec<String> = Vec::new();
        let dirs = self.snippet_dirs.clone();
        for dir in &dirs {
            let entries = match std::fs::read_dir(dir) {
                Ok(e) => e,
                Err(e) => {
                    errors.push(format!("{}: {e}", dir.display()));
                    continue;
                }
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }
                let stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                // `_global.json` -> all-language slot.
                let language = if stem == "_global" {
                    "*".to_string()
                } else {
                    stem
                };
                let json = match std::fs::read_to_string(&path) {
                    Ok(s) => s,
                    Err(e) => {
                        errors.push(format!("{}: {e}", path.display()));
                        continue;
                    }
                };
                match lattice_snippet::load::load_pack_from_str(&json) {
                    Ok(snips) => {
                        for s in snips {
                            next.insert(&language, s);
                            total += 1;
                        }
                    }
                    Err(e) => {
                        errors.push(format!("{}: {e}", path.display()));
                    }
                }
            }
        }
        self.snippet_registry = next;
        if errors.is_empty() {
            self.set_message(
                EchoLevel::Info,
                format!("reloaded {total} snippets"),
            );
        } else {
            self.set_message(
                EchoLevel::Warn,
                format!(
                    "reloaded {total} snippets ({} error(s); first: {})",
                    errors.len(),
                    errors[0]
                ),
            );
        }
    }

    /// Helper: move the cursor to the start of the first range in
    /// a tabstop group.
    fn move_cursor_to_snippet_group(
        &mut self,
        group: &lattice_snippet::TabstopGroup,
    ) {
        let Some(first) = group.ranges.first() else {
            return;
        };
        let snap = self.document.snapshot();
        if let Ok(pos) = snap.buffer.byte_to_position(first.start) {
            self.cursor = pos;
        }
    }
}

impl App {
    pub fn do_completion_next(&mut self) {
        if let Some(s) = self.insert_completion.as_mut() {
            s.select_next();
        }
        self.refresh_docs_popup_for_selection();
    }

    pub fn do_completion_prev(&mut self) {
        if let Some(s) = self.insert_completion.as_mut() {
            s.select_prev();
        }
        self.refresh_docs_popup_for_selection();
    }

    /// Page the docs popup body forward (`<C-f>` inside the
    /// completion-popup minor mode). Half-popup-height jump
    /// per press; clamps at the body's last visible line.
    pub fn do_completion_docs_scroll_down(&mut self) {
        if let Some(state) = self.insert_completion.as_mut() {
            if let Some(doc) = state.doc_popup.as_mut() {
                doc.scroll = doc.scroll.saturating_add(8);
            }
        }
    }

    /// Page the docs popup body backward (`<C-b>` inside the
    /// completion-popup minor mode).
    pub fn do_completion_docs_scroll_up(&mut self) {
        if let Some(state) = self.insert_completion.as_mut() {
            if let Some(doc) = state.doc_popup.as_mut() {
                doc.scroll = doc.scroll.saturating_sub(8);
            }
        }
    }

    /// Manual trigger / refresh. Opens the popup if it's
    /// closed; refreshes raw + rendered candidates if it's
    /// already open. Sources contributing today: buffer-words.
    /// LSP / snippets / path / tree-sitter follow in 4.2.g.2+.
    pub fn do_completion_trigger(&mut self) {
        if !matches!(self.modal, ModalState::Insert) {
            // Manual trigger from any other mode is a no-op
            // (completion is an Insert-mode surface). The
            // explicit echo-free no-op is intentional -- no
            // EchoLevel::Info clutter.
            return;
        }
        let snap = self.document.snapshot();
        let buffer = &snap.buffer;
        let line_text = buffer.line(self.cursor.line).unwrap_or_default();
        let bytes = line_text.as_bytes();
        let cursor_byte = self.cursor.byte as usize;
        // Detect path-completion context (Phase 4.2.g.6 (2/2)):
        // cursor sits inside a string literal AND the active
        // language enables `gen:path`. In that case the anchor
        // walks back over path-shaped bytes and stops at `/` so
        // the popup-supplied filename replaces just the current
        // path segment; non-path sources skip emit so the popup
        // doesn't show buffer words intermixed with filenames.
        let path_id = lattice_completion::SourceId::new(
            lattice_completion::PATH_SOURCE_ID,
        );
        let language = self.active_language_id();
        let path_source_enabled = self
            .effective_completion_for(&language)
            .source_enabled(&path_id);
        let path_context = path_source_enabled
            && match buffer.position_to_byte(self.cursor) {
                Ok(abs) => self
                    .syntax
                    .as_ref()
                    .map(|s| s.snapshot().cursor_in_string_scope(abs))
                    .unwrap_or(false),
                Err(_) => false,
            };
        self.completion_in_path_context = path_context;
        // Anchor: walk back from the cursor. In path context we
        // stop at `/` (dir/file boundary) or any non-path byte;
        // outside path context we stop at any non-word byte.
        // The query is the prefix `[anchor, cursor]`.
        let mut start = cursor_byte;
        while start > 0 && start <= bytes.len() {
            let b = bytes[start - 1];
            let is_boundary = if path_context {
                b == b'/' || !is_path_byte(b)
            } else {
                !is_word_char_byte(b)
            };
            if is_boundary {
                break;
            }
            start -= 1;
        }
        let anchor = Position::new(self.cursor.line, start as u32);
        let query: String = line_text
            .get(start..cursor_byte.min(line_text.len()))
            .unwrap_or("")
            .to_string();
        let trigger = if self.insert_completion.is_some() {
            // Refresh path: keep the original trigger so LSP's
            // `triggerKind` doesn't flip mid-popup.
            self.insert_completion
                .as_ref()
                .map(|s| s.trigger.clone())
                .unwrap_or(lattice_completion::CompletionTrigger::Manual)
        } else {
            lattice_completion::CompletionTrigger::Manual
        };
        let mut state = lattice_completion::InsertCompletionState::open(
            trigger.clone(),
            anchor,
            self.cursor,
            query.clone(),
        );
        self.populate_insert_completion_sync(&mut state, buffer, &trigger);
        self.insert_completion = Some(state);
        // Fire the async LSP source in parallel. It pushes
        // results back via `pending_insert_completion_lsp_rx`;
        // the runtime drains them per frame and merges into
        // `state.raw`. The popup stays open even when sync
        // sources produce nothing -- the LSP response may
        // still arrive with candidates.
        self.do_lsp_insert_completion_request();
        // If sync produced nothing AND no LSP server is
        // attached, close the popup with the standard echo.
        // We can detect "no LSP attached" without waiting on
        // the request: the URI lookup either succeeded (LSP is
        // attached and a response is in flight) or didn't
        // (in which case `do_lsp_insert_completion_request`
        // returned early without spawning).
        let lsp_pending = self.pending_insert_completion_lsp_token.is_some();
        if !lsp_pending {
            if let Some(state) = self.insert_completion.as_ref()
                && state.rendered.is_empty()
            {
                self.set_message(EchoLevel::Info, "no completions");
                self.insert_completion = None;
            }
        }
    }

    pub fn do_completion_cancel(&mut self) {
        self.insert_completion = None;
        self.completion_in_path_context = false;
    }

    /// When the focused candidate changes (next / prev /
    /// refilter pinning), re-target the docs popup. If the
    /// popup is open AND `for_index` no longer matches
    /// `selected`, re-derive the body and (when needed) fire
    /// a fresh `completionItem/resolve`.
    fn refresh_docs_popup_for_selection(&mut self) {
        let docs_open = self
            .insert_completion
            .as_ref()
            .map(|s| s.doc_popup.is_some())
            .unwrap_or(false);
        if !docs_open {
            return;
        }
        let new_index = self
            .insert_completion
            .as_ref()
            .map(|s| s.selected)
            .unwrap_or(0);
        let body = self.docs_body_for_selected();
        let needs_resolve = body.is_none() && self.selected_needs_resolve();
        if let Some(state) = self.insert_completion.as_mut() {
            if let Some(doc) = state.doc_popup.as_mut() {
                doc.for_index = new_index;
                doc.scroll = 0;
                doc.body = body;
            }
        }
        if needs_resolve {
            self.do_completion_resolve_focused();
        }
    }
}
