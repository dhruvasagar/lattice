//! Modal-state transitions -- the buffer-level state machine
//! (Normal / Insert / Visual / Op-pending / Command /
//! Search) and the major-mode activation hooks that fire
//! on transition.
//!
//! Methods that live here:
//! - `modal_label` (status-line label of the current
//!   modal-state).
//!
//! Methods that move here in R.1 (deferred):
//! - `enter_normal`, `enter_insert`, `enter_replace`,
//!   `enter_op_pending`, `enter_command`, `enter_search`.
//! - The `ModeContext` builder used by major modes during
//!   `on_activate` -- including the `setup_buffer_locals`
//!   path that seeds `BufferLocals` from struct fields.
//! - Cursor-shape recompute on mode transitions (the
//!   App-side hook; renderer reads the resulting state).
//! - `set_active_mode_for_buffer` and the `Mode` trait
//!   dispatch that calls `on_activate` / `on_deactivate`.
//!
//! Insert-mode entry methods (`enter_insert_after`,
//! `enter_insert_line_start`, `enter_insert_line_end`,
//! `enter_insert_open_below`, `enter_insert_open_above`)
//! landed in `app/edit.rs` via R.1.47 because they pair
//! with the Insert / Replace edit primitives that already
//! lived there; they read as edit-flow rather than mode-
//! flow.
//!
//! What does NOT live here: the `Mode` trait itself
//! (lives in `crate::modes`), the per-major-mode impls
//! (`TextMode`, `RustMode`, ...) -- those stay in
//! `crate::modes`.

use lattice_grammar::ModalState;
use lattice_mode::{CapabilitySet, ModeId, ModeKind};
use lattice_protocol::Event;

use super::{App, BufferId, BufferKind, EchoLevel};

impl App {
    /// Activate the resolved major mode for `buffer_id` based
    /// on its `kind` (and, for Document buffers, the detected
    /// language) and refresh the resolved-options cache. M.3.1.
    ///
    /// Lang detection happens inside
    /// `lattice_syntax::Lang::detect_from_path`; for buffers
    /// without a path (scratch documents) the resolver falls
    /// through to `text-mode` per `mode-architecture.md` §4.1.
    /// Help / FileTree / Oil are kind-driven (no language
    /// dimension); the `lang` argument is ignored for those
    /// kinds.
    ///
    /// On activation failure (mode not registered, capability
    /// missing, conflict with active major), the buffer ends
    /// up with no active major and the resolved options
    /// reflect only the registry defaults. Failure is logged;
    /// it isn't a fatal startup error because the design
    /// commits to "every buffer has a major mode" but the
    /// implementation tolerates the bootstrap window where the
    /// registration hasn't completed.
    pub fn activate_major_for_buffer_kind(&mut self, buffer_id: BufferId, kind: BufferKind) {
        // Only Document buffers consult Lang; the others have
        // a fixed mode regardless of content.
        let lang = match kind {
            BufferKind::Document => {
                let snap = self.document.snapshot();
                let path_owned = snap.path.as_ref().map(|p| (**p).clone());
                let path_ref = path_owned.as_deref();
                lattice_syntax::Lang::detect_from_path(path_ref)
            }
            _ => lattice_syntax::Lang::Plain,
        };
        let major_id = crate::modes::resolve_major_mode(kind, lang);
        let proto_id = lattice_protocol::ids::BufferId::new(buffer_id.0 as u64);
        let mut active = self.active_modes.remove(&buffer_id).unwrap_or_default();
        match self.mode_registry.activate_major(
            &mut active,
            &self.mode_guards,
            &self.config,
            &self.event_bus,
            &self.services,
            proto_id,
            major_id,
            lattice_mode::CapabilitySet::empty(),
        ) {
            Ok(_events) => {}
            Err(e) => {
                self.set_message(
                    EchoLevel::Warn,
                    format!(
                        "mode: activate_major({}) for buffer {} failed: {}",
                        major_id, buffer_id.0, e,
                    ),
                );
            }
        }
        if let Some(minor_id) = crate::modes::default_minor_mode_id_for_buffer_kind(kind)
            && let Err(e) = self.mode_registry.activate_minor(
                &mut active,
                &self.mode_guards,
                &self.config,
                &self.event_bus,
                &self.services,
                proto_id,
                minor_id,
                lattice_mode::CapabilitySet::empty(),
            )
        {
            self.set_message(
                EchoLevel::Warn,
                format!(
                    "mode: activate_minor({}) for buffer {} failed: {}",
                    minor_id, buffer_id.0, e,
                ),
            );
        }
        for minor_id in crate::modes::auto_activated_minors_for_buffer_kind(kind) {
            if let Err(e) = self.mode_registry.activate_minor(
                &mut active,
                &self.mode_guards,
                &self.config,
                &self.event_bus,
                &self.services,
                proto_id,
                minor_id,
                lattice_mode::CapabilitySet::empty(),
            ) {
                self.set_message(
                    EchoLevel::Warn,
                    format!(
                        "mode: activate_minor({}) for buffer {} failed: {}",
                        minor_id, buffer_id.0, e,
                    ),
                );
            }
        }
        self.active_modes.insert(buffer_id, active);
        self.recompute_options_for_buffer(buffer_id);
        // CSM.3: keep `ActiveCompletionSources` in lockstep with
        // the active-modes set. Empty in practice until CSM.4
        // ships the first source-contributing mode.
        self.recompute_active_completion_sources_for(buffer_id);
        // M.5.2: post-activation hook -- if the buffer is now on a
        // language major with a configured LSP server, auto-
        // activate `lsp-mode`. Modelled as a synchronous hook
        // here; converting to an event-bus subscription on
        // `MajorEntered` is a follow-up once the broader
        // subscriber API for App-level handlers lands.
        self.maybe_auto_activate_lsp_mode(buffer_id);
    }

