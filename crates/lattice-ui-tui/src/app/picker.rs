//! Picker / fuzzy-finder App surface -- buffer picker
//! (`:b` no-arg), LSP-instance picker (used by `:lsp-log`,
//! `:lsp-server-log`, `:lsp-trace-log`), and LSP-location
//! pickers (multi-result `gd` / `gr` / `:diagnostics` /
//! symbol pickers / code actions / completion items).
//!
//! Methods that live here:
//! - `open_buffer_picker` (`:b` no-arg, the vertico-style
//!   buffer switcher with live preview).
//! - `open_lsp_picker` (instance picker; `:lsp-log` etc.).
//! - `open_lsp_locations_picker` (multi-result navs).
//! - `snapshot_lsp_instances` (helper for the instance
//!   picker).
//! - `preview_picker_selection` (per-selection preview for
//!   the buffer picker -- no jump-list pollution).
//! - `do_picker_dismiss`, `do_picker_accept` (the two
//!   terminal actions; accept fans out by RoutingPayload
//!   into buffer / lsp-log / lsp-location / completion /
//!   code-action handlers).
//! - `raw_buffer_candidates` (free fn that builds the
//!   buffer-picker candidate set from the registry).
//!
//! What does NOT live here: the `Picker` type, matcher
//! engine, candidate scoring -- those live in the sibling
//! `lattice-picker` crate. This module is App's *workflow*
//! layer above that.

use super::App;

// Slice 3c.final.E.5h: `BufferId` import dropped — it was only
// reached from `_retired_do_picker_accept_body` (deleted) and the
// now-host-resident accept routing arms. `build_picker_context`
// + `bump_live_picker_debounce` App-side wrappers moved to the
// `#[cfg(test)] impl App` block at the bottom of this file — all
// surviving callers (picker_sources.rs test module + this file's
// tests) live in test fixtures; production code in both renderer
// peers reaches the host methods directly inside `Editor::dispatch`.

impl App {

    /// Translate a picker source's typed outcome into App-state
    /// mutation. Single dispatch site -- adding a new outcome
    /// variant requires editing exactly this match.
    pub(super) fn apply_picker_outcome(&mut self, outcome: lattice_picker::PickerAcceptOutcome) {
        // Slice 3c.final.E.3: route through `mutate_editor_with`.
        let out = self.mutate_editor_with(move |e| e.apply_picker_outcome(outcome));
        for effect in out.effects {
            self.apply_effect_app_arms(effect);
        }
        for s in out.renderer_signals {
            self.handle_renderer_signal(s);
        }
    }

    /// Write the MRU index to its configured path. Best-
    /// effort: persistence may be disabled (no cache dir) or
    /// fail mid-write (full disk, permission denied). On
    /// failure we `eprintln!` once and continue -- losing one
    /// accept's persistence is annoying, blocking the accept
    /// is unacceptable. Slice 14d (event bus + typed options)
    /// can elevate to a debounced background write.
    fn persist_picker_mru_best_effort(&self) {
        // Slice 3c.final.E.5e: body lives on
        // [`lattice_host::dispatch::Editor::persist_picker_mru_best_effort`].
        // Renderer-side wrapper is a 1-line `read_editor` delegate.
        self.read_editor(|e| e.persist_picker_mru_best_effort());
    }

    // Phase 5.8.AA.s: `picker_workspace_root_path` migrated to
    // `lattice_host::dispatch::Editor::picker_workspace_root_path`.
    // Callers route through `self.editor.picker_workspace_root_path(snap)`.

    /// Snapshot of every running LSP actor as picker rows.
    /// Built by reading the supervisor's `ArcSwap<SupervisorSnapshot>`,
    /// so the read is wait-free; the previous `try_lock`
    /// fall-through (degrade to empty if supervisor was
    /// busy) is gone -- the snapshot is always readable.
    fn snapshot_lsp_instances(&mut self) -> Vec<lattice_picker::LspInstanceRow> {
        // Slice 3c.final.E.3: route through `mutate_editor_with`.
        self.mutate_editor_with(|e| e.snapshot_lsp_instances())
    }

    /// Build + open an LSP location picker (multi-result `gd` /
    /// `gr` / `:diagnostics` / future symbol pickers).
    ///
    /// Reads the line text from each location's file once
    /// (cached per file in a `HashMap`) so the displayed rows
    /// look like ripgrep output. Empty `locations` is a no-op
    /// (caller already echoed "no X found" in that case).
    pub(super) fn open_lsp_locations_picker(
        &mut self,
        title: impl Into<String>,
        locations: &[lattice_lsp::lsp_types::Location],
    ) {
        // Slice 3c.final.E.3: clone owned for the `Send + 'static`
        // closure, then route through `mutate_editor`.
        let title = title.into();
        let locations = locations.to_vec();
        self.mutate_editor(move |e| e.open_lsp_locations_picker(title, &locations));
    }

    /// Build + open an LSP instance picker. Called by `:lsp-log`,
    /// `:lsp-server-log`, and `:lsp-trace-log`. The `prefilter`
    /// arg pre-narrows the candidate list to one server id while
    /// still allowing the user to disambiguate between multiple
    /// workspaces. `on_accept` decides which buffer the chosen
    /// row opens (`OpenLspLog` or `OpenLspTraceLog`).
    pub(super) fn open_lsp_picker(
        &mut self,
        title: &str,
        prefilter: Option<String>,
        on_accept: lattice_picker::PickerAction,
    ) {
        // Slice 3c.final.E.3: clone owned title for the closure.
        let title = title.to_string();
        self.mutate_editor(move |e| e.open_lsp_picker(&title, prefilter, on_accept));
    }

