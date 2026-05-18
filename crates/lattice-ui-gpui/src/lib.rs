//! Phase 5.7: GPUI peer renderer scaffold for lattice.
//!
//! Design anchor: `docs/dev/architecture/design.md` §5.6 (rendering
//! layered architecture) + `docs/dev/architecture/phase-5-extraction.md`
//! slice 5.7 + post-Option-E pivot notes (lines 348–349). The
//! original §5.6.1 `Renderer` trait (with `Frame` / `InputEvent` /
//! `LayoutConstraints` / `paint`) was dropped: ratatui's pull-based
//! draw loop and GPUI's retained-mode element tree are structurally
//! different, and forcing a shared `Frame` type buys nothing. The
//! current renderer-trait shape (`lattice_host::Renderer`) is the
//! composition-typed associated-types version that came out of 5.B.
//!
//! Per the design (§5.6.1) the **GPU UI is the primary v1 surface**;
//! the TUI is a first-class peer for headless / SSH / low-bandwidth
//! use. This crate is the GPU peer; both renderers share the host
//! substrate (`lattice-host`) and never depend on each other.
//!
//! ## Slicing
//!
//! This file ships the renderer-neutral scaffold types — they build
//! on every host (including headless CI and WSL2 without display
//! libs) and exercise the host-substrate decoupling claim:
//!
//! - [`GpuiTheme`] — stub theme cache. Mirrors the role of
//!   `lattice-ui-tui::theme::Theme` (cached pre-computed render
//!   primitives so per-frame reads are direct) but will hold GPUI
//!   native style primitives (`Hsla`, `TextStyle`) in 5.7.B+. Empty
//!   today so the scaffold has no transitive gpui-link requirement.
//!
//! - [`GpuiPaneRenderRegistry`] — stub registry implementing
//!   [`lattice_host::pane_render::ProviderLookup`]. The host's
//!   mode-walk ([`lattice_host::pane_render::resolve_pane_render_mode`])
//!   already routes through it; storage will fill with the GPUI-typed
//!   render fn signature in 5.8+.
//!
//! - [`GpuiRenderer`] — `impl lattice_host::Renderer` zero-sized
//!   marker that surfaces the renderer-specific associated types
//!   to the host's renderer-trait machinery.
//!
//! - [`GpuiApp`] — the renderer-side composition root. Mirrors
//!   `lattice-ui-tui::app::App` in shape: `{editor, theme,
//!   pane_render_registry}` (an `lsp_file_watcher` field joins when
//!   the LSP runtime adapter for GPUI lands in 5.8+).
//!
//! The window-opening entry point lives in the `lattice-gpui`
//! binary (`src/bin/lattice_gpui.rs`) behind the `window` Cargo
//! feature. That binary pulls real gpui at link time; the lib above
//! does not. This split keeps the scaffold's test target buildable
//! everywhere while the binary is opt-in for environments with
//! display libs installed.
//!
//! Phase 5.7.B.1 landed [`lattice_host::editor::Editor::boot`]:
//! the renderer-neutral construction body (LSP subsystem,
//! command registry, mode registry, snippet registry, event
//! bus, completion / picker / config registries, syntax +
//! buffer registry seeding) lives on the host substrate. Phase
//! 5.7.B.2 (this slice) wires [`GpuiApp::new`] to call it:
//! `GpuiApp::new(document)` mirrors `lattice-ui-tui::App::new`'s
//! shape — the renderer-neutral half goes through `Editor::boot`,
//! the renderer-side wrapper holds the theme + pane-render
//! registry. Real dispatch + paint wiring (key events ->
//! `Action` -> `editor.dispatch`; paint reads `editor.document`
//! snapshot + cursor) follows in 5.7.B.3 / 5.7.B.4.

use lattice_core::{BufferKind, Document};
use lattice_host::Renderer;
use lattice_host::action::Action;
use lattice_host::chord::KeyChord;
use lattice_host::dispatch::{DispatchOutcome, RendererSignal};
use lattice_host::editor::Editor;
use lattice_host::input::TranslateContext;
use lattice_host::pane_render::ProviderLookup;
use lattice_mode::ModeId;

pub mod gpui_chord;

/// GPUI window-opening entry point ([`run`], [`document_from_path`],
/// [`document_from_first_arg`]). Behind the `window` Cargo feature
/// so the lib's headless build doesn't link gpui. Lifted from the
/// `lattice-gpui` binary in Phase 5.9 so `lattice-cli --gpu` can
/// reuse the same entry without duplicating the window setup.
#[cfg(feature = "window")]
pub mod window;

#[cfg(feature = "window")]
pub use window::{document_from_path, run};

