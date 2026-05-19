//! GPUI window-opening entry point — [`run`] opens a native
//! GPUI window, constructs an [`EditorView`], drives the event
//! loop, and tears down cleanly on `editor.should_quit`.
//!
//! Phase 5.9 migration: the body used to live in
//! `src/bin/lattice_gpui.rs`. Moving it into the lib (behind
//! the `window` Cargo feature) lets `lattice-cli` route through
//! a single `lattice --gpu` flag without duplicating the
//! window setup. The `lattice-gpui` binary becomes a thin
//! shim: tracing init + CLI arg parse + [`run`].
//!
//! The whole module is `#[cfg(feature = "window")]`-gated by
//! `lib.rs`, so `cargo test -p lattice-ui-gpui` on a headless
//! host (without display libs) still works — gpui only links
//! when the `window` feature is on.
//!
//! ## Pipeline
//!
//! 1. [`run`] takes a [`Document`] (path-bearing or empty),
//!    constructs `gpui::Application::new()`, and opens a 720×480
//!    centered window whose root element is an [`EditorView`].
//! 2. [`EditorView::new`] calls `GpuiApp::new(document)`, which
//!    delegates to [`lattice_host::editor::Editor::boot`] for the
//!    full renderer-neutral setup (LSP, command + mode + snippet
//!    registries, syntax handle, buffer registry, persistent
//!    config, picker registry, ...).
//! 3. After `cx.open_window` returns, we do a second
//!    `window.update(cx, ...)` to focus the editor view + call
//!    `cx.activate(true)`. The focus call HAS to happen after
//!    `open_window` -- calling it inside the builder closure
//!    runs before gpui's focus tree is fully initialised and is
//!    silently dropped. The pattern matches gpui's
//!    `examples/input.rs`.
//! 4. Keystrokes flow `on_key_down` → `dispatch_keystroke` →
//!    `editor.dispatch` → renderer signals + next-action chain;
//!    `cx.notify()` schedules a repaint; `cx.stop_propagation()`
//!    prevents the platform default action map from claiming the
//!    chord.
//! 5. `editor.should_quit` → `cx.quit()` tears the application
//!    down.
//!
//! ## What renders today
//!
//! - **Document area**: per-character cells laid out under
//!   `flex_row` lines + `flex_col` columns on a monospace font,
//!   with a left-side line-number gutter (5.8.D). Syntax
//!   highlights (5.8.A) color each span via the Catppuccin Mocha
//!   palette when an `editor.syntax` handle exists.
//! - **Vim-style cursor**: three shapes via [`CursorShape`]
//!   (block / bar / underline) based on `editor.modal` (5.7.B.11).
//! - **Status line / minibuffer**: shows
//!   `<MODAL>   <path>[+]   L:<n>  C:<n>` (5.8.B), or the
//!   in-progress `:command` / `/pattern` line when in Command /
//!   Search modes (5.8.C).
//! - **DisplayBuffer popup** (5.7.B.10): centered overlay for
//!   `:ls`, `:describe-buffer`, etc. Esc dismisses.
//! - **Picker overlay** (5.8.E): for `:picker files`,
//!   `:picker commands`.
//! - **Insert-mode completion popup** (5.8.F): top-right
//!   anchored panel.

use anyhow::{Context as _, Result};
use gpui::{
    App, AppContext, Application, Bounds, Context, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyDownEvent, ParentElement, Render, Styled, Window, WindowBounds, WindowOptions,
    div, px, rgb, size,
};
use lattice_core::Document;
use lattice_core::ui::pane::{PaneNode, PaneState};
use lattice_grammar::ModalState;
use lattice_host::cursor_shape::CursorShape;
use lattice_syntax::Style as SyntaxStyle;

use crate::{GpuiApp, GpuiTheme};

/// Map a `lattice_syntax::Style` to a Catppuccin Mocha hex
/// palette value. Phase 5.8.A: keeps the palette inline so
/// the window can color highlighted spans without depending on
/// any host-side palette plumbing yet.
fn syntax_color(style: SyntaxStyle) -> u32 {
    match style {
        SyntaxStyle::Default => 0xcdd6f4,
        SyntaxStyle::Comment | SyntaxStyle::LineComment => 0x6c7086,
        SyntaxStyle::String => 0xa6e3a1,
        SyntaxStyle::Keyword => 0xcba6f7,
        SyntaxStyle::Type => 0xf9e2af,
        SyntaxStyle::Number => 0xfab387,
        SyntaxStyle::Function => 0x89b4fa,
        SyntaxStyle::Constant => 0xfab387,
        SyntaxStyle::Variable => 0xcdd6f4,
        SyntaxStyle::Operator => 0x94e2d5,
        SyntaxStyle::Punctuation => 0x9399b2,
        SyntaxStyle::Attribute => 0xf38ba8,
        SyntaxStyle::Heading1 => 0xf38ba8,
        SyntaxStyle::Heading2 => 0xfab387,
        SyntaxStyle::Heading3 => 0xf9e2af,
        SyntaxStyle::Heading4 => 0xa6e3a1,
        SyntaxStyle::Heading5 => 0x89b4fa,
        SyntaxStyle::Heading6 => 0xcba6f7,
        SyntaxStyle::Bold => 0xeba0ac,
        SyntaxStyle::Italic => 0xf5c2e7,
        SyntaxStyle::Link => 0x89b4fa,
        SyntaxStyle::Url => 0x74c7ec,
        SyntaxStyle::MarkupRaw => 0x6c7086,
        SyntaxStyle::Markup => 0x9399b2,
    }
}

/// Walk `spans` (one entry per line) and find the `Style` that
/// covers `byte`. Spans are non-overlapping so a linear scan is
/// sufficient and matches what the TUI peer does for the same
/// lookup.
fn style_at(spans: &[lattice_syntax::StyledSpan], byte: usize) -> SyntaxStyle {
    for span in spans {
        if byte >= span.start && byte < span.end {
            return span.style;
        }
    }
    SyntaxStyle::Default
}

/// Phase 5.8.AF.5 / Slice X3: per-char styling signature used by
/// `paint_pane`'s run-collapsing inner loop. Two adjacent chars
/// with the same `CellStyle` render as a single styled div
/// carrying a string of length N instead of N divs of length 1.
///
/// Fields are stripped to the colours that actually paint:
///   - `fg`: text colour (Catppuccin palette u32);
///   - `bg`: optional background colour (set by visual / search /
///     substitute / doc-highlight overlays);
///   - `underline`: optional bottom-border colour (set by diagnostic
///     severity at this byte).
///
/// `PartialEq` is derived so `run.style == new_style` is one
/// pointer-sized comparison. The compute fn collapses the
/// substitute > visual > current_match > hlsearch > doc_highlight
/// precedence stack into the final `(fg, bg)` pair so the loop
/// doesn't re-walk the precedence per char.
#[derive(Clone, Copy, PartialEq)]
struct CellStyle {
    fg: u32,
    bg: Option<u32>,
    underline: Option<u32>,
}

impl CellStyle {
    #[allow(clippy::too_many_arguments)]
    fn compute(
        span_style: SyntaxStyle,
        in_visual: bool,
        in_current_match: bool,
        in_hlsearch: bool,
        in_substitute: bool,
        in_doc_highlight: bool,
        diagnostic_color: Option<u32>,
        selection_bg: u32,
        current_match_bg: u32,
        current_match_fg: u32,
        hlsearch_bg: u32,
        substitute_bg: u32,
        substitute_fg: u32,
        doc_highlights_bg: u32,
    ) -> Self {
        // Overlay precedence (highest first):
        //   substitute > visual > current_match > hlsearch > doc_highlight.
        // `fg` defaults to the syntax colour; only substitute and
        // current_match override it.
        let syntax_fg = syntax_color(span_style);
        let (fg, bg) = if in_substitute {
            (substitute_fg, Some(substitute_bg))
        } else if in_visual {
            (syntax_fg, Some(selection_bg))
        } else if in_current_match {
            (current_match_fg, Some(current_match_bg))
        } else if in_hlsearch {
            (syntax_fg, Some(hlsearch_bg))
        } else if in_doc_highlight {
            (syntax_fg, Some(doc_highlights_bg))
        } else {
            (syntax_fg, None)
        };
        Self {
            fg,
            bg,
            underline: diagnostic_color,
        }
    }
}

