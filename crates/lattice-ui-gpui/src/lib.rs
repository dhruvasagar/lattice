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
use lattice_host::render_state::{ActiveDocumentRenderState, RenderState};
use lattice_mode::ModeId;
use std::sync::Arc;

pub mod gpui_chord;

/// GPUI window-opening entry point ([`run`], [`document_from_path`],
/// [`document_from_first_arg`]). Behind the `window` Cargo feature
/// so the lib's headless build doesn't link gpui. Lifted from the
/// `lattice-gpui` binary in Phase 5.9 so `lattice-cli --gpu` can
/// reuse the same entry without duplicating the window setup.
#[cfg(feature = "window")]
pub mod window;

/// Slice W.3: pure `decorations` → GPUI window-chrome map
/// (`window_chrome`/`full_titlebar`). Gated identically to `window` since
/// it names `gpui::{SharedString, TitlebarOptions, WindowDecorations}` and
/// is only reachable from `window.rs`'s `WindowOptions` construction.
#[cfg(feature = "window")]
pub mod window_chrome;

/// Phase 5.8.AF.5 / Slice X3.full.1: custom GPUI `Element` rendering
/// pane text via `ShapedLine::paint` -- replaces the per-char-Div
/// element tree that dominated `paint_us`.
#[cfg(feature = "window")]
// Phase 5.8.AF.6 / Slice X5: `editor_element` is normally
// crate-private (the only legitimate caller is `window::paint_pane`).
// With `bench-internals` it becomes `pub` so the frame-budget
// bench can call the pre-paint logic (`build_line_with_inlays`,
// `byte_to_combined_col`, ...) without spinning up a real GPUI
// window. The shaped-text + paint phases remain GPU-bound and
// are not reachable from the bench surface.
#[cfg(not(feature = "bench-internals"))]
pub(crate) mod editor_element;
#[cfg(feature = "bench-internals")]
pub mod editor_element;
// S4.0 (2026-05-26): cell-grid → GPUI TextRun converter. The
// substrate→GPUI translation layer that turns
// `lattice_cells::Cell` payloads into the
// `(combined_text, Vec<TextRun>, inlay_offsets)` shape
// `EditorElement::prepaint` consumes. Gated behind `window`
// because it depends on `gpui::{Font, TextRun}`; mirrors the
// `editor_element` gating. Anchor:
// `docs/dev/architecture/cell-grid-renderer.md`.
#[cfg(feature = "window")]
pub mod cells_paint;
// S4.final.a (2026-05-27): per-codepoint glyph-id cache. The
// software cache that converts `(FontId, char)` →
// `Option<ResolvedGlyph>` so the `paint_cells` loop can hand
// `(font_id, glyph_id)` to `Window::paint_glyph` without going
// through `shape_line`. Gated behind `window`; mirrors the
// `cells_paint` gating.
#[cfg(feature = "window")]
pub mod glyph_resolver;
// S4.final.b (2026-05-27): per-cell `paint_glyph` body path.
// `paint_cells_row` walks one CellRow and emits per-cell bg
// quads + glyphs. Gated behind a runtime env-var toggle
// (`LATTICE_PAINT_CELLS=1`) so both `shape_line` and
// `paint_cells` paths coexist until S4.final.f retires
// `shape_line` on the document body.
#[cfg(feature = "window")]
pub mod paint_cells;
// S4.final.c (2026-05-27): hit-testing primitives on the cell
// grid. `x_to_combined_col` / `col_to_x` / `combined_col_to_byte`
// for mapping mouse positions to buffer coordinates without
// going through `ShapedLine`. Forward-looking infrastructure
// for the eventual mouse-select / drag-select handler in
// `window.rs`.
#[cfg(feature = "window")]
pub mod hit_test;
// IG.4 (2026-08-16): indentation-guide paint geometry. NOT gated on
// `window`: the column arithmetic is the part worth testing, and gating
// it would put it out of reach of `cargo test -p lattice-ui-gpui`. The
// paint itself lives in `editor_element`, which is gated.
// Anchor: `docs/dev/architecture/indent-guides.md`.
pub(crate) mod indent_guides;

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
#[derive(Debug, Clone)]
pub struct GpuiTheme {
    /// Font family name for the editor window. Must be a monospace
    /// typeface. Configurable via `ui.font_family`; defaults to
    /// "Menlo" (built-in macOS monospace). Updated by
    /// `rebuild_gpui_theme` on every `RendererSignal::ThemeChanged`.
    pub font_family: String,
    /// Font size in points. Configurable via `ui.font_size`.
    pub font_size_pt: u32,
    /// Whether OpenType ligatures are enabled. Configurable via
    /// `ui.ligatures` (default `true`). When `false`,
    /// `FontFeatures::disable_ligatures()` is applied to every
    /// `Font` before shaping, suppressing `calt` (drives programming ligatures).
    pub ligatures: bool,
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
    /// Popup header TITLE colour (bold accent) — `ui.popup.title`.
    pub popup_title: u32,
    /// Popup header HINT colour (dim) — `ui.popup.hint`.
    pub popup_hint: u32,
    /// Notification-severity colours, sourced from the
    /// `diagnostic.{warning,error}` theme elements.
    ///
    /// The notification overlay referenced `diff_change_line_bg` /
    /// `diff_remove_line_bg` here — fields that never existed on this
    /// struct, so the `window`-feature build did not compile. Diff
    /// line backgrounds were the wrong source anyway: these tint
    /// notification *text* by severity, which is what
    /// `diagnostic.warning` / `diagnostic.error` already mean, and
    /// they recolour with `:colorscheme` like every other element.
    pub notification_warn: u32,
    pub notification_error: u32,
    /// Issue #35 (2026-05-22): picker match-range highlight
    /// color. Painted on the substring of each candidate that
    /// matched the query. Catppuccin Mocha peach by default —
    /// distinct from `foreground` so matches actually pop
    /// (previously used `cursor_background` which equals
    /// `foreground` in the default palette and made matches
    /// invisible).
    pub picker_match_highlight: u32,
    /// Picker marginalia (annotation / kind glyph) color.
    /// Mid-grey by default so it doesn't compete with the
    /// candidate text.
    pub picker_marginalia_fg: u32,
    /// IG.4: indentation-guide rule colour, from the `indent.guide`
    /// theme element. Dim by default — a guide bright enough to read
    /// directly is one that competes with the code it measures.
    pub indent_guide: u32,
    /// IG.4: the guide for the block enclosing the cursor, from
    /// `indent.guide.active`. Same hue undimmed; the contrast between
    /// the two is what carries the signal.
    pub indent_guide_active: u32,
}