/// GPUI peer's typed theme cache.
///
/// Phase 5.7.B.12: real fields land for the surfaces the
/// binary currently paints (background, foreground, status
/// line, cursor inversion). Stored as `Rgba` so the render
/// hot path is a direct `.bg(self.theme.background)` etc. --
/// no per-frame conversion.
///
/// The defaults match the Catppuccin Mocha-ish palette the
/// binary's render used inline pre-5.7.B.12. Future host-side
/// slices will grow `host_theme` to carry window bg / fg /
/// status / cursor fields; once that lands,
/// [`GpuiApp::rebuild_gpui_theme`] reads from there and
/// `RendererSignal::ThemeChanged` will visibly recolor the
/// window on every `:set ui.*`. For now the rebuild is a
/// shape-only no-op -- the wiring is what this slice unblocks.
///
/// Why `Rgba` and not `Hsla`: gpui's `rgb(0x...) -> Rgba`
/// builder is the natural literal form; `.bg(Rgba)` /
/// `.text_color(Rgba)` accept it directly via `Into<Background>`
/// / `Into<Hsla>` blanket impls. Sticking with `Rgba` keeps the
/// theme literal-friendly + the binary's render free of
/// conversion boilerplate.
/// Color storage discipline: the lib stores `u32` packed
/// `0xRRGGBB` hex so it builds without the `window` feature (no
/// transitive `gpui` link). The binary converts to `gpui::Rgba`
/// at render time via `gpui::rgb(theme.background)`.
#[derive(Debug, Clone, Copy)]
pub struct GpuiTheme {
    /// Main document background.
    pub background: u32,
    /// Main document foreground (text + non-cursor chars).
    pub foreground: u32,
    /// Status-line background.
    pub status_background: u32,
    /// Status-line foreground.
    pub status_foreground: u32,
    /// Cursor block background (Block shape) + bar / underline
    /// border color (Bar / Underline shapes).
    pub cursor_background: u32,
    /// Cursor block foreground -- the character color when the
    /// block inverts (Normal/Visual mode cursor cell). Unused
    /// by Bar / Underline since the underlying char text stays
    /// the document foreground.
    pub cursor_foreground: u32,
    /// Popup-overlay background (DisplayBuffer help surface).
    /// Slightly darker than the main bg for visual separation.
    pub popup_background: u32,
    /// Popup-overlay border / accent color.
    pub popup_border: u32,
}

impl Default for GpuiTheme {
    fn default() -> Self {
        Self {
            // Catppuccin Mocha base.
            background: 0x1e1e2e,
            // Catppuccin Mocha text.
            foreground: 0xcdd6f4,
            // Catppuccin Mocha surface0.
            status_background: 0x313244,
            // Catppuccin Mocha green.
            status_foreground: 0xa6e3a1,
            // Catppuccin Mocha text (matches block-cursor "highlight").
            cursor_background: 0xcdd6f4,
            // Catppuccin Mocha base (inverted, for block-cursor char).
            cursor_foreground: 0x1e1e2e,
            // Catppuccin Mocha mantle (deeper than base).
            popup_background: 0x181825,
            // Catppuccin Mocha lavender (accent).
            popup_border: 0xb4befe,
        }
    }
}

/// Stub GPUI pane-render registry. Implements [`ProviderLookup`] so
/// the host's mode-walk already routes through it; storage fills with
/// the GPUI-typed render fn signature in 5.8+.
#[derive(Default)]
pub struct GpuiPaneRenderRegistry {
    /// Forward-compat placeholder. Replaced by
    /// `HashMap<ModeId, GpuiPaneRenderProvider>` once the GPUI
    /// render fn shape stabilises.
    _registered: std::collections::HashSet<ModeId>,
}

impl ProviderLookup for GpuiPaneRenderRegistry {
    fn has_provider(&self, mode: ModeId) -> bool {
        self._registered.contains(&mode)
    }
}

/// GPUI renderer marker. Holds no state — the renderer-trait
/// associated types point at the renderer-specific theme +
/// pane-render registry types this crate owns.
pub struct GpuiRenderer;

impl Renderer for GpuiRenderer {
    type Theme = GpuiTheme;
    type PaneRenderRegistry = GpuiPaneRenderRegistry;
}

/// The GPUI-side renderer composition root. Mirrors
/// `lattice-ui-tui::app::App` in shape: renderer-side caches
/// plus the renderer-neutral [`Editor`]. A future `lsp_file_watcher`
/// field joins when the LSP runtime adapter for GPUI lands.
pub struct GpuiApp {
    pub editor: Editor,
    pub theme: GpuiTheme,
    pub pane_render_registry: GpuiPaneRenderRegistry,
    /// 5.7.B.10: active help popup content, set by
    /// `RendererSignal::DisplayBuffer`. The binary renders it as
    /// a centered overlay when `Some`; pressing `Esc` dismisses.
    /// `None` when no popup is showing. Box keeps the
    /// `GpuiApp` field-size sane (HelpContent is ~6 fields incl.
    /// a parsed highlight cache).
    pub popup_content: Option<Box<lattice_help::HelpContent>>,
}