    /// `:b` with no arg (DESIGN.md §5.9.7) -- open the vertico-style
    /// buffer switcher. Type to filter; `<Up>` / `<Down>` (or
    /// `<C-p>` / `<C-n>`) to move; `<CR>` to switch to the
    /// selected buffer; `<Esc>` to dismiss. Marginalia shows the
    /// kind (`doc` / `tree` / `help`) plus a `(current)` tag on
    /// the active buffer.
    ///
    /// **Live preview.** While the picker is open, every selection
    /// change activates the candidate buffer in the active pane
    /// (without polluting the jump list). On accept, that
    /// activation becomes the real switch; on dismiss, the
    /// pane reverts to whatever buffer was active when the
    /// picker opened.
    /// `:picker <source> [args]` -- canonical entry point.
    /// Looks the source up in `picker_registry`, fetches its
    /// `PickerSourceGenerator`, builds a `PickerContext`, and
    /// dispatches `gen.init(...)`. Inline results seat the
    /// picker immediately; async / streaming variants are
    /// rejected with a clear echo until the first async source
    /// (P.8 / P.9 LSP-flavored or `:picker grep`) migrates and
    /// wires the spawn path.
    ///
    /// Unknown source ids surface an error echo listing every
    /// registered id so the user can recover without `:apropos`.
    pub(crate) fn open_picker(&mut self, source: String, args: Vec<String>) {
        // Slice 3c.final.E.3: route through `mutate_editor_with`.
        let signals = self.mutate_editor_with(move |e| e.open_picker(source, args));
        for s in signals {
            self.handle_renderer_signal(s);
        }
    }

    /// Drain the pending async picker init, if any. Called
    /// from the main loop tick. Pumps the channel that the
    /// spawned future writes to; once a result arrives the
    /// picker is seated through the same path Inline init
    /// uses (so MRU snapshot + preview ergonomics behave
    /// identically). Empty channel = future still pending;
    /// closed channel = task dropped without sending (the
    /// cancel path took it).
    pub(crate) fn drain_pending_picker_init(&mut self) {
        // Slice 3c.final.E.3: route through `mutate_editor_with`.
        let signals = self.mutate_editor_with(|e| e.drain_pending_picker_init());
        for s in signals {
            self.handle_renderer_signal(s);
        }
    }

    // Slice 3c.final.E.5h: `bump_live_picker_debounce` moved to
    // `#[cfg(test)] impl App` below — the production path was
    // inlined into the host's picker keystroke arm by G.11; only
    // two test sites still poke the debounce directly to
    // fast-forward time-driven assertions.

    /// Slice 2: main-loop drain for the live picker.
    ///
    /// Two responsibilities, both cheap on an idle picker:
    ///
    /// 1. **Fire the source's re-fetch.** If the debounce
    ///    deadline has elapsed, take the deadline out, snapshot
    ///    the picker's current query, cancel any prior
    ///    in-flight task, call `on_query_changed`, and route
    ///    the result by `PickerInitResult` variant -- `Inline`
    ///    seats immediately, `Future` spawns + parks the rx in
    ///    `inflight`, `Stream` is rejected with an echo (slice
    ///    > 3 territory).
    /// 2. **Pump in-flight results.** Whatever's on `inflight.rx`
    ///    either seats new raw (if the future's query still
    ///    matches the picker's current query) or gets dropped
    ///    on the floor (if the user has moved on -- a fresher
    ///    fire will land).
    /// Phase 5.8.AA.t: per-tick live-picker query drain. Body
    /// migrated to [`lattice_host::dispatch::Editor::drain_pending_live_picker_query`]
    /// and now folded into the `run_tick_pending` aggregator so
    /// both renderer peers reach it through the same drain.
    /// Wrapper retained for tests that drive the live-query
    /// path directly.
    pub(crate) fn drain_pending_live_picker_query(&mut self) {
        // Slice 3c.final.E.3: route through `mutate_editor_with`.
        let signals = self.mutate_editor_with(|e| e.drain_pending_live_picker_query());
        for s in signals {
            self.handle_renderer_signal(s);
        }
    }

    /// Seat `pairs` into a freshly-constructed picker for
    /// `source`. Shared by sync (Inline) and async (Future)
    /// init paths so MRU bonus snapshot + source-id stamping
    /// + buffer-switcher preview ergonomics behave
    /// identically regardless of how the candidates were
    /// produced.
    fn seat_picker_from_pairs(&mut self, source: String, pairs: lattice_picker::CandidateBatch) {
        // Slice 3c.final.E.3: route through `mutate_editor_with`.
        let signals = self.mutate_editor_with(move |e| e.seat_picker_from_pairs(source, pairs));
        for s in signals {
            self.handle_renderer_signal(s);
        }
    }

    pub(super) fn open_buffer_picker(&mut self) {
        // Slice 3c.final.E.3: route through `mutate_editor_with`.
        let signals = self.mutate_editor_with(|e| e.do_open_buffer_picker());
        for s in signals {
            self.handle_renderer_signal(s);
        }
    }

    // Slice 3c.final.E.5h: `App::preview_picker_selection`
    // retired — zero callers anywhere. The host method
    // (`Editor::preview_picker_selection`) is invoked directly
    // from inside `Editor::dispatch` and returns its
    // `Vec<RendererSignal>` through the outcome surface.

    /// Apply `Action::PickerDismiss` -- close the picker and, if
    /// a buffer-switch picker was previewing, restore the active
    /// pane to whatever buffer it was on at picker-open. Tested
    /// by `picker_dismiss_restores_origin_when_previewing`.
    pub(super) fn do_picker_dismiss(&mut self) {
        // Slice 3c.final.E.3: route through `mutate_editor_with`.
        let signals = self.mutate_editor_with(|e| e.do_picker_dismiss());
        for s in signals {
            self.handle_renderer_signal(s);
        }
    }

    /// Apply `Action::PickerAccept`. Phase 5.8.AF: body migrated.
    pub(super) fn do_picker_accept(&mut self) {
        // Slice 3c.final.E.3: route through `mutate_editor_with`.
        // `do_picker_accept` now returns a full `DispatchOutcome`;
        // drain effects first (renderer-coupled App arms), then
        // renderer signals — same order as the main dispatch loop.
        let outcome = self.mutate_editor_with(|e| e.do_picker_accept());
        for effect in outcome.effects {
            self.apply_effect_app_arms(effect);
        }
        for s in outcome.renderer_signals {
            self.handle_renderer_signal(s);
        }
    }

    // Slice 3c.final.E.5h: `_retired_do_picker_accept_body`
    // deleted. The function was held under `#[cfg(any())]` (i.e.
    // never compiled) as a historical reference after G.11 moved
    // the live do_picker_accept routing host-side. Git history
    // (Phase 5.8.AF) is the canonical reference; carrying ~248
    // lines of cfg-disabled code in the file just inflated the
    // `self.editor.X` grep count without ever running.
}

// Phase 5.8.AF: `routing_payload_path` migrated to host.

