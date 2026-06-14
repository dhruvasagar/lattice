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
//! What does NOT live here at all: the completion provider
//! registry, source plugins, snippet parser -- those live
//! in `crate::completion` / `crate::snippet`. The actual
//! popup state machine (`populate_*`, `refilter_*`,
//! `do_completion_trigger`, accept paths, `expand_snippet`,
//! LSP request/drain/apply) lives host-side in
//! `lattice_host::dispatch::Editor`; the methods in this file
//! are renderer-thread delegates that route through the
//! `mutate_editor` / `read_editor` seam.

use super::App;

// Slice 3c.final.E.5g: `Position`, `SnippetCandidateMeta`, and
// `EffectiveCompletionConfig` are reached only from the
// `#[cfg(test)] impl App` block + the `mod tests` block below.
#[cfg(test)]
use lattice_protocol::position::Position;
#[cfg(test)]
use super::SnippetCandidateMeta;
#[cfg(test)]
use lattice_host::dispatch::EffectiveCompletionConfig;

impl App {
    /// `<C-x><C-s>` -- direct snippet expansion (Phase 4.2.g.4).
    /// 5.5.SNIPPET.1: body migrated to
    /// [`lattice_host::dispatch::Editor::do_snippet_expand_at_cursor`].
    /// Delegate retained for direct test callers in this file and
    /// for the `Effect::SnippetExpand` route through `App::apply`.
    pub fn do_snippet_expand_at_cursor(&mut self) {
        // Slice 3c.final.E.3: route through `mutate_editor`.
        self.mutate_editor(|e| e.do_snippet_expand_at_cursor());
    }

    // SN.2b (2026-06-12): the `do_snippet_next/prev_placeholder`
    // App delegates are gone. `<Tab>` / `<S-Tab>` placeholder
    // navigation is mode-owned — `active-snippet-mode` registers
    // `ActionContext -> Effect` closures on the
    // `ActionHandlerRegistry`, and the real chord flows through
    // the host's generic dispatch → registry-lookup path (the
    // same path the project-search `<CR>` / `gr` chords use). The
    // authoritative handler-level test lives in
    // `lattice_snippet::modes`.

    /// `:reload-snippets` -- 5.8.AF.3: body migrated to
    /// [`lattice_host::dispatch::Editor::do_reload_snippets`].
    pub fn do_reload_snippets(&mut self) {
        self.mutate_editor(|e| e.do_reload_snippets());
    }

    // 5.5.H: `move_cursor_to_snippet_group` retired (zero
    // callers; `Editor::move_cursor_to_snippet_group` host-side
    // is the live copy used by the migrated snippet-nav arms).
}

impl App {
    pub fn do_completion_next(&mut self) {
        // Phase 5.8.AD.4: body migrated.
        self.mutate_editor(|e| e.do_completion_next());
    }

    pub fn do_completion_prev(&mut self) {
        // Phase 5.8.AD.4: body migrated.
        self.mutate_editor(|e| e.do_completion_prev());
    }

    // 5.5.G.14: `do_completion_docs_scroll_down` /
    // `do_completion_docs_scroll_up` migrated to
    // [`lattice_host::dispatch::Editor`].

    // Slice 3c.final.E.5g: `populate_insert_completion_sync` +
    // `refilter_insert_completion` App-side delegates retired.
    // `populate_*` had no callers (host invokes its own version
    // inside `Editor::do_completion_trigger`); `refilter_*` is
    // test-fixture surface, now in the `#[cfg(test)] impl App`
    // block below.

    // Phase 5.8.AD.4: `completion_total_bonus` migrated to host.

    // Phase 5.8.AD.4: `priority_for_source` migrated to host.