impl GpuiApp {
    /// Build the GPUI peer's composition root from an initial
    /// [`Document`]. Mirrors `lattice-ui-tui::app::App::new`:
    /// delegates the renderer-neutral construction to
    /// [`Editor::boot`] (LSP subsystem, command / mode /
    /// completion / picker / config registries, snippet handle,
    /// event bus, syntax, buffer registry) and supplies the
    /// renderer-side caches alongside.
    ///
    /// Real dispatch (key events -> `Action` -> `editor.dispatch`)
    /// + paint (read `editor.document` snapshot + cursor) wire in
    /// 5.7.B.3 / 5.7.B.4. The renderer-side post-boot helpers
    /// the TUI peer runs after `Editor::boot`
    /// (`activate_major_for_buffer_kind`,
    /// `publish_document_opened_for_active`,
    /// `ensure_named_synthetic_document`,
    /// `ensure_messages_buffer`) plug in as the corresponding
    /// GPUI wiring lands -- their bodies are host-resident, so
    /// this peer will call the same methods via its own renderer-
    /// signal handler.
    pub fn new(document: Document) -> Self {
        let mut app = Self {
            editor: Editor::boot(document),
            theme: GpuiTheme::default(),
            pane_render_registry: GpuiPaneRenderRegistry::default(),
            popup_content: None,
        };
        app.finalize_boot();
        app
    }

    /// Dismiss any active help popup. Called from the binary's
    /// `on_key_down` when the user presses `Esc` while a popup
    /// is showing (pre-empting the chord dispatch so `Esc`
    /// doesn't also fire a mode transition). Idempotent.
    pub fn dismiss_popup(&mut self) {
        self.popup_content = None;
    }

    /// Refresh the active-document visible-spans cache. Phase
    /// 5.8.G: routes through `Editor::refresh_highlights_window`
    /// (host-side) so the GPUI peer shares the content-keyed
    /// short-circuit + stale-snapshot HOLD with the TUI peer.
    ///
    /// The GPUI peer doesn't track folds yet (`fold_hash = 0`)
    /// and doesn't have a per-frame viewport-height yet, so it
    /// passes the document's full line count as the end. Once
    /// pane splits + viewport tracking land (5.8.H+), this
    /// updates to feed the active pane's viewport extent.
    ///
    /// Call this at the top of the renderer's per-frame paint
    /// before reading `editor.visible_highlights`.
    pub fn refresh_highlights(&mut self) {
        let syntax = self.editor.syntax.clone();
        // 5.8.O: tighten the highlight range to the visible
        // viewport (`[scroll, scroll + viewport_height)`) so the
        // worker walks only what the user can see. For docs
        // shorter than the viewport we clamp to line_count.
        let line_count = self.editor.document.snapshot().buffer.line_count() as u32;
        let end_line = self
            .editor
            .scroll
            .saturating_add(self.editor.viewport_height.max(1))
            .min(line_count);
        self.editor
            .refresh_highlights_window(syntax.as_ref(), end_line, 0);
    }

    /// Auto-scroll the active pane so the cursor stays in the
    /// visible viewport (`[scroll, scroll + viewport_height)`).
    /// Mirrors the TUI peer's per-tick scroll-clamp; both renderer
    /// peers funnel through the host's `editor.scroll` so any
    /// host-side cursor jump (a motion, a `:N`, a search bounce)
    /// is reflected here on the next paint.
    ///
    /// Phase 5.8.O: foundational for cursorline / visual selection
    /// (5.8.P, 5.8.Q) — those features paint *within* the viewport
    /// and would mis-render if the cursor sat outside it.
    pub fn ensure_cursor_in_viewport(&mut self) {
        let cursor_line = self.editor.cursor.line;
        let viewport = self.editor.viewport_height.max(1);
        if cursor_line < self.editor.scroll {
            // Cursor moved above the visible window — scroll up so
            // it lands on the top row.
            self.editor.scroll = cursor_line;
        } else if cursor_line >= self.editor.scroll.saturating_add(viewport) {
            // Cursor moved below the visible window — scroll down
            // so it lands on the bottom row.
            self.editor.scroll = cursor_line.saturating_sub(viewport - 1);
        }
    }