/// Render a collapsed run of same-styled chars as a single styled
/// `gpui::Div` carrying the run's text. Mirrors what the per-char
/// loop in pre-X3 `paint_pane` did for ONE char, but applied to
/// the whole run.
fn run_to_cell(style: CellStyle, text: String) -> gpui::Div {
    let mut cell = div().text_color(rgb(style.fg)).child(text);
    if let Some(bg) = style.bg {
        cell = cell.bg(rgb(bg));
    }
    if let Some(uc) = style.underline {
        cell = cell.border_b_2().border_color(rgb(uc));
    }
    cell
}

/// Resolve the diagnostic gutter glyph + colour for a severity by
/// reading the host's `Theme` (the same source the TUI peer uses
/// via `theme::diagnostic_glyph_and_style`). Returns the glyph
/// character and a Catppuccin-compatible `0xRRGGBB` color the
/// renderer applies to that cell.
///
/// Phase 5.8.I introduced this as hardcoded matches; 5.8.N hoists
/// the source of truth to `host_theme` so `:set ui.diagnostics.*`
/// overrides flow to both renderer peers identically. Falls back
/// to overlay2 grey on Unknown severities (rare; future LSP versions
/// could add new variants).
fn diagnostic_glyph_and_color(
    host_theme: &lattice_host::ui::theme::Theme,
    severity: lattice_lsp::DiagnosticSeverity,
) -> (char, u32) {
    let (glyph, style) = match severity {
        lattice_lsp::DiagnosticSeverity::ERROR => (
            host_theme.diagnostic_error_glyph,
            host_theme.diagnostic_error_style,
        ),
        lattice_lsp::DiagnosticSeverity::WARNING => (
            host_theme.diagnostic_warning_glyph,
            host_theme.diagnostic_warning_style,
        ),
        lattice_lsp::DiagnosticSeverity::INFORMATION => (
            host_theme.diagnostic_info_glyph,
            host_theme.diagnostic_info_style,
        ),
        lattice_lsp::DiagnosticSeverity::HINT => (
            host_theme.diagnostic_hint_glyph,
            host_theme.diagnostic_hint_style,
        ),
        _ => (
            host_theme.diagnostic_info_glyph,
            host_theme.diagnostic_info_style,
        ),
    };
    // 0x9399b2 is Catppuccin overlay2 — the v1 muted fallback if
    // the theme uses `Color::Default` (no concrete RGB).
    let color = style.fg.map(|c| c.to_rgb_u32(0x9399b2)).unwrap_or(0x9399b2);
    (glyph, color)
}

// 5.8.N: `CursorShape` lives host-side
// (`lattice_host::cursor_shape::CursorShape`); both renderer
// peers map the host shape to their native cursor primitive.
// Re-imported via `use` at the top of the file.

/// The renderer-side composition root rendered as a GPUI
/// `Entity`. Holds the [`GpuiApp`] + a [`FocusHandle`] so the
/// window's key events actually route to our dispatcher.
struct EditorView {
    app: GpuiApp,
    focus_handle: FocusHandle,
}

impl EditorView {
    fn new(document: Document, cx: &mut Context<Self>) -> Self {
        Self {
            app: GpuiApp::new(document),
            focus_handle: cx.focus_handle(),
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        tracing::debug!(
            key = %ks.key,
            ctrl = ks.modifiers.control,
            alt = ks.modifiers.alt,
            shift = ks.modifiers.shift,
            platform = ks.modifiers.platform,
            "lattice-gpui: key down"
        );
        if self.app.editor.popup_buffer.is_some()
            && (ks.key.eq_ignore_ascii_case("escape") || ks.key.eq_ignore_ascii_case("esc"))
        {
            tracing::debug!("lattice-gpui: dismissing popup overlay (Esc)");
            self.app.dismiss_popup();
            cx.stop_propagation();
            cx.notify();
            return;
        }
        let outcome = self.app.dispatch_keystroke(
            &ks.key,
            ks.modifiers.control,
            ks.modifiers.alt,
            ks.modifiers.shift,
            ks.modifiers.platform,
        );
        // 3c.atomic.H: post-dispatch tracing reads through the
        // published render-state cell. `dispatch_keystroke`'s
        // chain ends with `publish_render_state()` so `ad()` is
        // current here.
        let ad = self.app.ad();
        tracing::debug!(
            modal = ?ad.modal,
            cursor_line = ad.cursor.line,
            cursor_byte = ad.cursor.byte,
            dispatched = outcome.is_some(),
            "lattice-gpui: post-dispatch state"
        );
        cx.stop_propagation();
        cx.notify();
        if self.app.editor.should_quit {
            tracing::info!("lattice-gpui: editor.should_quit set; closing application");
            cx.quit();
        }
    }
}

impl Focusable for EditorView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EditorView {
    /// Recursive walker over `editor.pane_tree.root()`. Each
    /// [`PaneNode::Leaf`] paints via [`Self::paint_pane`]; splits
    /// wrap children in `flex_col` (horizontal split: stacked
    /// vertically) or `flex_row` (vertical split: side by side)
    /// with a thin divider border between them. The active leaf
    /// gets the richer rendering (cursor + cached highlights);
    /// inactive leaves get a simpler text + gutter + per-pane
    /// status view.
    ///
    /// Phase 5.8.H: multi-pane visible in the GPUI peer. Single-
    /// pane case still works (the root is just `Leaf(0)`); v1
    /// inactive-pane rendering deliberately omits highlights +
    /// cursor markers — those need per-pane caches the host
    /// already populates for the TUI peer via
    /// `refresh_pane_highlights`, queued for a future slice.
    fn paint_pane_tree(&self, node: &PaneNode, theme: &GpuiTheme, active_idx: usize) -> gpui::Div {
        match node {
            PaneNode::Leaf(idx) => self.paint_pane(*idx, theme, *idx == active_idx),
            PaneNode::HorizontalSplit { top, bottom } => div()
                .flex()
                .flex_col()
                .flex_grow()
                .size_full()
                .child(
                    self.paint_pane_tree(top, theme, active_idx)
                        .flex_grow()
                        .border_b_1()
                        .border_color(rgb(theme.popup_border)),
                )
                .child(self.paint_pane_tree(bottom, theme, active_idx).flex_grow()),
            PaneNode::VerticalSplit { left, right } => div()
                .flex()
                .flex_row()
                .flex_grow()
                .size_full()
                .child(
                    self.paint_pane_tree(left, theme, active_idx)
                        .flex_grow()
                        .border_r_1()
                        .border_color(rgb(theme.popup_border)),
                )
                .child(self.paint_pane_tree(right, theme, active_idx).flex_grow()),
        }
    }

