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
    App, EchoLevel, PathCompletionCache, SNIPPET_COMPLETION_KIND_ID,
    SnippetCandidateMeta, dedup_rendered_by_text, is_path_byte, is_word_char_byte,
    lsp_position_to_app_byte, word_under_cursor,
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
        // Path-completion mode: when the cursor sits inside a
        // string literal AND the language enables `gen:path`,
        // the popup is path-only -- buffer-words / snippet /
        // tree-sitter would emit non-filename candidates that
        // confuse the experience (e.g., a buffer word matching
        // partway through a filename). Spec §3.4 has
        // `suppress_in = ["string", "comment"]` covering this
        // for the other sources; until that knob is enforced,
        // the path-context branch enforces the same effect.
        if self.completion_in_path_context {
            self.populate_path_completion(state);
            self.refilter_insert_completion(state);
            return;
        }
        let ctx = lattice_completion::InsertContext {
            buffer,
            cursor: state.cursor,
            anchor: state.anchor,
            query: &state.query,
            trigger,
            case_sensitive: false,
        };
        // Resolve the active language's effective config once;
        // both sync sources (buffer-words, snippet) gate emit on
        // it. The global default (no override) returns
        // `sources = None` -> every source contributes.
        let language = self.active_language_id();
        let effective = self.effective_completion_for(&language);
        let mut raw: Vec<lattice_completion::RawCandidate> = Vec::new();
        // Source 1: buffer-words.
        let buffer_words_id = lattice_completion::SourceId::new(
            lattice_completion::BufferWordsSource::ID,
        );
        if effective.source_enabled(&buffer_words_id) {
            let buffer_words = lattice_completion::BufferWordsSource::new();
            raw.extend(lattice_completion::InsertSource::produce(&buffer_words, &ctx));
        }
        // Source 2: snippets (Phase 4.2.g.4). Resolve the
        // active language so per-language snippets surface
        // ahead of any-language `*` packs. Snippet meta lives
        // in a sidecar; the candidate's Extension payload
        // points at the meta-vec index.
        self.insert_completion_snippet_meta.clear();
        let snippet_id = lattice_completion::SourceId::new(
            lattice_completion::SNIPPET_SOURCE_ID,
        );
        let snippet_matches: Vec<&lattice_snippet::Snippet> =
            if effective.source_enabled(&snippet_id) {
                self.snippet_registry.matching_prefix(&language, &state.query)
            } else {
                Vec::new()
            };
        for snip in snippet_matches {
            let idx = self.insert_completion_snippet_meta.len() as u32;
            let prefix = snip
                .prefixes
                .first()
                .cloned()
                .unwrap_or_else(|| snip.name.clone());
            let display = match snip.description.as_deref() {
                Some(d) if !d.is_empty() => format!("{prefix}  {d}"),
                _ => prefix.clone(),
            };
            let mut cand = lattice_completion::RawCandidate::plain(
                prefix.clone(),
                lattice_completion::CandidateKind::Plain,
            )
            .with_source(lattice_completion::SourceId::new(
                lattice_completion::SNIPPET_SOURCE_ID,
            ));
            cand.display = display;
            cand.data = lattice_completion::CandidateData::Extension {
                kind_id: SNIPPET_COMPLETION_KIND_ID,
                payload: idx.to_le_bytes().to_vec(),
            };
            raw.push(cand);
            self.insert_completion_snippet_meta.push(SnippetCandidateMeta {
                name: snip.name.clone(),
                prefix,
                description: snip.description.clone(),
                body: snip.body.clone(),
            });
        }
        // Source 3: tree-sitter local symbols (Phase 4.2.g.6
        // (1/2)). Walks the buffer's syntax tree per popup-
        // trigger; emits definition-position identifiers
        // (functions, structs, let bindings, parameters) as
        // candidates. Skipped when:
        //   - the per-language source filter excludes
        //     `gen:tree-sitter-symbol`
        //   - no `Syntax` is attached (e.g. plain-text buffer)
        //   - the language ships no `symbols.scm` query
        //     (`collect_symbols` returns empty)
        // Duplicates against buffer-words are deduped by text;
        // the tree-sitter-tagged copy wins so the ranker's
        // per-source priority applies.
        let tree_sitter_id = lattice_completion::SourceId::new(
            lattice_completion::TREE_SITTER_SYMBOL_SOURCE_ID,
        );
        if effective.source_enabled(&tree_sitter_id)
            && let Some(syntax) = self.syntax.as_ref()
        {
            // Each source emits independently. tree-sitter
            // names overlap heavily with buffer-words (which
            // walks every word in the rope), so cross-source
            // dedup at the producer level would always erase
            // tree-sitter -- buffer-words is a superset by
            // construction. Per spec §3.4 the per-source
            // priority handles ranking (buffer-words 100 vs
            // tree-sitter 80) so the user-typed prefix surfaces
            // the buffer-words copy ahead of the tree-sitter
            // copy on ties; visual deduping of identical text
            // in the popup is a 4.2.g.7 polish item that needs
            // the renderer to merge same-text rows by source
            // label.
            for sym in syntax.snapshot().collect_symbols() {
                if sym == state.query {
                    continue;
                }
                let cand = lattice_completion::RawCandidate::plain(
                    sym,
                    lattice_completion::CandidateKind::Plain,
                )
                .with_source(tree_sitter_id.clone());
                raw.push(cand);
            }
        }
        state.raw = raw;
        self.refilter_insert_completion(state);
    }

    /// Path-completion sync producer (Phase 4.2.g.6 (2/2)).
    /// Walks the directory referenced by the partial path
    /// the user has typed inside the active string literal and
    /// emits one candidate per filesystem entry (capped at 200,
    /// hidden / ignored entries skipped, directories carry a
    /// trailing `/`). Resolves relative paths against the
    /// active document's parent directory; falls back to
    /// `std::env::current_dir()` for unsaved buffers.
    fn populate_path_completion(
        &mut self,
        state: &mut lattice_completion::InsertCompletionState,
    ) {
        const MAX_ENTRIES: usize = 200;
        // Hardcoded ignore set for v1; `.gitignore` integration
        // queues for a follow-up (needs the `ignore` crate +
        // the workspace-root resolution we already do for the
        // config loader).
        const IGNORE_NAMES: &[&str] = &[".git", "node_modules", "target", "dist"];

        let snap = self.document.snapshot();
        let line_text = snap.buffer.line(state.cursor.line).unwrap_or_default();
        let line_bytes = line_text.as_bytes();
        let cursor_in_line = (state.cursor.byte as usize).min(line_bytes.len());
        // Walk back over path bytes (NOT stopping at `/`) to
        // recover the full partial path the user has typed
        // inside the string literal. The trigger anchor stops
        // at `/` so the popup-supplied filename only replaces
        // the tail; here we want the full thing so we know
        // *which* directory to walk.
        let mut path_start = cursor_in_line;
        while path_start > 0 {
            let b = line_bytes[path_start - 1];
            if b == b'/' || is_path_byte(b) {
                path_start -= 1;
            } else {
                break;
            }
        }
        let partial: &str = &line_text[path_start..cursor_in_line];
        // Split partial at the last `/` (boundary between dir
        // and the filename prefix).
        let dir_part = match partial.rfind('/') {
            Some(i) => &partial[..=i], // keep trailing slash
            None => "",
        };
        let base_dir: std::path::PathBuf = if dir_part.starts_with('/') {
            std::path::PathBuf::from(dir_part)
        } else {
            let buffer_dir = snap
                .path
                .as_ref()
                .and_then(|p| p.parent().map(|p| p.to_path_buf()));
            let base = buffer_dir
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            if dir_part.is_empty() {
                base
            } else {
                base.join(dir_part)
            }
        };

        // Cache check: re-use the previous read_dir if the
        // directory's mtime hasn't changed. The popup re-fires on
        // every Insert keystroke; without this cache the
        // consecutive keystrokes for the same dir each pay a
        // full read_dir + per-entry file_type() walk. With it,
        // each keystroke past the first pays one metadata()
        // call. Audit slice 5 / H5.
        let current_mtime = std::fs::metadata(&base_dir)
            .ok()
            .and_then(|m| m.modified().ok());
        let cache_hit = self
            .path_completion_cache
            .as_ref()
            .filter(|c| c.dir == base_dir && c.mtime == current_mtime);
        let entries: Vec<(String, bool)> = match cache_hit {
            Some(c) => c.entries.clone(),
            None => {
                let read = match std::fs::read_dir(&base_dir) {
                    Ok(it) => it,
                    Err(_) => {
                        // Directory unreadable / missing; popup
                        // stays empty + the cache is invalidated
                        // so a later mtime-bump triggers a fresh
                        // read.
                        self.path_completion_cache = None;
                        return;
                    }
                };
                let mut entries: Vec<(String, bool)> = read
                    .flatten()
                    .filter_map(|entry| {
                        entry
                            .file_name()
                            .to_str()
                            .map(|name| {
                                let is_dir = entry
                                    .file_type()
                                    .map(|t| t.is_dir())
                                    .unwrap_or(false);
                                (name.to_string(), is_dir)
                            })
                    })
                    .collect();
                entries.sort_by(|a, b| a.0.cmp(&b.0));
                self.path_completion_cache = Some(PathCompletionCache {
                    dir: base_dir.clone(),
                    mtime: current_mtime,
                    entries: entries.clone(),
                });
                entries
            }
        };
        let path_id = lattice_completion::SourceId::new(
            lattice_completion::PATH_SOURCE_ID,
        );
        let mut emitted = 0;
        for (name, is_dir) in entries {
            if emitted >= MAX_ENTRIES {
                break;
            }
            if name.starts_with('.') {
                // Skip hidden entries by default. The user can
                // type `.` and the popup will re-trigger to
                // show them once `auto_trigger` lands.
                continue;
            }
            if IGNORE_NAMES.contains(&name.as_str()) {
                continue;
            }
            let (text, kind) = if is_dir {
                (
                    format!("{name}/"),
                    lattice_completion::CandidateKind::Directory,
                )
            } else {
                (name, lattice_completion::CandidateKind::File)
            };
            let cand = lattice_completion::RawCandidate::plain(text, kind)
                .with_source(path_id.clone());
            state.raw.push(cand);
            emitted += 1;
        }
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
    /// (`docs/insert-completion.md` §3.4 / §3.6) plus the capped
    /// frequency lift. Future bonus terms (preselect,
    /// deprecated penalty) compose into this same closure as
    /// 4.2.g.5 / 4.2.g.6 land them.
    fn completion_total_bonus(
        &self,
        raw: &lattice_completion::RawCandidate,
    ) -> u32 {
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
    fn priority_for_source(
        &self,
        source: &lattice_completion::SourceId,
    ) -> u32 {
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
            ctx.directory = path
                .parent()
                .map(|p| p.display().to_string());
        }
        ctx.line_index = Some(self.cursor.line);
        if let Some(line) = snap.buffer.line(self.cursor.line) {
            ctx.current_line = Some(line);
        }
        if let Some(word) = word_under_cursor(&snap.buffer, self.cursor) {
            ctx.current_word = Some(word);
        }
        // CLIPBOARD via the system register.
        if let Some(reg) = self.registers.get(&Register::System) {
            ctx.clipboard = Some(reg.content.clone());
        }
        ctx
    }

    /// Look up the snippet meta sidecar entry for a candidate,
    /// when it's a snippet-sourced one. Returns `None` for
    /// non-snippet candidates.
    pub(super) fn snippet_meta_for(
        &self,
        candidate: &lattice_completion::RenderedCandidate,
    ) -> Option<&SnippetCandidateMeta> {
        let lattice_completion::CandidateData::Extension { kind_id, payload } =
            &candidate.raw.data
        else {
            return None;
        };
        if *kind_id != SNIPPET_COMPLETION_KIND_ID {
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
        self.insert_completion_snippet_meta.get(idx)
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
            let end_byte = lsp_position_to_app_byte(
                &snap.buffer,
                te.range.end.line,
                te.range.end.character,
            );
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
                && let Ok(pos) =
                    self.document.snapshot().buffer.byte_to_position(first.start)
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

    pub(super) fn expand_snippet(
        &mut self,
        body: &lattice_snippet::SnippetBody,
        anchor: Position,
    ) {
        let vars = self.snippet_variable_context();
        let rendered = lattice_snippet::render::render(body, &vars);
        // Splice the rendered text over `[anchor, cursor]`.
        let range = lattice_protocol::position::Range::new(anchor, self.cursor);
        let edit = Edit::replace(range, rendered.text.clone());
        let applied = match self.apply_edit_blocking(edit) {
            Ok(a) => a,
            Err(e) => {
                self.set_message(
                    EchoLevel::Error,
                    format!("snippet: apply failed: {e:?}"),
                );
                return;
            }
        };
        // The host's offset of the snippet origin = anchor
        // converted to a buffer byte offset. ActiveSnippet
        // tracks ranges in buffer bytes; since our rope edit
        // returned the inserted_range, we recompute the
        // origin from the buffer's positional API.
        let origin = match self
            .document
            .snapshot()
            .buffer
            .position_to_byte(applied.inserted_range.start)
        {
            Ok(b) => b,
            Err(_) => 0,
        };
        if !rendered.tabstops.is_empty() {
            let mut active =
                lattice_snippet::ActiveSnippet::from_render(&rendered, origin);
            // Focus the first tabstop and move the cursor.
            if let Some(group) = active.focus_first()
                && let Some(first) = group.ranges.first()
                && let Ok(pos) =
                    self.document.snapshot().buffer.byte_to_position(first.start)
            {
                self.cursor = pos;
            }
            self.active_snippet = Some(active);
            self.modal = ModalState::Insert;
        } else {
            self.cursor = applied.inserted_range.end;
        }
    }

    /// Effective completion config for `language` -- per-language
    /// override lays over the global typed option which lays
    /// over the spec fallback. Used at every producer-side
    /// enforcement seam (sync source filter, LSP fan-out, the
    /// `auto_insert_single` check at popup-open).
    pub(crate) fn effective_completion_for(
        &self,
        language: &str,
    ) -> EffectiveCompletionConfig {
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