    /// Run the renderer-side post-boot helpers the TUI peer
    /// runs at the tail of `App::new`. Phase 5.7.B.6 brings the
    /// GPUI peer to the same boot end-state as the TUI peer
    /// (without the synthetic-buffer seeding -- `*lsp*` and
    /// `*messages*` will be lazy-created on first use until
    /// `ensure_named_synthetic_document` /
    /// `ensure_messages_buffer` migrate from `impl App` (TUI) to
    /// `impl Editor`).
    ///
    /// What this currently does:
    ///
    /// 1. `editor.rebuild_option_cache()` — populate the hot-
    ///    path option cache from the freshly-`init_from_linkme()`'d
    ///    registry.
    /// 2. `editor.activate_major_for_buffer_kind(document_buffer_id,
    ///    Document)` — resolve + activate the appropriate major
    ///    mode for the initial buffer (text-mode for plain,
    ///    rust-mode / python-mode / ... for typed languages).
    ///    This is what makes mode-contributed bindings reachable
    ///    + option contributions visible.
    /// 3. `editor.publish_document_opened_for_active()` — emit
    ///    [`Event::DocumentOpened`] so LSP attach / project
    ///    watcher / completion warmer subscribers see the
    ///    buffer.
    ///
    /// `activate_major_for_buffer_kind` returns
    /// `Vec<RendererSignal>` (`#[must_use]`); 5.7.B.6 discards
    /// them. The signal handler is the 5.7.B.7 slice -- once it
    /// lands, theme cascades + option cascades + structural
    /// renderer notifications all propagate to GPUI caches.
    fn finalize_boot(&mut self) {
        // 5.7.B.12: rebuild the GPUI-typed theme cache from
        // `editor.host_theme`. Body is currently a stub but
        // wiring is in place for the `ThemeChanged` cascade.
        self.rebuild_gpui_theme();
        self.editor.rebuild_option_cache();
        // 5.8.O: seed viewport_height with a sensible default so
        // the host-side cache key + cursor-scroll math work from
        // t=0. The default matches the 720×480 boot window at
        // roughly 16px per text row; a future slice hooks the
        // gpui window resize event to recompute on resize.
        if self.editor.viewport_height == 0 {
            self.editor.viewport_height = 30;
        }
        let signals = self
            .editor
            .activate_major_for_buffer_kind(self.editor.document_buffer_id, BufferKind::Document);
        for signal in signals {
            self.handle_renderer_signal(signal);
        }
        self.editor.publish_document_opened_for_active();
        // 5.7.B.9: eager subsystem buffer seeding -- matches the
        // tail of the TUI peer's `App::new`. Creates the `*lsp*`
        // and `*messages*` Document buffers so `:b *lsp*` /
        // `:b *messages*` resolve from t=0 instead of lazy-
        // creating on first use. The subsystem-name +
        // mode-id knowledge lives host-side (no need for the
        // GPUI peer to depend on `lattice-lsp` directly).
        self.editor.ensure_subsystem_buffers();
        // Phase 5.8.AA.u: load persistent TOML config from
        // `~/.editor.config/lattice/lattice.toml` (user) +
        // `<workspace>/.lattice/config.toml` (project). Reaches
        // the same host method the TUI runtime calls, so a
        // GPUI launch and a TUI launch from the same cwd
        // produce identical Editor state on first frame. The
        // returned signals always include `ThemeChanged`; we
        // fan through the standard signal handler so the GPUI-
        // typed theme cache rebuilds before the first paint.
        let workspace_root = Editor::workspace_root_from_cwd();
        let signals = self.editor.load_persistent_config(workspace_root.as_deref());
        for signal in signals {
            self.handle_renderer_signal(signal);
        }
        self.editor.apply_per_language_toml_overrides();
    }

    /// Renderer-side handler for the [`RendererSignal`] stream
    /// the host emits from option cascades + mode lifecycle
    /// helpers. Phase 5.7.B.7 wiring -- mirrors the TUI peer's
    /// `App::handle_renderer_signal` in shape; each arm's body
    /// reflects the GPUI peer's current capability surface.
    ///
    /// As GPUI-side infrastructure grows (theme cache, popup +
    /// pane display, file-tree buffers, window-close bridge),
    /// the corresponding arms light up. The signal contract
    /// itself is stable -- the host doesn't know which renderer
    /// is on the other side; both peers see the same enum.
    pub fn handle_renderer_signal(&mut self, signal: RendererSignal) {
        match signal {
            // 5.7.B.12: the cache holds real `Rgba` fields now,
            // and the binary's render reads from `self.theme.*`
            // instead of inline rgb literals. Rebuilding from
            // `editor.host_theme` still needs window-bg / -fg /
            // -cursor mappings on the host side, so this arm
            // calls `rebuild_gpui_theme()` but the body is
            // currently a shape-only no-op. Once those host
            // mappings land, a `:set ui.bg=...` cascade will
            // visibly recolor the window on the next frame.
            RendererSignal::ThemeChanged => {
                self.rebuild_gpui_theme();
                tracing::debug!(
                    "ThemeChanged signal: rebuild_gpui_theme called (host_theme→GpuiTheme mapping is a stub for now)"
                );
            }
            // `editor.should_quit` was set alongside the signal
            // emission (see `Action::Quit` in `Editor::dispatch`).
            // The GPUI binary polls `should_quit` after each
            // `dispatch_keystroke` and calls `cx.quit()` from the
            // event handler -- the lib has no `gpui::App` context
            // to drive shutdown from here.
            RendererSignal::Quit => {
                tracing::debug!("Quit signal: editor.should_quit set; binary polls per-event");
            }
            // The TUI peer's file-tree buffers embed nerd-font
            // glyphs in their rendered rope, so a palette flip
            // needs a rope refresh. The GPUI peer doesn't have
            // file-tree buffers (or rope-side icon embedding)
            // yet; the toggle is read live from
            // `editor.host_theme.nerd_fonts` when those views
            // exist. No-op for now -- the host emits this
            // alongside `ThemeChanged` so renderers that don't
            // track file-tree state can drop it.
            RendererSignal::NerdFontsToggled => {
                tracing::debug!(
                    "NerdFontsToggled signal: no file-tree buffers in GPUI peer; no-op"
                );
            }
            // The fan-out body migrated to
            // `Editor::fan_out_did_change_configuration` in
            // Phase 5.7.B.7; both peers route through the same
            // LSP supervisor walk + per-actor `did_change_configuration`.
            RendererSignal::LspConfigChanged(server_id) => {
                self.editor.fan_out_did_change_configuration(&server_id);
            }
            // 5.7.B.10: surface the help content as a centered
            // popup overlay. Renderer-side state is just
            // `popup_content`; the binary's render reads the
            // field each frame and draws the overlay when
            // `Some`. The user dismisses with `Esc` (binary-
            // side pre-empt before chord dispatch). Future
            // refinement: resolve the request's `category` to a
            // typed `BufferDisplay` preference (popup vs
            // active-pane vs split) -- today's GPUI peer only
            // implements the popup surface, so every category
            // collapses to a popup overlay.
            RendererSignal::DisplayBuffer(req) => {
                let lattice_host::dispatch::DisplayBufferRequest { content, category } = *req;
                tracing::debug!(
                    category = ?category,
                    title = %content.buffer.title,
                    "DisplayBuffer signal: showing as popup overlay"
                );
                self.popup_content = Some(Box::new(content));
            }
        }
    }

