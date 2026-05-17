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

/// Stub GPUI theme cache. The TUI peer caches pre-computed
/// `ratatui::style::Style` primitives so the frame-hot path is a
/// direct read; the GPUI peer will cache native `gpui::Hsla` /
/// `gpui::TextStyle` / variable-font selections in the same shape.
/// Empty for now — fields land per parity slice (which is also when
/// this type starts depending on the gpui crate).
#[derive(Default)]
pub struct GpuiTheme {
    _placeholder: (),
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
/// `lattice-ui-tui::app::App` in shape: three renderer-side caches
/// plus the renderer-neutral [`Editor`]. A future `lsp_file_watcher`
/// field joins when the LSP runtime adapter for GPUI lands.
pub struct GpuiApp {
    pub editor: Editor,
    pub theme: GpuiTheme,
    pub pane_render_registry: GpuiPaneRenderRegistry,
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
        };
        app.finalize_boot();
        app
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
        self.editor.rebuild_option_cache();
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
            // GpuiTheme is currently a placeholder (`_placeholder:
            // ()`); when it grows GPUI-native style primitives
            // (`Hsla`, `TextStyle`, variable-font selections)
            // this arm rebuilds the cache from `editor.host_theme`.
            // The corresponding `rebuild_gpui_theme()` lands
            // alongside the first real theme field.
            RendererSignal::ThemeChanged => {
                tracing::debug!("ThemeChanged signal: GpuiTheme rebuild pending (cache is empty)");
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
            // Host-side `do_*` arms build `HelpContent` and want
            // the renderer to surface it. The GPUI peer doesn't
            // have its `display_buffer` dispatch yet (popup +
            // active-pane swap + split are TUI-only today); when
            // those land, this arm routes through the same
            // category -> BufferDisplay resolution the TUI peer
            // uses. Today's host call sites (`:ls`,
            // `:describe-buffer`) silently produce no visible
            // output in the GPUI peer; the host state mutations
            // already happened before the signal was emitted.
            RendererSignal::DisplayBuffer(req) => {
                tracing::debug!(
                    category = ?req.category,
                    "DisplayBuffer signal: GPUI display dispatch not yet implemented"
                );
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
