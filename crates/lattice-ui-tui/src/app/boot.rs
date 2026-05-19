//! Boot / config-load / sync paths the runtime calls before the
//! main loop starts -- the App's once-per-launch infrastructure.
//!
//! Methods that live here:
//! - `App::new` (the once-per-launch constructor). Phase 5.7.B.1
//!   delegates the renderer-neutral boot to
//!   [`lattice_host::editor::Editor::boot`]; this method only
//!   builds the renderer wrapper and runs renderer-side
//!   post-boot wiring.
//! - `sync_keymap_overlays` (re-stack the popup / snippet
//!   minor-mode keymap layers in lockstep with overlay state).
//! - `sync_theme_from_config` (re-derive `App.theme`'s renderer-
//!   specific `Style` values from `ui.*` typed options).
//! - `load_persistent_config` (read user + project TOML and
//!   apply scalar overrides + bucket structural sub-tables).
//!
//! What does NOT live here: the option resolver itself
//! (`lattice-config`), the keymap registry
//! (`crate::keymap_registry`), the theme parser
//! (`crate::theme`). This module is the App's *boot wiring*
//! over those.

use lattice_core::Document;

use super::{App, BufferKind};

impl App {
    pub fn new(document: Document) -> Self {
        // Phase 5.7.B.1: the renderer-neutral construction body
        // moved to `lattice_host::editor::Editor::boot`. Both
        // the TUI peer (this `App`) and the future GPUI peer
        // call the same entry point; each wrapper supplies its
        // own renderer-specific caches afterwards.
        let editor = lattice_host::editor::Editor::boot(document);
        // Slice 3c.atomic.A: renderer-side clone of the editor's
        // RenderState cell, captured before Editor moves. Once
        // 3c.atomic flips the writer to an `EditorActorHandle`,
        // the assignment swaps to the actor handle's exposed Arc;
        // every renderer-side reader stays unchanged.
        let render_state = editor.render_state.clone();
        let mut app = Self {
            editor,
            render_state,
            pane_render_registry: crate::render::build_pane_render_registry(),
            theme: crate::theme::Theme::default(),
        };
        // Sync derived theme styles from the freshly-registered
        // ui.* options so the renderer's first frame uses the
        // configured colors / separator (rather than the static
        // Theme::default values).
        app.sync_theme_from_config();
        // Populate the hot-path option cache from canonical config
        // values. Subsequent updates flow through the
        // `Event::OptionChanged` cascade in
        // `apply_option_cascade`.
        app.rebuild_option_cache();
        // M.3.1: activate the resolved major mode for the
        // initial document buffer. `resolve_major_mode(kind,
        // lang)` picks the right major (text-mode for
        // Lang::Plain, rust-mode/python-mode/... for typed
        // languages). The activation populates
        // `active_modes[buffer]` and triggers the option-cache
        // recompute so `ResolvedOptions` reflects the major's
        // contributions (e.g. ReadOnly = true for Help).
        app.activate_major_for_buffer_kind(app.editor.document_buffer_id, BufferKind::Document);
        // Initial-document attach. Path-bearing buffers register
        // their URI eagerly (the URI is a deterministic
        // `uri_from_path`; LSP attach is async and doesn't gate
        // the mapping) and publish `Event::DocumentOpened` --
        // the attach driver wired in `Editor::boot` consumes it
        // and submits to the supervisor on the LSP runtime, off
        // the UI thread. Path-less scratch buffers publish
        // nothing (no LSP work to drive) and the `buffer_uris`
        // entry stays absent.
        app.publish_document_opened_for_active();
        // Slice B / B'.7: LSP subsystem creates its global
        // `*lsp*` Document buffer eagerly at boot so `:b *lsp*`
        // works before any record has flowed. Per-instance
        // buffers (`*lsp:<server>:<workspace>*`,
        // `*lsp:<server>:<workspace>:trace*`) are created lazily
        // by the ex-command handlers (or by `:lsp-trace` toggle-
        // on) through the same generic
        // `ensure_named_synthetic_document` entry point. The
        // name + mode-id come from `lattice-lsp`; the host adds
        // no subsystem-specific create logic.
        app.ensure_named_synthetic_document(
            lattice_lsp::LSP_SUBSYSTEM_LOG_NAME,
            lattice_lsp::modes::LspLogMode::mode_id(),
            crate::app::App::SYNTHETIC_BUFFER_FLAGS,
        );
        // Slice E: `*messages*` follows the same pattern -- a
        // Document buffer in the registry, read-only, owner-
        // streamed via the MessagePushed event drain. Eager at
        // boot so `:b *messages*` works from t=0.
        app.ensure_messages_buffer();
        app
    }