    /// Convenience entry point for GPUI's key-down event handler:
    /// `dispatch_keystroke(&ev.keystroke.key, mods.control, mods.alt,
    /// mods.shift, mods.platform)`. Walks the same pipeline as the
    /// TUI peer (chord normalisation → renderer-neutral `translate`
    /// → `editor.dispatch`); returns the [`DispatchOutcome`] for
    /// callers that want to inspect renderer signals (or
    /// [`None`] if the key string didn't map to a chord).
    ///
    /// Phase 5.7.B.3 scope: the outcome's renderer signals are
    /// returned but not yet fanned out (no GPUI-side equivalent of
    /// `App::handle_renderer_signal` yet). 5.7.B.4+ adds a signal
    /// handler so theme changes, option-cache rebuilds, and major-
    /// mode activations propagate to the renderer caches.
    pub fn dispatch_keystroke(
        &mut self,
        key: &str,
        control: bool,
        alt: bool,
        shift: bool,
        platform: bool,
    ) -> Option<DispatchOutcome> {
        let chord = gpui_chord::from_keystroke(key, control, alt, shift, platform)?;
        Some(self.dispatch_chord(chord))
    }

    /// Translate a canonical [`KeyChord`] into an [`Action`] (via
    /// the renderer-neutral host pipeline) and dispatch it. Used
    /// by [`Self::dispatch_keystroke`] after the GPUI-shaped event
    /// has been normalised, and directly by tests that drive the
    /// editor with synthetic chords.
    ///
    /// Builds [`TranslateContext`] from current `Editor` state.
    /// `chord_capture` is hard-wired to `false` for now: the
    /// `App`-side `chord_capture_active()` predicate in the TUI
    /// peer touches a cmdline arg-slot field that has no GPUI
    /// equivalent yet; when the GPUI peer grows a cmdline this
    /// reads from the same shared editor state.
    pub fn dispatch_chord(&mut self, chord: KeyChord) -> DispatchOutcome {
        let action = {
            let ctx = TranslateContext {
                modal: self.editor.modal,
                builtins: &self.editor.builtins,
                pending_count: self.editor.pending_count,
                op_count: self.editor.op_count,
                recording_macro: self.editor.macro_recording.is_some(),
                active_buffer: self.editor.active_buffer,
                completion_open: self.editor.completion_state.is_some(),
                chord_capture: false,
                picker_open: self.editor.picker.is_some(),
                insert_completion_open: self.editor.insert_completion.is_some(),
                snippet_active: self.editor.active_snippet.is_some(),
                keymap: &self.editor.keymap,
                partial_chord: &self.editor.partial_chord,
            };
            lattice_host::input::translate(ctx, chord)
        };
        self.dispatch_action(action)
    }

