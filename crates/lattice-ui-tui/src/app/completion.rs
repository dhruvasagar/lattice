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

use lattice_core::Buffer;
use lattice_grammar::ModalState;
use lattice_grammar::register::Register;
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range as ProtoRange};

use super::{
    App, EchoLevel, SNIPPET_COMPLETION_KIND_ID, SnippetCandidateMeta, dedup_rendered_by_text,
    is_path_byte, is_word_char_byte, lsp_position_to_app_byte, word_under_cursor,
};

/// Effective insert-completion config for a given language.
/// Materialised by [`App::effective_completion_for`] from the
/// per-language overrides + global typed options + spec
/// fallbacks. Carried as a value type so the producer / fan-out
/// paths read it without re-resolving for every candidate.
#[derive(Debug, Clone)]
#[allow(dead_code)] // `auto_trigger` / `auto_insert_single` /
// `suppress_in` are plumbing the loader populates; production
// readers come with the scope-detect slice. Tests already read
// them, so the assertion shape is locked in.
pub(crate) struct EffectiveCompletionConfig {
    /// `Some(list)` -> only sources whose id appears in the list
    /// contribute. `None` -> every enabled source contributes
    /// (the "no per-language override" case).
    pub(crate) sources: Option<Vec<lattice_completion::SourceId>>,
    pub(crate) auto_trigger: bool,
    pub(crate) auto_insert_single: bool,
    /// Tree-sitter scopes where the popup should not fire.
    /// Plumbed today; enforcement awaits the scope-detect slice.
    pub(crate) suppress_in: Vec<String>,
}

