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
        // Slice 3c.final.E.1: cache the worker's output cell on
        // App. Pre-swap: a direct clone of `editor.syntax_visible_spans_cell`.
        // Post-swap (when Editor moves to the actor): the same Arc
        // is exposed through the actor handle; the constructor line
        // is the only place that changes.
        let syntax_visible_spans_cell = editor.syntax_visible_spans_cell.clone();
        let mut app = Self {
            editor,
            render_state,
            syntax_visible_spans_cell,
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
        // Slice 3c.final.E.5j: doc-id via App accessor (slice E.5d
        // rule-of-three helper) instead of direct `editor` field.
        app.activate_major_for_buffer_kind(app.document_buffer_id(), BufferKind::Document);
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
        // Slice 3c.atomic.B: prime the renderer-owned
        // `render_state` cell with the boot-time editor state.
        // Without this initial publish, `app.ad()` returns a
        // `Default`-constructed `ActiveDocumentRenderState`
        // (zero cursor, `BufferId::default()` document id, etc.)
        // until the first dispatch fires -- which would surface
        // as the renderer reading from an empty document on the
        // first frame, and as `lsp_diagnostics_mode_enabled_for`
        // / `lsp_*_mode_enabled_for` checks looking up state on
        // the wrong buffer id in render-only test fixtures that
        // skip dispatch.
        // Slice 3c.final.E.5j: route the boot-time initial publish
        // through `mutate_editor` (its body publishes after the
        // closure runs, so an empty closure does the publish for us).
        app.mutate_editor(|_| {});
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
        // Slice 3c.final.E.5e: body hoisted to
        // [`lattice_host::dispatch::Editor::sync_keymap_overlays`].
        // Renderer delegates through `mutate_editor` so post-swap
        // the closure crosses the actor channel like every other
        // mutation.
        self.mutate_editor(|e| e.sync_keymap_overlays());
    }

    // Slice 3c.final.E.5e: `sync_completion_popup_mode_activation`
    // retired -- inlined into
    // [`lattice_host::dispatch::Editor::sync_keymap_overlays`]
    // alongside its sole caller. Original doc-comment preserved
    // for grep:
    //
    /// CSM.K1: bring `completion-popup-mode`'s activation state
    /// on the active document buffer in line with `want_popup`.
    /// Called from `sync_keymap_overlays` so the transient
    /// popup-mode tracks the popup open / close transitions
    /// without each `self.editor.insert_completion = ...` site having
    /// to know about it.
    ///
    /// Per-buffer scope: the popup belongs to the document the
    /// user is typing in. v1 has a single document buffer
    /// (`self.document_buffer_id()`); multi-document support
    /// activates this mode on whichever doc owns the popup at
    /// open time when that lands. Deactivation is symmetric.
    // (Body retired: inlined into the host-side method.)

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
        // Slice 3c.final.E.3: route through `mutate_editor`.
        self.mutate_editor(|e| e.sync_host_theme_from_config());
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
        self.theme = crate::theme::Theme::from(&self.render_state.load().theme);
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
        // Slice 3c.final.E.3: clone path for the `Send + 'static`
        // closure, then route through `mutate_editor_with`.
        let workspace_root = workspace_root.map(|p| p.to_path_buf());
        let signals =
            self.mutate_editor_with(move |e| e.load_persistent_config(workspace_root.as_deref()));
        for s in signals {
            self.handle_renderer_signal(s);
        }
    }
}