    /// Paint a single pane. Active pane uses `editor.cursor` +
    /// `editor.visible_highlights` (refreshed at the top of
    /// render); inactive panes use the stashed `PaneState::cursor`
    /// and no highlights. Each pane gets its own status line at
    /// its bottom (path + cursor coords), which keeps the visible
    /// boundary between panes legible without a hard chrome border.
    fn paint_pane(&self, pane_idx: usize, theme: &GpuiTheme, is_active: bool) -> gpui::Div {
        let editor = &self.app.editor;
        // 3c.atomic.H: active-document fields (cursor / scroll /
        // viewport_height / modal / active_buffer /
        // document_buffer_id) read through the published
        // render-state cell. `editor.X` stays for fields not on
        // `ActiveDocumentRenderState` -- pane_tree, buffers,
        // visible_highlights, pane_highlights, etc.
        let ad = self.app.ad();
        let leaves = editor.pane_tree.leaves();
        if pane_idx >= leaves.len() {
            return div().child(format!("(stale pane index {pane_idx})"));
        }
        let pane: &PaneState = &leaves[pane_idx];
        // Resolve the buffer's document handle. Inactive panes may
        // reference buffers different from `editor.document`; lookup
        // by buffer_id.
        let snapshot_opt = editor
            .buffers
            .document_handle(pane.buffer_id)
            .map(|h| h.snapshot());
        let Some(snapshot) = snapshot_opt else {
            return div()
                .p_3()
                .child(format!("(buffer {:?} unavailable)", pane.buffer_id));
        };
        // Phase 5.8.AF.5 / Slice 3c.atomic.L: instrument the
        // suspected per-frame waste site -- `snapshot.text()` is
        // a full rope-to-String copy and `split('\n').collect()`
        // builds a `Vec<&str>` over it. For a 100KB file that's
        // hundreds of KB of allocation per pane per frame.
        // `lattice_runtime::DocumentSnapshot::text` docstring
        // explicitly warns against calling on the hot path.
        let text_t0 = std::time::Instant::now();
        let text = snapshot.text();
        let text_us = text_t0.elapsed().as_micros() as u64;
        let cursor = if is_active {
            ad.cursor
        } else {
            pane.cursor
        };
        let cursor_line = cursor.line as usize;
        let cursor_byte = cursor.byte as usize;
        let split_t0 = std::time::Instant::now();
        let raw_lines: Vec<&str> = text.split('\n').collect();
        let split_us = split_t0.elapsed().as_micros() as u64;
        tracing::info!(
            target: "lattice_gpui::perf",
            pane_idx,
            is_active,
            text_bytes = text.len() as u64,
            line_count = raw_lines.len() as u64,
            text_us,
            split_us,
            "paint_pane text materialisation"
        );
        // 5.8.O: clip the visible window to `[scroll, scroll +
        // viewport_height)` so large docs don't render every line
        // every frame. Active pane reads scroll from `editor.scroll`
        // (ensure_cursor_in_viewport keeps it sane); inactive
        // panes read their stashed `PaneState::scroll`. The
        // gutter, status, and cursor maths still work in terms of
        // absolute line indices — only the iter range tightens.
        let pane_scroll = if is_active {
            ad.scroll
        } else {
            pane.scroll
        };
        let viewport_height = ad.viewport_height.max(1);
        let visible_start = (pane_scroll as usize).min(raw_lines.len());
        let visible_end = (pane_scroll as usize)
            .saturating_add(viewport_height as usize)
            .min(raw_lines.len());

        let cursor_shape = if is_active {
            Some(CursorShape::for_mode(ad.modal))
        } else {
            None
        };
        let cursor_fg = rgb(theme.cursor_foreground);
        let cursor_bg = rgb(theme.cursor_background);

        // Highlights:
        //   - active pane: live cache (`visible_highlights`),
        //     refreshed at render entry.
        //   - inactive pane sharing the active doc: live cache
        //     too — one parse covers both panes' visible windows.
        //   - inactive pane with a *different* doc: per-pane cache
        //     (`pane_highlights[pane_idx]`), refreshed at render
        //     entry by `refresh_pane_highlights`.
        let active_doc_id = if matches!(ad.buffer_kind, lattice_core::BufferKind::Document) {
            Some(ad.document_buffer_id)
        } else {
            None
        };
        let same_doc_as_active = Some(pane.buffer_id) == active_doc_id;
        let highlights: &[Vec<lattice_syntax::StyledSpan>] = if is_active || same_doc_as_active {
            editor.visible_highlights.as_slice()
        } else {
            editor
                .pane_highlights
                .get(&pane_idx)
                .map(Vec::as_slice)
                .unwrap_or(&[])
        };

        let total_lines = raw_lines.len().max(1);
        let gutter_width = total_lines.to_string().len();
        // 5.8.I / 5.8.U: gutter pad = 1 (fold marker column) +
        // 1 (severity sign column) + N (line number width) + 1
        // (trailing space).
        let gutter_pad_len = 2 + gutter_width + 1;
        let gutter_normal = rgb(0x9399b2); // Catppuccin overlay2.

        let style_cursor_cell = |c: &str| -> gpui::Div {
            let cell = div().child(c.to_string());
            match cursor_shape {
                Some(CursorShape::Block) => cell.bg(cursor_bg).text_color(cursor_fg),
                Some(CursorShape::Bar) => cell.border_l_2().border_color(cursor_bg),
                Some(CursorShape::Underline) => cell.border_b_2().border_color(cursor_bg),
                None => cell,
            }
        };

        // 5.8.I: per-line severity lookup. URI for this pane's
        // buffer comes from `editor.buffer_uris` (populated when
        // LSP attaches). `None` means: unsaved scratch, no LSP
        // attachment, or LSP-mode disabled for this buffer. The
        // gutter then renders a blank sign column (one space) so
        // the line-number alignment stays stable regardless of
        // whether diagnostics are present.
        let uri = editor.buffer_uris.get(&pane.buffer_id);
        // Phase 5.8.AF.5 / Slice 3a: read through the renderer's
        // `RenderState` contract instead of `editor.lsp_diagnostics`
        // directly. Symmetric with the TUI peer's
        // `severity_for_line` migration. `load_full` is wait-free
        // (~2ns); the returned snapshot's diagnostics layer is
        // internally `Arc<ArcSwap<...>>`-backed so the inner
        // `line_severity` call stays wait-free too.
        let render_state = editor.render_state.load_full();
        let line_severity = |line_idx: u32| -> Option<lattice_lsp::DiagnosticSeverity> {
            uri.and_then(|u| render_state.diagnostics.layer.line_severity(u, line_idx))
        };

        // 5.8.J: per-line inlay hints. Read the buffer's
        // `LspInlayHintCache`; collect hints whose
        // `position.line == line_idx` for this paint. Hints are
        // virtual text (don't affect cursor offset), rendered
        // inline at their `position.character` byte offset in a
        // dimmed overlay colour. v1 doesn't gate on
        // `lsp-inlay-hint-mode` — if the cache exists, paint it
        // (when the mode is off, the driver doesn't repopulate
        // the cache, so the most-recent state is shown).
        let inlay_hints_for_line: Box<dyn Fn(u32, &str) -> Vec<(usize, String)>> = {
            // 5.8.AF.5 / Slice 3b.1: read inlay-hint cache
            // through `RenderState.lsp.inlay_hints`. Symmetric
            // with the TUI peer; the spawned LSP request task
            // writes into the same underlying PerBufferCache.
            use lattice_host::per_buffer_cache::PerBufferCacheExt;
            let buffer_id = pane.buffer_id;
            let rs = editor.render_state.load_full();
            let cache_opt = rs.lsp.inlay_hints.get_for(buffer_id);
            Box::new(
                move |line_idx: u32, line_text: &str| -> Vec<(usize, String)> {
                    let Some(cache) = cache_opt.as_ref() else {
                        return Vec::new();
                    };
                    let mut hits: Vec<(usize, String)> = cache
                        .hints
                        .iter()
                        .filter(|h| h.position.line == line_idx)
                        .map(|h| {
                            // 5.8.N: label flattening is renderer-
                            // neutral; helper lives on lattice-lsp.
                            let mut text = lattice_lsp::inlay_hint_label_text(&h.label);
                            if h.padding_left.unwrap_or(false) {
                                text.insert(0, ' ');
                            }
                            if h.padding_right.unwrap_or(false) {
                                text.push(' ');
                            }
                            let byte_offset = lattice_lsp::position::utf16_column_to_utf8_byte(
                                line_text,
                                h.position.character,
                            ) as usize;
                            (byte_offset.min(line_text.len()), text)
                        })
                        .collect();
                    hits.sort_by_key(|(off, _)| *off);
                    hits
                },
            )
        };

        // 5.8.N: severity glyph + colour come from host_theme so
        // `:set ui.diagnostics.*` overrides flow through identically
        // for both renderer peers.
        let host_theme = editor.host_theme;
        let make_gutter = |line_idx: usize, is_cursor_line: bool, fold_marker: bool| -> gpui::Div {
            // 5.8.I / 5.8.N: severity sign cell + line-number cell.
            // Painted as children of a flex_row so each can carry
            // its own colour.
            let sev = line_severity(line_idx as u32);
            let sign_cell: gpui::Div = match sev {
                Some(s) => {
                    let (glyph, color) = diagnostic_glyph_and_color(&host_theme, s);
                    div().text_color(rgb(color)).child(glyph.to_string())
                }
                None => div().child(" ".to_string()),
            };
            let label = format!("{:>width$} ", line_idx + 1, width = gutter_width);
            let label_color = if is_cursor_line && is_active {
                cursor_bg
            } else {
                gutter_normal
            };
            // 5.8.U: fold-start marker (►) sits in a third
            // gutter cell, on the left of the severity sign.
            // Lines that aren't a fold-start render a blank
            // space in that column so alignment stays stable
            // regardless of whether folds are present.
            let fold_cell = if fold_marker {
                div().text_color(rgb(0xfab387)).child("►".to_string())
            } else {
                div().child(" ".to_string())
            };
            div()
                .flex()
                .flex_row()
                .child(fold_cell)
                .child(sign_cell)
                .child(div().text_color(label_color).child(label))
        };

        // 5.8.J: dimmed Catppuccin overlay colour for inlay-hint
        // virtual text. Matches the TUI peer's `inlay_hint_style`
        // (subdued comment-like style).
        let inlay_color = rgb(0x7f849c); // Catppuccin overlay1.

        // 5.8.P: visual-mode selection range. `None` outside
        // Visual mode; inactive panes never paint a selection
        // since the visual range lives on `editor` (active pane).
        // Selection background uses Catppuccin surface1 — a
        // distinguishable highlight that doesn't fight syntax
        // colours.
        let visual_range = if is_active {
            editor.visual_selection_range()
        } else {
            None
        };
        let selection_bg = rgb(0x45475a); // Catppuccin surface1
        // 5.8.Q: hlsearch + current-match + substitute-preview
        // overlays. All three are protocol `Range`s the host
        // already maintains; the GPUI peer just paints them.
        // `current_match` (primary hit) uses a stronger colour
        // than `all_matches` (secondary hlsearch).
        let current_match = if is_active {
            editor.current_match
        } else {
            None
        };
        let all_matches: &[lattice_core::protocol::position::Range] = if is_active {
            editor.all_matches.as_slice()
        } else {
            &[]
        };
        let current_match_bg = rgb(0xf9e2af); // Catppuccin yellow
        let current_match_fg = rgb(0x1e1e2e); // Catppuccin base (contrast on yellow)
        let hlsearch_bg = rgb(0x6c7086); // Catppuccin overlay0

        // Per-line predicate that clamps the half-open range to
        // the actual line length so a linewise `u32::MAX` end
        // byte covers exactly the real characters.
        let byte_in_range = |range: &lattice_core::protocol::position::Range,
                             line_idx: usize,
                             byte_idx: usize,
                             line_len: usize|
         -> bool {
            let li = line_idx as u32;
            if li < range.start.line || li > range.end.line {
                return false;
            }
            let start = if li == range.start.line {
                range.start.byte as usize
            } else {
                0
            };
            let end = if li == range.end.line {
                (range.end.byte as usize).min(line_len)
            } else {
                line_len
            };
            byte_idx >= start && byte_idx < end
        };
        let byte_in_visual = |line_idx: usize, byte_idx: usize, line_len: usize| -> bool {
            visual_range
                .as_ref()
                .is_some_and(|r| byte_in_range(r, line_idx, byte_idx, line_len))
        };
        let byte_in_current_match = |line_idx: usize, byte_idx: usize, line_len: usize| -> bool {
            current_match
                .as_ref()
                .is_some_and(|r| byte_in_range(r, line_idx, byte_idx, line_len))
        };
        let byte_in_any_match = |line_idx: usize, byte_idx: usize, line_len: usize| -> bool {
            all_matches
                .iter()
                .any(|r| byte_in_range(r, line_idx, byte_idx, line_len))
        };

        // 5.8.V: LSP document-highlight overlay. Read
        // `editor.lsp_document_highlights.highlights` — each entry
        // is an `lsp_types::DocumentHighlight` with utf16 LSP
        // positions. Convert to per-line byte ranges via
        // `utf16_column_to_utf8_byte` keyed against the doc text.
        // Painted with Catppuccin overlay0 (matches hlsearch
        // intensity — a soft "related symbol" feel).
        let doc_highlights_bg = rgb(0x585b70); // Catppuccin surface2 (soft accent)
        // Phase 5.8.AF.5 / Slice 3b.0: read through the
        // `RenderState.lsp.document_highlights` ArcSwap. The
        // spawned LSP request task `.store()`s directly into the
        // same underlying slot, so this `load_full()` sees the
        // latest result without any tick-driven drain on the
        // renderer thread. Symmetric with the TUI peer.
        let rs = editor.render_state.load_full();
        let dh_guard = rs.lsp.document_highlights.load_full();
        let doc_highlight_in_buffer = |line_idx: usize, byte_idx: usize| -> bool {
            let Some(cache) = dh_guard.as_deref() else {
                return false;
            };
            // Only highlight when the cache is for this pane's
            // buffer (the host pump keys highlights per buffer).
            if cache.buffer_id != pane.buffer_id {
                return false;
            }
            for h in cache.highlights.iter() {
                let start = h.range.start;
                let end = h.range.end;
                let li = line_idx as u32;
                if li < start.line || li > end.line {
                    continue;
                }
                // utf16 → utf8 byte conversion is keyed by the
                // line's text. For multi-line highlights, lines
                // strictly between start/end are fully covered.
                let line_text = raw_lines.get(line_idx).copied().unwrap_or("");
                let start_byte = if li == start.line {
                    lattice_lsp::position::utf16_column_to_utf8_byte(line_text, start.character)
                        as usize
                } else {
                    0
                };
                let end_byte = if li == end.line {
                    lattice_lsp::position::utf16_column_to_utf8_byte(line_text, end.character)
                        as usize
                } else {
                    line_text.len()
                };
                if byte_idx >= start_byte && byte_idx < end_byte {
                    return true;
                }
            }
            false
        };

        // 5.8.S: substitute preview overlays. Read
        // `editor.substitute_preview.matches` — the about-to-be-
        // replaced ranges. Paint with a distinctive bg so the user
        // sees the change before pressing Enter. Active pane only.
        let substitute_matches: &[lattice_core::protocol::position::Range] = if is_active {
            editor
                .substitute_preview
                .as_ref()
                .map(|p| p.matches.as_slice())
                .unwrap_or(&[])
        } else {
            &[]
        };
        let substitute_bg = rgb(0xf38ba8); // Catppuccin red — destructive preview
        let substitute_fg = rgb(0x1e1e2e); // Catppuccin base — contrast on red
        let byte_in_substitute = |line_idx: usize, byte_idx: usize, line_len: usize| -> bool {
            substitute_matches
                .iter()
                .any(|r| byte_in_range(r, line_idx, byte_idx, line_len))
        };

        // 5.8.Q: cursorline background — paint a subtle row
        // background on the active pane's cursor line so the
        // user can locate the cursor at a glance. Read from
        // host_theme.cursor_line_bg; if it's Color::Default the
        // fallback is Catppuccin surface0 (close to bg, gentle).
        let cursorline_bg = editor.host_theme.cursor_line_bg.to_rgb_u32(0x313244);

        // 5.8.AB.2: per-character diagnostic underline overlay.
        // The TUI is limited to a colored gutter sign because
        // ratatui can't paint sub-character decorations; GPUI
        // can. Read the full diagnostic array for this pane's
        // URI once per paint (wait-free `Arc<[Diagnostic]>` via
        // `diagnostics_arc`), then a per-(line, byte) probe
        // finds the most-severe overlapping diagnostic for the
        // cell. Severity → colour mirrors the gutter sign so
        // the underline tells the user *which* error/warning
        // covers each token.
        let diagnostics_arc = uri.and_then(|u| editor.lsp_diagnostics.diagnostics_arc(u));
        let diagnostic_severity_at_byte = |line_idx: usize,
                                           byte_idx: usize,
                                           line_text: &str|
         -> Option<lattice_lsp::DiagnosticSeverity> {
            let arr = diagnostics_arc.as_ref()?;
            let mut best: Option<lattice_lsp::DiagnosticSeverity> = None;
            for d in arr.iter() {
                let start = d.range.start;
                let end = d.range.end;
                let li = line_idx as u32;
                if li < start.line || li > end.line {
                    continue;
                }
                // Convert LSP utf-16 columns to utf-8 byte
                // offsets keyed against the line text. For
                // multi-line diagnostics the in-between lines
                // are fully covered.
                let start_byte = if li == start.line {
                    lattice_lsp::position::utf16_column_to_utf8_byte(line_text, start.character)
                        as usize
                } else {
                    0
                };
                let end_byte = if li == end.line {
                    lattice_lsp::position::utf16_column_to_utf8_byte(line_text, end.character)
                        as usize
                } else {
                    line_text.len()
                };
                // LSP "zero-width" diagnostics (start == end) are
                // common for "missing X" errors — paint at least
                // one char so the underline is visible.
                let coverage_end = end_byte.max(start_byte + 1);
                if byte_idx >= start_byte && byte_idx < coverage_end {
                    // Lower severity rank wins (Error < Warning <
                    // Info < Hint), matching the gutter sign's
                    // most-severe rule.
                    let rank = |s: lattice_lsp::DiagnosticSeverity| -> u8 {
                        match s {
                            lattice_lsp::DiagnosticSeverity::ERROR => 0,
                            lattice_lsp::DiagnosticSeverity::WARNING => 1,
                            lattice_lsp::DiagnosticSeverity::INFORMATION => 2,
                            lattice_lsp::DiagnosticSeverity::HINT => 3,
                            _ => 4,
                        }
                    };
                    let sev = d.severity.unwrap_or(lattice_lsp::DiagnosticSeverity::HINT);
                    best = Some(match best {
                        Some(b) if rank(b) <= rank(sev) => b,
                        _ => sev,
                    });
                }
            }
            best
        };

        // 5.8.O: walk only the visible window [visible_start,
        // visible_end). `line_idx` is the ABSOLUTE buffer-line
        // index so gutter labels, cursor maths, and highlight
        // lookups stay 0-based against the document — not relative
        // to the viewport.
        // 5.8.U: skip lines hidden inside closed folds. The
        // fold-start row still paints (with a ► marker prepended
        // to the gutter); rows strictly inside the fold body are
        // filtered out of the iteration.
        let mut rows: Vec<gpui::Div> = (visible_start..visible_end)
            .filter(|line_idx| !editor.line_inside_closed_fold(*line_idx as u32))
            .map(|line_idx| {
                let line = raw_lines[line_idx];
                let fold_marker = editor.fold_start_at(line_idx as u32).is_some();
                let is_cursor_line = line_idx == cursor_line;
                // Phase 5.8.AF.5 / Slice 3c.atomic.M: index spans by
                // buffer-line DELTA from `pane_scroll`, not by absolute
                // buffer line. `highlight_lines(start, end)` returns
                // `Vec<Vec<StyledSpan>>` where entry `i` covers absolute
                // line `start + i` (per `lattice-syntax` docstring), so
                // the lookup must subtract `pane_scroll`. Same lesson
                // the TUI peer learned -- see
                // `crates/lattice-ui-tui/src/render.rs:4971-4972`.
                // Without this delta, every paint with `scroll > 0`
                // either reads wrong-line spans or falls through to
                // empty (`SyntaxStyle::Default` -> default text colour),
                // which is what the user observes as "everything is
                // just plain white".
                let rel_line = (line_idx as u32).saturating_sub(pane_scroll) as usize;
                let line_spans: &[lattice_syntax::StyledSpan] =
                    highlights.get(rel_line).map(Vec::as_slice).unwrap_or(&[]);
                // 5.8.J: pre-compute inlay hits for this line.
                let hints = inlay_hints_for_line(line_idx as u32, line);
                let mut hint_iter = hints.iter().peekable();
                let mut cells: Vec<gpui::Div> = Vec::with_capacity(line.len() + 2 + hints.len());
                cells.push(make_gutter(line_idx, is_cursor_line, fold_marker));
                // Phase 5.8.AF.5 / Slice X3: collapse consecutive
                // chars with identical styling into a single styled
                // div carrying a string run, instead of one div per
                // char. For a typical idle line (no visual, no
                // search, no doc-highlight, syntax colour changes
                // only at token boundaries), this cuts ~80 per-char
                // divs down to ~10-15 per-run divs -- a 5-8x
                // reduction in element-tree fan-out per line, and
                // proportionally cheaper downstream GPUI layout +
                // paint + composite. Visual output is identical:
                // monospace font, each char in a run has the same
                // fg/bg/underline, so concatenating into one string
                // and applying the style to the run preserves
                // appearance.
                //
                // Flush points (force a new run):
                //   - inlay hint insertion at this byte;
                //   - cursor char (renders as its own shaped cell);
                //   - any field of `CellStyle` differs from the
                //     current run.
                let mut current_run: Option<(CellStyle, String)> = None;
                let flush_run = |cells: &mut Vec<gpui::Div>,
                                 run: &mut Option<(CellStyle, String)>| {
                    if let Some((style, text)) = run.take() {
                        cells.push(run_to_cell(style, text));
                    }
                };
                for (byte_idx, c) in line.char_indices() {
                    // 5.8.J: drain hints whose byte offset is at
                    // or before this char — they render inline
                    // before the char. `position.character`
                    // typically sits on a token boundary so the
                    // hint visually appears between tokens.
                    while let Some(&&(off, _)) = hint_iter.peek() {
                        if off <= byte_idx {
                            flush_run(&mut cells, &mut current_run);
                            let (_, text) = hint_iter.next().unwrap();
                            cells.push(div().text_color(inlay_color).child(text.clone()));
                        } else {
                            break;
                        }
                    }
                    let is_cursor = is_active && is_cursor_line && byte_idx == cursor_byte;
                    if is_cursor {
                        flush_run(&mut cells, &mut current_run);
                        cells.push(style_cursor_cell(&c.to_string()));
                        continue;
                    }
                    // Compute the per-char style signature. See
                    // `CellStyle` + `compute_cell_style` at file
                    // scope. Overlay precedence (substitute >
                    // visual > current_match > hlsearch >
                    // doc_highlight) lives there too.
                    let in_visual = byte_in_visual(line_idx, byte_idx, line.len());
                    let in_current_match = byte_in_current_match(line_idx, byte_idx, line.len());
                    let in_hlsearch =
                        !in_current_match && byte_in_any_match(line_idx, byte_idx, line.len());
                    let in_substitute = byte_in_substitute(line_idx, byte_idx, line.len());
                    let in_doc_highlight = doc_highlight_in_buffer(line_idx, byte_idx);
                    let span_style = style_at(line_spans, byte_idx);
                    let style = CellStyle::compute(
                        span_style,
                        in_visual,
                        in_current_match,
                        in_hlsearch,
                        in_substitute,
                        in_doc_highlight,
                        diagnostic_severity_at_byte(line_idx, byte_idx, line)
                            .map(|sev| diagnostic_glyph_and_color(&host_theme, sev).1),
                        selection_bg,
                        current_match_bg,
                        current_match_fg,
                        hlsearch_bg,
                        substitute_bg,
                        substitute_fg,
                        doc_highlights_bg,
                    );
                    match &mut current_run {
                        Some((existing, buf)) if *existing == style => {
                            buf.push(c);
                        }
                        _ => {
                            flush_run(&mut cells, &mut current_run);
                            current_run = Some((style, c.to_string()));
                        }
                    }
                }
                flush_run(&mut cells, &mut current_run);
                // 5.8.J: drain trailing hints positioned at or
                // past EOL.
                for (_, text) in hint_iter {
                    cells.push(div().text_color(inlay_color).child(text.clone()));
                }
                if is_active && is_cursor_line && cursor_byte >= line.len() {
                    cells.push(style_cursor_cell(" "));
                }
                // 5.8.Q: paint cursorline bg under the row when
                // this is the active pane's cursor line. Per-cell
                // overlays (visual / match) still layer on top
                // because each cell carries its own bg.
                let row = div().flex().flex_row().children(cells);
                if is_active && is_cursor_line {
                    row.bg(rgb(cursorline_bg))
                } else {
                    row
                }
            })
            .collect();

        // 5.8.O: cursor-past-last-line marker only renders if the
        // synthetic row falls within the visible viewport.
        let cursor_past_last_line = is_active && cursor_line >= raw_lines.len();
        let trailing_row_in_viewport = cursor_past_last_line
            && cursor_line >= visible_start
            && cursor_line < (visible_start + (viewport_height as usize));
        if trailing_row_in_viewport {
            let blank_gutter = div().child(" ".repeat(gutter_pad_len));
            rows.push(
                div()
                    .flex()
                    .flex_row()
                    .child(blank_gutter)
                    .child(style_cursor_cell(" ")),
            );
        }

        // Per-pane status line at the pane's bottom. Format
        // matches the TUI's per-pane status: path + cursor coords.
        // Active pane uses the cursor color for the bar so the
        // user can tell which pane has focus at a glance.
        let path_label = snapshot
            .path()
            .map(|p| {
                let display = match std::env::current_dir()
                    .ok()
                    .and_then(|cwd| p.strip_prefix(&cwd).ok().map(|s| s.to_path_buf()))
                {
                    Some(rel) => rel.display().to_string(),
                    None => p.display().to_string(),
                };
                if snapshot.dirty {
                    format!("{display} [+]")
                } else {
                    display
                }
            })
            .unwrap_or_else(|| {
                if snapshot.dirty {
                    "[scratch][+]".to_string()
                } else {
                    "[scratch]".to_string()
                }
            });
        let status_line = format!("  {path_label}   L:{}  C:{}", cursor.line + 1, cursor.byte);
        let (status_bg, status_fg) = if is_active {
            (rgb(theme.cursor_background), rgb(theme.cursor_foreground))
        } else {
            (rgb(theme.status_background), rgb(theme.status_foreground))
        };

        div()
            .flex()
            .flex_col()
            .flex_grow()
            .overflow_hidden()
            .child(
                div()
                    .flex_grow()
                    .p_3()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .children(rows),
            )
            .child(
                div()
                    .bg(status_bg)
                    .text_color(status_fg)
                    .px_2()
                    .py_1()
                    .flex()
                    .flex_row()
                    .child(div().child(status_line)),
            )
    }
}

impl Render for EditorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Phase 5.8.AF.5 / Slice 3c.atomic.L: per-frame budget
        // breakdown on the `lattice_gpui::perf` tracing target.
        // Enable with `RUST_LOG=lattice_gpui::perf=info`. Emits
        // one info-line per frame summarising the time spent in
        // each phase (viewport, ensure_cursor, highlights, tick,
        // paint). One per-pane debug-line covers the document
        // text materialisation cost (`snapshot.text()` +
        // line split) -- the prime suspect for per-frame waste.
        let frame_start = std::time::Instant::now();
        // 5.8.T: per-frame viewport-height recompute from the
        // window's current pixel bounds. `text_sm` lands at ~16px
        // per row in the default font; subtract a row for the
        // status line + a row for the global minibuffer footer.
        // `window.viewport_size()` returns `Size<Pixels>` (gpui
        // 0.2.2). On resize, this picks up the new size on the
        // next frame — no event subscription required.
        let viewport_px = window.viewport_size();
        let estimated_row_px = 16.0_f32;
        let total_rows = (f32::from(viewport_px.height) / estimated_row_px).floor() as i32;
        let chrome_rows = 2; // status line + minibuffer
        let new_viewport = (total_rows - chrome_rows).max(1) as u32;
        // 3c.atomic.H: route through `App::set_viewport_height`,
        // which clamps to >= 1, runs `ensure_cursor_visible`,
        // AND publishes a fresh render-state. The previous form
        // wrote the field directly and then called
        // `ensure_cursor_in_viewport` without publishing -- so
        // paint-time reads of `ad().{viewport_height,scroll}`
        // would observe the previous frame's values. Same
        // publish gap the TUI peer fixed in 3c.atomic.D.
        if new_viewport != self.app.editor.viewport_height {
            self.app.set_viewport_height(new_viewport);
        }
        let after_viewport = std::time::Instant::now();
        // 5.8.O: keep the cursor inside the viewport before any
        // paint reads `editor.scroll`. Auto-scrolls if the cursor
        // moved past the visible window since the last frame.
        // `set_viewport_height` above already ran one round of
        // `ensure_cursor_visible`, but this also covers the case
        // where the viewport size didn't change but the cursor
        // moved past the existing window.
        self.app.ensure_cursor_in_viewport();
        let after_ensure = std::time::Instant::now();
        // 5.8.G: refresh the host-side highlight cache before
        // reading spans. Cache-hit path is ~50ns; cache-miss path
        // walks `highlight_lines` exactly once and stores into
        // `editor.visible_highlights`. The per-frame
        // `highlight_lines` call this replaces ran ~178µs at 80
        // lines unconditionally.
        self.app.refresh_highlights();
        // 5.8.R: rebuild the per-pane cache for inactive Document
        // panes whose buffer differs from the active pane's. The
        // host method handles the same-doc short-circuit + reparse
        // gating; this peer just makes the call so paint_pane can
        // read `editor.pane_highlights[idx]` for the inactive case.
        self.app.editor.refresh_pane_highlights();
        let after_highlights = std::time::Instant::now();
        // Phase 5.8.AF.5 / Slice X1: `run_tick_pending` no longer
        // runs in the renderer body. The drain moved to
        // `GpuiApp::dispatch_action`'s tail so it fires on the
        // keystroke that caused the work, not in `Render::render`.
        // Paramount goal #1 forbids I/O / event drain on the UI
        // thread. With X1 landed, `tick_us` in `lattice_gpui::perf`
        // should be ~0; if it ever climbs back, something has
        // re-introduced the violation -- audit per
        // `docs/dev/operations/render-thread-discipline-remediation.md`
        // §7 before merging.
        let after_tick = std::time::Instant::now();
        // Phase 5.8.AA.p/r/t: every per-tick drain (hover,
        // definitions, code-actions, live-picker, ...) is now
        // folded into `run_tick_pending` above; no per-paint
        // catch-up calls remain.
        // 3c.atomic.H: modeline label read through the published
        // render-state. Paint-time read; the apply loop above
        // has already published any modal change.
        let modal = self.app.ad().modal;