    /// M.5.2: language-mode auto-activation hook for
    /// `lsp-mode`. Runs after a major activates; when the active
    /// buffer's path has a server configured in the LSP registry
    /// and `lsp-mode` isn't already active, activate it.
    ///
    /// Lifecycle for `lsp-mode` (didOpen / didClose) lands in
    /// M.5.3; for now activation is a state flip (the gate is
    /// observable but no LSP traffic flows yet).
    ///
    /// **Asymmetry by design (mode-architecture §M.5):** there
    /// is no auto-deactivation hook on `MajorExited`. Active
    /// minors stay across major-mode swaps -- emacs's "kill all
    /// local variables" footgun is what we're avoiding. If a
    /// user wants `lsp-mode` off after a major change, they run
    /// `:lsp-mode` to toggle.
    pub(super) fn maybe_auto_activate_lsp_mode(&mut self, buffer_id: BufferId) {
        if self.lsp_mode_enabled_for(buffer_id) {
            return;
        }
        let path = match self.path_for_buffer(buffer_id) {
            Some(p) => p,
            // Scratch buffers with no path can still host LSP
            // (standalone-server scenarios), but only when the
            // user explicitly runs `:lsp-mode`. Auto-activation
            // is path-driven.
            None => return,
        };
        if !self.lsp.has_server_for_path(&path) {
            return;
        }
        self.activate_mode_by_id(buffer_id, lattice_lsp::modes::LspMode::mode_id());
    }

    /// Best-effort path lookup for `buffer_id`. Returns the
    /// document's path for Document buffers, `None` otherwise.
    /// Used by the LSP auto-activation hook above.
    fn path_for_buffer(&self, buffer_id: BufferId) -> Option<std::path::PathBuf> {
        if buffer_id == self.document_buffer_id {
            return self.document.path().map(|p| p.to_path_buf());
        }
        self.buffers.document_path(buffer_id)
    }

    /// M.5.1: programmatic activation of `mode_id` on `buffer_id`.
    /// Used by hooks (auto-activation on `MajorEntered` etc.) and
    /// by the auto-generated `:<mode-name>` toggle command. The
    /// registry decides Major-vs-Minor and runs the appropriate
    /// activation; for majors the previous major is deactivated
    /// first.
    ///
    /// On failure, surfaces an `EchoLevel::Warn` and returns
    /// without mutating state. Callers that need to know the
    /// outcome can read `self.active_modes[buffer_id]` after.
    pub fn activate_mode_by_id(&mut self, buffer_id: BufferId, mode_id: ModeId) {
        let Some(mode) = self.mode_registry.get(mode_id) else {
            self.set_message(
                EchoLevel::Warn,
                format!("mode: `{mode_id}` is not registered"),
            );
            return;
        };
        let kind = mode.kind();
        let proto_id = lattice_protocol::ids::BufferId::new(buffer_id.0 as u64);
        let mut active = self.active_modes.remove(&buffer_id).unwrap_or_default();
        let result = match kind {
            ModeKind::Major => self.mode_registry.activate_major(
                &mut active,
                &self.mode_guards,
                &self.config,
                &self.event_bus,
                &self.services,
                proto_id,
                mode_id,
                CapabilitySet::empty(),
            ),
            ModeKind::Minor => self.mode_registry.activate_minor(
                &mut active,
                &self.mode_guards,
                &self.config,
                &self.event_bus,
                &self.services,
                proto_id,
                mode_id,
                CapabilitySet::empty(),
            ),
        };
        if let Err(e) = result {
            self.set_message(
                EchoLevel::Warn,
                format!(
                    "mode: activate({mode_id}) for buffer {} failed: {e}",
                    buffer_id.0
                ),
            );
        }
        self.active_modes.insert(buffer_id, active);
        self.recompute_options_for_buffer(buffer_id);
        self.recompute_active_completion_sources_for(buffer_id);
        // M.5.2: when a major activates (whether by direct call,
        // `:<major-name>` toggle, or buffer-creation path), run
        // the LSP auto-activation hook. Skipped for minor
        // activations -- if `lsp-mode` is the one being
        // activated, the hook would just no-op (already-active
        // short-circuit).
        if matches!(kind, ModeKind::Major) {
            self.maybe_auto_activate_lsp_mode(buffer_id);
        }
        // Phase 3: the `lsp-mode` activate side-effects --
        // `LspBufferAttached` event publication (Phase 2)
        // and sub-mode cascade (Phase 3 -- now driven by
        // `Mode::implies()` so the registry handles it) --
        // are owned by the mode. No App-side hook here.
        // Modes that mutated options in their `on_activate`
        // (e.g. `lsp-folding-mode` swapping `foldmethod=lsp`)
        // have already published `OptionChanged` events into
        // the typed-options channel. Drain here so the
        // side-effect cascade (option cache recompute,
        // `recompute_folds` for foldmethod, theme refresh for
        // `ui.*`, ...) runs synchronously before the caller
        // observes the post-activation state.
        self.drain_option_changes();
    }