impl Default for GpuiTheme {
    fn default() -> Self {
        Self {
            font_family: String::from("Menlo"),
            font_size_pt: 14,
            ligatures: true,
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
            // Catppuccin Mocha blue — popup title accent (`ui.popup.title`).
            popup_title: 0x89b4fa,
            // Catppuccin Mocha overlay — dim popup hint (`ui.popup.hint`).
            popup_hint: 0x6c7086,
            // Catppuccin-ish peach / red, matching the diagnostic
            // defaults elsewhere; overridden by the resolved elements.
            notification_warn: 0xfab387,
            notification_error: 0xf38ba8,
            // Issue #35: Catppuccin Mocha peach — bright accent
            // distinct from `foreground` (text). Highly
            // visible against both light and dark backgrounds.
            picker_match_highlight: 0xfab387,
            // Catppuccin Mocha overlay1 — mid-grey for
            // marginalia / annotations. Softer than
            // `popup_border` (which doubles as the popup
            // accent) so kind glyphs don't dominate the row.
            picker_marginalia_fg: 0x7f849c,
            // Catppuccin Mocha surface1 / overlay0 — the same pair the
            // TUI resolves from `indent.guide{,.active}` when the theme
            // leaves them at their registered defaults.
            indent_guide: 0x45475a,
            indent_guide_active: 0x6c7086,
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
    /// Slice 3c.final.E.swap: cfg-gated. Production builds hold an
    /// [`EditorActorHandle`]; test builds keep direct `Editor`
    /// ownership for fixtures that mutate state without going
    /// through the dispatch path. Same shape as TUI peer's `App`.
    #[cfg(not(test))]
    pub editor_actor: lattice_host::editor_actor::EditorActorHandle,
    #[cfg(test)]
    pub editor: Editor,
    /// Phase 5.8.AF.5 / Slice 3c.atomic.K: renderer-side clone of
    /// the editor's `RenderState` cell. Parallel of the TUI peer's
    /// `App.render_state` field (3c.atomic.A): isolates the
    /// renderer's read path from `self.editor` so the eventual
    /// `App.editor: Editor → handle` swap doesn't disturb call
    /// sites. Today both Arc handles point at the same underlying
    /// `ArcSwap<RenderState>` instance — readers observe identical
    /// values byte-for-byte regardless of which they go through.
    pub render_state: Arc<arc_swap::ArcSwap<RenderState>>,
    pub theme: GpuiTheme,
    pub pane_render_registry: GpuiPaneRenderRegistry,
    /// Slice 3c.final.B-extension: `paint_request` cloned at boot so
    /// `EditorView::new` can subscribe without a `read_editor` round-trip.
    /// The `Arc<Notify>` is shared with the highlights worker; wakes
    /// propagate to GPUI's foreground executor via `cx.notify()`.
    pub paint_request: std::sync::Arc<tokio::sync::Notify>,
    // Phase 5.8.AE: `popup_content` retired. Popup state is
    // unified in `editor.popup_buffer` (+ buffer-locals for
    // links/anchors/highlights). The binary's render reads
    // `editor.popup_help()` for the buffer and the host
    // accessors for metadata, so both renderer peers paint
    // popups from the same source-of-truth state.
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
    /// Real dispatch (key events -> `Action` -> `editor.dispatch`) +
    /// paint (read `editor.document` snapshot + cursor) wire in
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
        // DB.5 (design.md §9.1): capture the opened-file path BEFORE
        // `document` moves into `Editor::boot` (which consumes it) —
        // mirrors the TUI seam in `lattice-ui-tui/src/app/boot.rs`.
        let opened_file = document.path().map(|p| p.to_path_buf());
        let mut editor = Editor::boot(document);
        // CB.4 (docs/dev/architecture/clipboard.md §4/§7): override CB.0's
        // default `FakeClipboard` with the shared native `arboard` backend
        // (the same `lattice_host::clipboard::ArboardClipboard` the TUI
        // peer uses — one impl, honoring the synchronous-read contract).
        // The GPUI peer always links display libs (the `window` feature),
        // so `arboard` is always available in a real GUI build and needs
        // no OSC52 fallback (a GUI is never a headless terminal). Fork-1
        // named "gpui-native" here, but gpui's own clipboard is reachable
        // only via `&App` on the main thread, which can't satisfy the
        // `Send + Sync` trait's synchronous read from the editor actor
        // thread; arboard is the sound resolution (see the slice plan
        // CB.4 note). Gated on `system-clipboard` (pulled by `window`) so
        // the non-window scaffold build stays dep-light. `Arc::get_mut`
        // succeeds here for the same reason as the TUI seam: the registry
        // is a freshly-frozen, uniquely-owned Arc immediately after
        // `Editor::boot`.
        //
        // `not(test)`: test builds keep CB.0's `FakeClipboard` so `cargo
        // test` never clobbers the developer's real system clipboard —
        // the same hermeticity rule the TUI peer applies in
        // `lattice-ui-tui/src/clipboard.rs`'s `boot_backend`. Nothing else
        // changes: leaving the fake in place is already this seam's
        // no-display path below.
        #[cfg(all(feature = "system-clipboard", not(test)))]
        match lattice_host::clipboard::ArboardClipboard::new() {
            Some(native) => {
                if let Some(services) = std::sync::Arc::get_mut(&mut editor.services) {
                    let clipboard: lattice_core::ClipboardHandle = std::sync::Arc::new(native);
                    services.register(clipboard);
                }
            }
            None => {
                // No reachable display clipboard — leave CB.0's
                // FakeClipboard (in-memory register behavior). Unusual for
                // a GUI (which owns a window), so note it once for
                // diagnosis; never fatal.
                tracing::debug!(
                    "clipboard: native (arboard) init failed at GPUI boot; \
                     paste-from-another-app unavailable this session"
                );
            }
        }
        // DB.5 (test isolation): disable the dashboard auto-open BEFORE
        // the `Startup` publish so unit tests (which build pathless
        // documents that look like a no-file launch) don't get the
        // dashboard buffer instead of their own text. Race-free — the
        // trigger's task reads `dashboard.enabled` only after receiving
        // `Startup`, which can't arrive before this publish. Mirrors the
        // TUI seam in `lattice-ui-tui/src/app/boot.rs`.
        #[cfg(test)]
        {
            let _ = editor
                .config
                .parse_and_set_command("dashboard.enabled=false");
        }
        // DB.5: publish `Startup` once `editor` exists, right after `boot`
        // returns — see the TUI seam for the full rationale.
        editor
            .event_bus
            .publish_typed(lattice_mode::Startup { opened_file });
        let render_state = editor.render_state.clone();
        // Slice 3c.final.E.swap: run boot-time setup directly on
        // the owned Editor BEFORE handing it to the actor. The
        // finalize_boot body's host-routed work runs here as
        // direct Editor calls. Renderer-side post-actor wiring
        // (theme rebuild) runs after the App is built.
        editor.rebuild_option_cache();
        if editor.viewport_height == 0 {
            editor.viewport_height = 30;
        }
        let doc_id = editor.document_buffer_id;
        let _ = editor.activate_major_for_buffer_kind(doc_id, BufferKind::Document);
        editor.publish_document_opened_for_active();
        editor.ensure_subsystem_buffers();
        let workspace_root = lattice_core::project::root_from_cwd();
        let _ = editor.load_persistent_config(workspace_root.as_deref());
        editor.apply_per_language_toml_overrides();
        // built-ins 2026-06-13: load embedded + user snippet packs
        // once at startup (parity with the TUI runtime). Quiet —
        // logs, no echo.
        editor.load_snippets_at_startup();
        // Initial RS publish so `app.ad()` returns boot state.
        editor.publish_render_state();

        // Clone before the actor consumes the editor so EditorView::new
        // can subscribe without a read_editor round-trip.
        let paint_request = editor.paint_request.clone();

        // Slice 3c.final.E.swap: hand Editor to the actor (prod)
        // or keep inline (test).
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
            theme: GpuiTheme::default(),
            pane_render_registry: GpuiPaneRenderRegistry::default(),
            paint_request,
        };
        // App-side post-actor: rebuild the cached GPUI theme from
        // the freshly-published `render_state.theme`.
        app.rebuild_gpui_theme();
        app
    }

    /// Phase 5.8.AF.5 / Slice 3c.atomic.K: wait-free snapshot of
    /// the active-buffer hot-path render state. Returns an `Arc`
    /// clone of `RenderState.active_document`. Parallel of the
    /// TUI peer's `App::ad()` -- the GPUI peer reads its
    /// renderer-side state through this method so the eventual
    /// `editor → handle` swap leaves call sites untouched.
    pub fn ad(&self) -> Arc<ActiveDocumentRenderState> {
        self.render_state.load().active_document.load_full()
    }

    /// Slice 3c.final.B.7: published echo-area state — parallel
    /// of TUI peer's `App::messages()`.
    pub fn messages(&self) -> Arc<lattice_host::render_state::MessagesRenderState> {
        self.render_state.load().messages.clone()
    }

    /// Slice 3c.final.B.7: published modeline + cmdline + search
    /// state — parallel of TUI peer's `App::modeline()`.
    pub fn modeline(&self) -> Arc<lattice_host::render_state::ModelineRenderState> {
        self.render_state.load().modeline.clone()
    }

    /// Slice 3c.final.B.10: published typed-options registry —
    /// parallel of TUI peer's `App::options()`.
    pub fn options(&self) -> Arc<lattice_host::render_state::OptionsRenderState> {
        self.render_state.load().options.clone()
    }