        let modal_label = match modal {
            ModalState::Normal => "NORMAL",
            ModalState::Insert => "INSERT",
            ModalState::Visual(_) => "VISUAL",
            ModalState::OperatorPending => "PENDING",
            ModalState::Command => "COMMAND",
            ModalState::Search(_) => "SEARCH",
            ModalState::Replace => "REPLACE",
        };
        // 5.8.C / 5.8.H: bottom global row. In Command/Search
        // modes it shows the in-progress `:cmd` / `/pattern`
        // minibuffer; otherwise it shows the global modal label.
        // Per-pane path + cursor coords now live inside each
        // pane's own status line (built in `paint_pane`).
        let bottom_row: String = match modal {
            ModalState::Command => format!(":{}", self.app.editor.command_line),
            ModalState::Search(dir) => {
                let prefix = match dir {
                    lattice_grammar::SearchDirection::Forward => '/',
                    lattice_grammar::SearchDirection::Backward => '?',
                };
                let pattern = self
                    .app
                    .editor
                    .search_line
                    .as_ref()
                    .map(|s| s.pattern.as_str())
                    .unwrap_or("");
                format!("{prefix}{pattern}")
            }
            _ => format!("  {modal_label}"),
        };
        let bottom_is_minibuffer = matches!(modal, ModalState::Command | ModalState::Search(_));