impl EffectiveCompletionConfig {
    /// True if `source` contributes for this language. `None`
    /// effective `sources` means "every source contributes."
    pub(crate) fn source_enabled(&self, source: &lattice_completion::SourceId) -> bool {
        match &self.sources {
            Some(list) => list.contains(source),
            None => true,
        }
    }
}

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
        while start > 0 && start <= bytes.len() && is_word_char_byte(bytes[start - 1]) {
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
        let snippet = {
            let registry = self.snippet_registry.load();
            registry
                .lookup(&language, &prefix)
                .first()
                .copied()
                .or_else(|| registry.lookup("*", &prefix).first().copied())
                .cloned()
        };
        let Some(snippet) = snippet else {
            self.set_message(EchoLevel::Info, format!("no snippet for prefix `{prefix}`"));
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
        self.snippet_registry.store(std::sync::Arc::new(next));
        if errors.is_empty() {
            self.set_message(EchoLevel::Info, format!("reloaded {total} snippets"));
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
    fn move_cursor_to_snippet_group(&mut self, group: &lattice_snippet::TabstopGroup) {
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
        if let Some(state) = self.insert_completion.as_mut()
            && let Some(doc) = state.doc_popup.as_mut()
        {
            doc.scroll = doc.scroll.saturating_add(8);
        }
    }

    /// Page the docs popup body backward (`<C-b>` inside the
    /// completion-popup minor mode).
    pub fn do_completion_docs_scroll_up(&mut self) {
        if let Some(state) = self.insert_completion.as_mut()
            && let Some(doc) = state.doc_popup.as_mut()
        {
            doc.scroll = doc.scroll.saturating_sub(8);
        }
    }

    /// Run sync sources against the supplied state, populating
    /// `state.raw` and re-running matcher + ranker so
    /// `state.rendered` reflects the current `query`. Async
    /// sources (LSP) hook into `state.raw` directly via
    /// host-side channels in 4.2.g.2.
    pub(super) fn populate_insert_completion_sync(
        &mut self,
        state: &mut lattice_completion::InsertCompletionState,
        buffer: &Buffer,
        trigger: &lattice_completion::CompletionTrigger,
    ) {
        let language = self.active_language_id();
        // CSM.6: pre-compute tree-sitter symbols once. The
        // tree-sitter source iterates this slice via
        // `ctx.tree_sitter_symbols` rather than re-walking the
        // tree per produce.
        let tree_sitter_symbols: Vec<String> = self
            .document_syntax_for(self.document_buffer_id)
            .map(|s| s.snapshot().collect_symbols())
            .unwrap_or_default();
        // CSM.7: resolve the path source's base directory once
        // (the buffer's parent dir or `cwd` fallback). The path
        // source joins relative segments onto this; absolute
        // partial paths bypass it.
        let buffer_dir_owned: Option<std::path::PathBuf> = {
            let snap = self.document.snapshot();
            snap.path
                .as_ref()
                .and_then(|p| p.parent().map(|p| p.to_path_buf()))
                .or_else(|| std::env::current_dir().ok())
        };
        // CSM.8b: pre-compute the LSP-side URI + position once;
        // the LSP source reads them from the snapshot rather
        // than threading them through its own struct. Both are
        // `None` for scratch buffers (no URI mapping) or when
        // the cursor's UTF-16 position can't be derived (out-
        // of-range -- shouldn't happen in practice).
        let uri_string: Option<String> = self
            .buffer_uris
            .get(&self.document_buffer_id)
            .map(|u| u.as_str().to_string());
        let lsp_position_pair: Option<(u32, u32)> = {
            let snap = self.document.snapshot();
            crate::app::app_to_lsp_position(&snap.buffer, self.cursor)
                .map(|p| (p.line, p.character))
        };
        let ctx = lattice_completion::InsertContext {
            buffer,
            cursor: state.cursor,
            anchor: state.anchor,
            query: &state.query,
            trigger,
            case_sensitive: false,
            language: &language,
            tree_sitter_symbols: &tree_sitter_symbols,
            path_context: self.completion_in_path_context,
            buffer_dir: buffer_dir_owned.as_deref(),
            uri: uri_string.as_deref(),
            lsp_position: lsp_position_pair,
        };
        // Resolve the active language's effective config once;
        // both sync sources (buffer-words, snippet) gate emit on
        // it. The global default (no override) returns
        // `sources = None` -> every source contributes.
        let effective = self.effective_completion_for(&language);
        let mut raw: Vec<lattice_completion::RawCandidate> = Vec::new();
        // CSM.3: read mode-contributed sources from the cached
        // `ActiveCompletionSources` buffer-local. CSM.4 -- CSM.8
        // migrate today's hardcoded sources into this path one at
        // a time; until then the cache is empty in practice and
        // this loop is a no-op (fallback to the hardcoded calls
        // below keeps the popup populated). Per-language
        // `EffectiveCompletionConfig.source_enabled` still gates
        // each contribution -- the per-language TOML filter
        // applies on top of the mode-contributed set.
        if let Some(active_sources) = self
            .buffer_locals
            .get(&self.document_buffer_id)
            .and_then(|locals| locals.get::<lattice_mode::ActiveCompletionSources>())
        {
            for contribution in &active_sources.0 {
                if !effective.source_enabled(&contribution.id) {
                    continue;
                }
                // CSM.7: inside a string scope the path source
                // owns the popup -- non-path contributions
                // would interleave buffer-words / snippets /
                // tree-sitter symbols with filenames, which
                // surprises the user. Suppress them at the
                // loop level.
                if ctx.path_context
                    && contribution.id.as_str() != lattice_completion::PATH_SOURCE_ID
                {
                    continue;
                }
                if let lattice_completion::CompletionSourceKind::Sync(src) = &contribution.kind {
                    raw.extend(src.produce(&ctx));
                }
                // Async sources are spawned at popup-open time
                // by the LSP / plugin-driver path, not here.
                // CSM.8 wires the LSP source through this branch.
            }
        }
        // CSM.4/CSM.5: buffer-words + snippet completion sources
        // are now contributed via their respective minor modes
        // (read from `ActiveCompletionSources` above). The
        // legacy hardcoded calls are retired; the cache-driven
        // path populates the popup. The snippet sidecar that
        // used to hold (name, prefix, description, body) tuples
        // is gone -- candidate payloads carry the snippet's
        // name; the accept path resolves the body via
        // `SnippetRegistry::by_name` (see `snippet_meta_for`).
        // CSM.6: tree-sitter local symbols are now contributed
        // via `tree-sitter-completion-mode` (read from
        // `ActiveCompletionSources` above). The host pre-walks
        // `collect_symbols()` once per populate and threads
        // the result via `ctx.tree_sitter_symbols`; the
        // contributed source iterates that slice without
        // re-traversing the tree. Per-language `source_enabled`
        // gate still applies through the cache-reader loop.
        state.raw = raw;
        self.refilter_insert_completion(state);
    }

    /// Re-run matcher + ranker over `state.raw` against the
    /// current `state.query`. Called every time the query
    /// mutates (each Insert-mode keystroke while the popup is
    /// open).
    pub(super) fn refilter_insert_completion(
        &self,
        state: &mut lattice_completion::InsertCompletionState,
    ) {
        let matcher = lattice_completion::FuzzyInsertMatcher::new();
        // CSM.K2: when `source_filter` is set, narrow the raw
        // pool to candidates from that source before matcher /
        // ranker run. `None` ⇒ unfiltered (every source).
        let source_filter = state.source_filter.clone();
        let mut scored: Vec<lattice_completion::ScoredCandidate> = state
            .raw
            .iter()
            .filter(|raw| match source_filter.as_ref() {
                Some(id) => raw.source.as_ref() == Some(id),
                None => true,
            })
            .filter_map(|raw| {
                lattice_completion::CandidateMatcher::matches(&matcher, &state.query, raw).map(
                    |(score, ranges)| lattice_completion::ScoredCandidate {
                        raw: raw.clone(),
                        score,
                        match_ranges: ranges,
                    },
                )
            })
            .collect();
        let ranker = lattice_completion::InsertRanker::new();
        ranker.rank_with_bonus(&mut scored, |raw| self.completion_total_bonus(raw));
        state.rendered = scored
            .into_iter()
            .map(lattice_completion::RenderedCandidate::from_scored)
            .collect();
        dedup_rendered_by_text(&mut state.rendered);
        if !state.rendered.is_empty() && state.selected >= state.rendered.len() {
            state.selected = state.rendered.len() - 1;
        }
    }

    /// Total ranker bonus for a candidate -- per-source priority
    /// (`docs/dev/architecture/insert-completion.md` §3.4 / §3.6) plus the capped
    /// frequency lift. Future bonus terms (preselect,
    /// deprecated penalty) compose into this same closure as
    /// 4.2.g.5 / 4.2.g.6 land them.
    fn completion_total_bonus(&self, raw: &lattice_completion::RawCandidate) -> u32 {
        let priority = raw
            .source
            .as_ref()
            .map(|s| self.priority_for_source(s))
            .unwrap_or(0);
        let freq = self
            .completion_accept_freq
            .get(&(raw.text.clone(), raw.kind))
            .copied()
            .unwrap_or(0)
            .min(lattice_completion::InsertRanker::FREQUENCY_BONUS_CAP);
        priority.saturating_add(freq)
    }

    /// Effective per-source priority for the insert-completion
    /// ranker. Reads the typed `completion.source.<id>.priority`
    /// option for known v1 sources (LSP / snippet /
    /// buffer-words). Unknown source ids -- plugin sources or
    /// future built-ins not yet wired into config -- get 0;
    /// the ranker still sorts them by their matcher score so
    /// they're not discarded, just not boosted.
    fn priority_for_source(&self, source: &lattice_completion::SourceId) -> u32 {
        use lattice_config::{
            CompletionSourceBufferWordsPriority, CompletionSourceLspPriority,
            CompletionSourcePathPriority, CompletionSourceSnippetPriority,
            CompletionSourceTreeSitterPriority,
        };
        // Type-keyed read per source. Five distinct option types
        // ⇒ the type can't be variable, so the dispatch is a
        // match on source.as_str() returning the read value
        // directly.
        let raw: i64 = match source.as_str() {
            "gen:lsp-completion" => *self
                .config
                .get_typed::<CompletionSourceLspPriority>()
                .expect("CompletionSourceLspPriority"),
            "gen:snippet" => *self
                .config
                .get_typed::<CompletionSourceSnippetPriority>()
                .expect("CompletionSourceSnippetPriority"),
            "gen:buffer-words" => *self
                .config
                .get_typed::<CompletionSourceBufferWordsPriority>()
                .expect("CompletionSourceBufferWordsPriority"),
            "gen:tree-sitter-symbol" => *self
                .config
                .get_typed::<CompletionSourceTreeSitterPriority>()
                .expect("CompletionSourceTreeSitterPriority"),
            "gen:path" => *self
                .config
                .get_typed::<CompletionSourcePathPriority>()
                .expect("CompletionSourcePathPriority"),
            _ => return 0,
        };
        // Validator clamps to [0, 9999] so this saturating cast
        // is a no-op in practice; defend against config writes
        // that bypass the validator (none today, but cheap).
        raw.clamp(0, u32::MAX as i64) as u32
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
        // CSM.K1: gate on `completion-mode` being active on the
        // active document buffer. The mode auto-activates on
        // writable kinds at buffer creation; read-only kinds
        // (Help, FileTree, Oil) never activate it, so `<C-Space>`
        // is a silent no-op there. Same shape as
        // `lsp-completion-mode` gating LSP fan-out.
        if !self.completion_mode_active_for(self.document_buffer_id) {
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
        let path_id = lattice_completion::SourceId::new(lattice_completion::PATH_SOURCE_ID);
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
        if !lsp_pending
            && let Some(state) = self.insert_completion.as_ref()
            && state.rendered.is_empty()
        {
            self.set_message(EchoLevel::Info, "no completions");
            self.insert_completion = None;
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
            .map(|cand| self.effective_commit_chars_for(cand).contains(&ch))
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
        // `docs/dev/architecture/insert-completion.md` §3.6, the ranker rereads
        // this map and adds a bounded bonus so the user's
        // recently-accepted picks float above tied peers next
        // time the popup opens. We bump unconditionally here:
        // if the apply path below fails, the user still
        // *intended* to accept this item -- recording that
        // intent matches expected behaviour.
        let freq_key = (item.raw.text.clone(), item.raw.kind);
        *self.completion_accept_freq.entry(freq_key).or_insert(0) += 1;
        // CSM.5: snippet (sync source) path. `snippet_meta_for`
        // now decodes the payload as a snippet name and looks up
        // the body in `App.snippet_registry`; no sidecar to
        // clear afterwards.
        if let Some(meta) = self.snippet_meta_for(&item) {
            self.expand_snippet(&meta.body, state.anchor);
            return;
        }
        // LSP path: typed metadata + additionalTextEdits
        // coalesce with the main edit. When the LSP item is
        // snippet-flavoured, route through the engine.
        if let Some(meta) = self.lsp_completion_meta_for(&item) {
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
                return;
            }
            self.apply_lsp_completion_accept(meta, state.anchor);
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
                self.set_message(EchoLevel::Error, format!("completion: apply failed: {e:?}"));
            }
        }
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
        let detail = meta.detail.as_ref().filter(|s| !s.is_empty()).map(|s| {
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
            if h.server_id() == meta.server_id.as_str() {
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
        if query.as_bytes().iter().any(|b| !is_word_char_byte(*b)) {
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
        // CSM.K2: honour the active source filter here too --
        // this is the LSP-aware refilter path that runs on
        // async-response drains; it must agree with
        // `refilter_insert_completion`'s filter logic so the
        // popup stays consistent when LSP candidates arrive
        // mid-filter.
        let source_filter = state.source_filter.clone();
        let mut scored: Vec<lattice_completion::ScoredCandidate> = state
            .raw
            .iter()
            .filter(|raw| match source_filter.as_ref() {
                Some(id) => raw.source.as_ref() == Some(id),
                None => true,
            })
            .filter_map(|raw| {
                lattice_completion::CandidateMatcher::matches(&matcher, &state.query, raw).map(
                    |(score, ranges)| lattice_completion::ScoredCandidate {
                        raw: raw.clone(),
                        score,
                        match_ranges: ranges,
                    },
                )
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
                state.trigger = lattice_completion::CompletionTrigger::IncompleteRefresh;
            }
            self.do_lsp_insert_completion_request();
        }
    }

    pub fn do_completion_cancel(&mut self) {
        self.insert_completion = None;
        self.completion_in_path_context = false;
    }

    /// CSM.K2: restrict the open completion popup to a single
    /// source. `id` is the `SourceId` as a raw string (e.g.
    /// `"gen:buffer-words"`, `"gen:lsp-completion"`). When the
    /// referenced source has no candidates in the current
    /// `state.raw`, refilter yields an empty rendered list --
    /// the popup stays open so the user can switch chords or
    /// clear the filter without losing the trigger context.
    pub fn do_completion_filter_to_source(&mut self, id: String) {
        let Some(mut state) = self.insert_completion.take() else {
            return;
        };
        state.source_filter = Some(lattice_completion::SourceId::new(id));
        self.refilter_insert_completion(&mut state);
        self.insert_completion = Some(state);
        self.refresh_docs_popup_for_selection();
    }

    /// CSM.K2: clear the active source filter (`<C-Space>`).
    /// Restores the mixed merged candidate list.
    pub fn do_completion_filter_clear(&mut self) {
        let Some(mut state) = self.insert_completion.take() else {
            return;
        };
        state.source_filter = None;
        self.refilter_insert_completion(&mut state);
        self.insert_completion = Some(state);
        self.refresh_docs_popup_for_selection();
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
        if let Some(state) = self.insert_completion.as_mut()
            && let Some(doc) = state.doc_popup.as_mut()
        {
            doc.for_index = new_index;
            doc.scroll = 0;
            doc.body = body;
        }
        if needs_resolve {
            self.do_completion_resolve_focused();
        }
    }

    /// Build a `VariableContext` for snippet expansion from
    /// the active buffer / cursor / clipboard / etc. Powers
    /// `$TM_FILENAME`, `$TM_CURRENT_LINE`, etc.
    pub(super) fn snippet_variable_context(&self) -> lattice_snippet::VariableContext {
        let mut ctx = lattice_snippet::VariableContext::default();
        let snap = self.document.snapshot();
        if let Some(path) = snap.path.as_ref() {
            ctx.filepath = Some(path.display().to_string());
            ctx.filename = path
                .file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string());
            ctx.directory = path.parent().map(|p| p.display().to_string());
        }
        ctx.line_index = Some(self.cursor.line);
        if let Some(line) = snap.buffer.line(self.cursor.line) {
            ctx.current_line = Some(line);
        }
        if let Some(word) = word_under_cursor(&snap.buffer, self.cursor) {
            ctx.current_word = Some(word);
        }
        // CLIPBOARD via the system register.
        if let Some(reg) = self.editor.registers.get(&Register::System) {
            ctx.clipboard = Some(reg.content.clone());
        }
        ctx
    }

    /// CSM.5: resolve a candidate's snippet metadata by decoding
    /// the payload (snippet name) and looking up in
    /// `App.snippet_registry`. Replaces the old sidecar-indexed
    /// path -- snippet candidates now carry their stable name
    /// rather than a vec-index that breaks on refilter.
    pub(super) fn snippet_meta_for(
        &self,
        candidate: &lattice_completion::RenderedCandidate,
    ) -> Option<SnippetCandidateMeta> {
        let lattice_completion::CandidateData::Extension { kind_id, payload } = &candidate.raw.data
        else {
            return None;
        };
        if *kind_id != SNIPPET_COMPLETION_KIND_ID {
            return None;
        }
        let name = std::str::from_utf8(payload).ok()?;
        let registry = self.snippet_registry.load();
        let snip = registry.by_name(name)?;
        let prefix = snip
            .prefixes
            .first()
            .cloned()
            .unwrap_or_else(|| snip.name.clone());
        Some(SnippetCandidateMeta {
            name: snip.name.clone(),
            prefix,
            description: snip.description.clone(),
            body: snip.body.clone(),
        })
    }

    /// Expand a parsed snippet body at the popup's anchor.
    /// Renders the body (variables resolved against the
    /// active buffer's context), splices the resulting text
    /// over `[anchor, cursor]`, sets up an `ActiveSnippet`,
    /// and moves the cursor to the first tabstop's range.
    /// Pure-literal snippets (no tabstops) skip the active-
    /// snippet step and just leave the cursor at end-of-insert.
    /// Expand a snippet body alongside LSP `additionalTextEdits`
    /// as one undo unit (Phase 4.2.g.7 polish).
    ///
    /// Coalesces the auto-import edits the server sent back
    /// with the snippet body's main splice into a single
    /// `apply_edit_batch_blocking` call -- one `<C-z>` reverts
    /// both. The main edit's position is recovered from the
    /// `Vec<AppliedEdit>` (its index in the reverse-sorted
    /// batch) so the active-snippet origin tracks the
    /// post-batch buffer state correctly.
    ///
    /// Returns `Err(message)` when the batch apply fails; the
    /// caller surfaces it via `set_message`. On success the
    /// active-snippet bookkeeping mirrors `expand_snippet` --
    /// focus the first tabstop, set `active_snippet`, etc.
    pub(super) fn expand_snippet_with_lsp_edits(
        &mut self,
        body: &lattice_snippet::SnippetBody,
        anchor: Position,
        additional: Vec<lsp_types::TextEdit>,
    ) -> Result<(), String> {
        let vars = self.snippet_variable_context();
        let rendered = lattice_snippet::render::render(body, &vars);
        // Build the main edit -- snippet body splices over
        // `[anchor, cursor]`.
        let main_range = lattice_protocol::position::Range::new(anchor, self.cursor);
        let main_edit = Edit::replace(main_range, rendered.text.clone());
        // Convert `additionalTextEdits` to lattice Edits.
        let snap = self.document.snapshot();
        let mut all_edits: Vec<Edit> = Vec::with_capacity(additional.len() + 1);
        for te in &additional {
            let start_byte = lsp_position_to_app_byte(
                &snap.buffer,
                te.range.start.line,
                te.range.start.character,
            );
            let end_byte =
                lsp_position_to_app_byte(&snap.buffer, te.range.end.line, te.range.end.character);
            let r = lattice_protocol::position::Range::new(
                Position::new(te.range.start.line, start_byte),
                Position::new(te.range.end.line, end_byte),
            );
            all_edits.push(Edit::replace(r, te.new_text.clone()));
        }
        all_edits.push(main_edit.clone());
        // Reverse-sort by start position so each edit's
        // original-document positions stay valid as we apply
        // sequentially. Same convention as `apply_lsp_text_edits`.
        all_edits.sort_by(|a, b| {
            b.range
                .start
                .line
                .cmp(&a.range.start.line)
                .then_with(|| b.range.start.byte.cmp(&a.range.start.byte))
        });
        // Track main's index post-sort so we can read its
        // post-batch range out of the applied vec.
        let main_idx = all_edits
            .iter()
            .position(|e| *e == main_edit)
            .ok_or_else(|| "main edit lost during sort".to_string())?;
        drop(snap);
        let applied = self
            .apply_edit_batch_blocking(all_edits)
            .map_err(|e| format!("{e:?}"))?;
        let main_applied = applied
            .get(main_idx)
            .ok_or_else(|| "main edit missing from applied batch".to_string())?;
        let origin = self
            .document
            .snapshot()
            .buffer
            .position_to_byte(main_applied.inserted_range.start)
            .unwrap_or(0);
        if !rendered.tabstops.is_empty() {
            let mut active = lattice_snippet::ActiveSnippet::from_render(&rendered, origin);
            if let Some(group) = active.focus_first()
                && let Some(first) = group.ranges.first()
                && let Ok(pos) = self
                    .document
                    .snapshot()
                    .buffer
                    .byte_to_position(first.start)
            {
                self.cursor = pos;
            }
            self.active_snippet = Some(active);
            self.modal = ModalState::Insert;
        } else {
            self.cursor = main_applied.inserted_range.end;
        }
        Ok(())
    }

    pub(super) fn expand_snippet(&mut self, body: &lattice_snippet::SnippetBody, anchor: Position) {
        let vars = self.snippet_variable_context();
        let rendered = lattice_snippet::render::render(body, &vars);
        // Splice the rendered text over `[anchor, cursor]`.
        let range = lattice_protocol::position::Range::new(anchor, self.cursor);
        let edit = Edit::replace(range, rendered.text.clone());
        let applied = match self.apply_edit_blocking(edit) {
            Ok(a) => a,
            Err(e) => {
                self.set_message(EchoLevel::Error, format!("snippet: apply failed: {e:?}"));
                return;
            }
        };
        // The host's offset of the snippet origin = anchor
        // converted to a buffer byte offset. ActiveSnippet
        // tracks ranges in buffer bytes; since our rope edit
        // returned the inserted_range, we recompute the
        // origin from the buffer's positional API.
        let origin = self
            .document
            .snapshot()
            .buffer
            .position_to_byte(applied.inserted_range.start)
            .unwrap_or_default();
        if !rendered.tabstops.is_empty() {
            let mut active = lattice_snippet::ActiveSnippet::from_render(&rendered, origin);
            // Focus the first tabstop and move the cursor.
            if let Some(group) = active.focus_first()
                && let Some(first) = group.ranges.first()
                && let Ok(pos) = self
                    .document
                    .snapshot()
                    .buffer
                    .byte_to_position(first.start)
            {
                self.cursor = pos;
            }
            self.active_snippet = Some(active);
            self.modal = ModalState::Insert;
        } else {
            self.cursor = applied.inserted_range.end;
        }
    }

    /// Active buffer's snippet language id. Maps the active
    /// document's filename extension to a language string the
    /// snippet registry indexes by (e.g. `"rs"` -> `"rust"`).
    /// Falls back to the empty string when no path is set
    /// (the registry's `"*"` any-language pack still applies).
    pub(super) fn active_language_id(&self) -> String {
        let snap = self.document.snapshot();
        let Some(path) = snap.path.as_ref() else {
            return String::new();
        };
        match path.extension().and_then(|e| e.to_str()) {
            Some("rs") => "rust".into(),
            Some("py") => "python".into(),
            Some("go") => "go".into(),
            Some("js") | Some("jsx") | Some("mjs") | Some("cjs") => "javascript".into(),
            Some("ts") | Some("tsx") => "typescript".into(),
            Some("c") | Some("h") => "c".into(),
            Some("cc") | Some("cpp") | Some("cxx") | Some("hpp") | Some("hxx") => "cpp".into(),
            Some("md") => "markdown".into(),
            Some("toml") => "toml".into(),
            Some("yaml") | Some("yml") => "yaml".into(),
            Some("json") => "json".into(),
            Some("sh") => "shellscript".into(),
            Some("lua") => "lua".into(),
            Some(other) => other.to_string(),
            None => String::new(),
        }
    }

    /// Effective completion config for `language` -- per-language
    /// override lays over the global typed option which lays
    /// over the spec fallback. Used at every producer-side
    /// enforcement seam (sync source filter, LSP fan-out, the
    /// `auto_insert_single` check at popup-open).
    pub(crate) fn effective_completion_for(&self, language: &str) -> EffectiveCompletionConfig {
        let overrides = self.per_language_completion.get(language);
        EffectiveCompletionConfig {
            sources: overrides.and_then(|o| o.sources.clone()),
            // No global `completion.auto_trigger` typed option
            // yet (auto-trigger firing itself lands in a future
            // slice); fall back to false per spec.
            auto_trigger: overrides.and_then(|o| o.auto_trigger).unwrap_or(false),
            auto_insert_single: overrides
                .and_then(|o| o.auto_insert_single)
                .unwrap_or_else(|| self.completion_auto_insert_single()),
            suppress_in: overrides
                .and_then(|o| o.suppress_in.clone())
                .unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use crate::app::completion_kind_glyph;
    use crate::app::test_helpers::{
        app_in_command_mode, app_with, app_with_path, fresh_path_workspace, install_snippet,
        open_popup_with_top_text, set_rust_syntax,
    };
    use crate::app::*;

    #[test]
    fn auto_insert_single_default_is_on() {
        let a = app_with("xx", 10);
        assert!(a.completion_auto_insert_single());
    }

    #[test]
    fn auto_insert_single_replaces_command_line_for_one_candidate() {
        // `:set foldmethod=ind` is a unique fuzzy match against the
        // four enumerated `foldmethod=*` values (manual / indent /
        // markdown / syntax) -- only `foldmethod=indent` survives.
        // Tab should auto-insert it without opening a popup.
        let mut a = app_in_command_mode("set foldmethod=ind");
        assert!(a.completion_auto_insert_single(), "default should be on");
        a.apply(Action::CommandLineCompleteOrAdvance);
        assert!(
            a.completion_state.is_none(),
            "popup must not open when the only candidate auto-inserts"
        );
        assert_eq!(a.command_line, "set foldmethod=indent");
    }

    #[test]
    fn auto_insert_single_off_keeps_popup_for_one_candidate() {
        // Disabling reverts to "always show popup, even with one row".
        let mut a = app_in_command_mode("set foldmethod=ind");
        a.set_completion_auto_insert_single_for_test(false);
        a.apply(Action::CommandLineCompleteOrAdvance);
        let state = a
            .completion_state
            .as_ref()
            .expect("popup should open when option is off");
        assert_eq!(state.candidates.len(), 1);
        assert_eq!(
            a.command_line, "set foldmethod=ind",
            "cmdline must not change until user confirms"
        );
    }

    #[test]
    fn auto_insert_single_does_not_fire_for_multiple_candidates() {
        // Multiple matches → popup opens whether or not the option
        // is on. The auto-insert path is only the one-candidate case.
        let mut a = app_in_command_mode("descri");
        a.apply(Action::CommandLineCompleteOrAdvance);
        let state = a
            .completion_state
            .as_ref()
            .expect("popup should open with multiple candidates");
        assert!(
            state.candidates.len() >= 2,
            "expected several describe-* candidates: {:?}",
            state
                .candidates
                .iter()
                .map(|c| &c.raw.text)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn auto_insert_single_does_not_fire_when_narrowing_open_popup() {
        // Sub-decision (i): only fires at popup-open. Opening on a
        // multi-candidate prefix and narrowing while typing must
        // leave the popup open (even if it shrinks to one) -- vim's
        // default and the less surprising behaviour.
        let mut a = app_in_command_mode("set foldmethod=");
        a.apply(Action::CommandLineCompleteOrAdvance);
        let initial = a
            .completion_state
            .as_ref()
            .expect("popup should open for the value list");
        assert!(initial.candidates.len() >= 2);
        // Narrow by typing toward `indent`.
        for c in "ind".chars() {
            a.apply(Action::CommandLineAppend(c));
        }
        // Popup is still open even after narrowing to the unique
        // match -- auto-insert only fires at popup-open, not on
        // refilter-while-open.
        assert!(
            a.completion_state.is_some(),
            "popup must stay open when narrowed mid-typing"
        );
        assert_eq!(a.command_line, "set foldmethod=ind");
    }

    #[test]
    fn auto_insert_single_set_via_set_command() {
        // `:set nocompletion.auto_insert_single` flips the bool;
        // `:set completion.auto_insert_single` flips it back.
        let mut a = app_with("xx", 10);
        a.command_line = "set nocompletion.auto_insert_single".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert!(!a.completion_auto_insert_single());
        a.command_line = "set completion.auto_insert_single".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert!(a.completion_auto_insert_single());
    }

    #[test]
    fn completion_kind_glyph_distinct_for_common_kinds() {
        use lsp_types::CompletionItemKind as K;
        let f = completion_kind_glyph(Some(K::FUNCTION));
        let s = completion_kind_glyph(Some(K::SNIPPET));
        let v = completion_kind_glyph(Some(K::VARIABLE));
        assert_ne!(f, s);
        assert_ne!(f, v);
    }

    #[test]
    fn snippet_expand_at_cursor_splices_body_and_focuses_first_tabstop() {
        // Buffer: `for `; cursor sits past the prefix `for`
        // so the lookup picks it up. After expansion we
        // expect the snippet's literal text in the buffer
        // and an active snippet pointing at $1.
        let mut a = app_with("for", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 3);
        install_snippet(
            &mut a,
            "*",
            "for-loop",
            "for",
            "for ${1:i} in ${2:iter} { $0 }",
        );
        a.do_snippet_expand_at_cursor();
        // Buffer text should be the rendered snippet.
        let text = a.document.snapshot().buffer.as_string();
        assert_eq!(text, "for i in iter {  }");
        // Active snippet present, focused on $1.
        let active = a.active_snippet.as_ref().expect("snippet active");
        assert_eq!(active.current_index(), Some(1));
        // Cursor at start of `i`.
        assert_eq!(a.cursor, Position::new(0, 4));
    }

    #[test]
    fn snippet_next_placeholder_walks_through_groups_and_drops_on_zero() {
        let mut a = app_with("for", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 3);
        install_snippet(
            &mut a,
            "*",
            "for-loop",
            "for",
            "for ${1:i} in ${2:iter} { $0 }",
        );
        a.do_snippet_expand_at_cursor();
        // Now at $1.
        assert_eq!(a.active_snippet.as_ref().unwrap().current_index(), Some(1));
        a.do_snippet_next_placeholder();
        assert_eq!(a.active_snippet.as_ref().unwrap().current_index(), Some(2));
        a.do_snippet_next_placeholder();
        // $0 is the exit; at this point we're focused on it.
        assert_eq!(a.active_snippet.as_ref().unwrap().current_index(), Some(0));
        a.do_snippet_next_placeholder();
        // Past $0 -> snippet dropped.
        assert!(a.active_snippet.is_none());
    }

    #[test]
    fn snippet_prev_placeholder_walks_back() {
        let mut a = app_with("for", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 3);
        install_snippet(&mut a, "*", "for-loop", "for", "for ${1:i} in ${2:iter} {}");
        a.do_snippet_expand_at_cursor();
        a.do_snippet_next_placeholder();
        assert_eq!(a.active_snippet.as_ref().unwrap().current_index(), Some(2));
        a.do_snippet_prev_placeholder();
        assert_eq!(a.active_snippet.as_ref().unwrap().current_index(), Some(1));
    }

    #[test]
    fn snippet_expand_with_no_match_is_a_no_op() {
        let mut a = app_with("xyz", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 3);
        a.do_snippet_expand_at_cursor();
        assert!(a.active_snippet.is_none());
        // Buffer unchanged.
        assert_eq!(a.document.snapshot().buffer.as_string(), "xyz");
    }

    #[test]
    fn snippet_expand_outside_insert_mode_is_a_no_op() {
        let mut a = app_with("for", 10);
        a.cursor = Position::new(0, 3);
        // Stay in Normal -- guard inside `do_snippet_expand_at_cursor`.
        install_snippet(&mut a, "*", "for-loop", "for", "for $1 {}");
        a.do_snippet_expand_at_cursor();
        assert!(a.active_snippet.is_none());
        assert_eq!(a.document.snapshot().buffer.as_string(), "for");
    }

    #[test]
    fn completion_trigger_includes_snippet_candidate_for_matching_prefix() {
        let mut a = app_with("for", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 3);
        install_snippet(&mut a, "*", "for-loop", "for", "for ${1:i} in ${2:iter} {}");
        a.do_completion_trigger();
        let state = a.insert_completion.as_ref().expect("popup open");
        // `for-loop` snippet appears as a candidate. The
        // candidate's text is the prefix; CSM.5 carries the
        // snippet's stable name in the `Extension::payload`
        // bytes, the accept path resolves the body via
        // `snippet_meta_for` -> `SnippetRegistry::by_name`.
        let cand = state
            .rendered
            .iter()
            .find(|r| r.raw.text == "for")
            .expect("snippet candidate present");
        let meta = a.snippet_meta_for(cand).expect("snippet meta resolves");
        assert_eq!(meta.name, "for-loop");
    }

    #[test]
    fn completion_accept_on_snippet_candidate_starts_active_snippet() {
        let mut a = app_with("for", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 3);
        install_snippet(&mut a, "*", "for-loop", "for", "for ${1:i} in ${2:iter} {}");
        a.do_completion_trigger();
        // Find the snippet candidate index and select it.
        let state = a.insert_completion.as_mut().expect("popup");
        let idx = state
            .rendered
            .iter()
            .position(|r| {
                matches!(
                    r.raw.data,
                    lattice_completion::CandidateData::Extension {
                        kind_id,
                        ..
                    } if kind_id == SNIPPET_COMPLETION_KIND_ID
                )
            })
            .expect("snippet candidate present");
        state.selected = idx;
        a.do_completion_accept();
        // Popup closed; active snippet is in flight focused on
        // $1; buffer reflects expansion.
        assert!(a.insert_completion.is_none());
        let active = a.active_snippet.as_ref().expect("active snippet");
        assert_eq!(active.current_index(), Some(1));
        let text = a.document.snapshot().buffer.as_string();
        assert_eq!(text, "for i in iter {}");
    }

    #[test]
    fn completion_accept_bumps_frequency_map_for_text_kind_pair() {
        // Trigger completion against a buffer-words source and
        // accept a candidate. The App's accept-frequency map
        // gets a new entry keyed by `(text, kind)` with count 1.
        let mut a = app_with("alpha bravo charlie ", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 20);
        a.do_completion_trigger();
        // Empty query at end of line -> all three buffer words
        // surface as candidates. Find `bravo` and select it.
        let state = a.insert_completion.as_mut().expect("popup");
        let idx = state
            .rendered
            .iter()
            .position(|r| r.raw.text == "bravo")
            .expect("bravo present");
        state.selected = idx;
        a.do_completion_accept();
        // Map records exactly one accept of (bravo, Plain).
        let key = (
            "bravo".to_string(),
            lattice_completion::CandidateKind::Plain,
        );
        assert_eq!(a.completion_accept_freq.get(&key).copied(), Some(1));
    }

    #[test]
    fn completion_trigger_ranks_previously_accepted_above_tied_peer() {
        // Two buffer words tie on matcher score (empty query
        // -> uniform 100); a previous accept of `bravo` lifts
        // it to the top of the rendered list.
        let mut a = app_with("alpha bravo charlie ", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 20);
        // Seed the freq map directly -- this is the integration
        // boundary we care about (the App's map fed into the
        // ranker), not the accept-then-retrigger cycle.
        a.completion_accept_freq.insert(
            (
                "bravo".to_string(),
                lattice_completion::CandidateKind::Plain,
            ),
            3,
        );
        a.do_completion_trigger();
        let state = a.insert_completion.as_ref().expect("popup");
        // First rendered candidate is the previously-accepted
        // one, ahead of its tied peers.
        assert_eq!(
            state.rendered.first().expect("at least one").raw.text,
            "bravo"
        );
    }

    #[test]
    fn path_source_emits_filesystem_entries_inside_string_literal() {
        let ws = fresh_path_workspace("emits-entries");
        // Populate the workspace with two files + one dir.
        std::fs::write(ws.join("alpha.rs"), "// alpha").unwrap();
        std::fs::write(ws.join("beta.rs"), "// beta").unwrap();
        std::fs::create_dir_all(ws.join("subdir")).unwrap();
        // Buffer with a string literal; we'll set the
        // document path so relative resolution lands in `ws`.
        let source = "let p = \"\";\n";
        let doc_path = ws.join("buffer.rs");
        let mut a = app_with_path(source, 10, doc_path);
        set_rust_syntax(&mut a, source);
        a.modal = ModalState::Insert;
        // Cursor between the empty string's quotes -> string
        // scope.
        a.cursor = Position::new(0, source.find("\"\"").unwrap() as u32 + 1);
        a.do_completion_trigger();
        assert!(a.completion_in_path_context, "path-context detected");
        let state = a.insert_completion.as_ref().expect("popup");
        let path_id = lattice_completion::PATH_SOURCE_ID;
        let texts: Vec<&str> = state
            .raw
            .iter()
            .filter(|c| c.source.as_ref().map(|s| s.as_str()) == Some(path_id))
            .map(|c| c.text.as_str())
            .collect();
        assert!(texts.contains(&"alpha.rs"), "alpha in {texts:?}");
        assert!(texts.contains(&"beta.rs"), "beta in {texts:?}");
        assert!(
            texts.contains(&"subdir/"),
            "subdir/ (with trailing slash) in {texts:?}",
        );
        // No buffer-words / tree-sitter / snippet candidates
        // intermix with the path popup.
        for cand in &state.raw {
            let src = cand.source.as_ref().map(|s| s.as_str()).unwrap_or("");
            assert_eq!(
                src, path_id,
                "non-path source `{src}` slipped into path-context popup",
            );
        }
    }

    #[test]
    fn path_source_skips_hidden_and_ignored_entries() {
        let ws = fresh_path_workspace("skip-hidden");
        std::fs::write(ws.join("visible.txt"), "v").unwrap();
        std::fs::write(ws.join(".hidden"), "h").unwrap();
        std::fs::create_dir_all(ws.join(".git")).unwrap();
        std::fs::create_dir_all(ws.join("node_modules")).unwrap();
        let source = "let p = \"\";\n";
        let mut a = app_with_path(source, 10, ws.join("buffer.rs"));
        set_rust_syntax(&mut a, source);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, source.find("\"\"").unwrap() as u32 + 1);
        a.do_completion_trigger();
        let state = a.insert_completion.as_ref().expect("popup");
        let texts: Vec<&str> = state.raw.iter().map(|c| c.text.as_str()).collect();
        assert!(texts.contains(&"visible.txt"));
        assert!(!texts.contains(&".hidden"), "dotfile filtered");
        assert!(!texts.contains(&".git/"), ".git filtered");
        assert!(!texts.contains(&"node_modules/"), "node_modules filtered",);
    }

    #[test]
    fn path_source_silent_outside_string_scope() {
        let source = "fn main() { let x = 1; }\n";
        let mut a = app_with(source, 10);
        set_rust_syntax(&mut a, source);
        a.modal = ModalState::Insert;
        // Cursor at end of line -- outside any string.
        a.cursor = Position::new(0, source.trim_end().len() as u32);
        a.do_completion_trigger();
        assert!(!a.completion_in_path_context);
        if let Some(state) = a.insert_completion.as_ref() {
            let path_id = lattice_completion::PATH_SOURCE_ID;
            for cand in &state.raw {
                assert_ne!(
                    cand.source.as_ref().map(|s| s.as_str()),
                    Some(path_id),
                    "no path candidates outside string scope",
                );
            }
        }
    }

    #[test]
    fn path_source_skipped_by_per_language_override() {
        let ws = fresh_path_workspace("disabled-via-override");
        std::fs::write(ws.join("alpha.rs"), "//").unwrap();
        let source = "let p = \"\";\n";
        let mut a = app_with_path(source, 10, ws.join("buffer.rs"));
        set_rust_syntax(&mut a, source);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, source.find("\"\"").unwrap() as u32 + 1);
        // Override the active language ("rust", since the
        // buffer path ends in `.rs`) to drop path source.
        a.per_language_completion.insert(
            "rust".into(),
            lattice_completion::PerLanguageOverrides {
                sources: Some(vec![lattice_completion::SourceId::new(
                    lattice_completion::BufferWordsSource::ID,
                )]),
                ..Default::default()
            },
        );
        a.do_completion_trigger();
        assert!(
            !a.completion_in_path_context,
            "path source disabled -> no path context",
        );
    }

    #[test]
    fn path_source_resolves_subdirectory_from_partial_path() {
        let ws = fresh_path_workspace("subdir-walk");
        std::fs::create_dir_all(ws.join("src")).unwrap();
        std::fs::write(ws.join("src/foo.rs"), "//").unwrap();
        std::fs::write(ws.join("src/bar.rs"), "//").unwrap();
        let source = "let p = \"src/\";\n";
        let mut a = app_with_path(source, 10, ws.join("buffer.rs"));
        set_rust_syntax(&mut a, source);
        a.modal = ModalState::Insert;
        // Cursor after `src/`.
        let after_slash = source.find("src/").unwrap() + "src/".len();
        a.cursor = Position::new(0, after_slash as u32);
        a.do_completion_trigger();
        assert!(a.completion_in_path_context);
        let state = a.insert_completion.as_ref().expect("popup");
        let texts: Vec<&str> = state.raw.iter().map(|c| c.text.as_str()).collect();
        assert!(
            texts.contains(&"foo.rs"),
            "src/foo.rs surfaced -- got {texts:?}"
        );
        assert!(texts.contains(&"bar.rs"), "src/bar.rs surfaced");
    }

    #[test]
    fn ghost_text_off_by_default_returns_none() {
        let mut a = app_with("foo", 5);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 3);
        open_popup_with_top_text(&mut a, "foo", "foobar");
        // Default: completion.ghost_text = false -> no ghost.
        assert!(a.completion_ghost_text_suffix().is_none());
    }

    #[test]
    fn ghost_text_returns_suffix_for_prefix_matching_top_candidate() {
        let mut a = app_with("foo", 5);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 3);
        a.do_set("completion.ghost_text=true");
        open_popup_with_top_text(&mut a, "foo", "foobar");
        assert_eq!(
            a.completion_ghost_text_suffix(),
            Some("bar".to_string()),
            "ghost suffix is the part of the candidate beyond the query prefix",
        );
    }

    #[test]
    fn ghost_text_case_insensitive_prefix_match() {
        let mut a = app_with("Foo", 5);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 3);
        a.do_set("completion.ghost_text=true");
        open_popup_with_top_text(&mut a, "Foo", "foobar");
        assert_eq!(a.completion_ghost_text_suffix(), Some("bar".to_string()),);
    }

    #[test]
    fn ghost_text_none_when_top_doesnt_prefix_match_query() {
        let mut a = app_with("xyz", 5);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 3);
        a.do_set("completion.ghost_text=true");
        // Top candidate is `bar`; query `xyz` doesn't prefix
        // it (matcher's substring tier still puts it on
        // screen, but ghost demands prefix-match).
        open_popup_with_top_text(&mut a, "xyz", "bar");
        assert!(a.completion_ghost_text_suffix().is_none());
    }

    #[test]
    fn ghost_text_none_when_query_is_empty() {
        let mut a = app_with("", 5);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 0);
        a.do_set("completion.ghost_text=true");
        open_popup_with_top_text(&mut a, "", "alpha");
        assert!(
            a.completion_ghost_text_suffix().is_none(),
            "empty query -> no ghost (any candidate would match)",
        );
    }

    #[test]
    fn ghost_text_none_in_path_context() {
        let mut a = app_with("\"\"", 5);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 1);
        a.do_set("completion.ghost_text=true");
        a.completion_in_path_context = true;
        open_popup_with_top_text(&mut a, "src", "src/foo.rs");
        assert!(
            a.completion_ghost_text_suffix().is_none(),
            "path popup already shows full filenames; ghost would double up",
        );
    }

    #[test]
    fn ghost_text_none_when_popup_closed() {
        let mut a = app_with("foo", 5);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 3);
        a.do_set("completion.ghost_text=true");
        // No open_popup_with_top_text call -> insert_completion = None.
        assert!(a.completion_ghost_text_suffix().is_none());
    }

    #[test]
    fn completion_high_priority_source_beats_tied_low_priority_peer() {
        // Two candidates tied on matcher score: one tagged
        // gen:lsp-completion (default priority 200), one tagged
        // gen:buffer-words (default 100). The LSP candidate
        // sorts above the buffer-words peer.
        let mut a = app_with("", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 0);
        let mut state = lattice_completion::InsertCompletionState::open(
            lattice_completion::CompletionTrigger::Manual,
            Position::ZERO,
            Position::ZERO,
            String::new(),
        );
        state.raw.push(
            lattice_completion::RawCandidate::plain(
                "from_words",
                lattice_completion::CandidateKind::Plain,
            )
            .with_source(lattice_completion::SourceId::new(
                lattice_completion::BufferWordsSource::ID,
            )),
        );
        state.raw.push(
            lattice_completion::RawCandidate::plain(
                "from_lsp",
                lattice_completion::CandidateKind::Plain,
            )
            .with_source(lattice_completion::SourceId::new(
                lattice_completion::LSP_COMPLETION_SOURCE_ID,
            )),
        );
        a.refilter_insert_completion(&mut state);
        assert_eq!(state.rendered[0].raw.text, "from_lsp");
        assert_eq!(state.rendered[1].raw.text, "from_words");
    }

    #[test]
    fn completion_priority_override_via_set_flips_source_order() {
        // After `:set completion.source.buffer-words.priority=300`
        // the buffer-words candidate outranks the LSP one
        // (300 > 200) at tied matcher score.
        let mut a = app_with("", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 0);
        a.do_set("completion.source.buffer-words.priority=300");
        let mut state = lattice_completion::InsertCompletionState::open(
            lattice_completion::CompletionTrigger::Manual,
            Position::ZERO,
            Position::ZERO,
            String::new(),
        );
        state.raw.push(
            lattice_completion::RawCandidate::plain(
                "from_lsp",
                lattice_completion::CandidateKind::Plain,
            )
            .with_source(lattice_completion::SourceId::new(
                lattice_completion::LSP_COMPLETION_SOURCE_ID,
            )),
        );
        state.raw.push(
            lattice_completion::RawCandidate::plain(
                "from_words",
                lattice_completion::CandidateKind::Plain,
            )
            .with_source(lattice_completion::SourceId::new(
                lattice_completion::BufferWordsSource::ID,
            )),
        );
        a.refilter_insert_completion(&mut state);
        assert_eq!(state.rendered[0].raw.text, "from_words");
        assert_eq!(state.rendered[1].raw.text, "from_lsp");
    }

    #[test]
    fn completion_untagged_candidate_gets_no_priority_lift() {
        // Candidate with no source field (plugin source not yet
        // wired into config, or test fixture) gets 0 priority
        // bonus; sorts below a tagged peer at tied matcher
        // score, but still appears in the rendered list.
        let mut a = app_with("", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 0);
        let mut state = lattice_completion::InsertCompletionState::open(
            lattice_completion::CompletionTrigger::Manual,
            Position::ZERO,
            Position::ZERO,
            String::new(),
        );
        state.raw.push(lattice_completion::RawCandidate::plain(
            "untagged",
            lattice_completion::CandidateKind::Plain,
        ));
        state.raw.push(
            lattice_completion::RawCandidate::plain(
                "tagged",
                lattice_completion::CandidateKind::Plain,
            )
            .with_source(lattice_completion::SourceId::new(
                lattice_completion::BufferWordsSource::ID,
            )),
        );
        a.refilter_insert_completion(&mut state);
        assert_eq!(state.rendered[0].raw.text, "tagged");
        assert_eq!(state.rendered[1].raw.text, "untagged");
    }

    #[test]
    fn completion_buffer_words_candidates_carry_their_source_tag() {
        // Regression: the buffer-words `InsertSource` impl
        // tags every produced candidate with its own id so the
        // ranker can apply per-source priority without the host
        // having to remember to tag.
        let mut a = app_with("alpha bravo ", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 12);
        a.do_completion_trigger();
        let state = a.insert_completion.as_ref().expect("popup");
        assert!(!state.rendered.is_empty());
        for cand in &state.rendered {
            let src = cand
                .raw
                .source
                .as_ref()
                .unwrap_or_else(|| panic!("candidate `{}` missing source tag", cand.raw.text));
            assert_eq!(src.as_str(), lattice_completion::BufferWordsSource::ID);
        }
    }

    #[test]
    fn completion_accept_increments_existing_frequency_count() {
        // Two accepts of the same item bump the count to 2.
        let mut a = app_with("alpha bravo charlie ", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 20);
        let key = (
            "bravo".to_string(),
            lattice_completion::CandidateKind::Plain,
        );
        a.completion_accept_freq.insert(key.clone(), 4);
        a.do_completion_trigger();
        let state = a.insert_completion.as_mut().expect("popup");
        let idx = state
            .rendered
            .iter()
            .position(|r| r.raw.text == "bravo")
            .expect("bravo present");
        state.selected = idx;
        a.do_completion_accept();
        assert_eq!(a.completion_accept_freq.get(&key).copied(), Some(5));
    }

    // ---- CSM.2: completion-mode tracks popup state ----

    /// Triggering the completion popup activates `completion-mode`
    /// on the document buffer. The mode is the architectural gate
    /// the keymap-overlay + active-source resolver read.
    #[test]
    fn completion_mode_activates_when_popup_opens() {
        let mut a = app_with("alpha bravo charlie ", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 20);
        assert!(
            !a.completion_popup_active(),
            "mode should be inactive before popup opens",
        );
        a.apply(Action::CompletionTrigger);
        assert!(
            a.completion_popup_active(),
            "mode should be active after popup opens",
        );
    }

    /// Cancelling the popup deactivates `completion-mode`.
    #[test]
    fn completion_mode_deactivates_after_cancel() {
        let mut a = app_with("alpha bravo charlie ", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 20);
        a.apply(Action::CompletionTrigger);
        assert!(a.completion_popup_active());
        a.apply(Action::CompletionCancel);
        assert!(
            !a.completion_popup_active(),
            "mode should deactivate on cancel",
        );
    }

    /// Accepting the popup deactivates `completion-mode`.
    #[test]
    fn completion_mode_deactivates_after_accept() {
        let mut a = app_with("alpha bravo charlie ", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 20);
        a.apply(Action::CompletionTrigger);
        assert!(a.completion_popup_active());
        a.apply(Action::CompletionAccept);
        assert!(
            !a.completion_popup_active(),
            "mode should deactivate on accept",
        );
    }

    // ---- CSM.K1: completion-mode / completion-popup-mode pair ----

    /// `completion-mode` auto-activates on the initial Document
    /// buffer so `<C-Space>` works out-of-the-box.
    #[test]
    fn completion_mode_auto_active_on_document_buffer() {
        let a = app_with("hi", 5);
        assert!(
            a.completion_mode_active_for(a.document_buffer_id),
            "completion-mode should be auto-active on the initial Document",
        );
    }

    /// Read-only buffer kinds (Help here) don't auto-activate
    /// `completion-mode`; `<C-Space>` is a silent no-op there.
    #[test]
    fn completion_mode_not_active_on_help_buffer() {
        let mut a = app_with("hi", 5);
        let help = crate::help::HelpContent::from_lines("t", vec!["body".into()]);
        let help_id = a.open_help_in_pane(help);
        assert!(
            !a.completion_mode_active_for(help_id),
            "completion-mode should be inactive on Help buffers",
        );
    }

    /// Trigger gate: `do_completion_trigger` no-ops when
    /// `completion-mode` is inactive on the active document.
    #[test]
    fn completion_trigger_noop_when_completion_mode_inactive() {
        use lattice_grammar::ModalState;
        let mut a = app_with("alpha bravo charlie ", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 20);
        // Force-deactivate completion-mode so the gate kicks in.
        let buffer_id = a.document_buffer_id;
        a.deactivate_mode_by_id(buffer_id, lattice_mode::CompletionMode::mode_id());
        assert!(!a.completion_mode_active_for(buffer_id));
        a.do_completion_trigger();
        assert!(
            a.insert_completion.is_none(),
            "popup should not open when completion-mode is inactive",
        );
    }

    /// `completion-popup-mode` (the transient) tracks popup state
    /// independently from `completion-mode` (the persistent
    /// gate). Both modes coexist while the popup is open.
    #[test]
    fn completion_popup_mode_distinct_from_completion_mode() {
        use lattice_grammar::ModalState;
        let mut a = app_with("alpha bravo charlie ", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 20);
        let buffer_id = a.document_buffer_id;
        // completion-mode is on (auto-activated); popup-mode is
        // off (no popup open yet).
        assert!(a.completion_mode_active_for(buffer_id));
        assert!(!a.completion_popup_mode_active_for(buffer_id));
        a.apply(Action::CompletionTrigger);
        // Both on once the popup opens.
        assert!(a.completion_mode_active_for(buffer_id));
        assert!(a.completion_popup_mode_active_for(buffer_id));
        a.apply(Action::CompletionCancel);
        // completion-mode stays on (persistent); popup-mode
        // deactivates (transient).
        assert!(a.completion_mode_active_for(buffer_id));
        assert!(!a.completion_popup_mode_active_for(buffer_id));
    }

    // ---- CSM.3: ActiveCompletionSources cache ----

    /// CSM.8a: the LSP completion source rides on the M.6.1
    /// cascade -- it does NOT auto-activate on Document at
    /// boot. The cache is empty for `gen:lsp-completion` until
    /// `lsp-mode` activates on the buffer; toggling
    /// `lsp-mode` on (via the auto-generated `:lsp-mode`
    /// command or its programmatic equivalent) attaches every
    /// LSP sub-mode including `lsp-completion-mode`, which the
    /// recompute hook picks up.
    #[test]
    fn lsp_completion_source_activates_on_lsp_mode_cascade() {
        let mut a = app_with("hi", 5);
        let buffer_id = a.document_buffer_id;
        let pre_ids: Vec<_> = a
            .buffer_locals
            .get(&buffer_id)
            .and_then(|locals| locals.get::<lattice_mode::ActiveCompletionSources>())
            .map(|c| c.0.iter().map(|c| c.id.as_str().to_string()).collect())
            .unwrap_or_default();
        assert!(
            !pre_ids.contains(&"gen:lsp-completion".to_string()),
            "pre-cascade cache should not contain LSP: {pre_ids:?}",
        );
        a.toggle_mode_by_name("lsp-mode");
        let cache = a
            .buffer_locals
            .get(&buffer_id)
            .and_then(|locals| locals.get::<lattice_mode::ActiveCompletionSources>())
            .expect("cache present");
        let post_ids: Vec<_> = cache.0.iter().map(|c| c.id.as_str().to_string()).collect();
        assert!(
            post_ids.contains(&"gen:lsp-completion".to_string()),
            "post-cascade cache should include gen:lsp-completion; got {post_ids:?}",
        );
        let lsp = cache
            .0
            .iter()
            .find(|c| c.id.as_str() == "gen:lsp-completion")
            .unwrap();
        assert_eq!(lsp.popup_filter_chord, Some('o'));
        assert_eq!(lsp.kind.kind_label(), "async");
    }

    /// CSM.4–CSM.7: source-contributing modes auto-activate on
    /// Document. The cache seeds with buffer-words, snippet,
    /// tree-sitter, and path contributions at boot. LSP (CSM.8a)
    /// rides on the M.6.1 cascade -- only attaches when the
    /// `lsp-mode` umbrella activates; tested separately.
    #[test]
    fn active_completion_sources_seeded_with_default_modes_at_boot() {
        let a = app_with("alpha bravo", 10);
        let cache = a
            .buffer_locals
            .get(&a.document_buffer_id)
            .and_then(|locals| locals.get::<lattice_mode::ActiveCompletionSources>())
            .expect("cache should be seeded at boot");
        let ids: Vec<_> = cache.0.iter().map(|c| c.id.as_str().to_string()).collect();
        assert!(ids.contains(&"gen:buffer-words".to_string()), "got {ids:?}");
        assert!(ids.contains(&"gen:snippet".to_string()), "got {ids:?}");
        assert!(
            ids.contains(&"gen:tree-sitter-symbol".to_string()),
            "got {ids:?}",
        );
        assert!(ids.contains(&"gen:path".to_string()), "got {ids:?}");
        let buffer_words = cache
            .0
            .iter()
            .find(|c| c.id.as_str() == "gen:buffer-words")
            .unwrap();
        assert_eq!(buffer_words.popup_filter_chord, Some('b'));
        let snippet = cache
            .0
            .iter()
            .find(|c| c.id.as_str() == "gen:snippet")
            .unwrap();
        // Snippets have no dedicated filter chord per §12.
        assert!(snippet.popup_filter_chord.is_none());
        let tree_sitter = cache
            .0
            .iter()
            .find(|c| c.id.as_str() == "gen:tree-sitter-symbol")
            .unwrap();
        assert_eq!(tree_sitter.popup_filter_chord, Some('t'));
        let path = cache
            .0
            .iter()
            .find(|c| c.id.as_str() == "gen:path")
            .unwrap();
        assert_eq!(path.popup_filter_chord, Some('f'));
    }

    /// CSM.4: triggering the popup populates candidates via
    /// the mode-contributed `buffer-words` source through the
    /// cache reader. No hardcoded call path anymore -- the
    /// only way candidates show up is via the
    /// `ActiveCompletionSources` walk.
    #[test]
    fn buffer_words_populates_via_mode_contributed_source() {
        let mut a = app_with("alpha bravo charlie ", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 20);
        a.do_completion_trigger();
        let state = a.insert_completion.as_ref().expect("popup open");
        let labels: Vec<String> = state.rendered.iter().map(|c| c.raw.text.clone()).collect();
        assert!(
            labels.contains(&"alpha".to_string()),
            "buffer-words should populate via the mode-contributed path; \
             got candidates: {labels:?}",
        );
    }

    /// The cache recomputes on mode transitions -- registering
    /// and activating a synthetic source-contributing mode adds
    /// its contribution to the cache (alongside the auto-active
    /// buffer-words contribution).
    #[test]
    fn active_completion_sources_recomputes_after_activation() {
        use lattice_completion::{
            CompletionSourceContribution, CompletionSourceKind, RawCandidate, SyncCompletionSource,
        };
        use std::sync::Arc;

        #[derive(Debug)]
        struct StubSource;
        impl SyncCompletionSource for StubSource {
            fn produce(&self, _ctx: &lattice_completion::InsertContext<'_>) -> Vec<RawCandidate> {
                vec![RawCandidate::plain(
                    "stub".to_string(),
                    lattice_completion::CandidateKind::Plain,
                )]
            }
        }
        struct StubMode;
        impl lattice_mode::Mode for StubMode {
            type Guard = ();
            fn id(&self) -> lattice_mode::ModeId {
                lattice_mode::ModeId::new("stub-csm3-mode")
            }
            fn kind(&self) -> lattice_mode::ModeKind {
                lattice_mode::ModeKind::Minor
            }
            fn completion_sources(&self) -> Vec<CompletionSourceContribution> {
                vec![CompletionSourceContribution {
                    id: lattice_completion::SourceId::new("gen:stub-csm3"),
                    default_priority: 50,
                    auto_trigger: true,
                    trigger_chars: Vec::new(),
                    popup_filter_chord: None,
                    kind: CompletionSourceKind::Sync(Arc::new(StubSource)),
                }]
            }
            fn on_activate(
                &self,
                _ctx: lattice_mode::ModeContext,
            ) -> lattice_mode::LifecycleFuture<'_, ()> {
                Box::pin(async { Ok(()) })
            }
        }

        let mut a = app_with("hi", 5);
        let registry = std::sync::Arc::make_mut(&mut a.mode_registry);
        let mode_id = registry.register(StubMode).expect("register");
        let buffer_id = a.document_buffer_id;
        a.activate_mode_by_id(buffer_id, mode_id);

        // CSM.4: buffer-words-mode contributes too, so the
        // cache holds two entries -- the auto-active
        // `gen:buffer-words` plus the freshly-activated
        // `gen:stub-csm3`.
        let cache = a
            .buffer_locals
            .get(&buffer_id)
            .and_then(|locals| locals.get::<lattice_mode::ActiveCompletionSources>())
            .expect("cache present");
        let ids: Vec<_> = cache.0.iter().map(|c| c.id.as_str().to_string()).collect();
        assert!(
            ids.contains(&"gen:buffer-words".to_string()),
            "buffer-words contribution should remain; got {ids:?}",
        );
        assert!(
            ids.contains(&"gen:stub-csm3".to_string()),
            "stub mode's source should be cached; got {ids:?}",
        );

        a.deactivate_mode_by_id(buffer_id, mode_id);
        let cache = a
            .buffer_locals
            .get(&buffer_id)
            .and_then(|locals| locals.get::<lattice_mode::ActiveCompletionSources>())
            .expect("cache present");
        let ids: Vec<_> = cache.0.iter().map(|c| c.id.as_str().to_string()).collect();
        assert!(
            !ids.contains(&"gen:stub-csm3".to_string()),
            "stub source should drop after deactivation; got {ids:?}",
        );
        // buffer-words remains (its mode is still active).
        assert!(ids.contains(&"gen:buffer-words".to_string()));
    }

    /// `completion_popup_active()` reads the mode-active state,
    /// not the `insert_completion` field. With the field manually
    /// nulled (test-only foot-gun -- production code uses
    /// `do_completion_cancel`), the mode stays active until the
    /// next reconcile. This pins the gate's source-of-truth
    /// inversion: external readers see the mode, not the state.
    #[test]
    fn completion_popup_active_reads_mode_not_state_field() {
        let mut a = app_with("alpha bravo charlie ", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 20);
        a.apply(Action::CompletionTrigger);
        assert!(a.completion_popup_active());
        // Manually drop the popup state (skipping the reconcile).
        a.insert_completion = None;
        // Mode is still active because nothing's run
        // `sync_keymap_overlays` between the manual drop and the
        // read. External readers see "popup is active" until the
        // next dispatch tail.
        assert!(a.completion_popup_active());
        // Reconcile brings mode + state back into lockstep.
        a.sync_keymap_overlays();
        assert!(!a.completion_popup_active());
    }

    /// CSM.K2: `do_completion_filter_to_source` narrows the
    /// rendered list to candidates whose `source` matches the
    /// supplied id. The other source's candidates stay in
    /// `state.raw` so a subsequent `do_completion_filter_clear`
    /// can restore them.
    #[test]
    fn completion_filter_to_source_narrows_rendered_list() {
        let mut a = app_with("", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 0);
        let mut state = lattice_completion::InsertCompletionState::open(
            lattice_completion::CompletionTrigger::Manual,
            Position::ZERO,
            Position::ZERO,
            String::new(),
        );
        state.raw.push(
            lattice_completion::RawCandidate::plain(
                "from_words",
                lattice_completion::CandidateKind::Plain,
            )
            .with_source(lattice_completion::SourceId::new(
                lattice_completion::BufferWordsSource::ID,
            )),
        );
        state.raw.push(
            lattice_completion::RawCandidate::plain(
                "from_lsp",
                lattice_completion::CandidateKind::Plain,
            )
            .with_source(lattice_completion::SourceId::new(
                lattice_completion::LSP_COMPLETION_SOURCE_ID,
            )),
        );
        a.refilter_insert_completion(&mut state);
        assert_eq!(state.rendered.len(), 2);
        a.insert_completion = Some(state);
        a.do_completion_filter_to_source(lattice_completion::LSP_COMPLETION_SOURCE_ID.to_string());
        let s = a.insert_completion.as_ref().unwrap();
        assert_eq!(s.rendered.len(), 1);
        assert_eq!(s.rendered[0].raw.text, "from_lsp");
        // Both raw rows survive so `clear` can restore them.
        assert_eq!(s.raw.len(), 2);
    }

    /// CSM.K2: `do_completion_filter_clear` removes the active
    /// filter and refilters against the full raw pool.
    #[test]
    fn completion_filter_clear_restores_full_list() {
        let mut a = app_with("", 10);
        a.modal = ModalState::Insert;
        a.cursor = Position::new(0, 0);
        let mut state = lattice_completion::InsertCompletionState::open(
            lattice_completion::CompletionTrigger::Manual,
            Position::ZERO,
            Position::ZERO,
            String::new(),
        );
        state.raw.push(
            lattice_completion::RawCandidate::plain(
                "from_words",
                lattice_completion::CandidateKind::Plain,
            )
            .with_source(lattice_completion::SourceId::new(
                lattice_completion::BufferWordsSource::ID,
            )),
        );
        state.raw.push(
            lattice_completion::RawCandidate::plain(
                "from_lsp",
                lattice_completion::CandidateKind::Plain,
            )
            .with_source(lattice_completion::SourceId::new(
                lattice_completion::LSP_COMPLETION_SOURCE_ID,
            )),
        );
        state.source_filter = Some(lattice_completion::SourceId::new(
            lattice_completion::LSP_COMPLETION_SOURCE_ID,
        ));
        a.refilter_insert_completion(&mut state);
        assert_eq!(state.rendered.len(), 1);
        a.insert_completion = Some(state);
        a.do_completion_filter_clear();
        let s = a.insert_completion.as_ref().unwrap();
        assert!(s.source_filter.is_none());
        assert_eq!(s.rendered.len(), 2);
    }
}