    /// M.5.1: programmatic deactivation of `mode_id` on
    /// `buffer_id`. Symmetric to [`Self::activate_mode_by_id`].
    /// Major deactivation leaves the buffer with no active major
    /// until the next activation; user-facing flows usually flow
    /// through the toggle command which performs swap rather
    /// than a bare deactivate.
    pub fn deactivate_mode_by_id(&mut self, buffer_id: BufferId, mode_id: ModeId) {
        let Some(mode) = self.mode_registry.get(mode_id) else {
            self.set_message(
                EchoLevel::Warn,
                format!("mode: `{mode_id}` is not registered"),
            );
            return;
        };
        let proto_id = lattice_protocol::ids::BufferId::new(buffer_id.0 as u64);
        let mut active = self.active_modes.remove(&buffer_id).unwrap_or_default();
        let result = match mode.kind() {
            ModeKind::Major => self.mode_registry.deactivate_major(
                &mut active,
                &self.mode_guards,
                &self.event_bus,
                proto_id,
            ),
            ModeKind::Minor => self.mode_registry.deactivate_minor(
                &mut active,
                &self.mode_guards,
                &self.event_bus,
                proto_id,
                mode_id,
            ),
        };
        if let Err(e) = result {
            self.set_message(
                EchoLevel::Warn,
                format!(
                    "mode: deactivate({mode_id}) for buffer {} failed: {e}",
                    buffer_id.0
                ),
            );
        }
        self.active_modes.insert(buffer_id, active);
        self.recompute_options_for_buffer(buffer_id);
        self.recompute_active_completion_sources_for(buffer_id);
        // Phase 3 + follow-up: `lsp-mode` deactivate side-
        // effects are fully owned by the mode. `LspBufferDetached`
        // is published from `LspMode::on_deactivate` via
        // `ctx.events()` (Phase 2); the App's per-tick drain
        // (`drain_lsp_detach_events`, wired in `runtime.rs`)
        // subscribes to that event at boot and calls
        // `lsp_close_buffer` for each detach. No App-side
        // hook here.
        // Symmetric to `activate_mode_by_id`: drain option
        // mutations the mode emitted in its `on_deactivate`
        // (e.g. `lsp-folding-mode` restoring the prior
        // `foldmethod`) so the side-effect cascade runs
        // before the caller observes the state.
        self.drain_option_changes();
    }

    /// M.5.3: lsp-mode activated on `buffer_id`. Emits
    /// `LspBufferAttached` on the editor's event bus so
    /// subscribers see the gate flip. Wire-level `didOpen` is
    /// already driven by the `attach_driver` subscribing to
    /// `Event::DocumentOpened` from the file-open path.
    // Phase 3 removed `on_lsp_mode_activated` /
    // `_deactivated`. The work they did is owned by the mode
    // now:
    //
    // - Event publication (`LspBufferAttached` /
    //   `LspBufferDetached`) lives in `LspMode::on_activate` /
    //   `on_deactivate` via `ctx.events()` (Phase 2).
    // - Sub-mode cascade (activate + deactivate of the 13
    //   LSP sub-modes) lives in `Mode::implies()` --
    //   `LspMode::new` builds the list once; the registry's
    //   `activate_minor` / `deactivate_minor` walk the list
    //   (the latter symmetrically via the Phase 3 cascade
    //   extension).
    //
    // The wire-level `didClose` (`lsp_close_buffer`) is the
    // only remaining App-side action; it lives at the
    // `deactivate_mode_by_id` call site directly. Moving it
    // requires a `LspBufferDetached` subscriber in boot;
    // queued for a follow-up slice.

    // 4.4.f: `lsp-folding-mode` lifecycle moved into
    // `LspFoldingMode::on_activate` / `on_deactivate` in
    // `lattice-lsp`. The mode owns its work; the App is just
    // the orchestrator. `drain_option_changes()` in the
    // activate/deactivate call sites picks up the option
    // mutation the mode emits via `ctx.config()`.

    // Phase 3 removed `activate_lsp_sub_modes_for` /
    // `deactivate_lsp_sub_modes_for`. The sub-mode cascade
    // lives in the registry now, driven by
    // `LspMode::implies()` (a Vec built once at
    // `LspMode::new()`). `ModeRegistry::activate_minor`
    // walks `implies()` on activation; the symmetric
    // `deactivate_minor` extension walks it on
    // deactivation. Adding a new LSP sub-mode is one
    // entry in `LspMode::new()`'s `vec![...]`; no App-side
    // edits.