        let theme = self.app.theme;
        // 5.8.H: render the pane tree. `paint_pane_tree` walks
        // `editor.pane_tree.root()` recursively; each leaf paints
        // via `paint_pane` with active/inactive style. The active
        // leaf gets the refreshed `visible_highlights` cache + a
        // visible cursor; inactive leaves show plain text + no
        // cursor (their own stashed `PaneState::cursor` is read
        // for the per-pane status coords but no visible marker is
        // painted).
        let active_idx = self.app.editor.pane_tree.active_index();
        let document_area = self
            .paint_pane_tree(self.app.editor.pane_tree.root(), &theme, active_idx)
            .flex_grow();
        let after_paint = std::time::Instant::now();

        let completion_overlay: Option<gpui::Div> = self
            .app
            .editor
            .insert_completion
            .as_ref()
            .filter(|ic| !ic.rendered.is_empty())
            .map(|ic| {
                let max_visible = 10usize;
                let total = ic.rendered.len();
                let window_start = ic
                    .selected
                    .saturating_sub(max_visible / 2)
                    .min(total.saturating_sub(max_visible.min(total)));
                let window_end = (window_start + max_visible).min(total);
                // 5.8.AB.1: match-range highlighting in the
                // insert-completion popup, same rules as the
                // picker overlay.
                let match_hl_fg = rgb(theme.cursor_background);
                let visible: Vec<gpui::Div> = ic.rendered[window_start..window_end]
                    .iter()
                    .enumerate()
                    .map(|(i, cand)| {
                        let abs_idx = window_start + i;
                        let selected = abs_idx == ic.selected;
                        let row_bg = if selected {
                            Some(rgb(theme.status_background))
                        } else {
                            None
                        };
                        let row_fg = if selected {
                            rgb(theme.status_foreground)
                        } else {
                            rgb(theme.foreground)
                        };
                        let display = &cand.raw.display;
                        if cand.match_ranges.is_empty() {
                            let row = div().child(display.clone()).text_color(row_fg);
                            return if let Some(bg) = row_bg { row.bg(bg) } else { row };
                        }
                        let in_match = |byte_idx: usize| -> bool {
                            cand.match_ranges
                                .iter()
                                .any(|r| byte_idx >= r.start && byte_idx < r.end)
                        };
                        let cells: Vec<gpui::Div> = display
                            .char_indices()
                            .map(|(byte_idx, c)| {
                                let cell = div().child(c.to_string());
                                if in_match(byte_idx) {
                                    cell.text_color(match_hl_fg)
                                } else {
                                    cell.text_color(row_fg)
                                }
                            })
                            .collect();
                        let row = div().flex().flex_row().children(cells);
                        if let Some(bg) = row_bg { row.bg(bg) } else { row }
                    })
                    .collect();
                div()
                    .flex()
                    .flex_col()
                    .max_w(px(360.0))
                    .p_2()
                    .bg(rgb(theme.popup_background))
                    .text_color(rgb(theme.foreground))
                    .border_2()
                    .border_color(rgb(theme.popup_border))
                    .children(visible)
                    .child(
                        div()
                            .pt_1()
                            .text_color(rgb(theme.popup_border))
                            .child(format!(
                                " {} of {}  ·  <C-n>/<C-p> · <Tab>/<CR> accept · <Esc> cancel ",
                                if total == 0 { 0 } else { ic.selected + 1 },
                                total,
                            )),
                    )
            });