    /// Manual trigger / refresh. Opens the popup if it's
    /// closed; refreshes raw + rendered candidates if it's
    /// already open. Sources contributing today: buffer-words.
    /// LSP / snippets / path / tree-sitter follow in 4.2.g.2+.
    pub fn do_completion_trigger(&mut self) {
        // Phase 5.8.AD.4: body migrated.
        self.mutate_editor(|e| e.do_completion_trigger());
    }

    // Slice 3c.final.E.5g: `do_completion_accept_then_insert`
    // App-side delegate moved to `#[cfg(test)] impl App` below
    // — only test fixtures call it; production path goes through
    // the host directly inside `Editor::dispatch`.

    /// Suffix of the top-ranked completion candidate for ghost
    /// text. Phase 5.8.AD.4: migrated.
    pub fn completion_ghost_text_suffix(&self) -> Option<String> {
        self.read_editor(move |e| e.completion_ghost_text_suffix())
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
    // Phase 5.8.AD.4: second `completion_ghost_text_suffix` body
    // retired (delegate above suffices).
    // Phase 5.8.AD.4: `effective_commit_chars_for` migrated to host.

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
        // Phase 5.8.AD.4: body migrated.
        self.mutate_editor(|e| e.do_completion_accept());
    }

    /// `<C-d>` inside the completion-popup minor mode.
    /// Toggles the side documentation popup. When opening,
    /// pre-fills `body` from the focused candidate's cached
    /// metadata when available; fires
    /// `completionItem/resolve` when the documentation is
    /// missing AND the originating server advertises the
    /// resolve provider.
    pub fn do_completion_toggle_docs(&mut self) {
        // Phase 5.8.AD.4: body migrated.
        self.mutate_editor(|e| e.do_completion_toggle_docs());
    }

    /// Build the docs body for the popup's currently-focused
    /// candidate. Phase 5.8.AD.4: migrated.
    pub(super) fn docs_body_for_selected(&self) -> Option<String> {
        self.read_editor(move |e| e.docs_body_for_selected())
    }

    /// True when the focused candidate needs resolve. Phase 5.8.AD.4.
    pub(super) fn selected_needs_resolve(&self) -> bool {
        self.read_editor(move |e| e.selected_needs_resolve())
    }

    // Slice 3c.final.E.5g:
    // `maybe_refresh_insert_completion_after_edit` App-side
    // delegate retired — zero callers anywhere. The host
    // version (`Editor::maybe_refresh_insert_completion_after_edit`)
    // is invoked directly inside `Editor::dispatch` after edit
    // application; LSP `isIncomplete` follow-up Actions flow
    // through `DispatchOutcome::next_actions` on the dispatch
    // tail like every other deferred action.

    // 5.5.G.14: body migrated to
    // [`lattice_host::dispatch::Editor::do_completion_cancel`].
    // Kept as a delegate -- a handful of LSP-side and App-side
    // paths still call `do_completion_cancel` directly
    // (`run_invocation` exit, `<Esc>` flush, snippet abort).
    #[allow(dead_code)]
    pub fn do_completion_cancel(&mut self) {
        self.mutate_editor(|e| e.do_completion_cancel());
    }

    /// CSM.K2: restrict the open completion popup to a single
    /// source. `id` is the `SourceId` as a raw string (e.g.
    /// `"gen:buffer-words"`, `"gen:lsp-completion"`). When the
    /// referenced source has no candidates in the current
    /// `state.raw`, refilter yields an empty rendered list --
    /// the popup stays open so the user can switch chords or
    /// clear the filter without losing the trigger context.
    pub fn do_completion_filter_to_source(&mut self, id: String) {
        // Phase 5.8.AD.4: body migrated.
        self.mutate_editor(move |e| e.do_completion_filter_to_source(id));
    }

    /// CSM.K2: clear the active source filter. Phase 5.8.AD.4.
    pub fn do_completion_filter_clear(&mut self) {
        self.mutate_editor(|e| e.do_completion_filter_clear());
    }

    // Phase 5.8.AD.4: `refresh_docs_popup_for_selection`
    // migrated to host as a private helper.