    /// Re-stack the Insert-mode minor-mode overlays
    /// (completion popup + active snippet) so the layered
    /// keymap registry mirrors the App's overlay state. Called
    /// from the apply loop after every `Action`; cheap when
    /// nothing changed (single mutex acquisition + early
    /// return).
    ///
    /// Push order is enforced here so popup always sits at the
    /// top of the stack when both overlays are active: the
    /// method pops everything, then pushes snippet (if active),
    /// then popup (if active). Popup's `LayerId` is therefore
    /// always higher than snippet's, and popup wins on
    /// overlapping chords (preserving the legacy "popup
    /// precedes snippet" gating in `input::translate`).
    ///
    /// Slice 8.f.
    pub fn sync_keymap_overlays(&mut self) {
        let want_popup = self.editor.insert_completion.is_some();
        let want_snippet = self.editor.active_snippet.is_some();
        let have_popup = self.editor.completion_popup_layer.is_some();
        let have_snippet = self.editor.snippet_layer.is_some();
        // CSM.K1: `completion-popup-mode` minor reflects popup
        // state (formerly `completion-mode` in CSM.2 -- renamed
        // for the two-mode split where `completion-mode` is now
        // the persistent buffer-participation gate). The
        // keymap-overlay push / pop is the same diff applied to
        // the keymap-registry side; reconcile both here so the
        // two stay in lockstep.
        self.sync_completion_popup_mode_activation(want_popup);
        if want_popup == have_popup && want_snippet == have_snippet {
            return;
        }
        // Re-stack: pop everything, then push in the canonical
        // order (snippet first, popup second).
        if let Some(id) = self.editor.completion_popup_layer.take() {
            self.editor.keymap.pop_layer(id);
        }
        if let Some(id) = self.editor.snippet_layer.take() {
            self.editor.keymap.pop_layer(id);
        }
        if want_snippet {
            let id = self.editor.keymap.push_layer(
                crate::keymap_registry::PushLayerKind::MinorMode,
                "active-snippet",
                crate::keymap_insert::active_snippet_layer_bindings(&self.editor.action_ids),
            );
            self.editor.snippet_layer = Some(id);
        }
        if want_popup {
            let id = self.editor.keymap.push_layer(
                crate::keymap_registry::PushLayerKind::MinorMode,
                "completion-popup",
                crate::keymap_insert::completion_popup_layer_bindings(&self.editor.action_ids),
            );
            self.editor.completion_popup_layer = Some(id);
        }
    }

    /// CSM.K1: bring `completion-popup-mode`'s activation state
    /// on the active document buffer in line with `want_popup`.
    /// Called from `sync_keymap_overlays` so the transient
    /// popup-mode tracks the popup open / close transitions
    /// without each `self.editor.insert_completion = ...` site having
    /// to know about it.
    ///
    /// Per-buffer scope: the popup belongs to the document the
    /// user is typing in. v1 has a single document buffer
    /// (`self.editor.document_buffer_id`); multi-document support
    /// activates this mode on whichever doc owns the popup at
    /// open time when that lands. Deactivation is symmetric.
    fn sync_completion_popup_mode_activation(&mut self, want_popup: bool) {
        let buffer_id = self.editor.document_buffer_id;
        let proto_id = lattice_protocol::ids::BufferId::new(buffer_id.0 as u64);
        let mode_id = lattice_mode::CompletionPopupMode::mode_id();
        let mut active = self
            .editor
            .active_modes
            .remove(&buffer_id)
            .unwrap_or_default();
        let currently = active.has_minor(mode_id);
        if want_popup && !currently {
            let _ = self.editor.mode_registry.activate_minor(
                &mut active,
                &self.editor.mode_guards,
                &self.editor.config,
                &self.editor.event_bus,
                &self.editor.services,
                proto_id,
                mode_id,
                lattice_mode::CapabilitySet::empty(),
            );
        } else if !want_popup && currently {
            let _ = self.editor.mode_registry.deactivate_minor(
                &mut active,
                &self.editor.mode_guards,
                &self.editor.event_bus,
                proto_id,
                mode_id,
            );
        }
        self.editor.active_modes.insert(buffer_id, active);
        // CSM.3: a transition into / out of completion-popup-mode
        // is a mode-set change for the buffer -- recompute the
        // active-sources cache so the engine reads a coherent
        // snapshot. (completion-popup-mode itself doesn't
        // contribute sources; the recompute walks all active
        // modes, so future source-contributing minors that
        // toggle alongside still get picked up.)
        self.recompute_active_completion_sources_for(buffer_id);
    }