    /// Rebuild the cached [`GpuiTheme`] from
    /// [`editor.host_theme`](lattice_host::editor::Editor).
    /// Mirrors the TUI peer's `App::rebuild_tui_theme`. Called
    /// at boot (inside `finalize_boot`) and on every
    /// [`RendererSignal::ThemeChanged`] so theme cascades
    /// propagate to the GPUI cache without the binary having
    /// to re-read `host_theme` per frame.
    ///
    /// Phase 5.7.B.12: the body is currently a shape-only
    /// no-op -- `host_theme` doesn't yet carry direct window-bg
    /// / fg / status / cursor fields the GPUI peer can map. The
    /// next iteration grows `host_theme` (or adds a GPUI-typed
    /// theme overlay in `lattice-host::ui::theme`) and this
    /// method translates each into `Rgba`. Until then the cache
    /// keeps its `Default` palette; the wiring is what this
    /// slice unblocks.
    pub fn rebuild_gpui_theme(&mut self) {
        // Phase 5.8.K: live-cascade host_theme → GpuiTheme for the
        // fields where the host's `Theme` carries a matching colour
        // today. Fields without a host source (the bulk of the
        // window-only palette: editor bg/fg, cursor inversion,
        // popup chrome) keep their Catppuccin defaults until the
        // host's `Theme` grows window-color fields. The wiring is
        // already in place — `RendererSignal::ThemeChanged` fires
        // this method — so adding host fields later flows through
        // automatically.
        let host = &self.editor.host_theme;
        let defaults = GpuiTheme::default();

        // Status line ↔ host's `pane_status_active` (mirrors the
        // TUI's active-pane status row). Inactive pane status maps
        // to GpuiTheme's `popup_border` accent for now (visually
        // distinct from the active row).
        if let Some(bg) = host.pane_status_active.bg {
            self.theme.status_background = bg.to_rgb_u32(defaults.status_background);
        }
        if let Some(fg) = host.pane_status_active.fg {
            self.theme.status_foreground = fg.to_rgb_u32(defaults.status_foreground);
        }
        // Popup border ↔ host's pane_separator (same conceptual
        // role: thin accent line between visual regions).
        if let Some(fg) = host.pane_separator.fg {
            self.theme.popup_border = fg.to_rgb_u32(defaults.popup_border);
        }
    }

    /// Dispatch a single [`Action`] through `editor.dispatch` and
    /// drain the deferred-action queue iteratively.
    ///
    /// `editor.dispatch` runs ONE action and returns the outcome;
    /// for many actions (`Action::Invoke` resolving to an
    /// `AppEffect::EnterMode` / `EnterVisual` / etc.) the host
    /// emits a follow-up [`Action`] in
    /// `outcome.next_actions` rather than mutating directly --
    /// see `Editor::apply_app_effect` in `lattice_host::dispatch`.
    /// The TUI peer's `App::apply` drains the queue with a
    /// recursive call; this method does the same shape with an
    /// explicit FIFO loop so basic state transitions land without
    /// needing the renderer's full effect / signal fan-out yet
    /// (5.7.B.4+ work).
    ///
    /// `outcome.effects` are aggregated across the chain but NOT
    /// re-applied -- the host has already called
    /// `editor.handle_effect(effect.clone())` on each one during
    /// the inner dispatch (the renderer-coupled match arms remain
    /// a 5.7.B.4+ follow-up). `renderer_signals` accumulate the
    /// same way; callers can inspect them once the GPUI peer
    /// gains a signal handler.
    pub fn dispatch_action(&mut self, action: Action) -> DispatchOutcome {
        // Phase 5.8.AC.1: GPUI-side intercepts for App-only Action
        // arms. The host's `handle_action` doesn't dispatch these
        // (the TUI App's `apply` catches them with renderer-coupled
        // bodies). Until the full set migrates, the GPUI peer
        // intercepts the most-common ones here. The host's matching
        // `do_*` helpers cover the simple Buffer / Path routings;
        // exotic routings (mark / register / command / snippet /
        // LSP code-action / completion) still warn until they
        // migrate.
        match &action {
            Action::PickerAccept => {
                let signals = self.editor.do_picker_accept();
                let mut outcome = DispatchOutcome::default();
                outcome.consumed = true;
                outcome.renderer_signals = signals.clone();
                for s in signals {
                    self.handle_renderer_signal(s);
                }
                return outcome;
            }
            Action::PickerDismiss => {
                let signals = self.editor.do_picker_dismiss();
                let mut outcome = DispatchOutcome::default();
                outcome.consumed = true;
                outcome.renderer_signals = signals.clone();
                for s in signals {
                    self.handle_renderer_signal(s);
                }
                return outcome;
            }
            _ => {}
        }
        let mut outcome = self.editor.dispatch(action);
        let mut pending: std::collections::VecDeque<Action> =
            outcome.next_actions.drain(..).collect();
        while let Some(follow_up) = pending.pop_front() {
            let mut next_out = self.editor.dispatch(follow_up);
            pending.extend(next_out.next_actions.drain(..));
            outcome.effects.append(&mut next_out.effects);
            outcome
                .renderer_signals
                .append(&mut next_out.renderer_signals);
            outcome.consumed |= next_out.consumed;
            if self.editor.should_quit {
                // Mirrors `App::apply`'s mid-macro-quit semantic:
                // a recorded `:q` short-circuits the rest of the
                // chain so we don't keep firing actions against
                // an editor that's tearing down.
                break;
            }
        }
        // Phase 5.8.AC.1: drain renderer-coupled effects. The
        // host's `editor.dispatch` already called
        // `editor.handle_effect` for every Effect, but the
        // renderer-coupled tail (`OpenBuffer` → `do_edit`,
        // `OpenBufferPicker` → `do_open_buffer_picker`,
        // `QuitEditor` → `do_quit`, ...) doesn't run inside the
        // host's `handle_effect`. The TUI peer drains these in
        // `App::apply_effect_app_arms`; the GPUI peer mirrors
        // those arms here for the variants whose handlers are
        // host-resident today. Variants whose handlers are still
        // App-only (file-tree / oil / hover / LSP-request /
        // diagnostics navigation / picker accept) log a one-line
        // trace; they'll light up as the matching host methods
        // land in subsequent slices.
        for effect in outcome.effects.iter().cloned() {
            self.apply_effect_gpui(effect);
        }
        // Action arms App handles that the host's `handle_action`
        // doesn't yet route through: `PickerAccept` / `PickerDismiss`
        // — these need a renderer-coupled close path. Until the
        // accept body migrates host-side, GPUI catches the action
        // by inspecting `outcome.consumed` and falling back to a
        // minimal close + activate-restore for the dismiss arm.
        // 5.7.B.7: drain renderer signals through the GPUI
        // handler. Drained BEFORE returning so callers see a
        // post-signal-handling editor state; the returned
        // outcome still carries the (now-handled) signal list
        // for callers that want to inspect or re-route them.
        let signals = std::mem::take(&mut outcome.renderer_signals);
        for signal in signals.iter().cloned() {
            self.handle_renderer_signal(signal);
        }
        // Restore the list on the outcome so the returned value
        // matches the historical TUI shape (App::apply also
        // hands these to the caller after fan-out).
        outcome.renderer_signals = signals;
        outcome
    }