    /// Build a `VariableContext` for snippet expansion from
    /// the active buffer / cursor / clipboard / etc. 5.5.SNIPPET.1:
    /// body migrated to
    /// [`lattice_host::dispatch::Editor::snippet_variable_context`].
    pub(super) fn snippet_variable_context(&self) -> lattice_snippet::VariableContext {
        self.read_editor(move |e| e.snippet_variable_context())
    }

    // Slice 3c.final.E.5g: `snippet_meta_for` App-side delegate
    // moved to `#[cfg(test)] impl App` below — only test fixtures
    // call it; production path is `Editor::snippet_meta_for`
    // invoked from inside `Editor::do_completion_accept`.

    // Slice 3c.final.E.5g: `expand_snippet_with_lsp_edits` +
    // `expand_snippet` App-side delegates retired — zero callers
    // anywhere. The host versions are invoked directly from
    // `Editor::do_completion_accept` and the snippet picker
    // accept arm.

    /// Active buffer's snippet language id. 5.5.SNIPPET.1: body
    /// migrated to
    /// [`lattice_host::dispatch::Editor::active_language_id`].
    /// Delegate retained for App-side completion / LSP callers.
    pub(super) fn active_language_id(&self) -> String {
        self.read_editor(move |e| e.active_language_id())
    }

    // Slice 3c.final.E.5g: `effective_completion_for` App-side
    // delegate moved to `#[cfg(test)] impl App` below — host-side
    // call sites (`Editor::do_completion_trigger`,
    // `Editor::do_completion_accept`, etc.) reach the host
    // method directly; only test fixtures need an App-side
    // delegate.
}

// Slice 3c.final.E.5g — test-fixture surface.
//
// These four delegates have no production callers; their bodies
// are reached directly inside `Editor::dispatch` for the
// production paths. Tests still need to poke the host method
// against a fully-built `App`, so the delegates survive behind a
// `#[cfg(test)]` gate. Once `App.editor: Editor` becomes
// `App.editor_actor: EditorActorHandle` (the slice-E.swap
// follow-up), each body flips to read through the audit's
// planned `App::editor()` / `App::editor_mut()` cfg-gated
// accessors.
#[cfg(test)]
impl App {
    pub(super) fn refilter_insert_completion(
        &self,
        state: &mut lattice_completion::InsertCompletionState,
    ) {
        self.editor.refilter_insert_completion(state);
    }

    pub fn do_completion_accept_then_insert(&mut self, ch: char) {
        let mut out = lattice_host::dispatch::DispatchOutcome::default();
        self.editor.do_completion_accept_then_insert(ch, &mut out);
        for follow_up in std::mem::take(&mut out.next_actions) {
            self.apply(follow_up);
        }
    }

    pub(super) fn snippet_meta_for(
        &self,
        candidate: &lattice_completion::RenderedCandidate,
    ) -> Option<SnippetCandidateMeta> {
        self.editor.snippet_meta_for(candidate)
    }