    /// Slice 3c.final.B.11: published active-modes map —
    /// parallel of TUI peer's `App::modes()`.
    pub fn modes(&self) -> Arc<lattice_host::render_state::ModesRenderState> {
        self.render_state.load().modes.clone()
    }

    /// Slice 3c.final.B.9: published buffer-locals map —
    /// parallel of TUI peer's `App::buffer_locals()`.
    pub fn buffer_locals(&self) -> Arc<lattice_host::render_state::BufferLocalsRenderState> {
        self.render_state.load().buffer_locals.clone()
    }

    /// Phase 5.8.AF.5 / Slice 3c.final.E.4: routing helpers for
    /// editor mutations. Same shape as the TUI peer's
    /// `App::mutate_editor` / `App::mutate_editor_with`. Pre-swap
    /// runs the closure against `&mut self.editor` + publishes RS;
    /// post-swap delegates to the actor handle. Forward-compatible
    /// `Send + 'static` bounds.
    pub fn mutate_editor<F>(&mut self, f: F)
    where
        F: FnOnce(&mut Editor) + Send + 'static,
    {
        #[cfg(not(test))]
        {
            self.editor_actor
                .mutate_blocking(Box::new(f))
                .expect("editor actor alive");
        }
        #[cfg(test)]
        {
            f(&mut self.editor);
            self.editor.publish_render_state();
        }
    }

    /// Variant for closures that return a value.
    pub fn mutate_editor_with<F, R>(&mut self, f: F) -> R
    where
        F: FnOnce(&mut Editor) -> R + Send + 'static,
        R: Send + 'static,
    {
        #[cfg(not(test))]
        {
            self.editor_actor.mutate_blocking_with(f)
        }
        #[cfg(test)]
        {
            let r = f(&mut self.editor);
            self.editor.publish_render_state();
            r
        }
    }