    /// M.5.1: toggle a mode by name on the active pane's buffer.
    /// This is the apply-fn target for the auto-generated
    /// `:<mode-name>` ex-commands (mode-architecture §9.6.1).
    /// Toggle semantics:
    /// - **Minor**: deactivate if active; activate if inactive.
    /// - **Major**: activate if not currently the major; if it's
    ///   already the active major, the registry treats this as
    ///   a *reload* (deactivate then re-activate, per §9.6).
    ///
    /// Activating a major that differs from the current major
    /// performs a swap -- the registry deactivates the previous
    /// major before activating the new one. Active minors stay
    /// untouched across the swap (their state lives in
    /// type-keyed `BufferLocals` owned per-mode; no
    /// `kill-all-local-variables` semantics).
    pub fn toggle_mode_by_name(&mut self, name: &str) {
        let mode_id = ModeId::new(name);
        let buffer_id = self.active_pane_buffer_id();
        let Some(mode) = self.mode_registry.get(mode_id) else {
            self.set_message(
                EchoLevel::Error,
                format!("mode: `{name}` is not a registered mode"),
            );
            return;
        };
        let active_now = self
            .active_modes
            .get(&buffer_id)
            .map(|m| m.is_active(mode_id))
            .unwrap_or(false);
        match (mode.kind(), active_now) {
            (ModeKind::Minor, true) => self.deactivate_mode_by_id(buffer_id, mode_id),
            (ModeKind::Minor, false) => self.activate_mode_by_id(buffer_id, mode_id),
            // Major: activating an inactive major swaps it in;
            // re-activating the current major reloads (registry
            // contract). Either way the call is the same.
            (ModeKind::Major, _) => self.activate_mode_by_id(buffer_id, mode_id),
        }
    }