    /// Re-derive `App.theme`'s renderer-specific `Style` values
    /// from the current `ui.*` option values in the config. Called
    /// at App-init time (after registration) and on every `:set
    /// ui.*` so the cached theme stays in lockstep with the
    /// canonical primitives in config.
    pub fn sync_theme_from_config(&mut self) {
        // Phase 5.5.E.6: the renderer-neutral half (read typed
        // options + write `editor.host_theme`) lives on the host
        // as `Editor::sync_host_theme_from_config`. Splitting the
        // function lets the option-cascade in `Editor::apply_option_cascade`
        // run the host half directly and emit `RendererSignal::ThemeChanged`;
        // the renderer (here) only owns the cached TUI-typed
        // mirror rebuild.
        self.editor.sync_host_theme_from_config();
        self.rebuild_tui_theme();
    }

    /// Rebuild the cached TUI-typed [`crate::theme::Theme`] from
    /// the renderer-neutral [`lattice_host::ui::theme::Theme`].
    /// Cheap (every field is `Copy`); the rebuild fires only on
    /// option cascade or on a host-emitted
    /// [`lattice_host::dispatch::RendererSignal::ThemeChanged`],
    /// never per frame. A future GPUI renderer implements an
    /// equivalent `rebuild_gpui_theme` on its own `App`.
    pub fn rebuild_tui_theme(&mut self) {
        self.theme = crate::theme::Theme::from(&self.editor.host_theme);
    }

    /// Load `~/.editor.config/lattice/lattice.toml` (user) and
    /// `<workspace_root>/.lattice/config.toml` (project) in
    /// precedence order, applying scalar overrides to
    /// `self.editor.config` and bucketing structural sub-tables (per-
    /// language overrides, plugin sections) into
    /// `self.editor.pending_config_structural_sections` for their
    /// owners to drain.
    ///
    /// Called once by the runtime startup before the main loop
    /// (so the first frame already reflects user overrides).
    /// NOT called from `App::new` -- tests stay isolated from
    /// the user's real `~/.editor.config/lattice/`. Test fixtures that
    /// want to exercise the load path can call this directly
    /// with a synthesized workspace root.
    ///
    /// Loader diagnostics (parse errors, unknown keys,
    /// validation rejects) collapse into a single echo at the
    /// most-severe level: `Error` if any file failed to
    /// parse / read, `Warn` if any key was rejected, otherwise
    /// silent. Per-file `path:body` detail rides the message
    /// body so the user can see *which* file complained.
    pub fn load_persistent_config(&mut self, workspace_root: Option<&std::path::Path>) {
        // Phase 5.8.AA.u: body migrated to
        // `lattice_host::dispatch::Editor::load_persistent_config`.
        // The returned `Vec<RendererSignal>` carries the
        // `ThemeChanged` so the renderer-side typed theme cache
        // rebuilds; we fan it through the App's standard signal
        // handler (which calls `rebuild_tui_theme`).
        let signals = self.editor.load_persistent_config(workspace_root);
        for s in signals {
            self.handle_renderer_signal(s);
        }
    }
}