        let picker_overlay: Option<gpui::Div> = self.app.editor.picker.as_ref().map(|picker| {
            let max_visible = 20usize;
            let total = picker.candidates.len();
            let window_start = picker
                .selected
                .saturating_sub(max_visible / 2)
                .min(total.saturating_sub(max_visible.min(total)));
            let window_end = (window_start + max_visible).min(total);
            // 5.8.AB.1: paint matched bytes in cursor_background
            // so the user can see *why* each row matched their
            // query. The TUI peer can't easily render styled
            // per-character spans mid-row; GPUI walks the byte
            // sequence and emits one cell-div per char with the
            // matched ones tinted. `match_ranges` is half-open
            // byte ranges into `raw.display` (the renderer's
            // canonical row text).
            let match_hl_fg = rgb(theme.cursor_background);
            let visible_candidates: Vec<gpui::Div> = picker.candidates[window_start..window_end]
                .iter()
                .enumerate()
                .map(|(i, cand)| {
                    let abs_idx = window_start + i;
                    let selected = abs_idx == picker.selected;
                    let row_bg = if selected {
                        Some(rgb(theme.status_background))
                    } else {
                        None
                    };
                    let row_fg = if selected {
                        rgb(theme.status_foreground)
                    } else {
                        rgb(theme.foreground)
                    };
                    let display = &cand.raw.display;
                    // Fast path: no matches → single child, no
                    // per-cell loop. Empty-query "show all" rows
                    // hit this branch every time.
                    if cand.match_ranges.is_empty() {
                        let row = div().child(display.clone()).text_color(row_fg);
                        return if let Some(bg) = row_bg { row.bg(bg) } else { row };
                    }
                    let in_match = |byte_idx: usize| -> bool {
                        cand.match_ranges
                            .iter()
                            .any(|r| byte_idx >= r.start && byte_idx < r.end)
                    };
                    let cells: Vec<gpui::Div> = display
                        .char_indices()
                        .map(|(byte_idx, c)| {
                            let cell = div().child(c.to_string());
                            if in_match(byte_idx) {
                                cell.text_color(match_hl_fg)
                            } else {
                                cell.text_color(row_fg)
                            }
                        })
                        .collect();
                    let row = div().flex().flex_row().children(cells);
                    if let Some(bg) = row_bg { row.bg(bg) } else { row }
                })
                .collect();
            div()
                .flex()
                .flex_col()
                .max_w(px(720.0))
                .max_h(px(440.0))
                .p_4()
                .bg(rgb(theme.popup_background))
                .text_color(rgb(theme.foreground))
                .border_2()
                .border_color(rgb(theme.popup_border))
                .child(
                    div()
                        .text_color(rgb(theme.popup_border))
                        .pb_1()
                        .child(format!(
                            " {} ({} / {}) ",
                            picker.title,
                            if total == 0 { 0 } else { picker.selected + 1 },
                            total,
                        )),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .pb_2()
                        .child(div().text_color(rgb(theme.cursor_background)).child("> "))
                        .child(div().child(picker.query.clone()))
                        .child(
                            div()
                                .border_l_2()
                                .border_color(rgb(theme.cursor_background))
                                .child(" "),
                        ),
                )
                .child(div().child("───────────────".to_string()))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .overflow_hidden()
                        .children(visible_candidates),
                )
                .child(
                    div()
                        .pt_2()
                        .text_color(rgb(theme.popup_border))
                        .child("[ <C-n>/<C-p> navigate · <CR> accept · <Esc> cancel ]".to_string()),
                )
        });

        // Phase 5.8.AE: read popup state from host editor instead
        // of the renderer-local `popup_content` field. Title +
        // body come from `editor.popup_help()`; highlights come
        // from the buffer-local seeded at popup-open time.
        let popup_overlay: Option<gpui::Div> = self.app.editor.popup_help().map(|buf| {
            let title = buf.title.clone();
            let body_text = buf.content.as_string();
            // M.3.2.c.5: highlights live in buffer-locals keyed by the
            // popup buffer id (see host's `Editor::popup_help_highlights`).
            let highlights_owned: Vec<Vec<lattice_syntax::StyledSpan>> = self
                .app
                .editor
                .popup_help_highlights()
                .map(|h| h.to_vec())
                .unwrap_or_default();
            let line_highlights: &[Vec<lattice_syntax::StyledSpan>] =
                highlights_owned.as_slice();
            let body_lines: Vec<&str> = body_text.split('\n').collect();
            let popup_lines: Vec<gpui::Div> = body_lines
                .iter()
                .enumerate()
                .map(|(idx, line)| {
                    let spans: &[lattice_syntax::StyledSpan] =
                        line_highlights.get(idx).map(Vec::as_slice).unwrap_or(&[]);
                    if spans.is_empty() {
                        return div().child(line.to_string());
                    }
                    let cells: Vec<gpui::Div> = line
                        .char_indices()
                        .map(|(byte_idx, c)| {
                            let style = style_at(spans, byte_idx);
                            div()
                                .text_color(rgb(syntax_color(style)))
                                .child(c.to_string())
                        })
                        .collect();
                    div().flex().flex_row().children(cells)
                })
                .collect();
            div()
                .flex()
                .flex_col()
                .max_w(px(640.0))
                .max_h(px(400.0))
                .p_4()
                .bg(rgb(theme.popup_background))
                .text_color(rgb(theme.foreground))
                .border_2()
                .border_color(rgb(theme.popup_border))
                .child(
                    div()
                        .text_color(rgb(theme.popup_border))
                        .pb_2()
                        .child(format!(" {title} "))
                        .child(div().child("───────────────".to_string())),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .overflow_hidden()
                        .children(popup_lines),
                )
                .child(
                    div()
                        .pt_2()
                        .text_color(rgb(theme.popup_border))
                        .child("[ Esc to dismiss ]".to_string()),
                )
        });

        let mut root = div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(theme.background))
            .text_color(rgb(theme.foreground))
            .text_sm()
            .font_family("DejaVu Sans Mono")
            .child(document_area)
            .child({
                let row = div()
                    .bg(rgb(theme.status_background))
                    .text_color(rgb(theme.status_foreground))
                    .px_2()
                    .py_1()
                    .flex()
                    .flex_row()
                    .child(div().child(bottom_row));
                if bottom_is_minibuffer {
                    row.child(
                        div()
                            .border_l_2()
                            .border_color(rgb(theme.cursor_background))
                            .child(" "),
                    )
                } else {
                    row
                }
            });

        if let Some(overlay) = picker_overlay {
            root = root.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .justify_center()
                    .items_center()
                    .child(overlay),
            );
        }
        if let Some(overlay) = completion_overlay {
            root = root.child(div().absolute().top_8().right_4().child(overlay));
        }
        if let Some(overlay) = popup_overlay {
            root = root.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .justify_center()
                    .items_center()
                    .child(overlay),
            );
        }
        // Phase 5.8.AF.5 / Slice 3c.atomic.L: per-frame budget log.
        // `after_paint` was captured immediately after
        // `paint_pane_tree` returned; the remaining work (overlay
        // assembly + return) is folded into the `tail_us` bucket.
        // Enable with `RUST_LOG=lattice_gpui::perf=info`.
        let frame_us = frame_start.elapsed().as_micros() as u64;
        tracing::info!(
            target: "lattice_gpui::perf",
            frame_us,
            viewport_us = (after_viewport - frame_start).as_micros() as u64,
            ensure_us = (after_ensure - after_viewport).as_micros() as u64,
            highlights_us = (after_highlights - after_ensure).as_micros() as u64,
            tick_us = (after_tick - after_highlights).as_micros() as u64,
            paint_us = (after_paint - after_tick).as_micros() as u64,
            tail_us = (frame_start.elapsed() - (after_paint - frame_start)).as_micros() as u64,
            "frame budget"
        );
        root
    }
}

