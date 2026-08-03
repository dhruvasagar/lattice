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

    /// Open a y/n confirmation transient dialog.
    pub(crate) fn do_confirm(
        &mut self,
        prompt: String,
        yes_action: String,
        args: lattice_grammar::Args,
    ) {
        let signals = self.mutate_editor_with(move |e| {
            let Some(cmd_reg) = e.services.get::<lattice_grammar::CommandRegistryHandle>() else {
                e.set_message(
                    crate::app::EchoLevel::Error,
                    "confirm: command registry unavailable".to_string(),
                );
                return Vec::new();
            };
            let Some(cmd_id) = cmd_reg.load().id_by_name(&yes_action) else {
                e.set_message(
                    crate::app::EchoLevel::Error,
                    format!("confirm: unknown action `{yes_action}`"),
                );
                return Vec::new();
            };
            let spec = lattice_picker::confirm_transient_spec(&prompt, cmd_id);
            // IX.1: seed the dialog's state with the yes-action's
            // arguments, so what fires on `y` is what the prompt named.
            // Without this the yes-half would re-derive its target when
            // it runs, and the context it derives from can change while
            // the dialog is open.
            let seed = match cmd_reg.load().lookup(cmd_id) {
                Some(spec) => e.seed_confirm_args(&spec.args_schema, &args),
                None => Default::default(),
            };
            let signals = e.open_transient(spec);
            e.extend_transient_state(seed);
            signals
        });
        for s in signals {
            self.handle_renderer_signal(s);
        }
    }

    /// Open a named transient picker menu — resolves `source`
    /// against the `TransientSourceRegistry` service (populated at
    /// boot by the owning mode crate; e.g. magit registers
    /// `magit-dispatch` / `magit-file-dispatch`) and opens the
    /// built spec. Mirrors `do_confirm`'s shape.
    pub(crate) fn do_open_transient(&mut self, source: String) {
        let signals = self.mutate_editor_with(move |e| {
            let Some(registry) = e
                .services
                .get::<lattice_picker::TransientSourceRegistryHandle>()
            else {
                e.set_message(
                    crate::app::EchoLevel::Error,
                    "transient: source registry unavailable".to_string(),
                );
                return Vec::new();
            };
            // MG.23h: the menu is built for the place it was opened
            // from. Resolved here rather than by whatever emitted the
            // effect, so the chord, the ex-command (whose context
            // carries no buffer) and any future plugin-emitted open are
            // uniformly context-aware.
            let ctx = e.transient_open_context();
            let Some(spec) = registry.build(&source, &ctx) else {
                e.set_message(
                    crate::app::EchoLevel::Error,
                    format!("transient: unknown source `{source}`"),
                );
                return Vec::new();
            };
            e.open_transient(spec)
        });
        for s in signals {
            self.handle_renderer_signal(s);
        }
    }

    /// Open a generic one-line minibuffer text prompt. Mirrors
    /// `do_confirm`/`do_open_transient`'s shape — the actual
    /// buffer-focus mutation lives host-side (`Editor::open_prompt_line`),
    /// this wrapper just routes the resulting renderer signals.
    pub(crate) fn do_open_prompt(
        &mut self,
        prompt: String,
        initial: String,
        on_submit_action: String,
        buffer_name: Option<String>,
    ) {
        let signals = self.mutate_editor_with(move |e| {
            e.open_prompt_line(prompt, initial, on_submit_action, buffer_name)
        });
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
        // PI.3: preview is an isolated projection — the pane's COMMITTED
        // buffer stays the doc; the preview rides the pane override.
        assert_eq!(
            app.active_pane_buffer_id(),
            doc_id,
            "committed buffer unchanged by preview"
        );
        assert!(matches!(app.editor.active_buffer, BufferKind::Document));
        let pane = app.editor.pane_tree.active().id;
        assert_eq!(
            app.editor.preview_override_for(pane).map(|o| o.buffer_id),
            Some(help_id),
            "active pane previews the alternate (help) buffer via the override"
        );
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
        // PI.3: preview rides the override; the committed buffer stays doc.
        let pane = app.editor.pane_tree.active().id;
        assert!(
            app.editor.preview_override_for(pane).is_some(),
            "an alternate is being previewed"
        );
        assert_eq!(app.active_pane_buffer_id(), doc_id, "committed stays doc");
        app.apply(Action::PickerDismiss);
        // Esc cleared the preview; the pane was never off the doc.
        assert!(app.editor.picker.is_none());
        assert!(
            app.editor.preview_override_for(pane).is_none(),
            "dismiss clears the preview override"
        );
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
        let pane = app.editor.pane_tree.active().id;
        let first_preview = app.editor.preview_override_for(pane).map(|o| o.buffer_id);
        // Move down -- previews the next candidate.
        app.apply(Action::PickerSelectNext);
        let second_preview = app.editor.preview_override_for(pane).map(|o| o.buffer_id);
        assert_ne!(
            first_preview, second_preview,
            "selection moved -> different preview override"
        );
        // Both previews land on one of the help buffers we set up.
        assert!(
            first_preview == Some(help_a)
                || first_preview == Some(help_b)
                || first_preview == Some(doc_id)
        );
        assert!(
            second_preview == Some(help_a)
                || second_preview == Some(help_b)
                || second_preview == Some(doc_id)
        );
        // Dismiss clears the preview; the committed buffer was always doc.
        app.apply(Action::PickerDismiss);
        assert!(app.editor.preview_override_for(pane).is_none());
        assert_eq!(app.active_pane_buffer_id(), doc_id);
    }

    /// Filtering the picker query down to ZERO matches must restore the
    /// preview to the original buffer — not leave the previous candidate's
    /// preview on screen (the user-reported "weird garbage" on no-match).
    #[test]
    fn picker_no_match_restores_origin_preview() {
        let mut app = app_with("origin\n", 5);
        let _a = app.open_help_in_pane(HelpContent::from_lines("alpha", vec!["a".into()]));
        let _b = app.open_help_in_pane(HelpContent::from_lines("beta", vec!["b".into()]));
        let doc_id = app
            .editor
            .buffers
            .document_ids_sorted()
            .first()
            .copied()
            .unwrap();
        app.activate_document(doc_id);
        app.open_buffer_picker();
        let pane = app.editor.pane_tree.active().id;
        // Move off the origin so a non-origin candidate is being previewed.
        app.apply(Action::PickerSelectNext);
        assert!(
            app.editor.preview_override_for(pane).is_some(),
            "a non-origin candidate is being previewed"
        );
        // Type a query that matches no buffer.
        for c in "zzzz".chars() {
            app.apply(Action::PickerAppend(c));
        }
        // No-match clears the preview projection (not leave a stale
        // candidate); the pane snaps back to its committed origin.
        assert!(
            app.editor.preview_override_for(pane).is_none(),
            "no-match must clear the preview override"
        );
        assert_eq!(app.active_pane_buffer_id(), doc_id);
        app.apply(Action::PickerDismiss);
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
        // PL8.F: `ids()` now borrows the registry (was `&'static str`), so bind
        // the `load()` guard for the collect's lifetime.
        let reg = app.editor.picker_registry.load();
        let ids: Vec<&str> = reg.ids().collect();
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

        // PI.3: the preview cursor rides the pane override — the committed
        // cursor is untouched. The override seats line 2 (the 3rd line,
        // 0-indexed) — the JumpInBuffer action's line.
        let pane = app.editor.pane_tree.active().id;
        let ov = app
            .editor
            .preview_override_for(pane)
            .expect("previewing a line in the current buffer");
        assert_eq!(
            ov.cursor.line, 2,
            "preview seats the cursor on the selected line via the override"
        );
        assert_eq!(app.editor.cursor.line, 0, "committed cursor untouched");
    }

    /// The preview centres the selected line (vim `zz`) so its context
    /// is visible above AND below, instead of landing at the viewport
    /// bottom. Drives the same `JumpInBuffer` preview path `gr` /
    /// references / grep previews use.
    #[test]
    fn lines_picker_preview_centers_the_selected_line() {
        let text: String = (0..20).map(|i| format!("line{i}\n")).collect();
        let mut app = app_with(&text, 5);
        app.open_picker("lines".into(), Vec::new());
        {
            let picker = app.editor.picker.as_mut().expect("picker open");
            picker.selected = 15;
        }
        let _ = app.editor.preview_picker_selection();
        // PI.3: cursor + centred scroll ride the override, not the
        // committed viewport.
        let pane = app.editor.pane_tree.active().id;
        let ov = app
            .editor
            .preview_override_for(pane)
            .expect("previewing a line in the current buffer");
        assert_eq!(ov.cursor.line, 15);
        // viewport 5 → centred scroll = 15 - 5/2 = 13 (line 15 sits at
        // the middle row), not 11 (bottom-anchored).
        assert_eq!(
            ov.scroll, 13,
            "preview centres the selected line via the override, not bottom-anchor it"
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
    /// **The data-loss bug.** A bracketed paste with a picker open used
    /// to fall through to the document-insert arm of `do_paste_text` —
    /// a picker is not a `ModalState`, so nothing matched it. The query
    /// stayed empty, the picker looked inert, and the file behind it
    /// silently gained the clipboard's contents.
    ///
    /// Non-vacuous by construction: the pre-fix behaviour is asserted
    /// against directly (the document must NOT change), so a regression
    /// that routes paste back to the buffer fails here rather than
    /// somewhere downstream.
    #[test]
    fn pasting_into_a_picker_fills_the_query_and_never_the_document() {
        let mut app = app_with("hi\n", 24);
        app.apply(lattice_host::action::Action::OpenCommandPicker);
        assert!(app.editor.picker.is_some(), "picker must be open");

        app.apply(lattice_host::action::Action::PasteText("edit".into()));

        assert_eq!(
            app.editor.picker.as_ref().map(|p| p.query.as_str()),
            Some("edit"),
            "the paste belongs to the picker that owns the keyboard"
        );
        assert_eq!(
            app.editor.document.snapshot().buffer.line(0),
            Some("hi".to_string()),
            "the document behind the picker must be untouched — this is \
             the data-loss bug, and it was silent"
        );
    }

    /// A multi-line paste has to become one line, because the query is
    /// one line. Joining with nothing would weld `foo.rs` to `bar.rs`
    /// and match neither.
    #[test]
    fn a_multi_line_paste_flattens_to_spaces_rather_than_welding_words() {
        let mut app = app_with("hi\n", 24);
        app.apply(lattice_host::action::Action::OpenCommandPicker);
        app.apply(lattice_host::action::Action::PasteText(
            "foo\nbar\r\nbaz".into(),
        ));
        assert_eq!(
            app.editor.picker.as_ref().map(|p| p.query.as_str()),
            Some("foo bar  baz"),
            "each line break becomes a space; CRLF is two of them"
        );
    }

    /// Other control characters are dropped rather than flattened: they
    /// cannot be typed into a query, so they cannot be meant in one, and
    /// a stray `\0` or `ESC` surviving a terminal round-trip would make
    /// the filter match nothing for no visible reason.
    #[test]
    fn a_paste_carrying_control_characters_drops_them() {
        let mut app = app_with("hi\n", 24);
        app.apply(lattice_host::action::Action::OpenCommandPicker);
        app.apply(lattice_host::action::Action::PasteText(
            "ed\u{0}it\u{1b}".into(),
        ));
        assert_eq!(
            app.editor.picker.as_ref().map(|p| p.query.as_str()),
            Some("edit")
        );
    }

    /// A paste that is nothing but control characters must not clear the
    /// selection or re-run the filter — it contributed no text, so the
    /// picker should be exactly as it was.
    #[test]
    fn a_paste_that_contributes_nothing_leaves_the_query_alone() {
        let mut app = app_with("hi\n", 24);
        app.apply(lattice_host::action::Action::OpenCommandPicker);
        app.apply(lattice_host::action::Action::PasteText("ed".into()));
        app.apply(lattice_host::action::Action::PasteText(
            "\u{0}\u{1b}".into(),
        ));
        assert_eq!(
            app.editor.picker.as_ref().map(|p| p.query.as_str()),
            Some("ed"),
            "a no-op paste is a no-op"
        );
    }

    /// A transient shows no query and takes no text, so a paste there
    /// goes nowhere — and specifically not into the document, which is
    /// the same bug wearing the other hat.
    #[test]
    fn pasting_into_a_transient_changes_neither_the_menu_nor_the_document() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        use crate::app::test_helpers::press;

        let mut app = app_with("hi\n", 24);
        press(
            &mut app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        );
        press(
            &mut app,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
        );
        assert!(
            app.editor
                .picker
                .as_ref()
                .and_then(|p| p.transient.as_ref())
                .is_some(),
            "C-c g must open a transient"
        );

        app.apply(lattice_host::action::Action::PasteText("nonsense".into()));

        assert_eq!(
            app.editor.picker.as_ref().map(|p| p.query.as_str()),
            Some(""),
            "a transient has no query to fill"
        );
        assert_eq!(
            app.editor.document.snapshot().buffer.line(0),
            Some("hi".to_string()),
            "and the document is still not where a paste goes"
        );
    }

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
            registry: &app.editor.registry.load(),
            case_sensitive: false,
        };
        let candidates = generator.inner.generate(&ctx);
        let ids: Vec<String> = candidates.iter().map(|c| c.text.clone()).collect();

        // **The invariant this test exists for**: the generator sees
        // exactly what the App's registry holds, id-sorted so popup
        // order is stable. That is the `Weak<PickerRegistry>` plumbing
        // working end-to-end, which is what the doc comment above
        // promises.
        let registry = app.editor.picker_registry.load();
        let mut expected: Vec<String> = registry.ids().map(str::to_string).collect();
        expected.sort();
        assert_eq!(ids, expected, "generator must mirror the registry exactly");
        assert!(
            !ids.is_empty(),
            "an empty registry would satisfy the equality above vacuously"
        );

        // Only the sources THIS crate is responsible for are named
        // here. Sources contributed by feature crates
        // (`lattice_magit::picker_sources::register`,
        // `lattice_snippet::…`) are asserted in those crates, next to
        // the registration — see
        // `lattice_magit::picker_sources::tests::every_registered_source_is_the_one_its_row_names`.
        //
        // **Why this is not one exhaustive list any more.** It used to
        // be, and the list rotted: MG.29 added
        // `magit-branch-checkout-pick` without extending it, leaving
        // this assertion failing on `main` until MG.32 found it. A
        // global inventory pinned in the renderer's test suite taxes
        // every crate that adds a source and cannot be owned by any of
        // them — so each crate now owns its own, and this keeps the
        // cross-crate invariant plus its own rows.
        for built_in in [
            "buffers",
            "colorscheme",
            "commands",
            "files",
            "grep",
            "history",
            "jumps",
            "lines",
            "marks",
            "outline",
            "recent",
            "registers",
            // MB.5: `q/` / `q?` / `:history search`.
            "search-history",
        ] {
            assert!(
                ids.iter().any(|id| id == built_in),
                "first-party source `{built_in}` is missing: {ids:?}"
            );
        }
    }

    /// Slice 3c: dropping the Arc<PickerRegistry> (simulating
    /// App teardown) makes the generator's Weak upgrade fail,
    /// and the generator returns an empty candidate set rather
    /// than panicking. Same discipline as `gen:modes`.
    #[test]
    fn gen_picker_sources_handles_dropped_registry_gracefully() {
        use std::sync::{Arc, Weak};

        let reg: lattice_picker::PickerRegistryHandle = Arc::new(arc_swap::ArcSwap::from_pointee(
            lattice_picker::PickerRegistry::new(),
        ));
        let weak: Weak<arc_swap::ArcSwap<lattice_picker::PickerRegistry>> = Arc::downgrade(&reg);
        drop(reg);
        let generator = crate::host_generators::PickerSourcesGenerator { registry: weak };
        // Build a minimal GenerateContext via an App fixture --
        // we just need a real Buffer + CommandRegistry.
        let app = app_with("hi\n", 5);
        let snap = app.editor.document.snapshot();
        let ctx = lattice_completion::GenerateContext {
            prefix: "",
            buffer: &snap.buffer,
            registry: &app.editor.registry.load(),
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

    /// The event-driven loop parks on `recv()` with no timer of its
    /// own, so a debounced live re-query must schedule its own wake or
    /// the grep never fires until the next keystroke. Assert that
    /// `bump_live_picker_debounce` both sets the deadline AND notifies
    /// `paint_request` after the debounce window (the loop's
    /// `Wake::Repaint` source). Without this the user sees "results
    /// don't show until a key press".
    #[test]
    fn live_picker_debounce_schedules_a_paint_wake() {
        let mut app = app_with("hi\n", 5);
        // Open grep with NO pattern: installs live state, no initial
        // grep future (so the only paint wake comes from the debounce
        // timer, not a result).
        app.open_picker("grep".into(), Vec::new());
        let paint = app.editor.paint_request.clone();
        app.editor.bump_live_picker_debounce();
        assert!(
            app.editor
                .live_picker_query
                .as_ref()
                .unwrap()
                .debounce_until
                .is_some(),
            "deadline armed"
        );
        // The timer sleeps the debounce window then notifies. `Notify`
        // is permit-style, so a notify landing before we await is not
        // lost. Block on a tiny runtime with generous margin.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let woke = rt.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(2), paint.notified())
                .await
                .is_ok()
        });
        assert!(
            woke,
            "debounce must notify paint_request so the loop wakes to fire the re-query"
        );
    }

    #[test]
    fn open_picker_grep_with_initial_pattern_seats_immediately_and_marks_loading() {
        let mut app = app_with("hi\n", 5);
        // Grep-UX fix: with an initial pattern, init() schedules a
        // Future (real grep on the blocking pool) AND the picker is
        // now SEATED IMMEDIATELY with the seeded query visible + a
        // `loading` indicator — instead of staying unseated until
        // results arrive (the old "command consumed, nothing
        // happens" feel). The future's result re-seats via
        // `drain_pending_picker_init`, clearing `loading`.
        app.open_picker("grep".into(), vec!["needle".to_string()]);
        let picker = app
            .editor
            .picker
            .as_ref()
            .expect("picker seated immediately, not parked unseated");
        assert_eq!(picker.query, "needle", "initial pattern visible at once");
        assert!(
            picker.loading,
            "in-flight grep shows the searching indicator"
        );
        // The stash was consumed on the immediate seat.
        let live = app
            .editor
            .live_picker_query
            .as_ref()
            .expect("live state must be installed");
        assert!(
            live.initial_query.is_none(),
            "initial_query consumed by the immediate seat"
        );
        // pending_picker_init still carries the in-flight future.
        assert!(
            app.editor.pending_picker_init.is_some(),
            "init returned Future -> drain rx parked",
        );
    }

    #[test]
    fn grep_live_reseat_preserves_typed_query_and_clears_loading() {
        // The typed query must survive an async result re-seat (a
        // LIVE source re-seats on every refresh). Drive the seat
        // paths by hand: open with "TODO" (seats, loading=true),
        // then re-seat as `drain_pending_picker_init` would when
        // results land — the query carries across and loading clears.
        let mut app = app_with("hi\n", 5);
        app.open_picker("grep".into(), vec!["TODO".to_string()]);
        assert!(app.editor.picker.as_ref().unwrap().loading);
        // Results arrive: re-seat with (empty) pairs.
        app.seat_picker_from_pairs("grep".to_string(), Vec::new());
        let picker = app.editor.picker.as_ref().expect("picker open");
        assert_eq!(
            picker.query, "TODO",
            "typed query carried across the live re-seat"
        );
        assert_eq!(picker.query_cursor, "TODO".len());
        assert!(
            !picker.loading,
            "loading cleared once results seated (fresh picker default)"
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

    /// Narrow regression coverage for one half of the `c_c_g_log_item_...`
    /// bug fixed below: `action:magit-global-log`'s handler must be
    /// registered in `ActionHandlerRegistry` right after `App::new()`.
    /// Isolates registration from the dispatch/effect-application path
    /// (fixed separately — see that test's doc comment) so a future
    /// regression in either half fails independently with a precise
    /// signal instead of both collapsing into one generic "does nothing"
    /// symptom.
    #[test]
    fn diagnostic_magit_global_log_handler_registered_after_boot() {
        let app = app_with("hi\n", 24);
        let cmd_reg = app
            .editor
            .services
            .get::<lattice_grammar::CommandRegistryHandle>()
            .expect("CommandRegistryHandle service must exist");
        let cid = cmd_reg
            .load()
            .id_by_name("action:magit-global-log")
            .expect("action:magit-global-log must be a registered command");
        let ah_reg = app
            .editor
            .services
            .get::<lattice_mode::ActionHandlerRegistryHandle>()
            .expect("ActionHandlerRegistryHandle service must exist");
        let handler = ah_reg.lookup(cid);
        assert!(
            handler.is_some(),
            "action:magit-global-log (cid={cid:?}) has no registered handler in ActionHandlerRegistry"
        );
    }

    /// End-to-end regression test for a live-reported bug: `C-c g`
    /// opened magit's root dispatch transient, but every item's key
    /// just closed the menu and did nothing.
    ///
    /// Two distinct root causes, both fixed:
    /// 1. `MagitGlobalMode` registered its `action:magit-global-*`
    ///    handlers from `on_activate`, gated by a `OnceLock` consumed
    ///    on the FIRST attempt regardless of success — a failure on
    ///    whichever buffer activated first (a real hazard: `on_activate`
    ///    futures run through a shared "try-sync-then-spawn" cascade,
    ///    `ModeRegistry::spawn_cascade`, where one mode's real async
    ///    work in the same batch defers EVERY step in that batch to a
    ///    background task with no completion guarantee) left every
    ///    handler permanently unregistered. Fixed by moving these to
    ///    `Mode::action_handlers()` — a plain synchronous list the host
    ///    walks once at boot, no activation-timing dependency at all.
    /// 2. Even with the handler correctly found and fired,
    ///    `do_transient_trigger` only ran the returned `Effect` through
    ///    `apply_effect_host` (host-only effect application) and never
    ///    queued it in `DispatchOutcome.effects` — so a renderer-coupled
    ///    effect like `Effect::OpenSyntheticBuffer` (which EVERY "open
    ///    the X buffer" transient item returns) was silently dropped:
    ///    `apply_effect_host`/`handle_effect` doesn't know about it: only
    ///    each renderer's own `apply_effect_app_arms` does, reached by
    ///    draining `out.effects` after dispatch returns. Fixed by also
    ///    pushing the effect into `out.effects`.
    ///
    /// This drives the REAL end-to-end path (`press` → `translate` →
    /// `App::apply`, full `App::new()` boot with `lattice-magit`
    /// installed exactly as production does) rather than hand-testing
    /// `DispatchActionIds` resolution in isolation — that narrower test
    /// (`lattice-magit`'s
    /// `every_root_dispatch_item_resolves_to_a_real_action_not_a_flag_fallback`)
    /// could not have caught either bug, since the `CommandId`s it
    /// checks resolve correctly regardless; only the buffer actually
    /// opening proves the fix. `#[tokio::test]`: the fired handler opens
    /// `magit-log-mode`, whose `on_activate` runs a real `git log` via
    /// `spawn_blocking` — needs a runtime, and needs a few yields for
    /// that spawned task to complete.
    #[tokio::test]
    async fn c_c_g_log_item_actually_opens_the_log_buffer_not_just_dismiss() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        use crate::app::test_helpers::press;

        let mut app = app_with("hi\n", 24);
        press(
            &mut app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        );
        press(
            &mut app,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
        );
        assert!(
            app.editor
                .picker
                .as_ref()
                .and_then(|p| p.transient.as_ref())
                .is_some(),
            "C-c g must open the magit dispatch transient"
        );

        press(
            &mut app,
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
        );

        // The transient always closes on an Action item (do_picker_dismiss
        // runs unconditionally) — that alone does NOT prove the handler
        // fired, which is exactly what made both bugs above invisible at
        // the dismiss level. The real proof is that the log buffer exists.
        let mut found = false;
        for _ in 0..50 {
            if app.editor.buffers.by_name("*magit:log*").is_some() {
                found = true;
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            found,
            "pressing 'l' must actually open *magit:log*, not just \
             dismiss the transient with no effect"
        );
    }

    /// Same shape as the log test above, for the file-dispatch
    /// transient (`C-c f`) — covers the OTHER global-action set
    /// (`action:magit-global-file-stage`/`-file-diff`), also
    /// contributed via `Mode::action_handlers()` now. `d` (diff) is
    /// used here since it has an observable effect (a new buffer)
    /// even with no staged changes in the test's temp repo-less
    /// directory; `s` (stage) would silently no-op outside a real
    /// repo regardless of the bugs above.
    #[tokio::test]
    async fn c_c_f_diff_item_actually_opens_a_diff_buffer_not_just_dismiss() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        use crate::app::test_helpers::{app_with_path, press};

        // Must live INSIDE a real git repo, not `/tmp` — the file-diff
        // handler's `Repository::discover(&path)` fails (and the
        // handler returns `None` via `?`, correctly, no bug) for a
        // path outside one. `cargo test`'s cwd is the lattice repo
        // itself.
        let tmp = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "lattice-magit-file-dispatch-test-{}.txt",
            std::process::id()
        ));
        std::fs::write(&tmp, "hello\n").expect("write temp file");
        // RAII cleanup: a bare `remove_file` at the end of the test
        // leaks the file whenever an assertion panics first — which is
        // exactly what happened while the transient-dispatch bug this
        // test guards was still live, littering the repo root with
        // `lattice-magit-file-dispatch-test-*.txt`.
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _cleanup = Cleanup(tmp.clone());
        let mut app = app_with_path("hello\n", 24, tmp.clone());

        press(
            &mut app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        );
        press(
            &mut app,
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE),
        );
        assert!(
            app.editor
                .picker
                .as_ref()
                .and_then(|p| p.transient.as_ref())
                .is_some(),
            "C-c f must open the magit file-dispatch transient"
        );

        press(
            &mut app,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
        );

        let mut found = false;
        for _ in 0..50 {
            if app.editor.buffers.document_ids_sorted().iter().any(|id| {
                app.editor
                    .buffers
                    .name_of(*id)
                    .is_some_and(|n| n.starts_with("*magit:diff:"))
            }) {
                found = true;
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            found,
            "pressing 'd' must actually open a *magit:diff:<path>* buffer, \
             not just dismiss the transient with no effect"
        );

        // `_cleanup` removes the file on the way out, panic or not.
    }

    /// Regression test for a live-reported bug: pressing `q` on the
    /// magit-status buffer closed the WHOLE EDITOR. Root cause:
    /// `action:magit-close` (bound to `q` in `magit-core-mode`, shared
    /// by every magit buffer) returned `Effect::QuitEditor { scope:
    /// QuitScope::Pane, .. }` — vim's `:q` semantics, "close the pane;
    /// on the last pane, quit". magit buffers open IN PLACE in the
    /// current pane (not a split), so with only one pane open (the
    /// common case — just launched the editor, opened magit-status),
    /// `q` quit the whole app. `q` on a magit buffer means "bury this
    /// buffer" (Emacs `bury-buffer`), never "close a window" — fixed
    /// by returning `Effect::DismissPopup` instead, which restores the
    /// pane's pre-open buffer via `Editor::prev_pane_for_popup`
    /// (stashed by `Editor::open_synthetic_buffer`, mirroring the same
    /// mechanism Help's own `q`/`<Esc>` already used correctly).
    #[test]
    fn q_on_magit_status_buries_it_and_never_quits_the_editor() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        use crate::app::test_helpers::press;

        let mut app = app_with("hi\n", 24);
        let original_buffer_id = app.editor.active_pane_buffer_id();

        press(
            &mut app,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL),
        );
        press(
            &mut app,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
        );
        assert_ne!(
            app.editor.active_pane_buffer_id(),
            original_buffer_id,
            "C-x g must switch the pane to the magit-status buffer"
        );
        assert!(
            !app.editor.should_quit,
            "opening magit-status must not quit"
        );

        press(
            &mut app,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        );

        assert!(
            !app.editor.should_quit,
            "q on magit-status must bury the buffer, never quit the editor"
        );
        assert_eq!(
            app.editor.active_pane_buffer_id(),
            original_buffer_id,
            "q on magit-status must restore the buffer that was active before it opened"
        );
    }
}