    pub fn modal_label(&self) -> &'static str {
        match self.modal {
            ModalState::Normal => "NORMAL",
            ModalState::Insert => "INSERT",
            ModalState::Visual(_) => "VISUAL",
            ModalState::OperatorPending => "O-PEND",
            ModalState::Command => "CMD",
            ModalState::Search(_) => "SEARCH",
            ModalState::Replace => "REPLACE",
        }
    }

    pub(super) fn enter_mode(&mut self, state: ModalState) {
        let prior = self.modal;
        // Reset Replace's history every time we enter (or re-enter) Replace
        // so backspace-restore is bounded to the current `R` session.
        if matches!(state, ModalState::Replace) {
            self.replace_history.clear();
        }
        let was_insert_like = matches!(self.modal, ModalState::Insert | ModalState::Replace);
        let entering_insert_like = matches!(state, ModalState::Insert | ModalState::Replace);
        // Insert-replay capture:
        //   - Entering Insert/Replace from anything else: start recording.
        //   - Leaving Insert/Replace to anything else: promote into last_insert.
        if entering_insert_like && !was_insert_like {
            self.recording_insert = Some(String::new());
        }
        if was_insert_like
            && !entering_insert_like
            && let Some(rec) = self.recording_insert.take()
        {
            // Snapshot the recording before consuming the block-
            // insert spec; we need both to replicate.
            let block_spec = self.pending_block_insert.take();
            if !rec.is_empty() {
                self.last_insert = Some(rec.clone());
            }
            if let Some(spec) = block_spec
                && !rec.is_empty()
            {
                self.replicate_block_insert(spec, &rec);
            }
        } else if was_insert_like && !entering_insert_like {
            // Insert exited but recording_insert was already None
            // (shouldn't happen given enter_mode pairs them, but
            // belt-and-braces -- still clear any spec so a future
            // I/A starts clean).
            self.pending_block_insert = None;
        }
        self.modal = state;
        if matches!(state, ModalState::Normal) {
            // Vim's behavior: leaving Insert mode pulls the cursor back one
            // byte if it's not already at the start of the line, so the
            // cursor sits on the last inserted char rather than past it.
            if self.cursor.byte > 0 {
                self.cursor.byte -= 1;
            }
        }
        // Publish ModalModeChanged whenever the modal axis actually
        // moves. (DESIGN.md §5.10 catalog.) Re-entering the same
        // mode -- e.g. the dot-repeat path that calls enter_mode
        // for the side-effect of recording/replay accounting --
        // doesn't fire the event.
        if prior != state {
            self.event_bus.publish(Event::ModalModeChanged {
                from: format!("{prior:?}"),
                to: format!("{state:?}"),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use crate::app::test_helpers::app_with;

    #[test]
    fn line_numbers_mode_overrides_typed_option_layer() {
        // M.7.0: the mode-contribution layer wins against the
        // typed-option layer. `Number` defaults to `true`; the
        // user can `:set nonumber` to flip the typed-option
        // layer to `false`, then `:line-numbers-mode` to
        // re-enable just for this buffer via the mode layer.
        // The resolved value reflects the mode contribution.
        let mut a = app_with("hi", 5);
        let id = a.pane_tree.active().buffer_id;
        // Flip the typed-option layer to false globally.
        a.do_set("nonumber");
        assert!(!a.show_line_numbers(), "set nonumber should flip cache");
        // Activate `:line-numbers-mode` for this buffer; the
        // mode-contribution layer overrides the typed-option
        // layer's false.
        a.toggle_mode_by_name("line-numbers-mode");
        assert!(
            *a.resolved_option::<lattice_config::Number>(id),
            "mode contribution should override typed-option false",
        );
        assert!(a.show_line_numbers(), "hot-path cache should track");
        // Deactivating the mode removes the contribution; the
        // typed-option layer (false) takes over again.
        a.toggle_mode_by_name("line-numbers-mode");
        assert!(!a.show_line_numbers());
    }

    #[test]
    fn relative_line_numbers_mode_implies_line_numbers() {
        // M.7.0: `:relative-line-numbers-mode` contributes both
        // RelativeNumber=true AND Number=true (vim's `:set rnu`
        // implies `:set nu` cascade, baked into the mode's
        // contribution so users who never touch
        // `:line-numbers-mode` still get a visible gutter).
        let mut a = app_with("hi", 5);
        let id = a.pane_tree.active().buffer_id;
        a.toggle_mode_by_name("relative-line-numbers-mode");
        assert!(*a.resolved_option::<lattice_config::RelativeNumber>(id));
        assert!(*a.resolved_option::<lattice_config::Number>(id));
    }

    #[test]
    fn wrap_mode_toggle_flips_wrap_lines() {
        let mut a = app_with("hi", 5);
        let id = a.pane_tree.active().buffer_id;
        assert!(!a.wrap_lines());
        a.toggle_mode_by_name("wrap-mode");
        assert!(*a.resolved_option::<lattice_config::Wrap>(id));
        a.toggle_mode_by_name("wrap-mode");
        assert!(!a.wrap_lines());
    }

    #[test]
    fn set_number_true_activates_line_numbers_mode_on_active_buffer() {
        // M.7.1 convergence: `:set number=true` activates
        // `line-numbers-mode` on the active buffer; `:set
        // nonumber` deactivates it. The two surfaces stay in
        // sync.
        let mut a = app_with("hi", 5);
        let id = a.pane_tree.active().buffer_id;
        // Default Number=true; M.7.1 means the mode should
        // already be active on the active buffer (initial cascade
        // when typed-option is true at startup). But initial
        // boot doesn't fire OptionChanged, so the mode isn't
        // pre-activated. Verify by explicit set.
        a.do_set("nonumber");
        assert!(
            !a.active_modes
                .get(&id)
                .unwrap()
                .has_minor(lattice_mode::modes::LineNumbersMode::mode_id())
        );
        a.do_set("number");
        assert!(
            a.active_modes
                .get(&id)
                .unwrap()
                .has_minor(lattice_mode::modes::LineNumbersMode::mode_id())
        );
        a.do_set("nonumber");
        assert!(
            !a.active_modes
                .get(&id)
                .unwrap()
                .has_minor(lattice_mode::modes::LineNumbersMode::mode_id())
        );
    }

    #[test]
    fn set_wrap_converges_with_wrap_mode() {
        let mut a = app_with("hi", 5);
        let id = a.pane_tree.active().buffer_id;
        let wrap_id = lattice_mode::modes::WrapMode::mode_id();
        a.do_set("wrap");
        assert!(a.active_modes.get(&id).unwrap().has_minor(wrap_id));
        a.do_set("nowrap");
        assert!(!a.active_modes.get(&id).unwrap().has_minor(wrap_id));
    }

    #[test]
    fn set_list_converges_with_whitespace_show_mode() {
        // M.7.2: `:set list` (vim alias for whitespace-show)
        // activates `whitespace-show-mode`; `:set nolist`
        // deactivates.
        let mut a = app_with("hi", 5);
        let id = a.pane_tree.active().buffer_id;
        let mode_id = lattice_mode::modes::WhitespaceShowMode::mode_id();
        a.do_set("list");
        assert!(a.active_modes.get(&id).unwrap().has_minor(mode_id));
        a.do_set("nolist");
        assert!(!a.active_modes.get(&id).unwrap().has_minor(mode_id));
    }

    #[test]
    fn set_cursorline_converges_with_current_line_highlight_mode() {
        // M.7.2: `:set cursorline` (vim alias) ↔
        // `:current-line-highlight-mode`.
        let mut a = app_with("hi", 5);
        let id = a.pane_tree.active().buffer_id;
        let mode_id = lattice_mode::modes::CurrentLineHighlightMode::mode_id();
        a.do_set("cursorline");
        assert!(a.active_modes.get(&id).unwrap().has_minor(mode_id));
        a.do_set("nocursorline");
        assert!(!a.active_modes.get(&id).unwrap().has_minor(mode_id));
    }

    #[test]
    fn line_numbers_mode_activation_matches_set_number() {
        // M.7.1 the other direction: activating the mode AND
        // running `:set number` produce the same observable
        // state. (Mode activation doesn't reach back into the
        // typed-option layer in v1 -- that's a separate
        // refinement -- but the resolved-option view converges
        // because the mode contribution wins regardless.)
        let mut a = app_with("hi", 5);
        let id = a.pane_tree.active().buffer_id;
        a.do_set("nonumber");
        assert!(!a.show_line_numbers());
        // Mode activation flips the mode-contribution layer.
        a.toggle_mode_by_name("line-numbers-mode");
        assert!(
            a.show_line_numbers(),
            "mode contribution overrides set false"
        );
        // `:set number` activates the mode (already active --
        // no-op) and flips the typed-option layer.
        a.do_set("number");
        assert!(a.show_line_numbers());
        assert!(
            a.active_modes
                .get(&id)
                .unwrap()
                .has_minor(lattice_mode::modes::LineNumbersMode::mode_id())
        );
    }

    #[test]
    fn read_only_mode_toggle_flips_read_only_resolved_value() {
        // M.7.0: `:read-only-mode` lets users mark an arbitrary
        // buffer read-only. `ReadOnly` itself is
        // `customizable = false` (no `:set` surface); this is
        // the user-typed pathway.
        let mut a = app_with("hi", 5);
        let id = a.pane_tree.active().buffer_id;
        assert!(!*a.resolved_option::<lattice_config::ReadOnly>(id));
        a.toggle_mode_by_name("read-only-mode");
        assert!(*a.resolved_option::<lattice_config::ReadOnly>(id));
        a.toggle_mode_by_name("read-only-mode");
        assert!(!*a.resolved_option::<lattice_config::ReadOnly>(id));
    }

    #[test]
    fn toggle_minor_mode_by_name_activates_then_deactivates() {
        // M.5.1: `:lsp-mode` (or any minor name) toggles. First
        // call activates, second deactivates. The mode is
        // registered at boot so name lookup succeeds.
        let mut a = app_with("hi", 5);
        let id = a.pane_tree.active().buffer_id;
        let lsp_mode = lattice_lsp::modes::LspMode::mode_id();
        assert!(!a.lsp_mode_enabled_for(id));
        a.toggle_mode_by_name("lsp-mode");
        assert!(a.lsp_mode_enabled_for(id));
        assert!(a.active_modes.get(&id).unwrap().has_minor(lsp_mode));
        a.toggle_mode_by_name("lsp-mode");
        assert!(!a.lsp_mode_enabled_for(id));
    }

    #[test]
    fn activating_lsp_mode_cascades_all_sub_modes_on() {
        // M.6.1 cascade-on: toggling `:lsp-mode` activates every
        // LSP sub-mode in one step. The umbrella is the gate;
        // the sub-modes are user-controllable disable switches
        // that default to "track the umbrella".
        let mut a = app_with("hi", 5);
        let id = a.pane_tree.active().buffer_id;
        a.toggle_mode_by_name("lsp-mode");
        assert!(a.lsp_mode_enabled_for(id));
        // All nine sub-modes flipped on.
        assert!(a.lsp_completion_mode_enabled_for(id));
        assert!(a.lsp_diagnostics_mode_enabled_for(id));
        assert!(a.lsp_hover_mode_enabled_for(id));
        assert!(a.lsp_signature_mode_enabled_for(id));
        assert!(a.lsp_format_mode_enabled_for(id));
        assert!(a.lsp_rename_mode_enabled_for(id));
        assert!(a.lsp_symbols_mode_enabled_for(id));
        assert!(a.lsp_code_action_mode_enabled_for(id));
        assert!(a.lsp_nav_mode_enabled_for(id));
    }

    #[test]
    fn deactivating_lsp_mode_cascades_all_sub_modes_off() {
        // M.6.1 cascade-off: toggling `:lsp-mode` off deactivates
        // every sub-mode atomically.
        let mut a = app_with("hi", 5);
        let id = a.pane_tree.active().buffer_id;
        a.toggle_mode_by_name("lsp-mode");
        assert!(a.lsp_hover_mode_enabled_for(id));
        a.toggle_mode_by_name("lsp-mode");
        assert!(!a.lsp_mode_enabled_for(id));
        // All nine sub-modes flipped off.
        assert!(!a.lsp_completion_mode_enabled_for(id));
        assert!(!a.lsp_diagnostics_mode_enabled_for(id));
        assert!(!a.lsp_hover_mode_enabled_for(id));
        assert!(!a.lsp_signature_mode_enabled_for(id));
        assert!(!a.lsp_format_mode_enabled_for(id));
        assert!(!a.lsp_rename_mode_enabled_for(id));
        assert!(!a.lsp_symbols_mode_enabled_for(id));
        assert!(!a.lsp_code_action_mode_enabled_for(id));
        assert!(!a.lsp_nav_mode_enabled_for(id));
    }

    #[test]
    fn user_disabling_a_sub_mode_after_cascade_keeps_others_active() {
        // M.6.1 contract: cascade-on activates everything; user
        // can then independently disable one sub-mode and the
        // others stay on. This is the "disable LSP completion
        // but keep diagnostics" use case from §4.2.1.
        let mut a = app_with("hi", 5);
        let id = a.pane_tree.active().buffer_id;
        a.toggle_mode_by_name("lsp-mode");
        // Independently disable `lsp-completion-mode`.
        a.toggle_mode_by_name("lsp-completion-mode");
        assert!(!a.lsp_completion_mode_enabled_for(id));
        // Other sub-modes still active.
        assert!(a.lsp_diagnostics_mode_enabled_for(id));
        assert!(a.lsp_hover_mode_enabled_for(id));
        assert!(a.lsp_format_mode_enabled_for(id));
        // Umbrella still active.
        assert!(a.lsp_mode_enabled_for(id));
    }

    #[test]
    fn re_activating_lsp_mode_after_user_disable_re_cascades_sub_modes_on() {
        // Edge case: user toggles lsp-mode off, then on again.
        // The cascade-on should re-activate every sub-mode,
        // including any the user had previously disabled.
        // (Toggling the umbrella is the user's "reset to defaults"
        // gesture.)
        let mut a = app_with("hi", 5);
        let id = a.pane_tree.active().buffer_id;
        a.toggle_mode_by_name("lsp-mode");
        a.toggle_mode_by_name("lsp-hover-mode");
        assert!(!a.lsp_hover_mode_enabled_for(id));
        // Cycle the umbrella: cascade-off then cascade-on.
        a.toggle_mode_by_name("lsp-mode");
        a.toggle_mode_by_name("lsp-mode");
        assert!(a.lsp_hover_mode_enabled_for(id));
    }

    #[test]
    fn lsp_sub_modes_default_off_and_independently_toggleable() {
        // M.6.0: each sub-mode accessor returns false on a fresh
        // buffer; toggling each by name flips only that sub-mode.
        // (M.6.1 will add capability-driven cascade from
        // `:lsp-mode` activation -- this test pins the manual-
        // toggle pathway, which is the v1 escape hatch when a
        // user wants a sub-mode active independently of the
        // umbrella's auto-activation logic.)
        let mut a = app_with("hi", 5);
        let id = a.pane_tree.active().buffer_id;
        // All nine sub-modes start off.
        assert!(!a.lsp_completion_mode_enabled_for(id));
        assert!(!a.lsp_diagnostics_mode_enabled_for(id));
        assert!(!a.lsp_hover_mode_enabled_for(id));
        assert!(!a.lsp_signature_mode_enabled_for(id));
        assert!(!a.lsp_format_mode_enabled_for(id));
        assert!(!a.lsp_rename_mode_enabled_for(id));
        assert!(!a.lsp_symbols_mode_enabled_for(id));
        assert!(!a.lsp_code_action_mode_enabled_for(id));
        assert!(!a.lsp_nav_mode_enabled_for(id));
        // Toggling `lsp-hover-mode` flips its accessor but no
        // other sub-mode's accessor.
        a.toggle_mode_by_name("lsp-hover-mode");
        assert!(a.lsp_hover_mode_enabled_for(id));
        assert!(!a.lsp_completion_mode_enabled_for(id));
        assert!(!a.lsp_diagnostics_mode_enabled_for(id));
        assert!(!a.lsp_format_mode_enabled_for(id));
        // Round-trip back off.
        a.toggle_mode_by_name("lsp-hover-mode");
        assert!(!a.lsp_hover_mode_enabled_for(id));
    }

    #[test]
    fn toggle_unknown_mode_name_emits_error_echo() {
        // Unknown name → error message; no state change.
        let mut a = app_with("hi", 5);
        let id = a.pane_tree.active().buffer_id;
        let before_minors_len = a
            .active_modes
            .get(&id)
            .map(|m| m.minors().len())
            .unwrap_or(0);
        a.toggle_mode_by_name("definitely-not-a-mode");
        let msg = a.last_message.as_ref().expect("error echo");
        assert!(msg.text.contains("not a registered mode"));
        let after_minors_len = a
            .active_modes
            .get(&id)
            .map(|m| m.minors().len())
            .unwrap_or(0);
        assert_eq!(before_minors_len, after_minors_len);
    }

    #[test]
    fn toggle_major_mode_by_name_swaps_active_major() {
        // Major-mode toggle = swap. The buffer starts on the
        // resolver's pick (text-mode for plain content); flipping
        // to `markdown-mode` deactivates text-mode and activates
        // markdown-mode. Active minors stay untouched.
        let mut a = app_with("# heading", 5);
        let id = a.pane_tree.active().buffer_id;
        // Activate a minor first so we can verify it survives.
        a.toggle_mode_by_name("lsp-mode");
        assert!(a.lsp_mode_enabled_for(id));
        // Swap major.
        a.toggle_mode_by_name("markdown-mode");
        let modes = a.active_modes.get(&id).expect("modes for buffer");
        assert_eq!(modes.major(), Some(lattice_syntax::MarkdownMode::mode_id()));
        // Minor unaffected by major swap (M.5 design).
        assert!(modes.has_minor(lattice_lsp::modes::LspMode::mode_id()));
    }

    #[test]
    fn lsp_mode_auto_activates_on_language_with_configured_server() {
        // M.5.2: opening a buffer whose path matches a configured
        // server's `file_patterns` auto-activates `lsp-mode`. The
        // bundled rust-analyzer config matches `*.rs`.
        use crate::app::test_helpers::app_with_path;
        let a = app_with_path("fn main() {}", 5, std::path::PathBuf::from("foo.rs"));
        let id = a.pane_tree.active().buffer_id;
        assert!(
            a.lsp_mode_enabled_for(id),
            "lsp-mode should auto-activate on a *.rs buffer with rust-analyzer configured"
        );
    }

    #[test]
    fn lsp_mode_does_not_auto_activate_for_unconfigured_extensions() {
        // No bundled server matches `*.unknown_ext`, so lsp-mode
        // should stay inactive even after the major activates.
        use crate::app::test_helpers::app_with_path;
        let a = app_with_path(
            "plain text",
            5,
            std::path::PathBuf::from("notes.unknown_ext"),
        );
        let id = a.pane_tree.active().buffer_id;
        assert!(
            !a.lsp_mode_enabled_for(id),
            "lsp-mode shouldn't auto-activate when no server config matches"
        );
    }

    #[test]
    fn lsp_mode_does_not_auto_activate_for_pathless_scratch_buffers() {
        // Scratch buffers without a path don't get auto-activation
        // (path-driven check). Standalone-server use cases require
        // explicit `:lsp-mode`.
        let a = app_with("fn main() {}", 5);
        let id = a.pane_tree.active().buffer_id;
        assert!(!a.lsp_mode_enabled_for(id));
    }

    #[test]
    fn deactivating_lsp_mode_emits_lsp_buffer_detached_event() {
        // M.5.3: deactivating `lsp-mode` publishes
        // `LspBufferDetached` on the editor bus. M.5.3.b moved
        // the event from the central enum to `lattice-lsp`,
        // delivered via the typed-bus path
        // (`subscribe_typed::<LspBufferDetached>`).
        // Subscribers (statusline, future telemetry) see the
        // gate flip without polling.
        use crate::app::test_helpers::app_with_path;
        let mut a = app_with_path("fn main() {}", 5, std::path::PathBuf::from("foo.rs"));
        let id = a.pane_tree.active().buffer_id;
        // Subscribe AFTER auto-activation so the channel only
        // captures the deactivate path.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<lattice_lsp::LspBufferDetached>();
        a.event_bus.subscribe_typed(tx);
        assert!(a.lsp_mode_enabled_for(id));
        a.toggle_mode_by_name("lsp-mode");
        assert!(!a.lsp_mode_enabled_for(id));
        let received = rx.try_recv();
        assert!(
            received.is_ok(),
            "expected LspBufferDetached on bus after `:lsp-mode` toggle off"
        );
    }

    #[test]
    fn deactivating_lsp_mode_clears_buffer_uri_mapping() {
        // Follow-up to Phase 3: the wire-level `didClose` +
        // `buffer_uris` cleanup runs from the
        // `LspBufferDetached` drain, not from the mode-
        // activation path itself. Test mirrors the runtime
        // tick: toggle the mode (publishes the event), then
        // call the drain (consumes the event + calls
        // `lsp_close_buffer`).
        use crate::app::test_helpers::app_with_path;
        let mut a = app_with_path("fn main() {}", 5, std::path::PathBuf::from("foo.rs"));
        let id = a.pane_tree.active().buffer_id;
        // The publish_document_opened path inserts the URI
        // mapping at App::new time.
        assert!(a.buffer_uri(id).is_some());
        a.toggle_mode_by_name("lsp-mode");
        a.drain_lsp_detach_events();
        assert!(
            a.buffer_uri(id).is_none(),
            "lsp-mode deactivate should clear buffer_uris[id] after detach drain"
        );
    }

    #[test]
    fn lsp_mode_survives_major_swap() {
        // M.5 design: major swaps don't touch active minors. Open
        // a rust file (auto-activates lsp-mode), swap to text-mode,
        // lsp-mode stays active. User runs `:lsp-mode` to flip off.
        use crate::app::test_helpers::app_with_path;
        let mut a = app_with_path("fn main() {}", 5, std::path::PathBuf::from("foo.rs"));
        let id = a.pane_tree.active().buffer_id;
        assert!(a.lsp_mode_enabled_for(id));
        a.toggle_mode_by_name("text-mode");
        assert!(
            a.lsp_mode_enabled_for(id),
            "lsp-mode should survive major-mode swap (M.5 design)"
        );
    }

    #[test]
    fn describe_events_renders_catalogue_grouped_by_source_crate() {
        // M.5.3.c: `:describe-events` walks `EVENT_DESCRIPTORS`
        // (the linkme distributed slice every `register_event!`
        // pushes into) and renders a help buffer. Every event
        // in the registry should appear in the rendered body.
        let mut a = app_with("hi", 5);
        a.do_describe_events();
        let h = a
            .popup_help()
            .expect(":describe-events should open a help popup");
        let body = h.content.as_string();
        // The three M.5.3.b LSP events should be listed.
        assert!(
            body.contains("lsp.buffer-attached"),
            "describe-events body missing `lsp.buffer-attached`; got:\n{body}"
        );
        assert!(
            body.contains("lsp.buffer-detached"),
            "describe-events body missing `lsp.buffer-detached`; got:\n{body}"
        );
        assert!(
            body.contains("lsp.log-pushed"),
            "describe-events body missing `lsp.log-pushed`; got:\n{body}"
        );
        // Source crate header for grouped section.
        assert!(
            body.contains("lattice-lsp"),
            "describe-events body should group by source crate; got:\n{body}"
        );
    }

    #[test]
    fn describe_event_renders_single_descriptor() {
        let mut a = app_with("hi", 5);
        a.do_describe_event("lsp.buffer-attached");
        let h = a
            .popup_help()
            .expect(":describe-event should open a help popup");
        let body = h.content.as_string();
        assert!(body.contains("lsp.buffer-attached"));
        assert!(body.contains("lattice-lsp"));
    }

    #[test]
    fn describe_event_unknown_name_emits_error_echo() {
        let mut a = app_with("hi", 5);
        a.do_describe_event("definitely-not-an-event");
        let msg = a.last_message.as_ref().expect("error echo");
        assert!(msg.text.contains("no event named"));
        assert!(a.popup_buffer.is_none());
    }

    #[test]
    fn toggle_command_resolves_through_ex_command_registry() {
        // M.5.1: the `:<mode-name>` ex-command auto-registered at
        // boot drives the same toggle. End-to-end through
        // `apply(Action::ExecuteEx(...))` would be the full flow;
        // here we verify the registry entry exists with the
        // expected name (the apply fn is exercised by the tests
        // above via direct `toggle_mode_by_name`).
        let a = app_with("hi", 5);
        // Every registered mode should have a corresponding
        // ex-command keyword in the registry.
        for (mode_id, _kind) in a.mode_registry.iter_meta() {
            let name = mode_id.to_string();
            assert!(
                a.registry.id_by_name(&name).is_some(),
                "no ex-command registered for mode `{name}`"
            );
        }
    }
}