    /// Renderer-coupled effect handler for the GPUI peer.
    ///
    /// Phase 5.8.AC.1: mirrors the role of TUI's
    /// `App::apply_effect_app_arms`. For ex-effects whose
    /// renderer-coupled tail lives host-side today
    /// (`OpenBuffer`, `QuitEditor`, `OpenBufferPicker`), call
    /// the host method and fan signals through the existing
    /// `RendererSignal` handler. For the rest (file-tree, oil,
    /// hover, picker open, LSP requests, diagnostics navigation),
    /// log a one-line trace + continue; those light up as the
    /// matching host methods land in subsequent slices.
    fn apply_effect_gpui(&mut self, effect: lattice_grammar::Effect) {
        use lattice_grammar::Effect;
        match effect {
            // Document-only effects the host has already
            // applied via `editor.handle_effect`; nothing for the
            // renderer to do.
            Effect::None
            | Effect::Edits(_)
            | Effect::SelectionChange(_)
            | Effect::EnterMode(_)
            | Effect::Yank { .. }
            | Effect::SetOption { .. }
            | Effect::ClearSearchHighlight
            | Effect::Echo { .. }
            | Effect::EchoRegisters
            | Effect::EchoMarks
            | Effect::ListBuffers
            | Effect::DescribeBuffer
            | Effect::ListKeymap
            | Effect::DescribeOption { .. }
            | Effect::ListOptions
            | Effect::DescribeOptionResolution { .. }
            | Effect::DescribeEvents
            | Effect::DescribeEvent { .. }
            | Effect::BufferNext
            | Effect::BufferPrev
            | Effect::BufferDelete { .. }
            | Effect::ListModes
            | Effect::DescribeMode { .. }
            | Effect::Customize { .. }
            | Effect::ListDiagnostics
            | Effect::DeleteCurrentLine
            | Effect::Substitute { .. }
            | Effect::DescribeCommand { .. }
            | Effect::Apropos { .. }
            | Effect::DescribeKey { .. }
            | Effect::AppAction(_) => {}
            // Renderer-coupled effects whose body lives host-side.
            Effect::QuitEditor { force } => self.editor.do_quit(force),
            Effect::OpenBuffer { path, force } => self.apply_open_buffer(path, force),
            // Phase 5.8.AD.3: `:w` save with full LSP fan-out
            // (BeforeSave / willSave / willSaveWaitUntil /
            // didSave / didCreateFiles) is now host-resident.
            Effect::SaveBuffer { path } => self.editor.do_write(path),
            Effect::OpenBufferPicker => {
                let signals = self.editor.do_open_buffer_picker();
                for s in signals {
                    self.handle_renderer_signal(s);
                }
            }
            // Phase 5.8.AD.1: oil + file-tree migrated host-side
            // so `:e .` / `:Oil` / `:Tree` work in GPUI.
            Effect::OpenOil { dir } => {
                let signals = self.editor.do_open_oil(dir);
                for s in signals {
                    self.handle_renderer_signal(s);
                }
            }
            Effect::OpenFileTree { root } => {
                let signals = self.editor.do_open_file_tree(root);
                for s in signals {
                    self.handle_renderer_signal(s);
                }
            }
            Effect::CloseFileTree => {
                let signals = self.editor.dismiss_file_tree();
                for s in signals {
                    self.handle_renderer_signal(s);
                }
            }
            // Many recurses through the same handler so inner
            // arms hit their renderer-coupled tail too.
            Effect::Many(parts) => {
                for p in parts {
                    self.apply_effect_gpui(p);
                }
            }
            // Effects whose handlers are still App-only in TUI;
            // not yet wired in GPUI. One-line trace so the
            // user sees *why* nothing happened.
            other => {
                tracing::warn!(
                    effect = ?other,
                    "lattice-gpui: effect not yet wired (App-side handler hasn't migrated to host)"
                );
            }
        }
    }