/// Open a GPUI window backed by [`EditorView`] and drive the
/// event loop until the editor signals quit. Synchronous —
/// blocks the calling thread for the lifetime of the window
/// (same shape as [`lattice_ui_tui::run`] for symmetry).
///
/// Phase 5.9: the body of `fn main` from the old `lattice-gpui`
/// binary, lifted into the lib so `lattice-cli` can route
/// `lattice --gpu <file>` through this single entry. The
/// `lattice-gpui` binary keeps a thin shim that calls this.
pub fn run(document: Document) -> Result<()> {
    Application::new().run(move |cx| {
        let bounds = Bounds::centered(None, size(px(720.0), px(480.0)), cx);
        let window = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            move |_window, cx| cx.new(|cx| EditorView::new(document, cx)),
        );
        let window = match window {
            Ok(w) => w,
            Err(e) => {
                tracing::error!(error = ?e, "lattice-gpui: failed to open editor window");
                return;
            }
        };
        // Focus + activation has to run AFTER `open_window`
        // returns -- calling `window.focus(...)` from inside the
        // builder closure runs before gpui has fully initialised
        // the window's focus tree, and the focus call is silently
        // dropped. The `window.update(cx, ...)` re-entry hands
        // back a real window context where focus actually sticks
        // (this is the pattern gpui's own `examples/input.rs`
        // uses).
        let focus_result = window.update(cx, |view, window, cx| {
            window.focus(&view.focus_handle.clone());
            cx.activate(true);
        });
        if let Err(e) = focus_result {
            tracing::error!(error = ?e, "lattice-gpui: failed to focus editor window");
        }
    });
    Ok(())
}

/// Open a [`Document`] from `path`, with friendly error
/// context. Available for callers that want to pre-open a
/// document before handing it to [`run`].
pub fn document_from_path(path: &std::path::Path) -> Result<Document> {
    Document::open(path).with_context(|| format!("opening {}", path.display()))
}

// 5.8.N: cursor-shape tests live host-side in
// `lattice_host::cursor_shape::tests`; the GPUI peer's local
// duplicates were removed. Window-side tests (popup focus, dispatch
// integration) still live in `crate::tests` at the lib's root.
