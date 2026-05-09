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
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range as ProtoRange};

use super::{App, EchoLevel, dedup_rendered_by_text, is_path_byte, is_word_char_byte};

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

    /// Insert-mode character key while the popup is open
    /// (Phase 4.2.g.7 commit-char polish). Accepts the focused
    /// candidate THEN inserts `ch` when `ch` is in the
    /// effective commit-char set; otherwise inserts `ch`
    /// plainly so the popup refilters as usual.
    pub fn do_completion_accept_then_insert(&mut self, ch: char) {
        let is_commit = self
            .insert_completion
            .as_ref()
            .and_then(|s| s.selected_candidate())
            .map(|cand| {
                self.effective_commit_chars_for(cand)
                    .iter()
                    .any(|c| *c == ch)
            })
            .unwrap_or(false);
        if is_commit {
            self.do_completion_accept();
        }
        self.do_insert_text(&ch.to_string());
    }

    /// Suffix of the top-ranked completion candidate that would
    /// extend the user's current query, or `None` when the
    /// renderer should paint nothing (Phase 4.2.g.7 ghost-text
    /// polish). Returned suffix is the part of the candidate
    /// `text` BEYOND the case-insensitive prefix-match against
    /// `state.query`.
    ///
    /// Returns `None` when:
    /// - `completion.ghost_text` option is off (default).
    /// - The popup is closed.
    /// - The top-ranked candidate doesn't case-insensitively
    ///   prefix-match the query.
    /// - The popup is in path-completion mode (filenames are
    ///   already shown in full inside the string literal --
    ///   ghost would double up).
    /// - The query is empty (an empty popup just lists
    ///   everything; ghosting the first arbitrary candidate
    ///   would surprise the user).
    pub fn completion_ghost_text_suffix(&self) -> Option<String> {
        if !*self
            .config
            .get_typed::<lattice_config::CompletionGhostText>()
            .expect("CompletionGhostText")
        {
            return None;
        }
        if self.completion_in_path_context {
            return None;
        }
        let state = self.insert_completion.as_ref()?;
        if state.query.is_empty() {
            return None;
        }
        let top = state.rendered.first()?;
        let text = top.raw.text.as_str();
        let prefix = state.query.as_str();
        if text.len() < prefix.len() {
            return None;
        }
        let (head, tail) = text.split_at(prefix.len());
        if !head.eq_ignore_ascii_case(prefix) {
            return None;
        }
        if tail.is_empty() {
            return None;
        }
        Some(tail.to_string())
    }

    /// Effective commit characters for `candidate` -- per-item
    /// list (LSP-supplied via `LspCompletionMeta.commit_characters`)
    /// unioned with the global `completion.extra_commit_chars`
    /// option. Sync sources (buffer-words, snippet,
    /// tree-sitter) carry no per-item list, so they only honour
    /// the global extras.
    fn effective_commit_chars_for(
        &self,
        candidate: &lattice_completion::RenderedCandidate,
    ) -> Vec<char> {
        let mut chars: Vec<char> = self
            .lsp_completion_meta_for(candidate)
            .map(|meta| meta.commit_characters.clone())
            .unwrap_or_default();
        let extra = self
            .config
            .get_typed::<lattice_config::CompletionExtraCommitChars>()
            .expect("CompletionExtraCommitChars");
        for c in extra.chars() {
            if !chars.contains(&c) {
                chars.push(c);
            }
        }
        chars
    }

    /// Accept the focused candidate. Three routing paths:
    /// 1. **Snippet candidate** (sync source `gen:snippet` or
    ///    LSP item with `insertTextFormat == Snippet`):
    ///    expand the body via `lattice-snippet`, splice the
    ///    rendered text, start an `ActiveSnippet`.
    /// 2. **LSP candidate**: apply the LSP-shaped insert
    ///    (`textEdit` range when present) plus any
    ///    `additionalTextEdits` as one undo unit.
    /// 3. **Sync-source candidate**: simple replace-`[anchor,
    ///    cursor]` splice.
    pub fn do_completion_accept(&mut self) {
        let Some(state) = self.insert_completion.take() else {
            self.completion_in_path_context = false;
            return;
        };
        let Some(item) = state.selected_candidate().cloned() else {
            self.completion_in_path_context = false;
            return;
        };
        // Clear the path-context flag now that the popup has
        // closed; the next trigger re-evaluates from scratch.
        self.completion_in_path_context = false;
        // Bump the accept-frequency counter for this item. Per
        // `docs/insert-completion.md` §3.6, the ranker rereads
        // this map and adds a bounded bonus so the user's
        // recently-accepted picks float above tied peers next
        // time the popup opens. We bump unconditionally here:
        // if the apply path below fails, the user still
        // *intended* to accept this item -- recording that
        // intent matches expected behaviour.
        let freq_key = (item.raw.text.clone(), item.raw.kind);
        *self.completion_accept_freq.entry(freq_key).or_insert(0) += 1;
        // Snippet (sync source) path -- snippet meta sidecar
        // points at a fully-parsed body.
        if let Some(meta) = self.snippet_meta_for(&item).cloned() {
            self.expand_snippet(&meta.body, state.anchor);
            self.insert_completion_snippet_meta.clear();
            self.insert_completion_lsp_meta.clear();
            return;
        }
        // LSP path: typed metadata + additionalTextEdits
        // coalesce with the main edit. When the LSP item is
        // snippet-flavoured, route through the engine.
        if let Some(meta) = self.lsp_completion_meta_for(&item).cloned() {
            if matches!(
                meta.insert_text_format,
                lsp_types::InsertTextFormat::SNIPPET
            ) {
                // Coalesce additionalTextEdits + the snippet
                // body's main splice into ONE undo unit (Phase
                // 4.2.g.7 polish). Pre-4.2.g.7 this path
                // applied the additionals first, then a
                // separate `expand_snippet`, leaving the user
                // with two `<C-z>` steps to revert one logical
                // accept.
                match lattice_snippet::parse(&meta.insert_text) {
                    Ok(body) => {
                        if let Err(e) = self.expand_snippet_with_lsp_edits(
                            &body,
                            state.anchor,
                            meta.additional_text_edits.clone(),
                        ) {
                            self.set_message(
                                EchoLevel::Error,
                                format!("completion: apply failed: {e}"),
                            );
                            return;
                        }
                    }
                    Err(_) => {
                        // Body didn't parse -- splice as plain.
                        // `apply_lsp_completion_accept` already
                        // coalesces additionals + main into one
                        // batch internally.
                        self.apply_lsp_completion_accept(meta, state.anchor);
                    }
                }
                self.insert_completion_snippet_meta.clear();
                self.insert_completion_lsp_meta.clear();
                return;
            }
            self.apply_lsp_completion_accept(meta, state.anchor);
            self.insert_completion_snippet_meta.clear();
            self.insert_completion_lsp_meta.clear();
            return;
        }
        // Sync-source path: simple replace.
        let insert_text = item.raw.text.clone();
        let range = ProtoRange::new(state.anchor, self.cursor);
        let edit = Edit::replace(range, insert_text);
        match self.apply_edit_blocking(edit) {
            Ok(applied) => {
                self.cursor = applied.inserted_range.end;
            }
            Err(e) => {
                self.set_message(
                    EchoLevel::Error,
                    format!("completion: apply failed: {e:?}"),
                );
            }
        }
        self.insert_completion_snippet_meta.clear();
        self.insert_completion_lsp_meta.clear();
    }

    /// `<C-d>` inside the completion-popup minor mode.
    /// Toggles the side documentation popup. When opening,
    /// pre-fills `body` from the focused candidate's cached
    /// metadata when available; fires
    /// `completionItem/resolve` when the documentation is
    /// missing AND the originating server advertises the
    /// resolve provider.
    pub fn do_completion_toggle_docs(&mut self) {
        let Some(state) = self.insert_completion.as_mut() else {
            return;
        };
        if state.doc_popup.is_some() {
            state.doc_popup = None;
            return;
        }
        let selected = state.selected;
        // Pull the immediate body from the focused candidate
        // when we already have it; this avoids paying a
        // resolve round-trip for items that arrived with
        // `documentation` already set.
        let body = self.docs_body_for_selected();
        let needs_resolve = body.is_none() && self.selected_needs_resolve();
        if let Some(state) = self.insert_completion.as_mut() {
            state.doc_popup = Some(lattice_completion::DocPopupState {
                for_index: selected,
                body,
                scroll: 0,
            });
        }
        if needs_resolve {
            self.do_completion_resolve_focused();
        }
    }

    /// Build the docs body for the popup's currently-focused
    /// candidate from cached metadata. Returns `None` when
    /// the candidate is sync-source (no docs) or LSP without
    /// pre-resolved documentation. The caller decides whether
    /// to fire resolve.
    pub(super) fn docs_body_for_selected(&self) -> Option<String> {
        let state = self.insert_completion.as_ref()?;
        let cand = state.rendered.get(state.selected)?;
        let meta = self.lsp_completion_meta_for(cand)?;
        // Header: signature / detail when present. The body
        // joins detail + documentation so the popup feels like
        // a hover panel for the candidate.
        let detail = meta
            .detail
            .as_ref()
            .filter(|s| !s.is_empty())
            .map(|s| {
                // Render detail as a fenced code block so the
                // popup's markdown highlighter (4.2.g.5+) picks
                // up syntax highlighting once wired.
                format!("```\n{s}\n```")
            });
        let docs = meta
            .documentation
            .as_ref()
            .filter(|s| !s.is_empty())
            .cloned();
        match (detail, docs) {
            (Some(d), Some(b)) => Some(format!("{d}\n\n{b}")),
            (Some(d), None) => Some(d),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        }
    }

    /// True when the focused candidate is LSP-sourced, has no
    /// documentation, and the originating server advertises
    /// the resolve provider.
    pub(super) fn selected_needs_resolve(&self) -> bool {
        let Some(state) = self.insert_completion.as_ref() else {
            return false;
        };
        let Some(cand) = state.rendered.get(state.selected) else {
            return false;
        };
        let Some(meta) = self.lsp_completion_meta_for(cand) else {
            return false;
        };
        if meta.resolved {
            return false;
        }
        if meta.documentation.is_some() {
            return false;
        }
        // Walk attached servers; check if the originating one
        // (by id) advertises `completionProvider.resolveProvider`.
        let Some(uri) = self.buffer_uris.get(&self.document_buffer_id) else {
            return false;
        };
        for h in self.lsp.servers_for(uri) {
            if h.server_id() == &*meta.server_id {
                return h.capabilities().completion_resolve_provider();
            }
        }
        false
    }

    /// On every Insert-mode text insertion, if the popup is
    /// open: re-derive the live query from
    /// `buffer[anchor..cursor]` and re-filter. If the cursor
    /// has moved outside the popup's anchor range, dismiss
    /// the popup (the user typed something that took them
    /// past the word boundary).
    pub(crate) fn maybe_refresh_insert_completion_after_edit(&mut self) {
        let Some(state) = self.insert_completion.as_mut() else {
            return;
        };
        // Cursor must still be on the anchor's line and at /
        // past the anchor.
        if self.cursor.line != state.anchor.line || self.cursor.byte < state.anchor.byte {
            self.insert_completion = None;
            return;
        }
        // Re-derive query.
        let snap_buf = self.document.snapshot().buffer.clone();
        let line_text = snap_buf.line(state.anchor.line).unwrap_or_default();
        let start = state.anchor.byte as usize;
        let end = (self.cursor.byte as usize).min(line_text.len());
        if end < start {
            self.insert_completion = None;
            return;
        }
        let query = line_text.get(start..end).unwrap_or("").to_string();
        // If the user typed past the word (e.g. inserted a
        // space), close the popup.
        if query
            .as_bytes()
            .iter()
            .any(|b| !is_word_char_byte(*b))
        {
            self.insert_completion = None;
            return;
        }
        state.query = query;
        state.cursor = self.cursor;
        let was_incomplete = state.lsp_incomplete;
        // Refilter against the current raw set. We hold a
        // mutable borrow on `state` here so calling the helper
        // would re-borrow self -- pull the freq map by direct
        // field reference (disjoint from `self.insert_completion`)
        // and feed it into the ranker's closure.
        let ranker = lattice_completion::InsertRanker::new();
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
        // Disjoint-field borrows: `state` aliases
        // `self.insert_completion` mutably, so the bonus closure
        // captures `freq` / `config` through direct field refs
        // (mirrors `completion_total_bonus`, which can't be
        // called here without re-borrowing self). Type-keyed
        // reads via `config.get_typed::<T>()` -- same TypeId
        // lookup the priority_for_source helper uses.
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
        if state.selected >= state.rendered.len() {
            state.selected = state.rendered.len().saturating_sub(1);
        }
        // If nothing matches, close the popup -- unless the
        // last LSP response said `isIncomplete`, in which
        // case we re-fire LSP and let the response arrive.
        if state.rendered.is_empty() && !was_incomplete {
            self.insert_completion = None;
            return;
        }
        // isIncomplete refresh: re-fire LSP on every keystroke
        // that mutates the query so the server's freshest set
        // shows up.
        if was_incomplete {
            // Mark the trigger as IncompleteRefresh so the
            // LSP request reports the right `triggerKind`.
            if let Some(state) = self.insert_completion.as_mut() {
                state.trigger =
                    lattice_completion::CompletionTrigger::IncompleteRefresh;
            }
            self.do_lsp_insert_completion_request();
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