    /// Read-only helper -- parallels `App::read_editor` on the TUI
    /// peer. In production routes through the actor handle's
    /// `with_editor` blocking RPC; in tests calls directly.
    pub fn read_editor<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&Editor) -> R + Send + 'static,
        R: Send + 'static,
    {
        #[cfg(not(test))]
        {
            self.editor_actor.with_editor(f)
        }
        #[cfg(test)]
        {
            f(&self.editor)
        }
    }

    /// Phase 5.8.AF.5 / Slice 3c.atomic.K: publishing wrapper
    /// around `editor.viewport_height` writes. Parallel of the
    /// TUI peer's `App::set_viewport_height` (3c.atomic.D): the
    /// per-frame viewport recompute happens outside dispatch, so
    /// it must republish render-state for `ad().viewport_height`
    /// and `ad().scroll` to observe the new values. Without this
    /// wrapper, a window resize would leave the published
    /// viewport / scroll one frame behind every time.
    pub fn set_viewport_height(&mut self, height: u32) {
        // Slice 3c.final.C: route through dispatch so the
        // mutation goes via `Action::SetViewportHeight`. Dispatch
        // tail publishes RenderState; no manual publish needed.
        self.dispatch_action(lattice_host::action::Action::SetViewportHeight(height));
    }

    /// Issue #25 (2026-05-22): per-pane geometry hand-off. The
    /// renderer's per-frame layout pass fires one of these per
    /// leaf in the pane tree. Production routes through the
    /// editor actor's typed `SetPaneViewport` command;
    /// cfg(test) mutates the in-process editor directly
    /// (parallels the `mutate_editor_with` pattern). The
    /// host's handler writes onto `PaneState[idx]` and mirrors
    /// the active leaf's height into `Editor::viewport_height`
    /// for cursor-clamp + highlights worker.
    pub fn set_pane_viewport(&mut self, idx: usize, rows: u32, cols: u32) {
        #[cfg(not(test))]
        {
            let _ = self.editor_actor.set_pane_viewport(idx, rows, cols);
        }
        #[cfg(test)]
        {
            let active_idx = self.editor.pane_tree.active_index();
            let leaves = self.editor.pane_tree.leaves_mut();
            if idx < leaves.len() {
                leaves[idx].viewport_height = rows.max(1);
                leaves[idx].viewport_width = cols.max(1);
            }
            if idx == active_idx {
                self.editor.viewport_height = rows.max(1);
                self.editor.ensure_cursor_visible();
            }
            self.editor.publish_render_state();
        }
    }

    /// PU.2: floating-popup inner-geometry hand-off — the GPUI peer of
    /// the TUI `App::set_popup_viewport`. The renderer is the sizing
    /// authority for the popup's inner rect, so it pushes the resolved
    /// `(rows, cols)` to the host; `build_cells_panes` reads
    /// `Editor::popup_viewport_{height,width}` to size the synthetic
    /// `PaneId::POPUP` `DisplayMatrix` the popup interior `EditorElement`
    /// paints from. Plain field writes (no special host handler), so it
    /// routes through `mutate_editor` (which republishes RS) rather than a
    /// typed actor command. Diff-then-send in the render loop keeps a
    /// steady-state popup at zero RPCs.
    pub fn set_popup_viewport(&mut self, rows: u32, cols: u32) {
        self.mutate_editor(move |e| {
            e.popup_viewport_height = rows.max(1);
            e.popup_viewport_width = cols.max(1);
        });
    }

    /// PU.5d: completion-docs side-popup inner-geometry hand-off — the peer
    /// of [`Self::set_popup_viewport`] for the second synthetic popup
    /// (`PaneId::COMPLETION_DOCS`). `build_cells_panes` reads
    /// `Editor::completion_docs_viewport_{height,width}` to size the docs
    /// `DisplayMatrix`. Diff-then-send in the render loop keeps churn down.
    pub fn set_completion_docs_viewport(&mut self, rows: u32, cols: u32) {
        self.mutate_editor(move |e| {
            e.completion_docs_viewport_height = rows.max(1);
            e.completion_docs_viewport_width = cols.max(1);
        });
    }

    /// Dismiss any active help popup. Phase 5.8.AE: routes
    /// through the host's `dismiss_popup` so popup state lands
    /// uniformly across both peers. Slice 3c.final.C: now goes
    /// through `Action::DismissPopup` instead of a direct host
    /// call.
    pub fn dismiss_popup(&mut self) {
        self.dispatch_action(lattice_host::action::Action::DismissPopup);
    }

    // display-line B4.2: the `refresh_highlights` no-op compatibility
    // shim was deleted. It carried no behaviour (the worker drove the
    // span cell that was itself deleted) and had no callers. Syntax
    // colour flows through the cells / `DisplayMatrix` substrate;
    // overlay backgrounds through `lattice_host::overlay_worker`.

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
        // Slice 3c.final.C: route through `Action::EnsureCursorVisible`
        // so the per-frame mutation goes via dispatch. Tail
        // publishes RS — no manual publish needed.
        self.dispatch_action(lattice_host::action::Action::EnsureCursorVisible);
    }

    // Slice 3c.final.E.swap: `finalize_boot` retired. Its body
    // (rebuild_option_cache / activate_major_for_buffer_kind /
    // publish_document_opened_for_active / ensure_subsystem_buffers
    // / load_persistent_config / apply_per_language_toml_overrides)
    // was inlined into `GpuiApp::new`, running directly on the
    // owned Editor BEFORE the actor spawns — the App-side
    // wrappers would have funnelled through the actor mailbox
    // which doesn't exist yet at that point in construction.
    // `rebuild_gpui_theme` runs post-actor against the published
    // RS theme.

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
                self.mutate_editor(move |e| e.fan_out_did_change_configuration(&server_id));
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
                // Phase 5.8.AE: route through host's display_buffer
                // so the popup state lands on `editor.popup_buffer`.
                // The binary's render reads `editor.popup_buffer` +
                // `editor.popup_help()` instead of the now-retired
                // `popup_content` field — keeping the popup state
                // unified across both peers.
                let lattice_host::dispatch::DisplayBufferRequest { content, category } = *req;
                tracing::debug!(
                    category = ?category,
                    title = %content.buffer.title,
                    "DisplayBuffer signal: routing through editor.display_buffer"
                );
                let (_id, mode_signals) =
                    self.mutate_editor_with(move |e| e.display_buffer(content, category));
                // Mode-activate signals don't recurse meaningfully
                // for the popup paths (no further DisplayBuffer);
                // drain through the handler so any ThemeChanged
                // etc. still propagates.
                for s in mode_signals {
                    tracing::debug!(signal = ?s, "popup mode-activate cascade signal");
                }
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
    /// `chord_capture` reads the published `ad().chord_capture` flag (the
    /// host computes it via `Editor::chord_capture_active` each publish), so
    /// the actor-based peer doesn't need a live `&Editor`. With it,
    /// `:describe-key` (and any `ArgKind::Chord` arg) captures the next
    /// keystroke instead of dispatching it — was hard-wired `false`.
    pub fn dispatch_chord(&mut self, chord: KeyChord) -> DispatchOutcome {
        // Investigation 2026-05-22: trace partial_chord at
        // dispatch_chord entry so we can see if it survives
        // publishes between keystrokes. info! so it lands in
        // *messages* without needing RUST_LOG.
        {
            let pre_partial = self.render_state.load().translator.partial_chord.clone();
            tracing::debug!(
                "[chord-trace] CHORD {:?} partial_chord_from_rs={:?}",
                chord,
                pre_partial,
            );
        }
        let action = {
            // Slice 3c.atomic.K: read value-typed TranslateContext
            // inputs through the published render-state snapshot
            // (`ad()`). Parallel of the TUI peer's runtime.rs
            // migration (3c.atomic.J). The borrowed-reference
            // inputs (`builtins`, `keymap`, `partial_chord`) still
            // come off `self.editor` -- they need `&` lifetimes
            // the published state doesn't carry; they migrate
            // when the actor swap (3c.final) replaces `editor`
            // with a handle.
            // Slice 3c.final.E.5: translator inputs via published
            // substate (same pattern as TUI runtime.rs).
            let ad = self.ad();
            let translator = self.render_state.load().translator.clone();
            let insert_completion_open = self.render_state.load().completion.insert.is_some();
            let ctx = TranslateContext {
                modal: ad.modal,
                builtins: &translator.builtins,
                pending_count: ad.pending_count,
                op_count: ad.op_count,
                recording_macro: ad.macro_recording,
                active_buffer: ad.buffer_kind,
                completion_open: ad.completion_open,
                chord_capture: ad.chord_capture,
                picker_open: ad.picker_open,
                insert_completion_open,
                snippet_active: ad.snippet_active,
                terminal_insert_active: ad.terminal_insert_active,
                terminal_esc_exits: ad.terminal_esc_exits,
                terminal_app_cursor_keys: ad.terminal_app_cursor_keys,
                terminal_insert_exit_pending: ad.terminal_insert_exit_pending,
                terminal_visual_active: ad.terminal_visual_active,
                keymap: &translator.keymap,
                partial_chord: &translator.partial_chord,
                active_minor_modes: &translator.active_minor_modes,
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
        // fields the resolved element table sources today. T.11.0b
        // closed the last canvas gap: editor bg/fg, the block-cursor
        // inversion, and the popup surface now read from the palette-
        // driven `editor.*` / `ui.popup.background` elements (below), so
        // a `:colorscheme` / palette swap recolors the whole canvas. The
        // wiring is driven by `RendererSignal::ThemeChanged` firing this
        // method, so a palette/theme swap flows through automatically.
        // Slice 3c.final.E.5: theme via published top-level field.
        // T.9: the pane-chrome STYLE fields moved off the host `Theme`
        // onto theme elements; read them from the published resolved
        // table (`resolved_theme.get(theme_ids.<elem>)`) which already
        // reflects any `:set ui.*` registry overrides.
        let rs = self.render_state.load();
        let resolved: &lattice_host::ui::theme::ResolvedTheme = &rs.resolved_theme;
        let ids: &lattice_host::ui::theme::BuiltinElementIds = &rs.theme_ids;
        let defaults = GpuiTheme::default();

        // Status line ↔ `pane.status.active` element (mirrors the
        // TUI's active-pane status row).
        let pane_status_active = resolved.get(ids.pane_status_active);
        if let Some(bg) = pane_status_active.bg {
            self.theme.status_background = bg.to_rgb_u32(defaults.status_background);
        }
        if let Some(fg) = pane_status_active.fg {
            self.theme.status_foreground = fg.to_rgb_u32(defaults.status_foreground);
        }
        // Popup border ↔ `pane.separator` element (same conceptual
        // role: thin accent line between visual regions).
        if let Some(fg) = resolved.get(ids.pane_separator).fg {
            self.theme.popup_border = fg.to_rgb_u32(defaults.popup_border);
        }
        // Popup header title / hint ↔ `ui.popup.title` / `ui.popup.hint`
        // (shared with the TUI peer so the accent is themeable + identical).
        if let Some(fg) = resolved.get(ids.ui_popup_title).fg {
            self.theme.popup_title = fg.to_rgb_u32(defaults.popup_title);
        }
        if let Some(fg) = resolved.get(ids.diagnostic_warning).fg {
            self.theme.notification_warn = fg.to_rgb_u32(defaults.notification_warn);
        }
        if let Some(fg) = resolved.get(ids.diagnostic_error).fg {
            self.theme.notification_error = fg.to_rgb_u32(defaults.notification_error);
        }
        if let Some(fg) = resolved.get(ids.ui_popup_hint).fg {
            self.theme.popup_hint = fg.to_rgb_u32(defaults.popup_hint);
        }
        // IG.4: guide colours ↔ `indent.guide` / `indent.guide.active`,
        // the same two elements the TUI peer resolves, so a
        // `:colorscheme` recolours the rules in both renderers together.
        if let Some(fg) = resolved.get(ids.indent_guide).fg {
            self.theme.indent_guide = fg.to_rgb_u32(defaults.indent_guide);
        }
        if let Some(fg) = resolved.get(ids.indent_guide_active).fg {
            self.theme.indent_guide_active = fg.to_rgb_u32(defaults.indent_guide_active);
        }
        // T.11.0b: source the canvas (window bg/fg, block-cursor
        // inversion, popup surface) from the resolved table so a
        // `:colorscheme` / palette swap recolors the whole canvas — the
        // light-theme seam. Each falls back to the GpuiTheme default
        // (Catppuccin Mocha) when the element leaves the channel unset,
        // keeping the default render byte-identical.
        self.theme.background = resolved
            .get(ids.editor_background)
            .bg
            .map(|c| c.to_rgb_u32(defaults.background))
            .unwrap_or(defaults.background);
        self.theme.foreground = resolved
            .get(ids.editor_foreground)
            .fg
            .map(|c| c.to_rgb_u32(defaults.foreground))
            .unwrap_or(defaults.foreground);
        let editor_cursor = resolved.get(ids.editor_cursor);
        self.theme.cursor_background = editor_cursor
            .bg
            .map(|c| c.to_rgb_u32(defaults.cursor_background))
            .unwrap_or(defaults.cursor_background);
        self.theme.cursor_foreground = editor_cursor
            .fg
            .map(|c| c.to_rgb_u32(defaults.cursor_foreground))
            .unwrap_or(defaults.cursor_foreground);
        self.theme.popup_background = resolved
            .get(ids.ui_popup_background)
            .bg
            .map(|c| c.to_rgb_u32(defaults.popup_background))
            .unwrap_or(defaults.popup_background);
        // Font family + size: read live from the published options
        // config so `:set ui.font_family=...` takes effect on the
        // next frame without restarting.
        let config = &self.render_state.load().options.config;
        if let Some(family) = config.get_typed::<lattice_host::ui::theme_options::UiFontFamily>() {
            self.theme.font_family = (**family).to_owned();
        }
        if let Some(size) = config.get_typed::<lattice_host::ui::theme_options::UiFontSize>() {
            self.theme.font_size_pt = (*size).clamp(4, 96) as u32;
        }
        if let Some(ligatures) = config.get_typed::<lattice_host::ui::theme_options::UiLigatures>()
        {
            self.theme.ligatures = *ligatures;
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
                let mut outcome = self.mutate_editor_with(|e| e.do_picker_accept());
                outcome.consumed = true;
                for s in std::mem::take(&mut outcome.renderer_signals) {
                    self.handle_renderer_signal(s);
                }
                return outcome;
            }
            // MG.29 lockstep: `<Esc>` routes here now, and this peer
            // intercepts the dismiss family rather than letting it reach
            // the generic path — so without this arm the unwind would
            // work in the TUI and do nothing in GPUI.
            Action::TransientDismiss => {
                let signals = self.mutate_editor_with(|e| {
                    let popped = e
                        .picker
                        .as_mut()
                        .map(|p| p.transient_unwind())
                        .unwrap_or(false);
                    if popped {
                        Vec::new()
                    } else {
                        e.do_picker_dismiss()
                    }
                });
                let outcome = DispatchOutcome {
                    consumed: true,
                    renderer_signals: signals.clone(),
                    ..Default::default()
                };
                for s in signals {
                    self.handle_renderer_signal(s);
                }
                return outcome;
            }
            Action::PickerDismiss => {
                let signals = self.mutate_editor_with(|e| e.do_picker_dismiss());
                let outcome = DispatchOutcome {
                    consumed: true,
                    renderer_signals: signals.clone(),
                    ..Default::default()
                };
                for s in signals {
                    self.handle_renderer_signal(s);
                }
                return outcome;
            }
            _ => {}
        }
        let mut outcome = self.mutate_editor_with(move |e| e.dispatch(action));
        let mut pending: std::collections::VecDeque<Action> =
            outcome.next_actions.drain(..).collect();
        while let Some(follow_up) = pending.pop_front() {
            let mut next_out = self.mutate_editor_with(move |e| e.dispatch(follow_up));
            pending.extend(next_out.next_actions.drain(..));
            outcome.effects.append(&mut next_out.effects);
            outcome
                .renderer_signals
                .append(&mut next_out.renderer_signals);
            outcome.consumed |= next_out.consumed;
            if self.render_state.load().lifecycle.should_quit {
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
        // Signals queued on the editor rather than returned — the
        // `ModeActivator` trait surface and `Editor::activate_buffer`,
        // both of which complete work whose signature cannot hand the
        // cascade back. Folded in beside the outcome's own so neither
        // producer depends on a caller remembering it. TUI parity:
        // `app/dispatch.rs` drains at the same point.
        let mut signals = self.mutate_editor_with(|e| e.drain_pending_renderer_signals());
        signals.append(&mut std::mem::take(&mut outcome.renderer_signals));
        for signal in signals.iter().cloned() {
            self.handle_renderer_signal(signal);
        }
        // Restore the list on the outcome so the returned value
        // matches the historical TUI shape (App::apply also
        // hands these to the caller after fan-out).
        outcome.renderer_signals = signals;
        // Phase 5.8.AF.5 / Slice X1: drain pending LSP / event /
        // mode-lifecycle results here at the keystroke-driven
        // dispatch tail rather than in the per-frame body
        // (`crates/lattice-ui-gpui/src/window.rs::Render::render`).
        // Paramount goal #1 forbids I/O / event drain on the UI
        // thread; the renderer body is the UI thread.
        // `run_tick_pending` is the host aggregator that polls
        // ~30 channels for async results -- on a busy frame
        // (file open) it can take 49ms, which is 6x over the
        // one-frame ceiling (8.3 ms at 120Hz). Running it
        // here makes that cost happen during the keystroke that
        // caused the work (the open) instead of on the next
        // paint after open. The post-X1 perf trace expects
        // `tick_us` in `lattice_gpui::perf` to drop to ~0 once
        // dispatch tails take over the drain.
        //
        // Idle LSP arrivals (response with no keystroke in
        // flight) are NOT drained until the next keystroke:
        // see slice X1b (`docs/dev/operations/render-thread-
        // discipline-remediation.md` §X1b) for the wake-bridge
        // that closes that gap.
        let tick_signals = self.mutate_editor_with(|e| e.run_tick_pending());
        for signal in tick_signals {
            self.handle_renderer_signal(signal);
        }
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
            // AP.0.2: a declined effect is consumed by the dispatcher
            // (fall-through) and never surfaces here; no-op for parity
            // with the TUI peer ([[feedback_tui_gpui_parity]]).
            Effect::Declined
            | Effect::None
            | Effect::Edits(_)
            // CR.0: host's `handle_effect` translates `ApplyEdit` into
            // `Action::ApplyEdit`; the renderer has nothing to do (parity
            // with the TUI peer per [[feedback_tui_gpui_parity]]).
            | Effect::ApplyEdit { .. }
            // XF.1: host-applied, parity with the TUI peer and with
            // `ApplyEdit` above — the host owns path→buffer, the insert and
            // the cut; there is nothing renderer-coupled in any of it.
            | Effect::WriteToFile { .. }
            | Effect::CursorMove(_)
            // MG.18d: the host applies (or drops) the targeted cursor
            // move in `handle_effect`; nothing for the renderer to do,
            // parity with `CursorMove` above.
            | Effect::CursorMoveIn { .. }
            | Effect::SelectionChange(_)
            | Effect::EnterMode(_)
            | Effect::Yank { .. }
            | Effect::SetOption { .. }
            | Effect::SetLocalOption { .. }
            | Effect::SetGlobalOption { .. }
            // T.9.b: `:colorscheme` swaps the registry palette/overrides
            // host-side in `editor.handle_effect`; the renderer rebuilds
            // off the emitted `RendererSignal::ThemeChanged`, so there's
            // nothing to apply here (parity with `SetOption`'s `ui.*`).
            | Effect::SetColorscheme(_)
            | Effect::ClearSearchHighlight
            | Effect::Echo { .. }
            | Effect::ShowDiagnosticsPopup { .. }
            // L7: LSP nav requests are host-applied (`editor.lsp_request`).
            | Effect::Lsp(_)
            | Effect::EchoRegisters
            | Effect::EchoMarks
            | Effect::ListBuffers
            | Effect::DescribeBuffer
            | Effect::ListKeymap
            | Effect::DescribeOption { .. }
            | Effect::DescribeElement { .. }
            | Effect::ListOptions
            | Effect::DescribePluginApi { .. }
            | Effect::ListPluginApis
            | Effect::ListCommands
            | Effect::DescribePlugin { .. }
            | Effect::ListPlugins
            | Effect::DescribeOptionResolution { .. }
            | Effect::DescribeEvents
            | Effect::DescribeEvent { .. }
            | Effect::DescribeDiff
            | Effect::DiffOpen
            // DB.2: host-applied via handle_effect; peer no-op.
            | Effect::OpenDashboard
            | Effect::DiffOff { .. }
            | Effect::Diffthis
            | Effect::Diffsplit { .. }
            | Effect::DiffGetCmd { .. }
            | Effect::DiffPutCmd { .. }
            | Effect::DiffAccept
            | Effect::DiffReject
            | Effect::DiffAcceptAll
            | Effect::DiffRejectAll
            // D-fix.6: session-scoped diff close is host-applied (the
            // renderer no-ops, parity with the TUI peer's classifier).
            | Effect::CloseSessionDiffs { .. }
            | Effect::CloseAllSessionDiffs { .. }
            | Effect::NextHunk
            | Effect::PrevHunk
            | Effect::BufferNext
            | Effect::BufferPrev
            | Effect::BufferDelete { .. }
            | Effect::ListModes
            | Effect::DescribeMode { .. }
            | Effect::DescribeActiveModes
            | Effect::DescribeActiveBindings
            | Effect::Customize { .. }
            | Effect::ListDiagnostics
            | Effect::ListErrors
            | Effect::DeleteCurrentLine
            | Effect::Substitute { .. }
            | Effect::DescribeCommand { .. }
            | Effect::Apropos { .. }
            | Effect::DescribeKey { .. }
            | Effect::AppAction(_)
            // BC.8c: showDocument open effects are host-applied in
            // `Editor::handle_effect` (TUI/GPUI parity); the peer no-ops them.
            | Effect::OpenExternalUri { .. }
            | Effect::OpenBufferAtColumn { .. }
            // I5.1: terminal spawn is host-applied; the peer no-ops it.
            | Effect::SpawnTerminal { .. }
            // D-fix.4: terminal input (`:claude-interrupt`) is host-applied.
            | Effect::TerminalInput(_)
            // I3/BC.8c follow-up: SaveBuffer host-applied (reuses do_write).
            | Effect::SaveBuffer { .. }
            | Effect::RecordJump
            // Host-applied cd/pwd effects (GPUI parity with TUI peer).
            | Effect::ChangeDir(_)
            | Effect::PrintWorkingDir
            | Effect::PrintProjectRoot => {}
            // Renderer-coupled effects whose body lives host-side.
            Effect::QuitEditor { force, scope } => {
                self.mutate_editor(move |e| e.do_quit(force, scope))
            }
            Effect::OpenBuffer { path, force } => self.apply_open_buffer(path, force),
            // M.10.3 bug fix (2026-06-03): atomic open-and-position.
            // GPUI parity with TUI per [[feedback_tui_gpui_parity]].
            Effect::OpenBufferAt { path, position, force } => {
                self.apply_open_buffer(path, force);
                self.mutate_editor(move |e| {
                    e.set_cursor_clamped(position);
                });
            }
            // I3/BC.8c follow-up: `SaveBuffer` is now HOST-applied in
            // `Editor::handle_effect` (reuses `do_write`; works on the
            // off-keystroke inbound tick path) — the peer arm is retired to the
            // grouped no-op above (TUI/GPUI parity). `:w`'s full LSP fan-out
            // (BeforeSave / willSave / didSave / …) was already host-resident.
            // Phase 5.8.AD.2: LSP commands whose bodies migrated.
            Effect::LspStatus => {
                let signals = self.mutate_editor_with(|e| e.do_lsp_status());
                for s in signals {
                    self.handle_renderer_signal(s);
                }
            }
            // EP.4: GPUI parity for the manual diagnostics pull.
            Effect::LspDiagnosticsToErrorList => {
                self.mutate_editor(|e| e.do_lsp_diagnostics_to_error_list());
            }
            Effect::LspRestart { server_id } => {
                self.mutate_editor(move |e| e.do_lsp_restart(&server_id));
            }
            Effect::LspExpandRegion => self.mutate_editor(|e| e.do_lsp_expand_region()),
            Effect::LspShrinkRegion => self.mutate_editor(|e| e.do_lsp_shrink_region()),
            Effect::LspProgressCancel { server_id } => {
                self.mutate_editor(move |e| e.do_lsp_progress_cancel(server_id.as_deref()));
            }
            Effect::SetLspLogLevel { server_id, level } => {
                self.mutate_editor(move |e| e.do_set_lsp_log_level(server_id.as_deref(), &level));
            }
            Effect::LspLogClear { server_id } => {
                self.mutate_editor(move |e| e.do_lsp_log_clear(server_id.as_deref()));
            }
            Effect::LspCodeAction => self.mutate_editor(|e| e.do_lsp_code_action_request()),
            // IN.8b: the LSP-independent cascade. Kept in lockstep
            // with the TUI arm per the cross-renderer rule.
            Effect::Format => self.mutate_editor(|e| e.do_format_request()),
            Effect::LspFormat => self.mutate_editor(|e| e.do_lsp_format_request(false)),
            Effect::LspFormatRange => self.mutate_editor(|e| e.do_lsp_format_request(true)),
            Effect::LspRename { new_name } => {
                self.mutate_editor(move |e| e.do_lsp_rename_request(&new_name))
            }
            Effect::LspIncomingCalls => {
                self.mutate_editor(|e| e.do_lsp_call_hierarchy_request(false))
            }
            Effect::LspOutgoingCalls => {
                self.mutate_editor(|e| e.do_lsp_call_hierarchy_request(true))
            }
            Effect::LspSupertypes => self.mutate_editor(|e| e.do_lsp_type_hierarchy_request(false)),
            Effect::LspSubtypes => self.mutate_editor(|e| e.do_lsp_type_hierarchy_request(true)),
            Effect::LspMoniker => self.mutate_editor(|e| e.do_lsp_moniker_request()),
            Effect::OpenLspLog { server_id } => {
                self.mutate_editor(move |e| e.do_open_lsp_log(server_id.as_deref()));
            }
            Effect::OpenAiLog { session } => {
                self.mutate_editor(move |e| e.do_open_ai_log(session.as_deref()));
            }
            Effect::ExportPluginApi { format } => {
                self.mutate_editor(move |e| e.do_export_plugin_api(format.as_deref()));
            }
            Effect::OpenSyntheticBuffer { name, mode_id } => {
                self.mutate_editor(move |e| e.open_synthetic_buffer(&name, &mode_id));
            }
            // MG.50: peer of `OpenBufferAt` for synthetic buffers — open
            // then position, in one arm, so the caret lands on the buffer
            // that was just opened rather than on whatever preceded it.
            Effect::OpenSyntheticBufferAt {
                name,
                mode_id,
                position,
            } => {
                self.mutate_editor(move |e| {
                    e.open_synthetic_buffer(&name, &mode_id);
                    e.set_cursor_clamped(position);
                });
            }
            Effect::OpenLspTraceLog { server_id } => {
                self.mutate_editor(move |e| e.do_open_lsp_trace_log(server_id.as_deref()));
            }
            Effect::ToggleLspTrace { server_id } => {
                self.mutate_editor(move |e| e.do_toggle_lsp_trace(&server_id));
            }
            Effect::LspServerLogListing => self.mutate_editor(|e| e.do_lsp_server_log_listing()),
            Effect::LspCodeLens => self.mutate_editor(|e| e.do_lsp_code_lens_picker()),
            Effect::LspColorPresentation => self.mutate_editor(|e| e.do_lsp_color_presentation()),
            // Phase 5.8.AD.4: completion / signature / snippet
            // entry points are host-resident; both peers reach
            // them through the same dispatch.
            Effect::LspSignatureHelp => self.mutate_editor(|e| e.lsp_signature_help_request()),
            Effect::LspComplete => self.mutate_editor(|e| e.lsp_completion_request()),
            // SN.3c.1: mode-owned `<C-x><C-s>` emits the trigger
            // range; the host resolves + expands (peer parity with
            // TUI). `feedback_tui_gpui_parity`.
            Effect::ExpandSnippet { replace_range } => {
                self.mutate_editor(move |e| e.expand_snippet_from_range(replace_range))
            }
            // 5.5.LSP.5: symbol helpers host-side; both peers reach
            // them through the same dispatch.
            Effect::LspDocumentSymbol => self.mutate_editor(|e| e.lsp_document_symbol_request()),
            Effect::LspWorkspaceSymbol { query } => {
                self.mutate_editor(move |e| e.lsp_workspace_symbol_request(&query));
            }
            // Phase 5.8.AD.5: describe / hover / tutor / customize
            // entries now host-resident.
            Effect::OpenHelpTopic { topic } => {
                let signals =
                    self.mutate_editor_with(move |e| e.do_open_help_topic(topic.as_deref()));
                for s in signals {
                    self.handle_renderer_signal(s);
                }
            }
            Effect::OpenHover { markdown } => {
                let signals = self.mutate_editor_with(move |e| e.do_open_hover(&markdown));
                for s in signals {
                    self.handle_renderer_signal(s);
                }
            }
            Effect::DismissPopup => {
                // DismissPopup is App-side popup state today; GPUI's
                // popup_content covers it directly.
                self.dismiss_popup();
            }
            Effect::BuryBuffer => {
                // Distinct from DismissPopup: a full-pane synthetic
                // buffer swapped the active document, so returning has
                // to swap it back — which `bury_buffer` does through
                // `activate_buffer`. Dismissing a popup would leave the
                // active document pointing at the buried buffer and
                // paint it over the file.
                self.mutate_editor(|e| {
                    e.bury_buffer();
                });
            }
            Effect::OpenPopup {
                name,
                mode_id,
                placement,
                focus,
            } => {
                // Content-agnostic popup open (popup-api.md §4.3): delegate to
                // the host primitive, same as the TUI peer. Signals are always
                // empty today, so the return is discarded.
                self.mutate_editor(move |e| {
                    e.open_popup_named(&name, &mode_id, placement, focus);
                });
            }
            Effect::Tutor { lesson } => {
                let signals = self.mutate_editor_with(move |e| e.do_tutor(lesson));
                for s in signals {
                    self.handle_renderer_signal(s);
                }
            }
            Effect::OpenBufferPicker => {
                let signals = self.mutate_editor_with(|e| e.do_open_buffer_picker());
                for s in signals {
                    self.handle_renderer_signal(s);
                }
            }
            // Phase 5.8.AF.3: `:messages` activates the
            // `*messages*` Document buffer host-side.
            Effect::OpenMessages => {
                let signals = self.mutate_editor_with(|e| e.do_open_messages());
                for s in signals {
                    self.handle_renderer_signal(s);
                }
            }
            // Phase 5.8.AF.3: diagnostic navigation host-side.
            Effect::NextDiagnostic => self.mutate_editor(|e| e.do_next_diagnostic()),
            Effect::PrevDiagnostic => self.mutate_editor(|e| e.do_prev_diagnostic()),
            // Phase 5.8.AF.3: `:reload-snippets` host-side.
            Effect::ReloadSnippets => self.mutate_editor(|e| e.do_reload_snippets()),
            // Phase 5.8.AF.3: `:toggle-mode` host-side. Returned
            // bool flags an unknown-mode-name miss (the host has
            // already echoed); GPUI has no further fan-out.
            Effect::ToggleMode { mode_name } => {
                let _ = self.mutate_editor_with(move |e| e.toggle_mode_by_name(&mode_name));
            }
            // Phase 5.8.AF.3: `:g` / `:v` host-side. Body effects
            // are drained through `apply_effect_gpui` so any
            // renderer-coupled tail (popup / picker fan-out) still
            // flows through this peer.
            Effect::Global {
                pattern,
                inverted,
                body,
            } => {
                // Slice 3c.final.E.swap: build outcome inside the
                // closure, return owned `DispatchOutcome` from
                // `mutate_editor_with`. Same pattern as TUI edit.rs.
                let mut out = self.mutate_editor_with(move |e| {
                    let mut out = lattice_host::dispatch::DispatchOutcome::default();
                    e.do_global(&pattern, inverted, body.as_ref(), &mut out);
                    out
                });
                for eff in std::mem::take(&mut out.effects) {
                    self.apply_effect_gpui(eff);
                }
            }
            // Phase 5.8.AF.3: `:picker <source>` host-side.
            Effect::OpenPicker { source, args } => {
                let signals = self.mutate_editor_with(move |e| e.open_picker(source, args));
                for s in signals {
                    self.handle_renderer_signal(s);
                }
            }
            // PU.6: confirmation dialog opens a transient picker for
            // y/n/q dismissal (TUI/GPUI parity per `do_confirm`).
            Effect::Confirm { prompt, yes_action, args } => {
                let signals = self.mutate_editor_with(move |e| {
                    let Some(cmd_reg) = e.services.get::<lattice_grammar::CommandRegistryHandle>()
                    else {
                        e.set_message(
                            lattice_host::action::EchoLevel::Error,
                            "confirm: command registry unavailable".to_string(),
                        );
                        return Vec::new();
                    };
                    let Some(cmd_id) = cmd_reg.load().id_by_name(&yes_action) else {
                        e.set_message(
                            lattice_host::action::EchoLevel::Error,
                            format!("confirm: unknown action `{yes_action}`"),
                        );
                        return Vec::new();
                    };
                    let spec = lattice_picker::confirm_transient_spec(&prompt, cmd_id);
                    // IX.1: seed the dialog's state with the
                    // yes-action's arguments so what fires on `y` is
                    // what the prompt named (parity with the TUI peer's
                    // `do_confirm`).
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
            // Fold audit fix: named transient menus (magit-dispatch /
            // magit-file-dispatch), resolved via the owning mode
            // crate's `TransientSourceRegistry` registration. TUI/GPUI
            // parity with `do_open_transient`.
            Effect::OpenTransient { source, args } => {
                // TR.2: the body is `Editor::open_named_transient` —
                // shared verbatim with the TUI peer rather than copied,
                // which is also what gives the guest-backed async build
                // path parity for free.
                let signals =
                    self.mutate_editor_with(move |e| e.open_named_transient(source, args));
                for s in signals {
                    self.handle_renderer_signal(s);
                }
            }
            // Generic one-line minibuffer text prompt. TUI/GPUI
            // parity with `do_open_prompt`.
            Effect::OpenPrompt {
                prompt,
                initial,
                on_submit_action,
                buffer_name,
            } => {
                let signals = self.mutate_editor_with(move |e| {
                    e.open_prompt_line(prompt, initial, on_submit_action, buffer_name)
                });
                for s in signals {
                    self.handle_renderer_signal(s);
                }
            }
            // Phase 5.8.AD.1: oil + file-tree migrated host-side
            // so `:e .` / `:Oil` / `:Tree` work in GPUI.
            Effect::OpenOil { dir } => {
                let signals = self.mutate_editor_with(move |e| e.do_open_oil(dir));
                for s in signals {
                    self.handle_renderer_signal(s);
                }
            }
            Effect::OpenFileTree { root } => {
                let signals = self.mutate_editor_with(move |e| e.do_open_file_tree(root));
                for s in signals {
                    self.handle_renderer_signal(s);
                }
            }
            Effect::CloseFileTree => {
                let signals = self.mutate_editor_with(|e| e.dismiss_file_tree());
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
        }
        // 5.8.AF.3 closeout: every renderer-neutral `Effect` is now
        // explicitly handled. The match is exhaustive — a future
        // variant becomes a compile error rather than a silent
        // runtime warning, which is the louder signal.
    }

    /// Wrapper around `editor.do_edit(path, force)` that translates
    /// the host's [`DoEditOutcome`] for the GPUI peer. Mirrors
    /// TUI's `App::do_edit`: `Directory` routes to `do_open_oil`,
    /// `Opened`/`Activated`/`Reloaded` fan signals through the
    /// renderer handler, `NoFileName`/`Failed` are silent (the
    /// host already echoed).
    fn apply_open_buffer(&mut self, path: Option<std::path::PathBuf>, force: bool) {
        use lattice_host::dispatch::DoEditOutcome;
        let outcome = self.mutate_editor_with(move |e| e.do_edit(path, force));
        match outcome {
            DoEditOutcome::NoFileName | DoEditOutcome::Failed => {}
            DoEditOutcome::Directory(dir) => {
                // Phase 5.8.AD.1: oil is now host-side; route the
                // directory `:e` through the same path the TUI peer
                // uses.
                let signals = self.mutate_editor_with(move |e| e.do_open_oil(Some(dir)));
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
        assert!(app.editor.mode_registry.load().iter_meta().next().is_some());
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

    /// CB.4 regression guard: `GpuiApp::new` must always leave a working
    /// `ClipboardHandle` registered and never panic through its
    /// clipboard-override path. The arboard override is `not(test)`-gated,
    /// so every test run — feature on or off, display or none — keeps CB.0's
    /// `FakeClipboard`, and `cargo test` can never clobber the developer's
    /// real system clipboard (the TUI peer's `clipboard::boot_backend`
    /// applies the same rule). Hence the hermeticity assertions below:
    /// starts empty, round-trips. The production arboard path is exercised
    /// by the `--features window` build on the dev machine.
    #[test]
    fn new_leaves_a_hermetic_clipboard_handle() {
        let app = GpuiApp::new(Document::empty());
        let clipboard = app
            .editor
            .services
            .get::<lattice_core::ClipboardHandle>()
            .expect("GpuiApp::new must leave a ClipboardHandle registered");
        assert_eq!(
            clipboard.read(),
            None,
            "a freshly booted test GpuiApp must expose an empty in-memory \
             clipboard -- a non-None read means the suite is talking to the \
             OS clipboard"
        );
        clipboard.write("cb4-gpui-probe".to_string());
        assert_eq!(clipboard.read(), Some("cb4-gpui-probe".to_string()));
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

    /// Test-coverage gap user flagged 2026-05-21: chord sequences
    /// like `gg` had no GPUI end-to-end test. The TUI peer covers
    /// the keymap-handler layer (key_harness_gg_jumps_to_first_line)
    /// but GPUI's specific dispatch_keystroke path — including
    /// partial_chord publish + reload between keys — wasn't
    /// exercised.
    ///
    /// This test drives `gg` through `dispatch_keystroke`, the
    /// SAME path GPUI's `on_key_down` uses. If `partial_chord`
    /// doesn't survive the publish-and-reload cycle between the
    /// two `g` presses, the second `g` won't see the first one
    /// as prefix and dispatch will fail.
    #[test]
    fn gpui_dispatch_keystroke_handles_gg_chord_sequence() {
        // Seed a multi-line document so `gg` has somewhere to
        // jump to.
        let doc = Document::from_text("alpha\nbeta\ngamma\ndelta\nepsilon\n");
        let mut app = GpuiApp::new(doc);
        // Move cursor down 2 lines so `gg` has work to do.
        app.dispatch_keystroke("j", false, false, false, false);
        app.dispatch_keystroke("j", false, false, false, false);
        assert_eq!(
            app.editor.cursor.line, 2,
            "cursor should be on line 2 before gg"
        );

        // First `g`: should absorb into partial_chord, NOT execute.
        let outcome1 = app.dispatch_keystroke("g", false, false, false, false);
        assert!(outcome1.is_some(), "first `g` should dispatch");
        assert_eq!(
            app.editor.partial_chord.len(),
            1,
            "first `g` should populate partial_chord"
        );
        assert_eq!(
            app.editor.cursor.line, 2,
            "first `g` alone should NOT move the cursor"
        );

        // Verify the published RS reflects the partial_chord update
        // — this is the key invariant. If RS still shows empty
        // partial_chord, the second `g` won't find the prefix and
        // dispatch silently fails.
        assert_eq!(
            app.render_state.load().translator.partial_chord.len(),
            1,
            "published RS must observe the partial_chord update before next keystroke"
        );

        // Second `g`: with [g] as prefix, trie resolves gg →
        // motion:goto-first-line.
        let outcome2 = app.dispatch_keystroke("g", false, false, false, false);
        assert!(outcome2.is_some(), "second `g` should dispatch");
        assert_eq!(
            app.editor.cursor.line, 0,
            "gg should jump to line 0; got line {}",
            app.editor.cursor.line
        );
        assert!(
            app.editor.partial_chord.is_empty(),
            "partial_chord should clear after gg resolves"
        );
    }

    /// PU.2: the GPUI floating-popup geometry hand-off. `set_popup_viewport`
    /// writes the popup's resolved inner `(rows, cols)` onto the editor so
    /// the host's `build_cells_panes` can size the synthetic `PaneId::POPUP`
    /// `DisplayMatrix` the popup interior `EditorElement` paints from — the
    /// GPUI peer of the TUI's `App::set_popup_viewport`. (Host-side coverage
    /// that these fields actually build the synthetic pane lives in
    /// `floating_popup_gets_synthetic_cells_pane_when_geometry_fed`; this
    /// just pins the GPUI plumbing + the zero→1 clamp.)
    #[test]
    fn set_popup_viewport_writes_editor_geometry_fields() {
        let mut app = GpuiApp::new(Document::empty());
        // Both 0 until the first feedback (the gate `popup_viewport_width
        // > 0` skips the synthetic pane until then).
        assert_eq!(app.editor.popup_viewport_height, 0);
        assert_eq!(app.editor.popup_viewport_width, 0);
        app.set_popup_viewport(18, 60);
        assert_eq!(app.editor.popup_viewport_height, 18);
        assert_eq!(app.editor.popup_viewport_width, 60);
        // A zero-sized matrix is meaningless; both axes clamp to >= 1.
        app.set_popup_viewport(0, 0);
        assert_eq!(app.editor.popup_viewport_height, 1);
        assert_eq!(app.editor.popup_viewport_width, 1);
    }

    /// Test-coverage gap (sibling to gg): `zz` / `zt` / `zb`.
    /// These also live behind a chord-prefix state machine; the
    /// `z` keystroke absorbs into partial_chord, then the second
    /// keystroke (`z`/`t`/`b`) selects the action.
    #[test]
    fn gpui_dispatch_keystroke_handles_zz_chord_sequence() {
        // Long enough document that zz centering would be
        // observable (we don't assert viewport position, just
        // that the action dispatched and partial_chord clears).
        let doc = Document::from_text("line\n".repeat(20));
        let mut app = GpuiApp::new(doc);

        let outcome1 = app.dispatch_keystroke("z", false, false, false, false);
        assert!(outcome1.is_some(), "first `z` should dispatch");
        assert_eq!(
            app.editor.partial_chord.len(),
            1,
            "first `z` should populate partial_chord"
        );
        assert_eq!(
            app.render_state.load().translator.partial_chord.len(),
            1,
            "published RS must observe the z partial_chord update"
        );

        let outcome2 = app.dispatch_keystroke("z", false, false, false, false);
        assert!(outcome2.is_some(), "second `z` should dispatch");
        assert!(
            app.editor.partial_chord.is_empty(),
            "partial_chord should clear after zz resolves"
        );
    }

    /// Slice 3c.atomic.K: `GpuiApp::ad()` reflects the modal
    /// transition driven by dispatch. Proves the renderer-side
    /// publish chain (`dispatch_chord` → `dispatch_action` →
    /// dispatch tail `publish_render_state()`) reaches the
    /// `GpuiApp.render_state` cell, so paint-time reads through
    /// `ad()` see the freshest editor state.
    #[test]
    fn gpui_app_ad_reflects_dispatched_state() {
        use lattice_host::chord::{KeyChord, KeyKind, KeyMods};
        let mut app = GpuiApp::new(Document::empty());
        // Boot publish hydrates ad() before any dispatch.
        assert_eq!(app.ad().modal, ModalState::Normal);
        app.dispatch_chord(KeyChord::new(KeyKind::Char('i'), KeyMods::NONE));
        assert_eq!(
            app.ad().modal,
            ModalState::Insert,
            "ad() must observe the post-dispatch modal state through render_state"
        );
    }

    /// LG.1: `rebuild_gpui_theme` propagates `ui.ligatures` to
    /// `GpuiTheme.ligatures`. Default is `true`; setting the option
    /// to `false` flips the field.
    #[test]
    fn rebuild_gpui_theme_propagates_ligatures() {
        let mut app = GpuiApp::new(Document::empty());
        // Default is true (ligatures on by default).
        assert!(app.theme.ligatures, "default ligatures should be true");
        // Override via the config registry that's already in the render state.
        let rs = app.render_state.load();
        rs.options
            .config
            .parse_and_set_command("ui.ligatures=off")
            .unwrap();
        drop(rs);
        app.rebuild_gpui_theme();
        assert!(
            !app.theme.ligatures,
            "ligatures should be false after ui.ligatures=off"
        );
        // Toggle back on.
        let rs = app.render_state.load();
        rs.options
            .config
            .parse_and_set_command("ui.ligatures=on")
            .unwrap();
        drop(rs);
        app.rebuild_gpui_theme();
        assert!(
            app.theme.ligatures,
            "ligatures should be true after ui.ligatures=on"
        );
    }

    /// Slice 3c.atomic.K: `set_viewport_height` clamps height
    /// to a minimum of 1 and republishes so `ad().viewport_height`
    /// observes the change. Matches the contract the TUI peer's
    /// `App::set_viewport_height` exposes (3c.atomic.D).
    #[test]
    fn gpui_app_set_viewport_height_publishes() {
        let mut app = GpuiApp::new(Document::empty());
        app.set_viewport_height(24);
        assert_eq!(app.ad().viewport_height, 24);
        // Clamp: 0 becomes 1 (renderer's per-frame layout never
        // gives the buffer a zero-height pane, but the wrapper's
        // contract guarantees a usable lower bound regardless).
        app.set_viewport_height(0);
        assert_eq!(app.ad().viewport_height, 1);
    }
}