// Phase 5.8.AA.s: `picker_buffer_entry` migrated to
// `lattice_host::dispatch::picker_buffer_entry`. Both renderer
// peers reach it through `Editor::build_picker_context`.

// Phase 5.8.AC.1: `raw_buffer_candidates` migrated to
// `lattice_host::dispatch::raw_buffer_candidates`. App-side
// callers route through `editor.do_open_buffer_picker`.

// Slice 3c.final.E.5h — test-fixture surface.
//
// Same shape as the `#[cfg(test)] impl App` block in
// `app/completion.rs`: production code reaches the host methods
// directly inside `Editor::dispatch`; the two wrappers below
// exist so test fixtures (this file's `mod tests` + the
// `picker_sources.rs` test module) can build a `PickerContext`
// or fast-forward the live-picker debounce against a fully-built
// `App`. When the actor-swap lands, each body flips to read via
// the audit's planned `App::editor()` / `App::editor_mut()`
// cfg-gated accessors.
#[cfg(test)]
impl App {
    pub fn build_picker_context<'a>(
        &'a self,
        snap: &'a lattice_runtime::DocumentSnapshot,
    ) -> lattice_picker::PickerContext<'a> {
        self.editor.build_picker_context(snap)
    }

    pub(crate) fn bump_live_picker_debounce(&mut self) {
        let Some(state) = self.editor.live_picker_query.as_mut() else {
            return;
        };
        state.debounce_until = Some(std::time::Instant::now() + crate::app::LIVE_PICKER_DEBOUNCE);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use crate::app::test_helpers::app_with;
    use crate::app::*;
    use crate::help::HelpContent;

    #[test]
    fn open_buffer_picker_seeds_with_every_registry_entry() {
        let mut app = app_with("hi\n", 5);
        // Add a help buffer so the picker has more than just the
        // initial document to filter against.
        let _help_id = app.open_help_in_pane(HelpContent::from_lines("lsp:rust", vec!["a".into()]));
        // Activate back to the document so the picker's "active"
        // marker doesn't land on the help buffer.
        let doc_id = app
            .editor
            .buffers
            .document_ids_sorted()
            .first()
            .copied()
            .unwrap();
        app.activate_document(doc_id);
        app.open_buffer_picker();
        let p = app.editor.picker.as_ref().expect("picker should be open");
        // Initial: every buffer in the registry. With no filter,
        // both the doc and the help buffer should be present.
        assert!(p.candidates.len() >= 2);
        assert_eq!(p.title, "buffers");
    }

    #[test]
    fn picker_accept_switches_to_selected_buffer() {
        let mut app = app_with("hi\n", 5);
        let help_id =
            app.open_help_in_pane(HelpContent::from_lines("test-target", vec!["body".into()]));
        let doc_id = app
            .editor
            .buffers
            .document_ids_sorted()
            .first()
            .copied()
            .unwrap();
        // Start on the doc.
        app.activate_document(doc_id);
        assert!(matches!(app.editor.active_buffer, BufferKind::Document));
        // Open picker, type the help title, accept.
        app.open_buffer_picker();
        for c in "test-target".chars() {
            app.apply(Action::PickerAppend(c));
        }
        app.apply(Action::PickerAccept);
        // Picker is dismissed; active pane is on the help buffer.
        assert!(app.editor.picker.is_none());
        assert_eq!(app.active_pane_buffer_id(), help_id);
        assert!(matches!(app.editor.active_buffer, BufferKind::Help));
    }

    #[test]
    fn picker_dismiss_leaves_active_pane_unchanged() {
        let mut app = app_with("hi\n", 5);
        let doc_id = app
            .editor
            .buffers
            .document_ids_sorted()
            .first()
            .copied()
            .unwrap();
        app.activate_document(doc_id);
        app.open_buffer_picker();
        app.apply(Action::PickerDismiss);
        assert!(app.editor.picker.is_none());
        assert_eq!(app.active_pane_buffer_id(), doc_id);
    }

    #[test]
    fn buffer_picker_previews_initial_selection_in_active_pane() {
        // With doc + help in registry, opening the picker on the
        // doc immediately previews the alternate (help) buffer in
        // the active pane.
        let mut app = app_with("hi\n", 5);
        let help_id =
            app.open_help_in_pane(HelpContent::from_lines("alt", vec!["alt body".into()]));
        let doc_id = app
            .editor
            .buffers
            .document_ids_sorted()
            .first()
            .copied()
            .unwrap();
        app.activate_document(doc_id);
        // Sanity: starting state.
        assert_eq!(app.active_pane_buffer_id(), doc_id);
        app.open_buffer_picker();
        // Picker open + preview switched the pane to the help
        // buffer (the alternate -- "(current)" is the doc).
        assert_eq!(app.active_pane_buffer_id(), help_id);
        assert!(matches!(app.editor.active_buffer, BufferKind::Help));
    }

    #[test]
    fn picker_dismiss_restores_origin_when_previewing() {
        let mut app = app_with("hi\n", 5);
        let _help_id =
            app.open_help_in_pane(HelpContent::from_lines("alt", vec!["alt body".into()]));
        let doc_id = app
            .editor
            .buffers
            .document_ids_sorted()
            .first()
            .copied()
            .unwrap();
        app.activate_document(doc_id);
        app.open_buffer_picker();
        // Preview moved us off the doc.
        assert_ne!(app.active_pane_buffer_id(), doc_id);
        app.apply(Action::PickerDismiss);
        // Esc restored the original.
        assert!(app.editor.picker.is_none());
        assert_eq!(app.active_pane_buffer_id(), doc_id);
        assert!(matches!(app.editor.active_buffer, BufferKind::Document));
    }

    #[test]
    fn picker_select_next_re_previews_new_candidate() {
        let mut app = app_with("hi\n", 5);
        let help_a = app.open_help_in_pane(HelpContent::from_lines("alpha-help", vec!["a".into()]));
        let help_b = app.open_help_in_pane(HelpContent::from_lines("beta-help", vec!["b".into()]));
        let doc_id = app
            .editor
            .buffers
            .document_ids_sorted()
            .first()
            .copied()
            .unwrap();
        app.activate_document(doc_id);
        app.open_buffer_picker();
        let first_preview = app.active_pane_buffer_id();
        // Move down -- previews the next candidate.
        app.apply(Action::PickerSelectNext);
        let second_preview = app.active_pane_buffer_id();
        assert_ne!(
            first_preview, second_preview,
            "selection moved -> different preview"
        );
        // Both previews land on one of the help buffers we set up.
        assert!(first_preview == help_a || first_preview == help_b || first_preview == doc_id);
        assert!(second_preview == help_a || second_preview == help_b || second_preview == doc_id);
        // Dismiss restores the original document.
        app.apply(Action::PickerDismiss);
        assert_eq!(app.active_pane_buffer_id(), doc_id);
    }

    #[test]
    fn picker_preview_does_not_pollute_position_history() {
        // Hover-previewing through several candidates should not
        // push to the jump list; only an *accepted* switch should.
        let mut app = app_with("hi\n", 5);
        let _h1 = app.open_help_in_pane(HelpContent::from_lines("h-one", vec!["a".into()]));
        let _h2 = app.open_help_in_pane(HelpContent::from_lines("h-two", vec!["b".into()]));
        let doc_id = app
            .editor
            .buffers
            .document_ids_sorted()
            .first()
            .copied()
            .unwrap();
        app.activate_document(doc_id);
        let history_before = app.editor.position_history.len();
        app.open_buffer_picker();
        app.apply(Action::PickerSelectNext);
        app.apply(Action::PickerSelectNext);
        app.apply(Action::PickerSelectPrev);
        app.apply(Action::PickerDismiss);
        let history_after = app.editor.position_history.len();
        assert_eq!(
            history_before, history_after,
            "preview hovers should leave the jump list alone"
        );
    }

    /// P.2: `push_recent_file` keeps MRU order (newest first),
    /// dedups repeats, and caps at the configured ceiling. The
    /// canonicalised path is what lands in the list, so
    /// re-pushing the same path collapses to one entry.
    #[test]
    fn push_recent_file_is_mru_and_dedupes() {
        let tmp = std::env::temp_dir().join(format!("lattice-recent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let a_path = tmp.join("a.rs");
        let b_path = tmp.join("b.rs");
        let c_path = tmp.join("c.rs");
        std::fs::write(&a_path, "").unwrap();
        std::fs::write(&b_path, "").unwrap();
        std::fs::write(&c_path, "").unwrap();
        let mut app = app_with("hi\n", 5);
        app.push_recent_file(&a_path);
        app.push_recent_file(&b_path);
        app.push_recent_file(&c_path);
        // Newest first.
        let canon_a = std::fs::canonicalize(&a_path).unwrap();
        let canon_b = std::fs::canonicalize(&b_path).unwrap();
        let canon_c = std::fs::canonicalize(&c_path).unwrap();
        assert_eq!(
            app.editor.recent_files,
            vec![canon_c.clone(), canon_b.clone(), canon_a.clone()]
        );
        // Re-pushing `a` floats it to the front and drops the
        // older occurrence -- list length stays at 3.
        app.push_recent_file(&a_path);
        assert_eq!(app.editor.recent_files, vec![canon_a, canon_c, canon_b]);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Slice 12: built-in registry seeds the well-known sources so
    /// `:picker <Tab>` (and downstream slice-13 tests) can rely on
    /// them being present.
    #[test]
    fn boot_registers_builtin_picker_sources() {
        let app = app_with("hi\n", 5);
        let ids: Vec<&'static str> = app.editor.picker_registry.ids().collect();
        assert!(ids.contains(&"files"));
        assert!(ids.contains(&"recent"));
        assert!(ids.contains(&"buffers"));
    }

    /// Slice 12: `:picker files` routes through the registry +
    /// dispatch table and seeds the same picker shape `:files`
    /// does today (every row routes to `OpenFile`).
    #[test]
    fn open_picker_files_seeds_open_file_routing() {
        let tmp = std::env::temp_dir().join(format!("lattice-picker-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("a.rs"), "").unwrap();
        let mut app = app_with("hi\n", 5);
        app.open_picker("files".into(), vec![tmp.display().to_string()]);
        let p = app.editor.picker.as_ref().expect("picker open");
        assert!(!p.candidates.is_empty());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Slice 12: `:picker buffers` shares the buffer-switcher
    /// shape `:b` produces -- candidates per registry entry.
    #[test]
    fn open_picker_buffers_opens_buffer_switcher() {
        let mut app = app_with("hi\n", 5);
        app.open_picker("buffers".into(), Vec::new());
        let p = app.editor.picker.as_ref().expect("picker open");
        assert!(!p.candidates.is_empty());
    }

    /// Slice 7g: `:picker lines` previews the line under the
    /// selection — closes the past gap where preview only
    /// fired for buffer switcher. The candidate's typed
    /// `accept_action` (`JumpInBuffer`) drives the new
    /// preview path; cursor moves to the row's line in
    /// previewing mode (no position-history push).
    #[test]
    fn lines_picker_preview_moves_cursor_to_selected_line() {
        let mut app = app_with("alpha\nbeta\ngamma\ndelta\nepsilon\n", 5);
        app.open_picker("lines".into(), Vec::new());
        let p = app.editor.picker.as_ref().expect("picker open");
        assert!(p.candidates.len() >= 4);

        // Move the picker selection to the 3rd line ("gamma").
        // Picker selection is index-based; bump twice.
        {
            let picker = app.editor.picker.as_mut().unwrap();
            picker.selected = 2;
        }
        // The cursor should still be at row 0 before preview.
        assert_eq!(app.editor.cursor.line, 0);

        let _signals = app.editor.preview_picker_selection();

        // After preview, cursor sits on line 2 (the 3rd line,
        // 0-indexed) — the JumpInBuffer action's line.
        assert_eq!(
            app.editor.cursor.line, 2,
            "preview should move cursor to selected line via typed accept_action"
        );
    }

    /// Slice 7d.1: engine-shape sources registered via
    /// `CompletionRegistry::register_source` open through
    /// `:picker <id>`. Verifies the dual-lookup branch in
    /// `Editor::open_picker` consults CompletionRegistry first.
    #[test]
    fn open_picker_resolves_engine_shape_source_from_completion_registry() {
        use lattice_completion::{
            AcceptAction, CandidateKind, CandidateSourceKind, RawCandidate, SourceRegistration,
            SourceSpec,
        };

        let mut app = app_with("hi\n", 5);

        // Build two candidates carrying typed accept_action.
        let mut a = RawCandidate::plain("alpha", CandidateKind::Plain);
        a.accept_action = Some(Box::new(AcceptAction::OpenFile {
            path: std::path::PathBuf::from("/tmp/alpha"),
        }));
        let mut b = RawCandidate::plain("beta", CandidateKind::Plain);
        b.accept_action = Some(Box::new(AcceptAction::OpenFile {
            path: std::path::PathBuf::from("/tmp/beta"),
        }));

        let reg = SourceRegistration {
            spec: SourceSpec {
                id: "test:engine-shape".to_string(),
                doc: "smoke test for dual-lookup".to_string(),
                args_schema: None,
                live: false,
            },
            kind: CandidateSourceKind::PreSupplied(std::sync::Arc::new(vec![a, b])),
            accept: None,
            matcher_override: None,
            ranker_overrides: Vec::new(),
            annotator_extras: Vec::new(),
        };
        app.editor.completion_registry.register_source(reg);

        // Open via the unified picker path.
        app.open_picker("test:engine-shape".into(), Vec::new());

        let p = app
            .editor
            .picker
            .as_ref()
            .expect("dual-lookup must seat the picker");
        assert_eq!(p.candidates.len(), 2, "both candidates should survive");
        for cand in &p.candidates {
            assert!(matches!(
                cand.raw.accept_action.as_deref().unwrap(),
                AcceptAction::OpenFile { .. }
            ));
        }
        // Source id stamp lets do_picker_accept route through
        // the typed-action branch.
        assert_eq!(p.source_id.as_deref(), Some("test:engine-shape"));
    }

    /// Slice 7d.0: every candidate emitted by the first-party
    /// buffers source carries a typed `accept_action`. This is
    /// the production proof that 7b's accept_action plumbing
    /// survives the path through Picker::seat_with_routing /
    /// Pipeline::match_and_rank — both stages operate on
    /// `RawCandidate` and must preserve the field.
    ///
    /// If this regresses (e.g. a future refactor of
    /// match_and_rank rebuilds RawCandidate without copying
    /// accept_action), `do_picker_accept` would silently fall
    /// back to the legacy trait path. This test catches that.
    #[test]
    fn buffers_picker_candidates_preserve_accept_action_after_seat() {
        let mut app = app_with("hi\n", 5);
        app.open_picker("buffers".into(), Vec::new());
        let p = app.editor.picker.as_ref().expect("picker open");
        assert!(!p.candidates.is_empty());
        for cand in &p.candidates {
            assert!(
                cand.raw.accept_action.is_some(),
                "buffers candidate missing accept_action after seat+refilter: {}",
                cand.raw.display
            );
            assert!(matches!(
                cand.raw.accept_action.as_deref().unwrap(),
                lattice_completion::AcceptAction::SwitchBuffer { .. }
            ));
        }
    }

    /// Slice 12: empty MRU `:picker recent` echoes the same
    /// message `:recent` does (closed picker, info echo).
    #[test]
    fn open_picker_recent_with_empty_mru_echoes() {
        let mut app = app_with("hi\n", 5);
        app.open_picker("recent".into(), Vec::new());
        assert!(app.editor.picker.is_none());
        let msg = app.editor.last_message.as_ref().expect("echo");
        assert!(msg.text.contains("no recent files"));
    }

    /// Slice 3c: the `gen:picker-sources` completion generator
    /// emits one candidate per source registered with the App's
    /// `picker_registry`. Confirms the Weak<PickerRegistry>
    /// plumbing is wired correctly end-to-end -- the generator
    /// can read the registry the App owns and yields the
    /// expected id-sorted set.
    #[test]
    fn gen_picker_sources_emits_candidate_per_registered_source() {
        let app = app_with("hi\n", 5);
        let generator = app
            .editor
            .completion_registry
            .generator_by_name("gen:picker-sources")
            .expect("gen:picker-sources must be registered at boot");
        let snap = app.editor.document.snapshot();
        let ctx = lattice_completion::GenerateContext {
            prefix: "",
            buffer: &snap.buffer,
            registry: &app.editor.registry,
            case_sensitive: false,
        };
        let candidates = generator.inner.generate(&ctx);
        let ids: Vec<String> = candidates.iter().map(|c| c.text.clone()).collect();
        // Built-in registry seeds the first-party sources;
        // PickerRegistry::iter is id-sorted so popup order is
        // stable. Each new source migration extends this list.
        assert_eq!(
            ids,
            vec![
                "buffers",
                "commands",
                "files",
                "grep",
                "jumps",
                "lines",
                "marks",
                "outline",
                "recent",
                "registers",
                "snippets",
            ]
        );
        // Sanity: matches what the registry itself reports.
        let registry_ids: Vec<&'static str> = app.editor.picker_registry.ids().collect();
        let mut expected: Vec<String> = registry_ids.iter().map(|s| s.to_string()).collect();
        expected.sort();
        assert_eq!(ids, expected);
    }

    /// Slice 3c: dropping the Arc<PickerRegistry> (simulating
    /// App teardown) makes the generator's Weak upgrade fail,
    /// and the generator returns an empty candidate set rather
    /// than panicking. Same discipline as `gen:modes`.
    #[test]
    fn gen_picker_sources_handles_dropped_registry_gracefully() {
        use std::sync::{Arc, Weak};

        let reg: Arc<lattice_picker::PickerRegistry> =
            Arc::new(lattice_picker::PickerRegistry::new());
        let weak: Weak<lattice_picker::PickerRegistry> = Arc::downgrade(&reg);
        drop(reg);
        let generator = crate::host_generators::PickerSourcesGenerator { registry: weak };
        // Build a minimal GenerateContext via an App fixture --
        // we just need a real Buffer + CommandRegistry.
        let app = app_with("hi\n", 5);
        let snap = app.editor.document.snapshot();
        let ctx = lattice_completion::GenerateContext {
            prefix: "",
            buffer: &snap.buffer,
            registry: &app.editor.registry,
            case_sensitive: false,
        };
        let candidates = lattice_completion::traits::CandidateGenerator::generate(&generator, &ctx);
        assert!(candidates.is_empty());
    }

    /// P.3: end-to-end `:picker lines` -- dispatch routes
    /// through `LinesSource::init` (trait-driven path) and
    /// seats a picker stamped with `source_id: Some("lines")`.
    /// The candidates count matches the active buffer's line
    /// count (sans phantom trailing newline).
    #[test]
    fn open_picker_lines_seeds_one_row_per_line() {
        let mut app = app_with("alpha\nbeta\ngamma\n", 10);
        app.open_picker("lines".into(), Vec::new());
        let p = app.editor.picker.as_ref().expect("picker open");
        assert_eq!(p.candidates.len(), 3);
        assert_eq!(p.source_id.as_deref(), Some("lines"));
    }

    /// P.3: accepting a row from the lines picker routes
    /// through `LinesSource::accept` -> `JumpInBuffer`
    /// outcome -> `apply_picker_outcome` and moves the cursor
    /// to the chosen line.
    #[test]
    fn open_picker_lines_accept_jumps_cursor() {
        let mut app = app_with("alpha\nbeta\ngamma\n", 10);
        app.open_picker("lines".into(), Vec::new());
        // Move selection to the second row (beta, line index 1)
        // and accept.
        app.apply(Action::PickerSelectNext);
        app.apply(Action::PickerAccept);
        assert!(app.editor.picker.is_none());
        assert_eq!(app.editor.cursor.line, 1);
        assert_eq!(app.editor.cursor.byte, 0);
    }

    /// P.10: `:picker snippets` against an empty registry
    /// echoes the source's no-snippets message and leaves
    /// the picker closed (fresh-boot fixture has no snippets
    /// loaded). Confirms the feature-crate registration
    /// pattern -- lattice-snippet's
    /// `picker_sources::register` -- wires through to App
    /// dispatch correctly.
    #[test]
    fn open_picker_snippets_empty_registry_echoes() {
        let mut app = app_with("hi\n", 5);
        app.open_picker("snippets".into(), Vec::new());
        assert!(app.editor.picker.is_none());
        let msg = app.editor.last_message.as_ref().expect("echo");
        assert!(
            msg.text.contains("no snippets registered"),
            "got `{}`",
            msg.text
        );
    }

    /// Slice 14c: accepting a candidate records it in
    /// `picker_mru`; the next open observes a non-zero
    /// frecency bonus for that identity, and -- with two
    /// otherwise-equivalent rows -- floats the recorded one
    /// to the top of the popup.
    #[test]
    fn picker_mru_floats_previously_accepted_to_top() {
        let tmp = std::env::temp_dir().join(format!("lattice-mru-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("alpha.rs"), "").unwrap();
        std::fs::write(tmp.join("beta.rs"), "").unwrap();
        let mut app = app_with("hi\n", 5);
        // Disable persistence -- we don't want this test
        // touching the user's real cache. Also clear any
        // pre-loaded entries from disk so the assertions
        // measure deltas, not absolute counts.
        app.editor.picker_mru.clear();
        app.editor.picker_mru_path = None;
        // Open the files picker and accept the alphabetically-
        // first candidate (alpha.rs sorts before beta.rs in
        // walker output, but order depends on read_dir so use
        // whichever the picker surfaces).
        app.open_picker("files".into(), vec![tmp.display().to_string()]);
        let first_id = {
            let p = app.editor.picker.as_ref().expect("picker open");
            let c = p.selected_candidate().expect("first selected");
            match p.routing_for(c).expect("routing") {
                lattice_picker::RoutingPayload::OpenFile { path } => path.clone(),
                other => panic!("expected OpenFile, got {other:?}"),
            }
        };
        app.apply(Action::PickerAccept);
        assert!(app.editor.picker.is_none());
        // The MRU should now have one entry under `files`.
        let identity = format!("file:{}", first_id.display());
        assert!(
            app.editor.picker_mru.lookup("files", &identity).is_some(),
            "expected MRU entry for {identity}"
        );
        // Re-open the picker. The accepted file should now
        // float to the top (MRU bonus > 0 vs 0 for the other).
        app.open_picker("files".into(), vec![tmp.display().to_string()]);
        let top = {
            let p = app.editor.picker.as_ref().expect("picker open");
            let c = p.selected_candidate().expect("top selected");
            match p.routing_for(c).expect("routing") {
                lattice_picker::RoutingPayload::OpenFile { path } => path.clone(),
                other => panic!("expected OpenFile, got {other:?}"),
            }
        };
        assert_eq!(
            top, first_id,
            "previously-accepted file should float to top"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Slice 14d events: accepting a candidate publishes a
    /// `PickerAccepted` typed event on the §5.10 bus.
    /// Subscribers (the MRU index today; plugin telemetry
    /// hooks tomorrow) receive a synchronous fan-out with
    /// `source_id`, `identity`, and `ts` populated.
    #[test]
    fn picker_accept_publishes_typed_event() {
        let tmp = std::env::temp_dir().join(format!("lattice-evt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("alpha.rs"), "").unwrap();
        let mut app = app_with("hi\n", 5);
        app.editor.picker_mru_path = None;
        // Subscribe before firing the picker.
        let (tx, mut rx) =
            tokio::sync::mpsc::unbounded_channel::<lattice_picker::events::PickerAccepted>();
        app.editor.event_bus.subscribe_typed(tx);
        app.open_picker("files".into(), vec![tmp.display().to_string()]);
        let _ = app.editor.picker.as_ref().expect("picker open");
        app.apply(Action::PickerAccept);
        // The event lands synchronously through the bus's
        // forwarder closures; try_recv should see it.
        let evt = rx.try_recv().expect("PickerAccepted should fire");
        assert_eq!(evt.source_id, "files");
        assert!(evt.identity.as_deref().unwrap().starts_with("file:"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Slice 14d events: a successful `:picker <source>`
    /// seat publishes `PickerOpened` for subscribers
    /// (telemetry, plugin hooks). Sources that error in
    /// `init` skip the publish because the picker never
    /// actually opens.
    #[test]
    fn picker_open_publishes_typed_event() {
        let mut app = app_with("hi\n", 5);
        let (tx, mut rx) =
            tokio::sync::mpsc::unbounded_channel::<lattice_picker::events::PickerOpened>();
        app.editor.event_bus.subscribe_typed(tx);
        // `:picker buffers` always seats (the App's
        // BufferRegistry has at least the active doc).
        app.open_picker("buffers".into(), Vec::new());
        let evt = rx.try_recv().expect("PickerOpened should fire");
        assert_eq!(evt.source_id, "buffers");
    }

    /// Slice 14d: `picker.mru.enabled = false` short-circuits
    /// both the bonus snapshot (every candidate gets 0.0) and
    /// the record-on-accept path. After accepting a row, the
    /// MRU index is unchanged.
    #[test]
    fn picker_mru_enabled_false_skips_record() {
        let tmp = std::env::temp_dir().join(format!("lattice-mru-off-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("alpha.rs"), "").unwrap();
        let mut app = app_with("hi\n", 5);
        // Start from an empty MRU regardless of what's on the
        // user's disk cache; we measure the delta caused by
        // the accept, not the absolute count.
        app.editor.picker_mru.clear();
        app.editor.picker_mru_path = None;
        let before = app.editor.picker_mru.len();
        // Disable MRU.
        app.editor
            .config
            .parse_and_set_command("picker.mru.enabled=false")
            .unwrap();
        app.open_picker("files".into(), vec![tmp.display().to_string()]);
        let _ = app.editor.picker.as_ref().expect("picker open");
        app.apply(Action::PickerAccept);
        // With MRU off, the accept must not add a record.
        assert_eq!(
            app.editor.picker_mru.len(),
            before,
            "accept with MRU off must not change the index"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ---- Slice 2: live-picker debounce + drain tests --------
    //
    // These pin the App-side wiring (state install, debounce
    // bump, drain firing on_query_changed, dismiss-cancels-
    // inflight) without needing to register a generator into
    // the Arc<PickerRegistry> -- tests construct the live state
    // by hand and exercise the drain methods directly.

    use lattice_completion::CandidateKind;
    use lattice_completion::candidate::RawCandidate;
    use lattice_picker::context::PickerContext;
    use lattice_picker::outcome::PickerAcceptOutcome;
    use lattice_picker::{
        PickerInitResult, PickerSourceGenerator, PickerSourceSpec, RoutingPayload, SourceResult,
    };
    use std::sync::{Arc, Mutex};

    /// Test-only live source. Records every `on_query_changed`
    /// invocation into a shared `Vec<String>` so tests can
    /// assert call count + ordering, and returns one Inline
    /// candidate carrying the query so the picker's `raw` is
    /// observably refreshed.
    struct LiveStubSource {
        spec: PickerSourceSpec,
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl LiveStubSource {
        fn new(id: &'static str) -> Self {
            Self {
                spec: PickerSourceSpec::no_args(id, "live stub").with_live(true),
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl PickerSourceGenerator for LiveStubSource {
        fn spec(&self) -> &PickerSourceSpec {
            &self.spec
        }

        fn init(
            &self,
            _ctx: &PickerContext<'_>,
            _args: &[String],
        ) -> SourceResult<PickerInitResult> {
            Ok(PickerInitResult::Inline(Vec::new()))
        }

        fn accept(
            &self,
            _ctx: &PickerContext<'_>,
            _routing: &RoutingPayload,
        ) -> SourceResult<PickerAcceptOutcome> {
            Err("stub does not accept".into())
        }

        fn on_query_changed(
            &self,
            _ctx: &PickerContext<'_>,
            query: &str,
        ) -> Option<SourceResult<PickerInitResult>> {
            self.calls.lock().unwrap().push(query.to_string());
            let raw = RawCandidate::plain(format!("hit:{query}"), CandidateKind::Plain);
            let pairs = vec![(
                raw,
                RoutingPayload::OpenFile {
                    path: "/dev/null".into(),
                },
            )];
            Some(Ok(PickerInitResult::Inline(pairs)))
        }
    }

    /// Build an App, hand-install a live picker for `stub`, and
    /// return the calls handle so the test can assert. Mirrors
    /// the state `open_picker` would set up if the test source
    /// were registered.
    fn app_with_live_stub(stub: Arc<LiveStubSource>) -> super::App {
        let mut app = app_with("hi\n", 5);
        let mut picker = lattice_picker::Picker::new(
            "live-stub",
            lattice_picker::PickerSource::Files,
            lattice_picker::PickerAction::OpenFile,
        );
        picker.set_live_source_mode(true);
        picker.source_id = Some("live-stub".to_string());
        app.editor.picker = Some(picker);
        app.editor.live_picker_query = Some(crate::app::LivePickerQueryState {
            source_id: "live-stub".to_string(),
            generator: stub as Arc<dyn PickerSourceGenerator>,
            debounce_until: None,
            inflight: None,
            initial_query: None,
        });
        app
    }

    #[test]
    fn live_picker_keystroke_bumps_debounce_then_drain_fires_on_query_changed() {
        let stub = Arc::new(LiveStubSource::new("live-stub"));
        let calls = stub.calls.clone();
        let mut app = app_with_live_stub(stub);
        // Type a single character. The dispatch handler in
        // dispatch.rs calls `bump_live_picker_debounce` after
        // the picker mutation, so we mirror both here.
        app.editor.picker.as_mut().unwrap().append_query('h');
        app.bump_live_picker_debounce();
        // Verify the debounce deadline got installed.
        assert!(
            app.editor
                .live_picker_query
                .as_ref()
                .and_then(|s| s.debounce_until)
                .is_some(),
            "debounce deadline should be set after a keystroke",
        );
        // Force the deadline into the past so the drain fires
        // without sleeping.
        if let Some(state) = app.editor.live_picker_query.as_mut() {
            state.debounce_until =
                Some(std::time::Instant::now() - std::time::Duration::from_millis(1));
        }
        app.drain_pending_live_picker_query();
        // Stub got exactly one call with the current query.
        let recorded = calls.lock().unwrap().clone();
        assert_eq!(recorded, vec!["h".to_string()]);
        // Picker raw was refreshed from the stub's Inline result.
        let picker = app.editor.picker.as_ref().expect("picker still open");
        assert_eq!(picker.candidates.len(), 1, "stub seated one candidate");
        assert!(picker.candidates[0].raw.display.contains("hit:h"));
        // Debounce slot cleared after fire.
        assert!(
            app.editor
                .live_picker_query
                .as_ref()
                .and_then(|s| s.debounce_until)
                .is_none(),
            "debounce deadline should be cleared after fire",
        );
    }

    #[test]
    fn live_picker_multiple_keystrokes_within_debounce_coalesce_to_one_fire() {
        // Three keystrokes, all bumping the deadline; only the
        // final query (after the third bump) is what the source
        // sees -- the source isn't called between bumps because
        // the deadline keeps getting pushed forward.
        let stub = Arc::new(LiveStubSource::new("live-stub"));
        let calls = stub.calls.clone();
        let mut app = app_with_live_stub(stub);
        for c in "foo".chars() {
            app.editor.picker.as_mut().unwrap().append_query(c);
            app.bump_live_picker_debounce();
            // Drain BETWEEN bumps -- deadline is still in the
            // future so the drain is a no-op. Mirrors what the
            // main loop tick does between fast keystrokes.
            app.drain_pending_live_picker_query();
        }
        assert!(
            calls.lock().unwrap().is_empty(),
            "no fire while bouncing the deadline forward"
        );
        // Now collapse: force deadline to past, drain once.
        if let Some(state) = app.editor.live_picker_query.as_mut() {
            state.debounce_until =
                Some(std::time::Instant::now() - std::time::Duration::from_millis(1));
        }
        app.drain_pending_live_picker_query();
        let recorded = calls.lock().unwrap().clone();
        assert_eq!(
            recorded,
            vec!["foo".to_string()],
            "burst keystrokes coalesce into one source call carrying the final query",
        );
    }

    #[test]
    fn live_picker_dismiss_clears_state() {
        let stub = Arc::new(LiveStubSource::new("live-stub"));
        let mut app = app_with_live_stub(stub);
        assert!(app.editor.live_picker_query.is_some());
        app.do_picker_dismiss();
        assert!(app.editor.picker.is_none(), "picker closed");
        assert!(
            app.editor.live_picker_query.is_none(),
            "live-picker state torn down on dismiss",
        );
    }

    // ---- Slice 4: end-to-end live-grep integration --------
    //
    // These go through the real registered `grep` source (boot
    // wires it in `register_first_party_picker_sources`), not
    // the synthetic `LiveStubSource`. They pin the public
    // surface: `:picker grep` opens an empty live picker;
    // `:picker grep <pattern>` stashes the pattern as the
    // initial query and schedules the first grep as a Future.
    // We deliberately don't wait for the real `rg` to land
    // here -- the spawn is on the lsp-runtime + blocking pool,
    // and adding a `wait_for(...)` would make the test depend
    // on `rg` being on PATH + the workspace contents. The unit
    // tests in `picker_sources::tests` cover the grep-specific
    // logic; these cover the App-side wiring.

    #[test]
    fn open_picker_grep_no_args_installs_live_state() {
        let mut app = app_with("hi\n", 5);
        app.open_picker("grep".into(), Vec::new());
        // Picker open, empty candidates (init returned Inline(empty)).
        let picker = app.editor.picker.as_ref().expect("picker open");
        assert_eq!(picker.title, "grep");
        assert!(picker.is_live_source_mode(), "grep must be live");
        assert!(
            picker.candidates.is_empty(),
            "no candidates without a pattern"
        );
        assert!(
            picker.query.is_empty(),
            "prompt empty when no initial pattern"
        );
        // Live-picker state installed; no initial query stashed.
        let live = app
            .editor
            .live_picker_query
            .as_ref()
            .expect("live state installed");
        assert_eq!(live.source_id, "grep");
        assert!(live.initial_query.is_none());
        assert!(
            live.debounce_until.is_none(),
            "no keystroke yet -> no deadline"
        );
    }

    #[test]
    fn open_picker_grep_with_initial_pattern_stashes_query_until_seat() {
        let mut app = app_with("hi\n", 5);
        // We pass an initial pattern. init() schedules a Future
        // (real grep on the blocking pool) -- the picker isn't
        // seated yet, so the stashed `initial_query` survives
        // on `live_picker_query`. Once the future resolves and
        // `seat_picker_from_pairs` runs, it'll consume the
        // stash and seed `picker.query`. We assert the stash
        // shape; the seat-consumes path is exercised by the
        // synthetic-source tests above + by integration runs
        // against a real workspace.
        app.open_picker("grep".into(), vec!["needle".to_string()]);
        let live = app
            .editor
            .live_picker_query
            .as_ref()
            .expect("live state must be installed");
        assert_eq!(live.initial_query.as_deref(), Some("needle"));
        // pending_picker_init carries the in-flight future.
        assert!(
            app.editor.pending_picker_init.is_some(),
            "init returned Future -> drain rx parked",
        );
    }

    #[test]
    fn picker_grep_seeds_query_on_seat_when_initial_query_stashed() {
        // Drive the seat path directly without spawning real
        // grep. After `open_picker("grep", ["TODO"])` stashes
        // the initial query, calling `seat_picker_from_pairs`
        // by hand (as `drain_pending_picker_init` would) should
        // seed `picker.query = "TODO"` and clear the stash.
        let mut app = app_with("hi\n", 5);
        app.open_picker("grep".into(), vec!["TODO".to_string()]);
        // Synthesise the future's would-be result: empty pairs.
        // The seed-on-seat behaviour fires regardless of the
        // batch contents.
        app.seat_picker_from_pairs("grep".to_string(), Vec::new());
        let picker = app.editor.picker.as_ref().expect("picker open");
        assert_eq!(picker.query, "TODO", "initial pattern seeded into prompt");
        assert_eq!(picker.query_cursor, "TODO".len());
        // Stash consumed.
        let live = app
            .editor
            .live_picker_query
            .as_ref()
            .expect("live state present");
        assert!(live.initial_query.is_none(), "initial_query taken on seat");
    }

    /// Slice 12: an unknown source id surfaces an error echo
    /// listing every known id so the user can recover without
    /// `:apropos`.
    #[test]
    fn open_picker_unknown_source_echoes_with_known_ids() {
        let mut app = app_with("hi\n", 5);
        app.open_picker("nope".into(), Vec::new());
        assert!(app.editor.picker.is_none());
        let msg = app.editor.last_message.as_ref().expect("echo");
        assert!(
            msg.text.contains("unknown source `nope`"),
            "missing unknown-source prefix: {}",
            msg.text
        );
        assert!(
            msg.text.contains("files") && msg.text.contains("recent"),
            "missing known-ids listing: {}",
            msg.text
        );
    }
}
