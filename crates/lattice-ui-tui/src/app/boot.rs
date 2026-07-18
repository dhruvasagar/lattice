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
        // DB.5 (design.md §9.1): capture the opened-file path BEFORE
        // `document` moves into `Editor::boot` (which consumes it) — this
        // is the only point where the renderer still owns the `Document`.
        let opened_file = document.path().map(|p| p.to_path_buf());
        // Phase 5.7.B.1: the renderer-neutral construction body
        // moved to `lattice_host::editor::Editor::boot`.
        let mut editor = lattice_host::editor::Editor::boot(document);
        // CB.2 (docs/dev/architecture/clipboard.md): override the
        // FakeClipboard CB.0 registers by default with the TUI's real
        // backend (native arboard when available, OSC52 write-only
        // fallback otherwise). Must run before anything else can have
        // cloned `editor.services` -- `Editor::boot` hands back a fresh
        // Arc (built via `BootContext`'s owned, non-Arc `ServiceRegistry`
        // during boot, wrapped only at the very end), so `Arc::get_mut`
        // is guaranteed to succeed here.
        if let Some(services) = std::sync::Arc::get_mut(&mut editor.services) {
            let clipboard: lattice_core::ClipboardHandle =
                std::sync::Arc::new(crate::clipboard::TuiClipboard::detect());
            services.register(clipboard);
        } else {
            debug_assert!(
                false,
                "editor.services should be uniquely owned immediately after Editor::boot"
            );
        }
        // DB.5 (test isolation): unit tests construct `App::new` with a
        // pathless `Document::from_text`, which looks exactly like a
        // real no-file launch — so the startup trigger below would
        // auto-open `*dashboard*` and every render test would see the
        // dashboard buffer instead of its own text. Disable the auto-open
        // BEFORE the `Startup` publish: the trigger's async task reads
        // `dashboard.enabled` only AFTER it receives `Startup`, and its
        // `recv()` cannot complete before `publish_typed` sends — so
        // setting it here is race-free (any later `set` would race the
        // background task). Production launch (`main`) never takes this
        // branch, so the real no-file→dashboard behavior is untouched.
        #[cfg(test)]
        {
            let _ = editor
                .config
                .parse_and_set_command("dashboard.enabled=false");
        }
        // DB.5: publish `Startup` once `editor` exists (right after `boot`
        // returns), so `lattice_dashboard::install`'s subscription (wired
        // during `boot`, Phase-B) can decide whether to auto-open
        // `*dashboard*`. One publish per boot, TUI + GPUI both wire this at
        // their own post-boot seam; the subscription itself lives once in
        // `lattice-dashboard`.
        editor
            .event_bus
            .publish_typed(lattice_mode::Startup { opened_file });
        // Slice 3c.atomic.A: renderer-side clone of the editor's
        // RenderState cell, captured before Editor moves to the
        // actor thread.
        let render_state = editor.render_state.clone();
        // Perf plan B.2 slice B.2.a: clone the overlay worker's
        // output cell. display-line B4.2: the dead span/row prepaint
        // cell clones were deleted with the worker's span/row cache.
        let syntax_static_overlay_quads_cell = editor.syntax_static_overlay_quads_cell.clone();
        // Slice 3c.final.E.swap: run boot-time setup directly on
        // the owned Editor BEFORE handing it to the actor. Every
        // call below resolves to a host-side method; the App-side
        // wrappers that previously routed through `mutate_editor`
        // would all funnel through the actor's mailbox, which
        // doesn't exist yet at this point in construction.
        editor.sync_host_theme_from_config();
        editor.rebuild_option_cache();
        let doc_buf = editor.document_buffer_id;
        let _ = editor.activate_major_for_buffer_kind(doc_buf, BufferKind::Document);
        editor.publish_document_opened_for_active();
        editor.ensure_named_synthetic_document(
            lattice_lsp::LSP_SUBSYSTEM_LOG_NAME,
            lattice_lsp::modes::LspLogMode::mode_id(),
            crate::app::App::SYNTHETIC_BUFFER_FLAGS,
        );
        editor.ensure_messages_buffer();
        // Slice 3c.atomic.B: initial RS publish so `app.ad()`
        // returns boot-time editor state, not the Default.
        editor.publish_render_state();

        // Slice 3c.final.E.swap: hand Editor to the actor thread
        // (prod) or keep it inline (test). The cfg-gate is the
        // architectural split — production code can only reach
        // Editor through the actor handle's blocking RPCs, while
        // test code retains direct field access for fixtures that
        // mutate state without going through the dispatch path.
        #[cfg(not(test))]
        let editor_field = lattice_host::editor_actor::spawn_editor_actor(editor);
        #[cfg(test)]
        let editor_field = editor;

        let mut app = Self {
            #[cfg(not(test))]
            editor_actor: editor_field,
            #[cfg(test)]
            editor: editor_field,
            render_state,
            frame_ad: std::sync::Mutex::new(None),
            syntax_static_overlay_quads_cell,
            pane_render_registry: crate::render::build_pane_render_registry(),
            theme: crate::theme::Theme::default(),
        };
        // App-side post-actor setup: rebuild the cached TUI theme
        // from the freshly-published `render_state.theme`. Reads
        // the published RS (already primed above), no editor borrow.
        app.rebuild_tui_theme();
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
    /// the renderer-neutral [`lattice_host::ui::theme::Theme`] **plus**
    /// the resolved theme table (T.4). Cheap (every field is `Copy`);
    /// the rebuild fires only on option cascade or on a host-emitted
    /// [`lattice_host::dispatch::RendererSignal::ThemeChanged`],
    /// never per frame — so a `:colorscheme` / palette swap (which
    /// bumps `ResolvedTheme::version()` and emits `ThemeChanged`)
    /// recolors the cache without any per-frame style adaptation. A
    /// future GPUI renderer implements an equivalent
    /// `rebuild_gpui_theme` on its own `App`.
    pub fn rebuild_tui_theme(&mut self) {
        let rs = self.render_state.load();
        // T.6.t: non-style chrome (glyphs, separator chars, dim/
        // nerd-fonts flags) sources from the published typed-options
        // registry; style fields from the resolved table.
        self.theme =
            crate::theme::build_tui_theme(&rs.options.config, &rs.resolved_theme, &rs.theme_ids);
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

    /// Load built-in + user snippet packs into the registry at
    /// startup (built-ins 2026-06-13). Delegates to
    /// [`lattice_host::dispatch::Editor::load_snippets_at_startup`].
    /// Called by the runtime right after `load_persistent_config`
    /// so a fresh editor has its snippet set ready; quiet (logs, no
    /// echo). Kept out of `App::new` so test `App`s start with an
    /// empty registry.
    pub fn load_snippets_at_startup(&mut self) {
        self.mutate_editor(|e| e.load_snippets_at_startup());
    }

    /// T.5: open the tutor at lesson `lesson`. Called from
    /// `lattice-cli` when `--tutor [N]` is passed; fires after
    /// `load_persistent_config` so user config lands first.
    pub fn open_tutor(&mut self, lesson: u32) {
        let signals = self.mutate_editor_with(move |e| e.do_tutor(Some(lesson)));
        for s in signals {
            self.handle_renderer_signal(s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CB.2 regression guard: `App::new`'s `Arc::get_mut(&mut
    /// editor.services)` override must succeed. If a future boot change
    /// clones `editor.services` before this line runs, `Arc::get_mut`
    /// returns `None` and the `debug_assert!` in `App::new` fires --
    /// which would turn into a panic on EVERY `App::new`-constructed test
    /// across the whole TUI suite (App::new is the universal test
    /// fixture), not just this one. This test pins the invariant
    /// explicitly rather than relying on that incidental discovery.
    ///
    /// Behavioral assertion is gated to the default (no `system-clipboard`)
    /// build: without that feature, `TuiClipboard::detect()` always
    /// resolves to the OSC52 fallback regardless of the host machine's
    /// display, so the "write then read returns None" spot-check
    /// (distinguishing it from the CB.0 default `FakeClipboard`, which
    /// always roundtrips) is environment-independent. With the feature on,
    /// a real display would make `Native(ArboardClipboard)` roundtrip too
    /// -- a legitimate difference, not a regression -- so that combination
    /// only gets the panic-free assertion below.
    #[test]
    fn new_registers_tui_clipboard_without_panicking() {
        let app = App::new(Document::from_text("hello\n"));
        let clipboard = app
            .editor
            .services
            .get::<lattice_core::ClipboardHandle>()
            .expect("App::new must leave a ClipboardHandle registered");
        #[cfg(not(feature = "system-clipboard"))]
        {
            clipboard.write("cb2-boot-probe".to_string());
            assert_eq!(
                clipboard.read(),
                None,
                "without system-clipboard, TuiClipboard is always the OSC52 \
                 fallback, which never round-trips a write through read"
            );
        }
        // With `system-clipboard` on, `Native(ArboardClipboard)` would
        // round-trip on a real display, so there's no environment-independent
        // behavioral spot-check -- the `.expect()` above is the panic-free
        // presence assertion for this config.
        #[cfg(feature = "system-clipboard")]
        let _ = clipboard;
    }

    /// Agent boot-presence guard: `lattice_ai::install` runs in `Editor::boot`'s
    /// Phase-B list, so a freshly-booted `App` must have both opencode paths
    /// wired: `:opencode` (the primary ACP buffer conversation, which owns the
    /// `AiClientHandle` service) and `:opencode-term` (the terminal-TUI spawn,
    /// kept best-effort). Without them the commands would silently no-op at the
    /// parser layer. The `:ai-log` picker itself is a later task; this only pins
    /// that boot wiring landed.
    #[test]
    fn new_registers_ai_command_and_service_without_panicking() {
        let app = App::new(Document::from_text("hello\n"));
        assert!(
            app.editor.registry.id_by_name("opencode").is_some(),
            "App::new must register the `:opencode` (ACP conversation) ex-command"
        );
        assert!(
            app.editor.registry.id_by_name("opencode-term").is_some(),
            "App::new must register the `:opencode-term` (terminal) ex-command"
        );
        app.editor
            .services
            .get::<lattice_ai::AiClientHandle>()
            .expect("App::new must leave an AiClientHandle registered (ACP path)");
    }

    /// AI-1b T12b: `:ai-log` is registered at boot, and the host
    /// `do_open_ai_log` empty-session path (no `:opencode` run yet →
    /// `AiLogger::known_sessions()` empty) echoes a hint instead of
    /// panicking or opening a stray buffer. Exercises the
    /// `AiLogger`-service lookup + count branch host-side.
    #[test]
    fn ai_log_registered_and_empty_session_path_is_safe() {
        let mut app = App::new(Document::from_text("hello\n"));
        assert!(
            app.editor.registry.id_by_name("ai-log").is_some(),
            "App::new must register the `:ai-log` ex-command"
        );
        let before = app.editor.active_pane_buffer_id();
        // No session started, so known_sessions() is empty → the
        // 0-session arm echoes a hint and opens nothing.
        app.do_open_ai_log(None);
        assert_eq!(
            app.editor.active_pane_buffer_id(),
            before,
            "empty-session `:ai-log` must not switch buffers"
        );
    }

    /// `:ai-log <provider>` that matches nothing, while some *other*
    /// provider's session is live, must name what is running rather than
    /// telling the user to start an agent that already started. Mirrors
    /// `open_lsp_picker`'s "(running: ...)" listing.
    #[test]
    fn ai_log_unmatched_provider_lists_the_running_sessions() {
        let mut app = App::new(Document::from_text("hello\n"));
        let logger = app
            .editor
            .services
            .get::<lattice_ai::AiLogger>()
            .expect("App::new must leave an AiLogger registered");

        // A record under this key is what makes the session "known".
        let key = lattice_ai::SessionKey::new(std::sync::Arc::<str>::from("opencode"), 1);
        logger.log(
            Some(&key),
            lattice_ai::AiLogLevel::Info,
            lattice_ai::AiLogSource::Lifecycle,
            "session opened",
        );

        let before = app.editor.active_pane_buffer_id();
        app.do_open_ai_log(Some("bogus"));

        let message = app
            .editor
            .last_message
            .as_ref()
            .map(|m| m.text.clone())
            .unwrap_or_default();
        assert!(
            message.contains("opencode:1"),
            "unmatched provider must list the live sessions, got {message:?}"
        );
        assert!(
            !message.contains(":opencode`)"),
            "must not tell the user to start an agent that is already running, got {message:?}"
        );
        assert_eq!(
            app.editor.active_pane_buffer_id(),
            before,
            "an unmatched `:ai-log` must not switch buffers"
        );
    }
}