    /// Wrapper around `editor.do_edit(path, force)` that translates
    /// the host's [`DoEditOutcome`] for the GPUI peer. Mirrors
    /// TUI's `App::do_edit`: `Directory` routes to `do_open_oil`,
    /// `Opened`/`Activated`/`Reloaded` fan signals through the
    /// renderer handler, `NoFileName`/`Failed` are silent (the
    /// host already echoed).
    fn apply_open_buffer(&mut self, path: Option<std::path::PathBuf>, force: bool) {
        use lattice_host::dispatch::DoEditOutcome;
        let outcome = self.editor.do_edit(path, force);
        match outcome {
            DoEditOutcome::NoFileName | DoEditOutcome::Failed => {}
            DoEditOutcome::Directory(dir) => {
                // Phase 5.8.AD.1: oil is now host-side; route the
                // directory `:e` through the same path the TUI peer
                // uses.
                let signals = self.editor.do_open_oil(Some(dir));
                for s in signals {
                    self.handle_renderer_signal(s);
                }
            }
            DoEditOutcome::Reloaded(signals)
            | DoEditOutcome::Activated(signals)
            | DoEditOutcome::Opened(signals) => {
                for s in signals {
                    self.handle_renderer_signal(s);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use lattice_grammar::ModalState;

    /// The 5.7 scaffold's core claim: a GPUI-side composition root
    /// constructs from the host substrate alone. If this builds, the
    /// host crate has no transitive ratatui / crossterm dep — that's
    /// the entire point of the slice.
    ///
    /// 5.7.B.2: `GpuiApp::new(document)` goes through
    /// `Editor::boot`, so this test also exercises that the host's
    /// boot path is callable without `lattice-ui-tui` in the dep
    /// tree (no transitive ratatui / crossterm pull-in).
    #[test]
    fn scaffold_constructs_without_ui_tui() {
        let app = GpuiApp::new(Document::empty());
        // `Editor::boot` populates the mode registry; the entry
        // count is nonzero so the host's mode-registration path
        // ran.
        assert!(app.editor.mode_registry.iter_meta().next().is_some());
        // 5.7.B.6: `GpuiApp::new` now also runs `finalize_boot`
        // which calls `activate_major_for_buffer_kind` for the
        // initial document buffer. The active-modes map should
        // therefore contain an entry for `document_buffer_id`.
        assert!(
            app.editor
                .active_modes
                .contains_key(&app.editor.document_buffer_id),
            "finalize_boot should activate a major mode for the initial document buffer"
        );
        // ProviderLookup is implemented on the registry; the
        // empty stub answers `false` for every mode id.
        let probe: &dyn ProviderLookup = &app.pane_render_registry;
        let dummy = lattice_mode::ModeId::new("__scaffold_probe__");
        assert!(!probe.has_provider(dummy));
    }

    /// End-to-end pipeline assertion: a key string flows through
    /// the GPUI peer's adapter, the host's `translate` path, and
    /// `editor.dispatch`, and the editor's modal state actually
    /// changes. This is the smallest credible "input dispatch
    /// works" test — it doesn't need GPUI's native event types
    /// or a window.
    ///
    /// `"i"` in Normal mode is canonical vim "enter Insert"; if
    /// the pipeline is wired correctly the editor's `modal` ends
    /// up in [`ModalState::Insert`].
    #[test]
    fn dispatch_keystroke_routes_i_to_insert_mode() {
        let mut app = GpuiApp::new(Document::empty());
        assert_eq!(app.editor.modal, ModalState::Normal);
        let outcome = app.dispatch_keystroke("i", false, false, false, false);
        assert!(
            outcome.is_some(),
            "`i` should normalise to a chord and dispatch"
        );
        assert_eq!(app.editor.modal, ModalState::Insert);
    }

    /// `dispatch_chord` is the path tests + programmatic drivers
    /// take. Sanity-check that handing it a canonical chord
    /// reaches the same state change as the keystroke entry
    /// point.
    #[test]
    fn dispatch_chord_drives_normal_to_insert_transition() {
        use lattice_host::chord::{KeyChord, KeyKind, KeyMods};
        let mut app = GpuiApp::new(Document::empty());
        let chord = KeyChord::new(KeyKind::Char('i'), KeyMods::NONE);
        app.dispatch_chord(chord);
        assert_eq!(app.editor.modal, ModalState::Insert);
    }
}