    pub(crate) fn effective_completion_for(&self, language: &str) -> EffectiveCompletionConfig {
        self.editor.effective_completion_for(language)
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
            a.editor.completion_state.is_none(),
            "popup must not open when the only candidate auto-inserts"
        );
        assert_eq!(a.editor.command_line, "set foldmethod=indent");
    }

    #[test]
    fn auto_insert_single_off_keeps_popup_for_one_candidate() {
        // Disabling reverts to "always show popup, even with one row".
        let mut a = app_in_command_mode("set foldmethod=ind");
        a.set_completion_auto_insert_single_for_test(false);
        a.apply(Action::CommandLineCompleteOrAdvance);
        let state = a
            .editor
            .completion_state
            .as_ref()
            .expect("popup should open when option is off");
        assert_eq!(state.candidates.len(), 1);
        assert_eq!(
            a.editor.command_line, "set foldmethod=ind",
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
            .editor
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
            .editor
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
            a.editor.completion_state.is_some(),
            "popup must stay open when narrowed mid-typing"
        );
        assert_eq!(a.editor.command_line, "set foldmethod=ind");
    }

    #[test]
    fn auto_insert_single_set_via_set_command() {
        // `:set nocompletion.auto_insert_single` flips the bool;
        // `:set completion.auto_insert_single` flips it back.
        let mut a = app_with("xx", 10);
        a.editor.command_line = "set nocompletion.auto_insert_single".into();
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert!(!a.completion_auto_insert_single());
        a.editor.command_line = "set completion.auto_insert_single".into();
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert!(a.completion_auto_insert_single());
    }

    #[test]
    fn completion_kind_glyph_distinct_for_common_kinds() {
        use lattice_lsp::lsp_types::CompletionItemKind as K;
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
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(0, 3);
        install_snippet(
            &mut a,
            "*",
            "for-loop",
            "for",
            "for ${1:i} in ${2:iter} { $0 }",
        );
        a.do_snippet_expand_at_cursor();
        // Buffer text should be the rendered snippet.
        let text = a.editor.document.snapshot().buffer.as_string();
        assert_eq!(text, "for i in iter {  }");
        // Active snippet present, focused on $1.
        let snippet_index = a.editor.snippet_session.with_mut(|s| s.as_ref().expect("snippet active").current_index());
        assert_eq!(snippet_index, Some(1));
        // Cursor at start of `i`.
        assert_eq!(a.editor.cursor, Position::new(0, 4));
    }

    // SN.2b (2026-06-12): the placeholder-navigation tests
    // (`snippet_next_placeholder_walks_through_groups_and_drops_on_zero`,
    // `snippet_prev_placeholder_walks_back`) moved to
    // `lattice_snippet::modes` as a handler-level dispatch test —
    // the `<Tab>` / `<S-Tab>` bodies are now `active-snippet-mode`
    // `ActionHandlerRegistry` closures, so the session-transition
    // coverage belongs where the handlers live. The expand path
    // below stays host-resident (`do_snippet_expand_at_cursor`).

    #[test]
    fn snippet_expand_with_no_match_is_a_no_op() {
        let mut a = app_with("xyz", 10);
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(0, 3);
        a.do_snippet_expand_at_cursor();
        assert!(!a.editor.snippet_session.is_active());
        // Buffer unchanged.
        assert_eq!(a.editor.document.snapshot().buffer.as_string(), "xyz");
    }

    #[test]
    fn snippet_expand_outside_insert_mode_is_a_no_op() {
        let mut a = app_with("for", 10);
        a.editor.cursor = Position::new(0, 3);
        // Stay in Normal -- guard inside `do_snippet_expand_at_cursor`.
        install_snippet(&mut a, "*", "for-loop", "for", "for $1 {}");
        a.do_snippet_expand_at_cursor();
        assert!(!a.editor.snippet_session.is_active());
        assert_eq!(a.editor.document.snapshot().buffer.as_string(), "for");
    }

    #[test]
    fn completion_trigger_includes_snippet_candidate_for_matching_prefix() {
        let mut a = app_with("for", 10);
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(0, 3);
        install_snippet(&mut a, "*", "for-loop", "for", "for ${1:i} in ${2:iter} {}");
        a.do_completion_trigger();
        let state = a.editor.insert_completion.as_ref().expect("popup open");
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
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(0, 3);
        install_snippet(&mut a, "*", "for-loop", "for", "for ${1:i} in ${2:iter} {}");
        a.do_completion_trigger();
        // Find the snippet candidate index and select it.
        let state = a.editor.insert_completion.as_mut().expect("popup");
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
        assert!(a.editor.insert_completion.is_none());
        let snippet_index = a.editor.snippet_session.with_mut(|s| s.as_ref().expect("active snippet").current_index());
        assert_eq!(snippet_index, Some(1));
        let text = a.editor.document.snapshot().buffer.as_string();
        assert_eq!(text, "for i in iter {}");
    }

    #[test]
    fn completion_accept_bumps_frequency_map_for_text_kind_pair() {
        // Trigger completion against a buffer-words source and
        // accept a candidate. The App's accept-frequency map
        // gets a new entry keyed by `(text, kind)` with count 1.
        let mut a = app_with("alpha bravo charlie ", 10);
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(0, 20);
        a.do_completion_trigger();
        // Empty query at end of line -> all three buffer words
        // surface as candidates. Find `bravo` and select it.
        let state = a.editor.insert_completion.as_mut().expect("popup");
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
        assert_eq!(a.editor.completion_accept_freq.get(&key).copied(), Some(1));
    }

    #[test]
    fn completion_trigger_ranks_previously_accepted_above_tied_peer() {
        // Two buffer words tie on matcher score (empty query
        // -> uniform 100); a previous accept of `bravo` lifts
        // it to the top of the rendered list.
        let mut a = app_with("alpha bravo charlie ", 10);
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(0, 20);
        // Seed the freq map directly -- this is the integration
        // boundary we care about (the App's map fed into the
        // ranker), not the accept-then-retrigger cycle.
        a.editor.completion_accept_freq.insert(
            (
                "bravo".to_string(),
                lattice_completion::CandidateKind::Plain,
            ),
            3,
        );
        a.do_completion_trigger();
        let state = a.editor.insert_completion.as_ref().expect("popup");
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
        a.editor.modal = ModalState::Insert;
        // Cursor between the empty string's quotes -> string
        // scope.
        a.editor.cursor = Position::new(0, source.find("\"\"").unwrap() as u32 + 1);
        a.do_completion_trigger();
        assert!(a.editor.completion_in_path_context, "path-context detected");
        let state = a.editor.insert_completion.as_ref().expect("popup");
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
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(0, source.find("\"\"").unwrap() as u32 + 1);
        a.do_completion_trigger();
        let state = a.editor.insert_completion.as_ref().expect("popup");
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
        a.editor.modal = ModalState::Insert;
        // Cursor at end of line -- outside any string.
        a.editor.cursor = Position::new(0, source.trim_end().len() as u32);
        a.do_completion_trigger();
        assert!(!a.editor.completion_in_path_context);
        if let Some(state) = a.editor.insert_completion.as_ref() {
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
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(0, source.find("\"\"").unwrap() as u32 + 1);
        // Override the active language ("rust", since the
        // buffer path ends in `.rs`) to drop path source.
        a.editor.per_language_completion.insert(
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
            !a.editor.completion_in_path_context,
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
        a.editor.modal = ModalState::Insert;
        // Cursor after `src/`.
        let after_slash = source.find("src/").unwrap() + "src/".len();
        a.editor.cursor = Position::new(0, after_slash as u32);
        a.do_completion_trigger();
        assert!(a.editor.completion_in_path_context);
        let state = a.editor.insert_completion.as_ref().expect("popup");
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
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(0, 3);
        open_popup_with_top_text(&mut a, "foo", "foobar");
        // Default: completion.ghost_text = false -> no ghost.
        assert!(a.completion_ghost_text_suffix().is_none());
    }

    #[test]
    fn ghost_text_returns_suffix_for_prefix_matching_top_candidate() {
        let mut a = app_with("foo", 5);
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(0, 3);
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
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(0, 3);
        a.do_set("completion.ghost_text=true");
        open_popup_with_top_text(&mut a, "Foo", "foobar");
        assert_eq!(a.completion_ghost_text_suffix(), Some("bar".to_string()),);
    }

    #[test]
    fn ghost_text_none_when_top_doesnt_prefix_match_query() {
        let mut a = app_with("xyz", 5);
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(0, 3);
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
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(0, 0);
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
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(0, 1);
        a.do_set("completion.ghost_text=true");
        a.editor.completion_in_path_context = true;
        open_popup_with_top_text(&mut a, "src", "src/foo.rs");
        assert!(
            a.completion_ghost_text_suffix().is_none(),
            "path popup already shows full filenames; ghost would double up",
        );
    }

    #[test]
    fn ghost_text_none_when_popup_closed() {
        let mut a = app_with("foo", 5);
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(0, 3);
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
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(0, 0);
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
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(0, 0);
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
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(0, 0);
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
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(0, 12);
        a.do_completion_trigger();
        let state = a.editor.insert_completion.as_ref().expect("popup");
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
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(0, 20);
        let key = (
            "bravo".to_string(),
            lattice_completion::CandidateKind::Plain,
        );
        a.editor.completion_accept_freq.insert(key.clone(), 4);
        a.do_completion_trigger();
        let state = a.editor.insert_completion.as_mut().expect("popup");
        let idx = state
            .rendered
            .iter()
            .position(|r| r.raw.text == "bravo")
            .expect("bravo present");
        state.selected = idx;
        a.do_completion_accept();
        assert_eq!(a.editor.completion_accept_freq.get(&key).copied(), Some(5));
    }

    // ---- CSM.2: completion-mode tracks popup state ----

    /// Triggering the completion popup activates `completion-mode`
    /// on the document buffer. The mode is the architectural gate
    /// the keymap-overlay + active-source resolver read.
    #[test]
    fn completion_mode_activates_when_popup_opens() {
        let mut a = app_with("alpha bravo charlie ", 10);
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(0, 20);
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
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(0, 20);
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
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(0, 20);
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
            a.completion_mode_active_for(a.editor.document_buffer_id),
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
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(0, 20);
        // Force-deactivate completion-mode so the gate kicks in.
        let buffer_id = a.editor.document_buffer_id;
        a.deactivate_mode_by_id(buffer_id, lattice_mode::CompletionMode::mode_id());
        assert!(!a.completion_mode_active_for(buffer_id));
        a.do_completion_trigger();
        assert!(
            a.editor.insert_completion.is_none(),
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
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(0, 20);
        let buffer_id = a.editor.document_buffer_id;
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
        let buffer_id = a.editor.document_buffer_id;
        let pre_ids: Vec<_> = a
            .editor
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
            .editor
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
            .editor
            .buffer_locals
            .get(&a.editor.document_buffer_id)
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
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(0, 20);
        a.do_completion_trigger();
        let state = a.editor.insert_completion.as_ref().expect("popup open");
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
        let registry = std::sync::Arc::make_mut(&mut a.editor.mode_registry);
        let mode_id = registry.register(StubMode).expect("register");
        let buffer_id = a.editor.document_buffer_id;
        a.activate_mode_by_id(buffer_id, mode_id);

        // CSM.4: buffer-words-mode contributes too, so the
        // cache holds two entries -- the auto-active
        // `gen:buffer-words` plus the freshly-activated
        // `gen:stub-csm3`.
        let cache = a
            .editor
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
            .editor
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
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(0, 20);
        a.apply(Action::CompletionTrigger);
        assert!(a.completion_popup_active());
        // Manually drop the popup state (skipping the reconcile).
        a.editor.insert_completion = None;
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
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(0, 0);
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
        a.editor.insert_completion = Some(state);
        a.do_completion_filter_to_source(lattice_completion::LSP_COMPLETION_SOURCE_ID.to_string());
        let s = a.editor.insert_completion.as_ref().unwrap();
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
        a.editor.modal = ModalState::Insert;
        a.editor.cursor = Position::new(0, 0);
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
        a.editor.insert_completion = Some(state);
        a.do_completion_filter_clear();
        let s = a.editor.insert_completion.as_ref().unwrap();
        assert!(s.source_filter.is_none());
        assert_eq!(s.rendered.len(), 2);
    }
}
