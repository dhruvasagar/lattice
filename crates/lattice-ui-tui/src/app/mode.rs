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
    pub fn activate_major_for_buffer_kind(
        &mut self,
        buffer_id: BufferId,
        kind: BufferKind,
    ) {
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
        // Convert App-level BufferId to lattice_protocol::BufferId for
        // the registry's expectation. The registry only uses the
        // value for event emission; for M.3.1 we synthesise a
        // dummy value because mode-event subscribers don't use
        // it yet.
        let proto_id = lattice_protocol::ids::BufferId::new(buffer_id.0 as u64);
        let mut active = self
            .active_modes
            .remove(&buffer_id)
            .unwrap_or_default();
        let mut locals = self
            .buffer_locals
            .remove(&buffer_id)
            .unwrap_or_default();
        match self.mode_registry.activate_major(
            &mut active,
            &mut locals,
            proto_id,
            major_id,
            // Capability set: M.3.1 doesn't yet plumb per-buffer
            // capabilities, so pass empty. Modes that require
            // BUFFER_URI / LSP / etc. (M.5+) will get this from
            // a real capability lookup.
            lattice_mode::CapabilitySet::empty(),
        ) {
            Ok(_events) => {
                // Events go to the typed event bus when M.4
                // wires it; ignore for now.
            }
            Err(e) => {
                // Don't fail startup; surface as an echo and
                // continue with defaults. The buffer just has
                // no active major; resolved options reflect
                // registry defaults.
                self.set_message(
                    EchoLevel::Warn,
                    format!(
                        "mode: activate_major({}) for buffer {} failed: {}",
                        major_id, buffer_id.0, e,
                    ),
                );
            }
        }
        // M.4 (Option B): per-kind default minor (help-mode for
        // Help kinds today). Activated AFTER the major so it
        // layers correctly in the resolver's priority stack.
        if let Some(minor_id) = crate::modes::default_minor_mode_id_for_buffer_kind(kind) {
            if let Err(e) = self.mode_registry.activate_minor(
                &mut active,
                &mut locals,
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
        self.buffer_locals.insert(buffer_id, locals);
        self.recompute_options_for_buffer(buffer_id);
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
        self.buffers
            .document(buffer_id)
            .and_then(|entry| entry.handle.path().map(|p| p.to_path_buf()))
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
        let mut locals = self.buffer_locals.remove(&buffer_id).unwrap_or_default();
        let result = match kind {
            ModeKind::Major => self.mode_registry.activate_major(
                &mut active,
                &mut locals,
                proto_id,
                mode_id,
                CapabilitySet::empty(),
            ),
            ModeKind::Minor => self.mode_registry.activate_minor(
                &mut active,
                &mut locals,
                proto_id,
                mode_id,
                CapabilitySet::empty(),
            ),
        };
        if let Err(e) = result {
            self.set_message(
                EchoLevel::Warn,
                format!("mode: activate({mode_id}) for buffer {} failed: {e}", buffer_id.0),
            );
        }
        self.active_modes.insert(buffer_id, active);
        self.buffer_locals.insert(buffer_id, locals);
        self.recompute_options_for_buffer(buffer_id);
        // M.5.2: when a major activates (whether by direct call,
        // `:<major-name>` toggle, or buffer-creation path), run
        // the LSP auto-activation hook. Skipped for minor
        // activations -- if `lsp-mode` is the one being
        // activated, the hook would just no-op (already-active
        // short-circuit).
        if matches!(kind, ModeKind::Major) {
            self.maybe_auto_activate_lsp_mode(buffer_id);
        }
        // M.5.3: lsp-mode activate side-effect -- emit
        // `LspBufferAttached` for subscribers (statusline,
        // diagnostics renderer, future plugin hooks). The actual
        // wire-level didOpen happens via the existing
        // `attach_driver` listening on `Event::DocumentOpened`,
        // which fires from the file-open path.
        if mode_id == lattice_lsp::modes::LspMode::mode_id() {
            self.on_lsp_mode_activated(buffer_id);
        }
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
        let mut locals = self.buffer_locals.remove(&buffer_id).unwrap_or_default();
        let result = match mode.kind() {
            ModeKind::Major => self.mode_registry.deactivate_major(
                &mut active,
                &mut locals,
                proto_id,
            ),
            ModeKind::Minor => self.mode_registry.deactivate_minor(
                &mut active,
                &mut locals,
                proto_id,
                mode_id,
            ),
        };
        if let Err(e) = result {
            self.set_message(
                EchoLevel::Warn,
                format!("mode: deactivate({mode_id}) for buffer {} failed: {e}", buffer_id.0),
            );
        }
        self.active_modes.insert(buffer_id, active);
        self.buffer_locals.insert(buffer_id, locals);
        self.recompute_options_for_buffer(buffer_id);
        // M.5.3: lsp-mode deactivate side-effect -- send didClose
        // to attached servers (`lsp_close_buffer` already does
        // this for the bdelete path) and emit
        // `LspBufferDetached`. Server connection persists if
        // other buffers are still attached.
        if mode_id == lattice_lsp::modes::LspMode::mode_id() {
            self.on_lsp_mode_deactivated(buffer_id);
        }
    }

    /// M.5.3: lsp-mode activated on `buffer_id`. Emits
    /// `LspBufferAttached` on the editor's event bus so
    /// subscribers see the gate flip. Wire-level `didOpen` is
    /// already driven by the `attach_driver` subscribing to
    /// `Event::DocumentOpened` from the file-open path.
    fn on_lsp_mode_activated(&mut self, buffer_id: BufferId) {
        let path = self.path_for_buffer(buffer_id);
        self.event_bus.publish(Event::LspBufferAttached {
            id: lattice_protocol::ids::DocumentId::new(buffer_id.0 as u64),
            path,
        });
    }

    /// M.5.3: lsp-mode deactivated on `buffer_id`. Sends
    /// `textDocument/didClose` per attached server (the LSP wire
    /// mechanism for "stop tracking this URI") and emits
    /// `LspBufferDetached`. Mirrors nvim's
    /// `vim.lsp.buf_detach_client` and emacs `lsp-mode`'s
    /// disable path. The buffer stays open in the editor; only
    /// LSP tracking ends. Server connection persists if other
    /// buffers are still attached.
    fn on_lsp_mode_deactivated(&mut self, buffer_id: BufferId) {
        let path = self.path_for_buffer(buffer_id);
        self.lsp_close_buffer(buffer_id);
        self.event_bus.publish(Event::LspBufferDetached {
            id: lattice_protocol::ids::DocumentId::new(buffer_id.0 as u64),
            path,
        });
    }

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
        assert_eq!(
            modes.major(),
            Some(lattice_syntax::MarkdownMode::mode_id())
        );
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
        let a = app_with_path("plain text", 5, std::path::PathBuf::from("notes.unknown_ext"));
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
        // `Event::LspBufferDetached` on the editor bus.
        // Subscribers (statusline, future telemetry) see the
        // gate flip without polling.
        use crate::app::test_helpers::{app_with_path, subscribe_all_events};
        let mut a = app_with_path("fn main() {}", 5, std::path::PathBuf::from("foo.rs"));
        let id = a.pane_tree.active().buffer_id;
        // Subscribe AFTER auto-activation so the channel only
        // captures the deactivate path.
        let mut rx = subscribe_all_events(&a);
        assert!(a.lsp_mode_enabled_for(id));
        a.toggle_mode_by_name("lsp-mode");
        assert!(!a.lsp_mode_enabled_for(id));
        // Drain the receiver synchronously and look for our
        // event. (The bus is sync; events are queued
        // immediately on publish.)
        let mut found_detached = false;
        while let Ok(ev) = rx.try_recv() {
            if matches!(ev, lattice_protocol::Event::LspBufferDetached { .. }) {
                found_detached = true;
                break;
            }
        }
        assert!(
            found_detached,
            "expected Event::LspBufferDetached on bus after `:lsp-mode` toggle off"
        );
    }

    #[test]
    fn deactivating_lsp_mode_clears_buffer_uri_mapping() {
        // M.5.3: the deactivate path runs through `lsp_close_buffer`
        // which also clears `App::buffer_uris` for that id (so
        // future requests don't leak the URI). Verifies the
        // detach side-effect on App state, not just the event.
        use crate::app::test_helpers::app_with_path;
        let mut a = app_with_path("fn main() {}", 5, std::path::PathBuf::from("foo.rs"));
        let id = a.pane_tree.active().buffer_id;
        // The publish_document_opened path inserts the URI
        // mapping at App::new time.
        assert!(a.buffer_uri(id).is_some());
        a.toggle_mode_by_name("lsp-mode");
        assert!(
            a.buffer_uri(id).is_none(),
            "lsp-mode deactivate should clear buffer_uris[id]"
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
