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
    AnyElement, App, AppContext, Application, Bounds, Context, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Render, SharedString, Styled,
    TextRun, Window, WindowBounds, WindowOptions, div, font, px, rgb, size,
};
use lattice_core::Document;
use lattice_core::ui::pane::{PaneNode, PaneState};
use lattice_grammar::ModalState;
use lattice_host::cursor_shape::CursorShape;
use lattice_host::per_buffer_cache::PerBufferCacheExt;
use lattice_syntax::Style as SyntaxStyle;

use crate::{GpuiApp, GpuiTheme};

// Phase 5.8.AF.5 / Slice X3.full.2: `CellStyle` + `run_to_cell`
// (per-cell styled-Div construction) deleted -- replaced by the
// shaping-layer logic in `crate::editor_element::build_text_runs`.
// `syntax_color` + `style_at` are kept because the popup-overlay
// renderer below still walks chars and emits one Div per cell.
// Slice X3.full.3 absorbs the popup into a shaped-line path; at
// that point these helpers move into `editor_element` too.

/// Adapter: host-canonical [`Theme::syntax_style`] -> packed 24-bit
/// `0xRRGGBB`. Phase 5.8.AF.6 / issue-2 hoist: prior to this the
/// GPUI peer carried its own Catppuccin Mocha hex table that
/// diverged from the TUI's named-ANSI table. Both peers now route
/// through `lattice_host::ui::theme::Theme::syntax_style`, so a
/// single edit reflects everywhere.
///
/// `Theme::default()` is used today because per-instance theme
/// customization for syntax styles isn't wired through the cmdline
/// yet. The fallback when `fg` is unset is the Catppuccin Text
/// (`0xcdd6f4`) — matches what `SyntaxStyle::Default` resolves to
/// and what `EditorElement` paints on un-spanned bytes.
fn syntax_color(style: SyntaxStyle) -> u32 {
    let host_default = lattice_host::ui::theme::Theme::default();
    let host_style = host_default.syntax_style(style);
    host_style
        .fg
        .map(|c| c.to_rgb_u32(0xcdd6f4))
        .unwrap_or(0xcdd6f4)
}

/// Popup sizing constants. 2026-05-27: popup geometry is locked to
/// a window-relative size so the box does not grow with content
/// (which previously caused width-jump on long-line scroll). Both
/// `:describe-*` (Centered) and `K` hover (CursorAnchored) use the
/// same fixed dimensions for visual consistency.
///
/// Width / height are computed each frame as
///   `clamp(MIN, RATIO × viewport_dim, MAX)`
/// so small windows shrink the popup, normal windows hit MAX, and
/// the body never expands to fit text. The inner content area
/// (where the popup body paints) is the outer size minus chrome
/// (border + `.p_4()` padding + header row + separator row +
/// `.pb_2()` header gap); see `popup_chrome_v_px` for the
/// breakdown. The integer row count derived from inner pixels
/// doubles as the cursor-clamp viewport when the popup is focused
/// (mirrors TUI's `help_popup_inner_height`).
pub(crate) const POPUP_MAX_W_PX: f32 = 900.0;
pub(crate) const POPUP_MAX_H_PX: f32 = 600.0;
pub(crate) const POPUP_MIN_W_PX: f32 = 480.0;
pub(crate) const POPUP_MIN_H_PX: f32 = 240.0;
pub(crate) const POPUP_W_RATIO: f32 = 0.70;
pub(crate) const POPUP_H_RATIO: f32 = 0.60;

/// Compute the popup's outer pixel dimensions from the window's
/// viewport pixels. Window-relative with hard min/max caps so the
/// popup is readable on small windows and not absurd on large ones.
pub(crate) fn popup_outer_dims_px(viewport_w_px: f32, viewport_h_px: f32) -> (f32, f32) {
    let w = (viewport_w_px * POPUP_W_RATIO).clamp(POPUP_MIN_W_PX, POPUP_MAX_W_PX);
    let h = (viewport_h_px * POPUP_H_RATIO).clamp(POPUP_MIN_H_PX, POPUP_MAX_H_PX);
    (w, h)
}

/// Pixel cost of the popup's vertical chrome (border + .p_4 padding
/// top+bottom + header title row + separator row + .pb_2 header
/// gap). Subtract from the popup's outer height to get the inner
/// body area.
///
/// 2026-05-27: chrome is now exact (no safety margin) because the
/// popup paint locks each header row AND each body row to exactly
/// `row_px`. Previously the safety margin compensated for GPUI's
/// default `text-sm` line-height (~20px) being larger than the
/// editor row_px (~18.2px); with explicit `.h(px(row_px))` on
/// every header / body div the row metric is the single source
/// of truth.
pub(crate) fn popup_chrome_v_px(rem: f32, row_px: f32) -> f32 {
    let border_v = 2.0 * 2.0; // .border_2() top + bottom
    let p4_v = rem * 1.0 * 2.0; // .p_4() top + bottom = 2rem
    let header_text = row_px; // " title (hint) " row
    let separator_row = row_px; // "───" row
    let pb_2_v = rem * 0.5; // header .pb_2() = 0.5rem
    border_v + p4_v + header_text + separator_row + pb_2_v
}

/// Derive integer body-row count from popup outer height + chrome.
pub(crate) fn popup_inner_height_rows(popup_h_px: f32, rem: f32, row_px: f32) -> u32 {
    let chrome = popup_chrome_v_px(rem, row_px);
    ((popup_h_px - chrome) / row_px).floor().max(1.0) as u32
}

/// Horizontal chrome: border (left + right) + .p_4 padding (left +
/// right). Drives `popup_inner_cols` for wrap-at-width.
pub(crate) fn popup_chrome_h_px(rem: f32) -> f32 {
    let border_h = 2.0 * 2.0; // .border_2() left + right
    let p4_h = rem * 1.0 * 2.0; // .p_4() left + right = 2rem
    border_h + p4_h
}

/// Derive integer body-col count from popup outer width + chrome.
/// `glyph_advance_px` is the monospace cell width (measured by
/// shaping a reference char on the popup's font).
pub(crate) fn popup_inner_cols(popup_w_px: f32, rem: f32, glyph_advance_px: f32) -> u32 {
    let chrome = popup_chrome_h_px(rem);
    ((popup_w_px - chrome) / glyph_advance_px).floor().max(1.0) as u32
}

/// Pixel height the popup body is locked to (so its rendered
/// content cannot exceed `popup_inner_height_rows` × `row_px`).
/// Passed to the body div's `min_h == max_h` so flex layout
/// can't oversize the body and push rows past the popup's
/// bottom edge.
pub(crate) fn popup_body_h_px(popup_h_px: f32, rem: f32, row_px: f32) -> f32 {
    popup_inner_height_rows(popup_h_px, rem, row_px) as f32 * row_px
}

/// Walk `spans` (one entry per line) and find the `Style` that
/// covers `byte`.
fn style_at(spans: &[lattice_syntax::StyledSpan], byte: usize) -> SyntaxStyle {
    for span in spans {
        if byte >= span.start && byte < span.end {
            return span.style;
        }
    }
    SyntaxStyle::Default
}

/// Reads the typed `picker.display` option and returns `true` iff
/// the user wants the vertico-style minibuffer layout (default).
/// Unknown / missing values fall back to the design default rather
/// than panicking -- the validator already gates set-time. Matches
/// the TUI peer's `picker_display_is_minibuffer` so both renderers
/// agree on the same source of truth.
/// Issue #25 (2026-05-22): per-pane geometry collector. Walks
/// the pane tree distributing the container's pixel rectangle
/// to each leaf based on split orientation, then converts each
/// leaf's allocated pixels into `(rows, cols)` for the host's
/// `PaneState.viewport_height` / `viewport_width` fields.
///
/// - `HorizontalSplit { top, bottom }` divides HEIGHT between
///   children; both get the full width.
/// - `VerticalSplit { left, right }` divides WIDTH between
///   children; both get the full height.
/// - `Leaf` consumes per-leaf chrome (status row + py + p_3
///   padding vertically; p_3 padding horizontally), converts
///   to row/col count via `row_px` / `col_px`.
///
/// Each leaf's `(pane_idx, rows, cols)` is pushed into `out`.
/// The caller fires `editor_actor.set_pane_viewport(idx, rows,
/// cols)` once per entry — the host writes per-pane values
/// into PaneState and mirrors the active pane's height into
/// `Editor::viewport_height` for cursor-clamp / highlights
/// worker.
fn collect_pane_geometries(
    node: &lattice_core::ui::pane::PaneNode,
    available_w_px: f32,
    available_h_px: f32,
    per_leaf_v_chrome_px: f32,
    per_leaf_h_chrome_px: f32,
    row_px: f32,
    col_px: f32,
    out: &mut Vec<(usize, u32, u32)>,
) {
    use lattice_core::ui::pane::PaneNode;
    match node {
        PaneNode::Leaf(idx) => {
            let usable_h = (available_h_px - per_leaf_v_chrome_px).max(0.0);
            let usable_w = (available_w_px - per_leaf_h_chrome_px).max(0.0);
            let rows = (usable_h / row_px).floor().max(1.0) as u32;
            let cols = (usable_w / col_px).floor().max(1.0) as u32;
            out.push((*idx, rows, cols));
        }
        PaneNode::HorizontalSplit { top, bottom, ratio } => {
            // Issue #28: ratio-aware split (0.5 = even).
            let top_h = available_h_px * *ratio;
            let bot_h = available_h_px - top_h;
            collect_pane_geometries(
                top,
                available_w_px,
                top_h,
                per_leaf_v_chrome_px,
                per_leaf_h_chrome_px,
                row_px,
                col_px,
                out,
            );
            collect_pane_geometries(
                bottom,
                available_w_px,
                bot_h,
                per_leaf_v_chrome_px,
                per_leaf_h_chrome_px,
                row_px,
                col_px,
                out,
            );
        }
        PaneNode::VerticalSplit { left, right, ratio } => {
            let left_w = available_w_px * *ratio;
            let right_w = available_w_px - left_w;
            collect_pane_geometries(
                left,
                left_w,
                available_h_px,
                per_leaf_v_chrome_px,
                per_leaf_h_chrome_px,
                row_px,
                col_px,
                out,
            );
            collect_pane_geometries(
                right,
                right_w,
                available_h_px,
                per_leaf_v_chrome_px,
                per_leaf_h_chrome_px,
                row_px,
                col_px,
                out,
            );
        }
    }
}

fn picker_display_is_minibuffer(app: &GpuiApp) -> bool {
    // Slice 3c.final.B.10: typed-options registry via published
    // `options()` sub-state — wait-free Arc clone.
    app.options()
        .config
        .get_typed::<lattice_config::core_options::PickerDisplay>()
        .map(|s| s.as_str() != "popup")
        .unwrap_or(true)
}

/// 2026-05-27: read `popup.wrap` (bool) — controls whether the
/// help / hover popup wraps long lines at the popup's inner cols
/// or clips at the right edge.
fn popup_wrap_enabled(app: &GpuiApp) -> bool {
    app.options()
        .config
        .get_typed::<lattice_host::ui::theme_options::PopupWrap>()
        .map(|v| *v)
        .unwrap_or(true)
}

/// 2026-05-27: read `ui.inactive_pane_opacity` (percent 0-100) and
/// convert to a 0.0..=1.0 alpha. `ui.dim_inactive=false` short-
/// circuits to 1.0 so the inactive pane paints at full opacity (the
/// user has opted out of the dim entirely).
///
/// Used by `pane_chrome` for every pane kind via a single threaded
/// parameter; renderers without alpha support (TUI) ignore the
/// number and use their own dim modifier instead.
fn inactive_pane_opacity(app: &GpuiApp) -> f32 {
    let dim_on = app
        .options()
        .config
        .get_typed::<lattice_host::ui::theme_options::UiDimInactive>()
        .map(|v| *v)
        .unwrap_or(true);
    if !dim_on {
        return 1.0;
    }
    let percent = app
        .options()
        .config
        .get_typed::<lattice_host::ui::theme_options::UiInactivePaneOpacity>()
        .map(|v| *v)
        .unwrap_or(50);
    (percent.clamp(0, 100) as f32) / 100.0
}

/// 2026-05-27: insert-completion filter-chord footer helpers.
/// Mirror the TUI peer's adaption logic — full form when it fits,
/// compact `[b]uf │ [o]lsp …` otherwise, prune from the right
/// when even compact overflows. Order matches the popup keymap
/// (buffer / lsp / path / tree-sitter / snippet).
struct GpuiFilterChordEntry {
    key: &'static str,
    label: &'static str,
}

fn gpui_filter_chord_entries(
    sources_present: &std::collections::BTreeSet<&str>,
) -> Vec<GpuiFilterChordEntry> {
    let mut out: Vec<GpuiFilterChordEntry> = Vec::new();
    if sources_present.contains(lattice_completion::insert::BufferWordsSource::ID) {
        out.push(GpuiFilterChordEntry { key: "b", label: "buf" });
    }
    if sources_present.contains(lattice_completion::insert::LSP_COMPLETION_SOURCE_ID) {
        out.push(GpuiFilterChordEntry { key: "o", label: "lsp" });
    }
    if sources_present.contains(lattice_completion::insert::PATH_SOURCE_ID) {
        out.push(GpuiFilterChordEntry { key: "f", label: "path" });
    }
    if sources_present.contains(lattice_completion::insert::TREE_SITTER_SYMBOL_SOURCE_ID) {
        out.push(GpuiFilterChordEntry { key: "t", label: "ts" });
    }
    if sources_present.contains(lattice_completion::insert::SNIPPET_SOURCE_ID) {
        out.push(GpuiFilterChordEntry { key: "s", label: "snip" });
    }
    out
}

fn gpui_render_filter_chord_footer(
    entries: &[GpuiFilterChordEntry],
    width_cols: u16,
) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let w = width_cols as usize;
    let render = |full: bool, take: usize| -> String {
        let parts: Vec<String> = entries
            .iter()
            .take(take)
            .map(|e| {
                if full {
                    format!("<C-{}> {}", e.key, e.label)
                } else {
                    format!("[{}]{}", e.key, e.label)
                }
            })
            .collect();
        format!(" {} ", parts.join(" │ "))
    };
    let full = render(true, entries.len());
    if full.chars().count() <= w {
        return full;
    }
    let compact = render(false, entries.len());
    if compact.chars().count() <= w {
        return compact;
    }
    for take in (1..entries.len()).rev() {
        let pruned = render(false, take);
        if pruned.chars().count() <= w {
            return pruned;
        }
    }
    compact.chars().take(w).collect()
}

fn gpui_source_display_label(id: &str) -> &'static str {
    match id {
        "gen:buffer-words" => "buffer-words",
        "gen:lsp-completion" => "lsp",
        "gen:path" => "path",
        "gen:tree-sitter-symbol" => "tree-sitter",
        "gen:snippet" => "snippet",
        _ => "source",
    }
}

/// Slice `3c.unify.gpui-annotation-render`: shared candidate-row
/// builder for the picker (minibuffer + overlay) and cmdline-
/// completion (minibuffer + overlay) surfaces. Replaces four
/// near-identical inline row builders.
///
/// Layout: `[display + match highlights]   [annotations]` —
/// candidate text on the left, annotations right-aligned in a
/// dimmer colour. Matches the TUI peer's `candidate_to_line`
/// shape. Empty `annotations` → no right column (no extra width
/// reserved, no two-space gap).
///
/// `padded`: when true, applies `px_2()` for the strip variants
/// that need horizontal padding. The overlay variants paint
/// inside a bordered container that already has its own padding.
fn paint_candidate_row(
    cand: &lattice_completion::RenderedCandidate,
    selected: bool,
    theme: &GpuiTheme,
    padded: bool,
    display_col_chars: usize,
) -> gpui::Div {
    // Issue #35 (2026-05-22): match highlight now uses
    // `picker_match_highlight` (Catppuccin peach by default,
    // distinct from `foreground`). Previously used
    // `cursor_background` which is identical to `foreground`
    // in the Catppuccin Mocha defaults — match highlights
    // were invisible.
    let match_hl_fg = rgb(theme.picker_match_highlight);
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
    // Marginalia (kind glyph on the left + annotations on the
    // right) uses `picker_marginalia_fg`. The selected-row
    // case bumps to a slightly brighter shade so it stays
    // legible against the status background.
    let marginalia_fg = if selected {
        rgb(theme.foreground)
    } else {
        rgb(theme.picker_marginalia_fg)
    };
    let display = &cand.raw.display;

    // Left side: display text with optional per-char match
    // highlighting. Fast path: no match ranges → single child
    // (empty-query "show all" hits this every row).
    let display_div: gpui::Div = if cand.match_ranges.is_empty() {
        div().child(display.clone()).text_color(row_fg)
    } else {
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
        div().flex().flex_row().children(cells)
    };

    // 2026-05-27 column-aligned annotations. Caller computes
    // `display_col_chars` as the widest display in the visible
    // set; we pad this row's display with trailing spaces to
    // that column count so all rows' annotations start at the
    // same x. Bounded by candidate width, not container width
    // (an earlier `flex_grow` approach right-justified to the
    // container edge — fine for narrow popups but pushed the
    // annotation absurdly far in maximized windows). `+ 2`
    // leaves a small gap between the longest display and the
    // annotation column.
    let annotation_text = cand.annotations.join("  ");
    let mut row = div().flex().flex_row().w_full();
    if padded {
        row = row.px_2();
    }
    // Left-margin kind glyph (one ASCII char) so the user can
    // scan candidates by kind (`f` = file, `b` = buffer,
    // `:` = command, etc.). Marginalia color so it doesn't
    // compete with the display text.
    let kind_glyph = format!("{} ", cand.raw.kind.glyph());
    row = row
        .child(div().text_color(marginalia_fg).flex_shrink_0().child(kind_glyph))
        .child(display_div.flex_shrink_0());
    if !annotation_text.is_empty() {
        // Pad spaces so this row's annotation lands at the same
        // column as every other row. `+ 2` leaves a small gap.
        let display_chars = display.chars().count();
        let pad_chars = display_col_chars
            .saturating_sub(display_chars)
            .saturating_add(2);
        if pad_chars > 0 {
            row = row.child(
                div()
                    .text_color(marginalia_fg)
                    .flex_shrink_0()
                    .child(" ".repeat(pad_chars)),
            );
        }
        row = row.child(
            div()
                .text_color(marginalia_fg)
                .flex_shrink_0()
                .child(annotation_text),
        );
    }
    if let Some(bg) = row_bg { row.bg(bg) } else { row }
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

/// Per-frame ensure-work gate keys (perf plan A.3).
///
/// `EditorView::render` used to fire `ensure_cursor_in_viewport()`
/// and `dispatch_action(RefreshPaneHighlights)` unconditionally every
/// frame. Both dispatches walk the full `publish_render_state` tail
/// (rebuilding every sub-state `Arc`, notifying the highlights worker,
/// nudging `paint_request`) regardless of whether the underlying
/// inputs changed — the dominant slice of `ensure_us` in the trace
/// data the perf plan opens with.
///
/// Each key captures the inputs that would actually change the
/// dispatch's effect. When the current frame's key matches the stored
/// one, the dispatch is skipped — paramount goal #1 (sub-frame input
/// latency).
///
/// `cursor_snap_key` is stored as the POST-dispatch key so the cache
/// settles in one frame after a snap (the dispatch mutates `scroll`,
/// which is part of the key; storing the pre-dispatch key would
/// re-fire on the next frame for a guaranteed no-op).
///
/// `pane_refresh_key` uses `Arc::as_ptr(&pane_tree)` as a cheap
/// identity probe: the tree Arc is rebuilt by `publish_render_state`
/// whenever any pane state (buffer_id, scroll, split, focus) changes.
/// Trade-off: if an *inactive* pane's document text version advances
/// (rare — typically only on LSP edits in an unfocused buffer), the
/// gate stays closed until the next pane switch. The user can't see
/// stale highlights on a pane they're not focused on; the moment they
/// switch focus, `active_idx` changes → key changes → refresh fires.
#[derive(Default)]
struct EnsureGateCache {
    cursor_snap_key: Option<(
        lattice_core::protocol::position::Position,
        u32,
        u32,
        lattice_core::BufferKind,
    )>,
    pane_refresh_key: Option<(usize, usize, lattice_core::BufferId)>,
}

/// The renderer-side composition root rendered as a GPUI
/// `Entity`. Holds the [`GpuiApp`] + a [`FocusHandle`] so the
/// window's key events actually route to our dispatcher.
struct EditorView {
    app: GpuiApp,
    focus_handle: FocusHandle,
    /// Perf plan A.3: per-frame ensure-work delta cache.
    ensure_gate: EnsureGateCache,
    /// S4.final.b (2026-05-27): per-window glyph-id cache
    /// (cached `char → ResolvedGlyph` map keyed by FontId).
    /// Shared across panes within this window — paint_cells
    /// looks up cells from this resolver instead of going
    /// through `shape_line`. Wrapped in `Mutex` for
    /// `&mut self` access during paint without conflicting with
    /// any other mutable borrows on `EditorView` itself.
    /// Gated only by the `window` feature (mirrors `app`);
    /// the resolve path uses `&mut Window` which is paint-only.
    glyph_resolver: std::sync::Arc<std::sync::Mutex<crate::glyph_resolver::GlyphResolver>>,
}

impl EditorView {
    fn new(document: Document, cx: &mut Context<Self>) -> Self {
        let app = GpuiApp::new(document);
        // X1b: spawn the worker-paint-request bridge. The
        // highlights worker fires `editor.paint_request.notify_one()`
        // after every `WorkerDecision::Recomputed`; this future
        // awaits each wake and calls `cx.notify()` so GPUI
        // schedules a paint even when no user input is in flight.
        // Without this bridge, an async worker recompute that
        // finishes while the user is idle (e.g. final reparse
        // after a held-key burst settles) would publish fresh
        // spans into `syntax_visible_spans_cell` that nothing
        // reads until the next keystroke -- breaking goal-#4
        // asynchronicity (the renderer would effectively poll on
        // keystrokes for worker output).
        //
        // `cx.spawn` runs the future on GPUI's foreground
        // executor; the `AsyncWindowContext` argument lets us
        // upgrade the weak entity handle and call `cx.notify()`.
        // The future exits cleanly when the weak handle can't
        // upgrade (window closed).
        // Slice 3c.final.B-extension: paint_request cached on GpuiApp
        // at boot (no read_editor round-trip — wait-free Arc clone).
        //
        // 2026-05-27 X1b extension: also drain pending LSP / event /
        // mode-lifecycle results inside the bridge. Without this,
        // an idle LSP response (e.g. the first `K` hover) fires
        // paint_request.notify_one() but the resulting paint runs
        // a render that never calls `run_tick_pending` (X1 moved
        // it to the keystroke dispatch tail for perf), so the
        // response sits in `pending_hover_rx` until the user
        // presses another key. Draining here closes that gap
        // while keeping the keystroke fast path's amortization.
        let paint_request = app.paint_request.clone();
        cx.spawn(async move |this, cx| {
            loop {
                paint_request.notified().await;
                if this
                    .update(cx, |view, cx| {
                        let signals = view.app.mutate_editor_with(|e| e.run_tick_pending());
                        for signal in signals {
                            view.app.handle_renderer_signal(signal);
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        Self {
            app,
            focus_handle: cx.focus_handle(),
            ensure_gate: EnsureGateCache::default(),
            glyph_resolver: std::sync::Arc::new(std::sync::Mutex::new(
                crate::glyph_resolver::GlyphResolver::new(),
            )),
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
        // Slice 3c.final.B (group 3): popup gate via published
        // substate (`render_state.popup.is_open()`).
        // Slice 3c.final.E.5j: read RS via `App::render_state` (App
        // owns the same Arc, cloned at construction time).
        if self.app.render_state.load().popup.is_open()
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
        // Slice 3c.final.B (group 6): lifecycle read via published
        // substate.
        if self.app.render_state.load().lifecycle.should_quit {
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
    fn paint_pane_tree(
        &self,
        node: &PaneNode,
        theme: &GpuiTheme,
        active_idx: usize,
        row_px: f32,
    ) -> gpui::Div {
        // 2026-05-27: split branches drop `.size_full()`. With both
        // `.flex_grow()` and `.size_full()`, the split's hypothetical
        // main size resolved to `height: 100%` of the parent (which
        // is the document area, but for the second split level the
        // parent is the WINDOW root because flex children's percentage
        // resolves up the tree). With three+ panes the cumulative
        // hypothetical sum exceeded the window height; the cmdline
        // row at the bottom got pushed off-screen — `:q<CR>` worked
        // but no minibuffer was visible. `.flex_grow()` alone gives
        // the split a `basis: auto` so it consumes only free space,
        // never claiming an explicit 100% that displaces siblings.
        //
        // `row_px` is the editor row height threaded through so
        // terminal panes can lock each painted row to exactly that
        // metric (default GPUI `text-sm` line-height is ~20px while
        // the editor uses ~18.2px; the mismatch made a terminal pane
        // claim more vertical space than allocated and pushed the
        // modeline/cmdline off-screen when split alongside a doc).
        // 2026-05-27: each split child gets a ratio-weighted
        // `flex_basis` + `min_w(px(0))` / `min_h(px(0))`.
        //
        // The `min_*` line lets the item shrink below its content's
        // intrinsic min-size (default `min: auto`).
        //
        // The basis is set to `ratio × RATIO_SCALE` (where RATIO_SCALE
        // is large enough that the basis sum always exceeds the
        // container width on any plausible window — at which point
        // the flex shrink algorithm distributes the deficit
        // proportionally to basis, giving each child exactly its
        // `ratio` fraction of the container). This decouples the
        // visual split ratio from content (so a wide terminal can't
        // hijack the split) AND honours the host's `ratio` field
        // (so `<C-w>>` / `<C-w><` user resizing actually moves the
        // visible boundary instead of just rescaling the underlying
        // alacritty grid).
        //
        // Earlier `flex_basis(px(0))` was content-independent but
        // ignored ratio entirely (always 50/50); reverting to
        // `flex_basis: auto` let content win (terminal hijack).
        // Ratio-weighted basis is the third path that satisfies
        // both invariants.
        const RATIO_SCALE: f32 = 1_000_000.0;
        match node {
            PaneNode::Leaf(idx) => self.paint_pane(*idx, theme, *idx == active_idx, row_px),
            PaneNode::HorizontalSplit { top, bottom, ratio } => {
                let ratio = ratio.clamp(0.05, 0.95);
                div()
                    .flex()
                    .flex_col()
                    .flex_grow()
                    .child(
                        self.paint_pane_tree(top, theme, active_idx, row_px)
                            .flex_grow()
                            .flex_basis(px(ratio * RATIO_SCALE))
                            .min_h(px(0.0))
                            .border_b_1()
                            .border_color(rgb(theme.popup_border)),
                    )
                    .child(
                        self.paint_pane_tree(bottom, theme, active_idx, row_px)
                            .flex_grow()
                            .flex_basis(px((1.0 - ratio) * RATIO_SCALE))
                            .min_h(px(0.0)),
                    )
            }
            PaneNode::VerticalSplit { left, right, ratio } => {
                let ratio = ratio.clamp(0.05, 0.95);
                div()
                    .flex()
                    .flex_row()
                    .flex_grow()
                    .child(
                        self.paint_pane_tree(left, theme, active_idx, row_px)
                            .flex_grow()
                            .flex_basis(px(ratio * RATIO_SCALE))
                            .min_w(px(0.0))
                            .border_r_1()
                            .border_color(rgb(theme.popup_border)),
                    )
                    .child(
                        self.paint_pane_tree(right, theme, active_idx, row_px)
                            .flex_grow()
                            .flex_basis(px((1.0 - ratio) * RATIO_SCALE))
                            .min_w(px(0.0)),
                    )
            }
        }
    }

    /// Paint a single pane. Active pane uses `editor.cursor` +
    /// `editor.visible_highlights` (refreshed at the top of
    /// Shared pane chrome (2026-05-25): wraps `inner` content with
    /// the per-pane structural layout — flex-column outer that
    /// `flex_grow`s into the parent allocation and `overflow_hidden`s
    /// excess content, an inner padded content slot for the buffer
    /// painter, and the per-pane status bar at the bottom. Applied
    /// uniformly to every buffer kind (document, terminal, oil,
    /// file-tree, ...) so no kind can render past its allocated
    /// vertical space and bleed into the modeline / cmdline
    /// [[feedback_buffers_no_special_case]].
    fn pane_chrome(
        inner: AnyElement,
        status_text: String,
        render_active: bool,
        theme: &GpuiTheme,
        inactive_opacity: f32,
    ) -> gpui::Div {
        let (status_bg, status_fg) = if render_active {
            (rgb(theme.cursor_background), rgb(theme.cursor_foreground))
        } else {
            (rgb(theme.status_background), rgb(theme.status_foreground))
        };
        // 2026-05-27: dim the buffer content (NOT the status row)
        // when this pane is inactive. The TUI peer composes
        // `inactive_pane_overlay = Style::empty().dim()` over every
        // inactive pane's painted text — visible separation between
        // "where input goes" and "everywhere else". The GPUI peer
        // had no equivalent; only the status row colour changed,
        // which the user reported as "too subtle".
        //
        // Applied as `.opacity()` on the content wrapper. Status row
        // stays at full opacity so the pane identity / cursor coords
        // are still legible on inactive panes.
        //
        // `inactive_opacity` is user-configurable via
        // `:set ui.inactive_pane_opacity=N` (percent 0-100). Default
        // 50 (= 0.5 alpha).
        let content_opacity: f32 = if render_active { 1.0 } else { inactive_opacity };
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
                    .opacity(content_opacity)
                    .child(inner),
            )
            .child(
                div()
                    .bg(status_bg)
                    .text_color(status_fg)
                    .px_2()
                    .py_1()
                    .flex()
                    .flex_row()
                    .child(div().child(status_text)),
            )
    }

    /// render); inactive panes use the stashed `PaneState::cursor`
    /// and no highlights. Each pane gets its own status line at
    /// its bottom (path + cursor coords), which keeps the visible
    /// boundary between panes legible without a hard chrome border.
    fn paint_pane(
        &self,
        pane_idx: usize,
        theme: &GpuiTheme,
        is_active: bool,
        row_px: f32,
    ) -> gpui::Div {
        // Slice 3c.final.E.swap: paint reads route through the
        // App's own `render_state` Arc (cloned from
        // `editor.render_state` at construction). No `&Editor`
        // borrow held across the function body.
        let ad = self.app.ad();
        let rs_guard = self.app.render_state.load();
        let active_spans_guard = rs_guard.syntax.visible_spans.load();
        // S4.3 (2026-05-27): the prepaint `visible_rows` load
        // retired here — `EditorElement`'s active-pane shaping
        // now reads from `rs_guard.cells.matrix` (S4.1 wiring),
        // falling back to `active_spans_guard` / pane-cached
        // spans for boot frames / folded rows / inactive panes.
        // The highlights worker still publishes `visible_rows`
        // for TUI markdown / help / messages bodies in other
        // render functions.
        // Perf plan B.2 slice B.2.a: worker's per-row pre-bucketed
        // static-overlay quads (doc_highlight / all_matches /
        // substitute). Active pane consumes this directly; inactive
        // panes fall through to the legacy per-frame bucket (only
        // doc_highlight is painted for them and N is small).
        let active_overlay_quads_guard =
            rs_guard.syntax.static_overlay_quads.load();
        // Phase 5.8.AF.5 / Slice 3c.final.B (group 1): pane tree
        // + buffer registry read through `rs_guard.panes` /
        // `rs_guard.buffers` instead of `editor.X` directly.
        let leaves = rs_guard.panes.tree.leaves();
        if pane_idx >= leaves.len() {
            return div().child(format!("(stale pane index {pane_idx})"));
        }
        let pane: &PaneState = &leaves[pane_idx];
        // Issue #40 / Terminal-mode T1: paint terminal-kind panes
        // from the PTY-reader's published `TerminalSnapshot`. T2
        // promotes the inner-content build to a pane-render
        // provider registered by `terminal-mode`; until then the
        // dispatch lives inline. Critically — content is returned
        // as an AnyElement that flows through the same
        // [`Self::pane_chrome`] wrapper as every other buffer kind
        // so the modeline / cmdline / per-pane status bar always
        // reserves its row, no kind-specific layout
        // [[feedback_buffers_no_special_case]].
        if matches!(pane.buffer, lattice_core::BufferKind::Terminal) {
            // T2.b: the cursor cell renders as a left vertical
            // bar in Terminal-Insert and as a full bg/fg-swap
            // block in Normal-in-terminal. Only the active pane
            // honours `terminal_insert_active`; inactive panes
            // always paint the block so the user can still see
            // where each shell's cursor sits.
            let insert_active = is_active && ad.terminal_insert_active;
            let (inner, status_text) = self.build_terminal_inner(
                pane,
                &rs_guard,
                theme,
                insert_active,
                is_active,
                row_px,
            );
            return Self::pane_chrome(
                inner,
                status_text,
                is_active,
                theme,
                inactive_pane_opacity(&self.app),
            );
        }
        // Resolve the buffer's document handle. Inactive panes may
        // reference buffers different from `editor.document`; the
        // registry clone on `rs_guard.buffers` shares the editor's
        // `Arc<Mutex<...>>` so the lookup sees the latest state.
        let snapshot_opt = rs_guard
            .buffers
            .registry
            .document_handle(pane.buffer_id)
            .map(|h| h.snapshot());
        let Some(snapshot) = snapshot_opt else {
            return div()
                .p_3()
                .child(format!("(buffer {:?} unavailable)", pane.buffer_id));
        };
        // Stage A.1 [DONE]: full `snapshot.text()` + `split('\n')`
        // materialisation removed; visible rows are pulled from the
        // rope below. The 3c.atomic.L timing scaffold that surrounded
        // that work is now gated behind `profile-frames` (perf plan
        // A.4) — see the `frame_us` block at the end of `render`.
        #[cfg(feature = "profile-frames")]
        let text_us: u64 = 0;
        // 2026-05-22 issue #24 (cursor companion of pane_scroll
        // below): when popup owns active_buffer, ad.cursor
        // describes the popup buffer's cursor — the document
        // pane's cursor must come from its stashed snapshot.
        let popup_owns_active = ad.buffer_kind == lattice_core::BufferKind::Help;
        // When the popup has focus the document pane should look
        // inactive — no cursorline, no selection, no active status
        // bar — the same appearance it has when a different pane has
        // focus.
        let render_active = is_active && !popup_owns_active;
        let cursor = if render_active {
            ad.cursor
        } else {
            pane.cursor
        };
        let total_lines_u32 = snapshot.buffer.line_count();
        let total_lines = total_lines_u32 as usize;
        // we will fill raw_lines after computing visible_start/end
        #[cfg(feature = "profile-frames")]
        tracing::debug!(
            target: "lattice_gpui::perf",
            pane_idx,
            is_active,
            text_bytes = snapshot.buffer.byte_len(),
            line_count = total_lines_u32 as u64,
            text_us,
            split_us = 0u64,
            "paint_pane text materialisation"
        );
        // 5.8.O: clip the visible window to `[scroll, scroll +
        // viewport_height)` so large docs don't render every line
        // every frame. Active pane reads scroll from `editor.scroll`
        // (ensure_cursor_in_viewport keeps it sane); inactive
        // panes read their stashed `PaneState::scroll`. The
        // gutter, status, and cursor maths still work in terms of
        // absolute line indices — only the iter range tightens.
        // 2026-05-22 issue #24: when active_buffer is Help, the
        // popup has "stolen" ad.scroll / ad.cursor (they now
        // describe the popup buffer's state, not the document
        // pane's). The document pane is still painted underneath
        // — its scroll should come from the stashed `pane.scroll`
        // captured at popup-open via `snapshot_active_pane`,
        // NOT from ad.scroll. Without this guard, opening
        // `:describe-buffer` scrolled the background document to
        // line 0 (the help buffer's initial scroll).
        let pane_scroll = if render_active {
            ad.scroll
        } else {
            pane.scroll
        };
        // 2026-05-27: when the popup is focused (State B), `ad`
        // describes the POPUP's viewport_height (the
        // `popup_inner_rows` override the render loop applies up
        // top), not the document pane's. The doc pane sits behind
        // the popup, dimmed but fully painted — read its OWN
        // leaf viewport so `visible_end` paints the full pane area,
        // not the popup-sized subset. Same fallback applies to
        // inactive split panes (their `pane.viewport_height` is
        // their leaf row count).
        let viewport_height = if render_active {
            ad.viewport_height.max(1)
        } else {
            pane.viewport_height.max(1)
        };
        let visible_start = (pane_scroll as usize).min(total_lines);
        let visible_end = (pane_scroll as usize)
            .saturating_add(viewport_height as usize)
            .min(total_lines);

        // Stage A.1: materialise only visible lines into a small Vec<String>.
        let mut raw_lines: Vec<String> = Vec::with_capacity(visible_end.saturating_sub(visible_start));
        for li in visible_start..visible_end {
            raw_lines.push(snapshot.buffer.line(li as u32).unwrap_or_default());
        }
        let cursor_shape = if render_active {
            Some(CursorShape::for_mode(ad.modal))
        } else {
            None
        };

        // Slice X3.full.2: the element always paints the active
        // pane's `visible_spans`. Inactive panes whose buffer
        // differs from the active doc currently paint with the
        // active spans (visually-stale highlights) -- the
        // pre-existing slice 1 limitation; the per-pane span
        // cache resync into the element is a follow-up slice.
        let total_lines_for_gutter = total_lines.max(1);
        let gutter_width = total_lines_for_gutter.to_string().len();

        // 5.8.I: per-line severity lookup. URI for this pane's
        // buffer comes from `rs_guard.buffers.uris` (slice 3c.final.B
        // group 1: published HashMap clone of `editor.buffer_uris`,
        // populated when LSP attaches). `None` means: unsaved
        // scratch, no LSP attachment, or LSP-mode disabled for this
        // buffer. The gutter then renders a blank sign column (one
        // space) so the line-number alignment stays stable
        // regardless of whether diagnostics are present.
        let uri = rs_guard.buffers.uris.get(&pane.buffer_id);
        // Phase 5.8.AF.5 / Slice 3a: read through the renderer's
        // `RenderState` contract instead of `editor.lsp_diagnostics`
        // directly. Symmetric with the TUI peer's
        // `severity_for_line` migration. `load_full` is wait-free
        // (~2ns); the returned snapshot's diagnostics layer is
        // internally `Arc<ArcSwap<...>>`-backed so the inner
        // `line_severity` call stays wait-free too.
        // Slice 3c.final.E.swap: render_state via App's own Arc.
        let render_state = self.app.render_state.load_full();
        let line_severity = |line_idx: u32| -> Option<lattice_lsp::DiagnosticSeverity> {
            uri.and_then(|u| render_state.diagnostics.layer.line_severity(u, line_idx))
        };

        // 5.8.N: severity glyph + colour come from host_theme so
        // `:set ui.diagnostics.*` overrides flow through identically
        // for both renderer peers.
        // Slice 3c.final.B (group 6): host theme via published
        // top-level field. Theme is `Copy` so this is a plain
        // struct move.
        let host_theme = rs_guard.theme;

        // Phase 5.8.AF.5 / Slice X3.full.2 + 3c.final.B (group 2):
        // gather per-row gutter metadata for the visible window.
        // Caller does the LSP / fold lookups so the element holds
        // only owned values. Folds + foldenable now come from
        // `rs_guard.active_document` rather than `editor.X` — the
        // predicate is inlined here since the published `Arc<[Fold]>`
        // doesn't carry the helper methods. Behaviour matches
        // `Editor::line_inside_closed_fold` + `fold_start_at`
        // (both gate on `option_cache.foldenable`).
        // Perf plan C: build a fold lookup index once per pane (build
        // cost O(folds), typically <1 µs). The two predicates below
        // used to walk the entire fold list per visible line —
        // O(rows × folds) per pane per frame. The index drops the
        // per-line check to a partition-point binary search with a
        // constant-time fast path for non-overlapping folds.
        let fold_index = lattice_host::folds::FoldIndex::from_folds(
            &rs_guard.active_document.folds,
            rs_guard.active_document.option_cache.foldenable,
        );
        let gutter_meta: Vec<crate::editor_element::GutterLineMeta> = (visible_start..visible_end)
            .filter(|line_idx| !fold_index.line_inside_closed_fold(*line_idx as u32))
            .map(|line_idx| {
                let fold_start = fold_index.closed_fold_start_at(line_idx as u32);
                let severity =
                    line_severity(line_idx as u32).map(|s| diagnostic_glyph_and_color(&host_theme, s));
                crate::editor_element::GutterLineMeta {
                    line_idx: line_idx as u32,
                    fold_start,
                    severity,
                }
            })
            .collect();

        // Active-pane cursor state. `None` on inactive panes so the
        // element doesn't paint a cursor marker there.
        //
        // Issue #19/#21 (2026-05-22): also `None` when active_buffer
        // is Help (State B: popup focused). The document pane is
        // still painted, but the cursor is "inside" the popup
        // conceptually. Hiding the document cursor signals to the
        // user that focus has shifted, complementing the popup
        // border-color change above.
        let cursor_state = match (render_active, cursor_shape) {
            (true, Some(shape)) => {
                // 2026-05-26: read the cursor's source line from
                // the document snapshot. `EditorElement.text` was
                // zeroed in slice A.4 (`1e1da8d`) so the element
                // can't recover the cursor's line text itself —
                // pass it via `CursorState.line_text` here.
                let line_text = snapshot.buffer.line(cursor.line).unwrap_or_default();
                Some(crate::editor_element::CursorState {
                    line: cursor.line,
                    byte: cursor.byte,
                    shape,
                    line_text,
                })
            }
            _ => None,
        };

        // Slice X3.full.3 decoration data. All `Range`s in host
        // state are utf-8 byte coordinates; the lone exception is
        // LSP document-highlights (utf-16 columns) -- we convert
        // them here against actual line text so the element sees
        // only utf-8.
        // Slice 3c.final.B (group 2): visual_range / current_match /
        // all_matches / substitute_preview read through the published
        // `rs_guard.active_document` snapshot instead of `editor.X`.
        let visual_range = if render_active {
            rs_guard.active_document.visual_range
        } else {
            None
        };
        let current_match = if render_active {
            rs_guard.active_document.current_match
        } else {
            None
        };
        let all_matches: Vec<lattice_core::protocol::position::Range> = if render_active {
            rs_guard.active_document.all_matches.to_vec()
        } else {
            Vec::new()
        };
        let substitute_matches: Vec<lattice_core::protocol::position::Range> = if render_active {
            rs_guard
                .active_document
                .substitute_preview
                .as_ref()
                .map(|p| p.matches.to_vec())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        // LSP document-highlights: utf-16 columns at the protocol
        // boundary; convert to utf-8 byte offsets against the
        // hit's host line text. Painted on any pane sharing the
        // highlighted buffer (the per-buffer pump model).
        let rs_for_dh = self.app.render_state.load_full();
        let dh_guard = rs_for_dh.lsp.document_highlights.load_full();
        let doc_highlights: Vec<lattice_core::protocol::position::Range> = dh_guard
            .as_deref()
            .filter(|cache| cache.buffer_id == pane.buffer_id)
            .map(|cache| {
                cache
                    .highlights
                    .iter()
                    .map(|h| {
                        let start_line = h.range.start.line;
                        let end_line = h.range.end.line;
                        let start_text = if (start_line as usize) < total_lines { snapshot.buffer.line(start_line).unwrap_or_default() } else { String::new() };
                        let end_text = if (end_line as usize) < total_lines { snapshot.buffer.line(end_line).unwrap_or_default() } else { String::new() };
                        let start_byte = lattice_lsp::position::utf16_column_to_utf8_byte(
                            &start_text,
                            h.range.start.character,
                        );
                        let end_byte = lattice_lsp::position::utf16_column_to_utf8_byte(
                            &end_text,
                            h.range.end.character,
                        );
                        lattice_core::protocol::position::Range {
                            start: lattice_core::protocol::position::Position {
                                line: start_line,
                                byte: start_byte,
                            },
                            end: lattice_core::protocol::position::Position {
                                line: end_line,
                                byte: end_byte,
                            },
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        // Cursorline bg: host_theme drives `:set ui.cursor-line-bg`
        // overrides. Fallback Catppuccin surface0.
        // Slice 3c.final.B (group 6): reuse `host_theme` bound
        // above from `rs_guard.theme`.
        let cursorline_bg = host_theme.cursor_line_bg.to_rgb_u32(0x313244);

        // Slice X3.full.4: gather LSP inlay hints + diagnostic
        // underline ranges for this pane's buffer. Both arrive
        // in LSP coordinates (utf-16 character columns) and are
        // converted to utf-8 bytes against the buffer's actual
        // line text here, at the boundary. The element only sees
        // utf-8.
        let inlay_hints: Vec<crate::editor_element::InlayHintRow> = render_state
            .lsp
            .inlay_hints
            .get_for(pane.buffer_id)
            .map(|cache| {
                cache
                    .hints
                    .iter()
                    .map(|h| {
                        let line_idx = h.position.line;
                        let line_text = if (line_idx as usize) < total_lines { snapshot.buffer.line(line_idx).unwrap_or_default() } else { String::new() };
                        let byte = lattice_lsp::position::utf16_column_to_utf8_byte(
                            &line_text,
                            h.position.character,
                        );
                        let mut text = lattice_lsp::inlay_hint_label_text(&h.label);
                        if h.padding_left.unwrap_or(false) {
                            text.insert(0, ' ');
                        }
                        if h.padding_right.unwrap_or(false) {
                            text.push(' ');
                        }
                        crate::editor_element::InlayHintRow {
                            line: line_idx,
                            byte,
                            text,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Diagnostic underlines: pull `Arc<[Diagnostic]>` for the
        // pane's URI (`None` => unsaved scratch / no LSP /
        // disabled). For each diagnostic, convert utf-16 → utf-8
        // against the corresponding line, resolve severity →
        // color via `diagnostic_glyph_and_color`.
        let diagnostic_underlines: Vec<crate::editor_element::DiagnosticUnderline> = uri
            .and_then(|u| render_state.diagnostics.layer.diagnostics_arc(u))
            .map(|diags| {
                diags
                    .iter()
                    .map(|d| {
                        let start_line = d.range.start.line;
                        let end_line = d.range.end.line;
                        let start_text = if (start_line as usize) < total_lines { snapshot.buffer.line(start_line).unwrap_or_default() } else { String::new() };
                        let end_text = if (end_line as usize) < total_lines { snapshot.buffer.line(end_line).unwrap_or_default() } else { String::new() };
                        let start_byte = lattice_lsp::position::utf16_column_to_utf8_byte(
                            &start_text,
                            d.range.start.character,
                        );
                        let end_byte = lattice_lsp::position::utf16_column_to_utf8_byte(
                            &end_text,
                            d.range.end.character,
                        );
                        let color = d
                            .severity
                            .map(|s| diagnostic_glyph_and_color(&host_theme, s).1)
                            // Unknown severity: fall back to overlay2
                            // (matches `diagnostic_glyph_and_color`).
                            .unwrap_or(0x9399b2);
                        crate::editor_element::DiagnosticUnderline {
                            range: lattice_core::protocol::position::Range {
                                start: lattice_core::protocol::position::Position {
                                    line: start_line,
                                    byte: start_byte,
                                },
                                end: lattice_core::protocol::position::Position {
                                    line: end_line,
                                    byte: end_byte,
                                },
                            },
                            color,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Inlay color: Catppuccin overlay1; host-theme override
        // hook lands when `host_theme.inlay_foreground` is added.
        let inlay_color: u32 = 0x7f849c;

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
        // Status-bar fg/bg colours are picked inside `pane_chrome`
        // off `render_active` so every kind goes through one
        // styling path; the document arm no longer derives them
        // locally.

        // Phase 5.8.AF.5 / Slice X3.full.2: pane body is one
        // `EditorElement` that shapes + paints visible lines
        // directly via `WindowTextSystem::shape_line` +
        // `ShapedLine::paint`. The legacy `rows` Vec construction
        // (one Div per cell, per line) was deleted in this slice;
        // gutter + cursor moved into the element. Decoration
        // backgrounds (selection / hlsearch / doc-highlight /
        // cursorline / substitute) restore in slice X3.full.3;
        // inlay-hint virtual text + per-cell diagnostic
        // underlines restore in slice X3.full.4.
        let editor_element = crate::editor_element::EditorElement {
            pane_idx,
            theme: theme.clone(),
            // 2026-05-26: pass the visible-window text joined so
            // EditorElement's `raw_lines.split('\n')` recovers the
            // visible lines. Slice A.4 (`1e1da8d`) zeroed this
            // field to stop materialising the FULL document text
            // — but the prepaint loop still indexes `raw_lines`
            // by absolute line_idx for the body+row_meta source,
            // and for synthetic buffers (*lsp* / *messages*) the
            // worker prepaint isn't populated so `shape_row`'s
            // line-text fallback fell to `""` and rendered empty.
            // The element subtracts `scroll` from line_idx to
            // index this visible-window subset.
            text: std::sync::Arc::new(raw_lines.join("\n")),
            // Issue #25 (2026-05-22): per-pane visible_spans for
            // multi-split support. Active pane reads the live
            // visible_spans cell (the highlights worker writes
            // there continuously). Inactive panes read from
            // `pane_highlights[pane_idx]` populated by the
            // RefreshPaneHighlights dispatch fired in the render
            // body. Empty when the cache hasn't refreshed yet
            // (first frame after split); the renderer paints
            // plain text in that case until the next frame.
            visible_spans: if render_active {
                (*active_spans_guard).clone()
            } else {
                let pane_spans = rs_guard
                    .syntax
                    .pane_highlights
                    .get(&pane_idx)
                    .cloned()
                    .unwrap_or_else(|| {
                        std::sync::Arc::new(Vec::<Vec<lattice_syntax::StyledSpan>>::new())
                    });
                // Perf plan D.1: `VisibleSpans.spans` is now
                // `Arc<[Vec<StyledSpan>]>`. The pane cache still
                // stores `Arc<Vec<Vec<StyledSpan>>>` (host-side
                // shape unchanged), so we clone its inner Vec into
                // a fresh `Arc<[T]>` here. A future slice could
                // migrate `pane_highlights` storage to match,
                // collapsing this clone to an Arc bump.
                lattice_host::render_state::VisibleSpans {
                    spans: (*pane_spans).clone().into(),
                    computed_for_key: lattice_host::render_state::VisibleHighlightsKey::default(),
                }
                .into()
            },
            // Perf plan B.2 slice B.2.a: active pane consumes the
            // worker's static-overlay bucket; inactive panes keep
            // the per-frame `push_range_quads` path (only
            // doc_highlight is painted there and N is small).
            worker_static_overlay_quads: if render_active {
                Some((*active_overlay_quads_guard).clone())
            } else {
                None
            },
            scroll: pane_scroll,
            viewport_height,
            gutter: gutter_meta,
            gutter_width,
            cursor: cursor_state,
            is_active: render_active,
            visual_range,
            current_match,
            all_matches,
            substitute_matches,
            doc_highlights,
            cursorline_bg,
            inlay_hints,
            diagnostic_underlines,
            inlay_color,
            // S4.1 (2026-05-27): active pane consumes the cell
            // matrix published by the cell-builder worker;
            // inactive panes pass `None` (mirrors `visible_rows`
            // — the cells worker only publishes for the active
            // document). The `prepaint` body branches use this
            // as the first try in a `cells → prepaint → legacy`
            // fallback chain; folded rows / boot frames / the
            // brief buffer-switch gap fall through to the
            // existing prepaint and legacy paths.
            cell_matrix: if render_active {
                Some(rs_guard.cells.matrix.load_full())
            } else {
                None
            },
            // S4.final.b (2026-05-27): per-window glyph-id
            // cache. Always carries the shared resolver from
            // `EditorView`; consumption is gated on
            // `paint_cells_enabled()` in `EditorElement::paint`.
            // Sharing across panes means a buffer-switch keeps
            // the cache warm.
            glyph_resolver: self.glyph_resolver.clone(),
        };

        Self::pane_chrome(
            editor_element.into_any_element(),
            status_line,
            render_active,
            theme,
            inactive_pane_opacity(&self.app),
        )
    }

    /// Issue #40 / Terminal-mode T1: build the inner content of a
    /// terminal-kind pane from the `TerminalSnapshot` published
    /// by the PTY reader task. T1 renders monochrome cell text
    /// only; T2 layers SGR colors + cursor-shape + alt-screen
    /// handling once alacritty_terminal is wired into
    /// `lattice-terminal::reader`.
    ///
    /// Returns the inner pane content + a status-bar label. The
    /// caller wraps both via [`Self::pane_chrome`] so the
    /// terminal's vertical extent is bounded by the standard
    /// pane chrome and can never render past the modeline
    /// [[feedback_buffers_no_special_case]].
    ///
    /// The substrate stays decoupled: this helper touches only
    /// `TerminalSnapshot`'s public accessors (`rows`, `cols`,
    /// `cell_at`) — no reader / grid internals.
    fn build_terminal_inner(
        &self,
        pane: &PaneState,
        rs_guard: &lattice_host::render_state::RenderState,
        theme: &GpuiTheme,
        insert_active: bool,
        is_active: bool,
        row_px: f32,
    ) -> (AnyElement, String) {
        let snap_opt = rs_guard.buffers.registry.with_terminal(pane.buffer_id, |t| {
            (
                t.snapshot.load_full(),
                t.current_match,
                t.visual,
                t.all_matches.clone(),
                t.nav_cursor,
            )
        });
        let Some((snap, current_match, visual, all_matches, nav_cursor)) = snap_opt else {
            let placeholder = div()
                .bg(rgb(theme.background))
                .text_color(rgb(theme.foreground))
                .child(format!("(terminal #{} unavailable)", pane.buffer_id.0))
                .into_any_element();
            return (
                placeholder,
                format!("  [terminal #{} unavailable]", pane.buffer_id.0),
            );
        };
        tracing::trace!(
            target: "lattice_gpui::terminal",
            buf_id = pane.buffer_id.0,
            rows = snap.rows,
            cols = snap.cols,
            seq = snap.seq,
            "build_terminal_inner: loaded snapshot",
        );
        // Status label format matches the document path's
        // `  {path}   L:{row}  C:{col}` shape so the per-pane
        // bar reads the same regardless of buffer kind. 2026-05-25:
        // prefer the registry `name` slot ("[zsh]", "[bash]", …)
        // populated at spawn time, falling back to the legacy
        // `terminal #N` form when the buffer hasn't been named.
        let name_label = rs_guard
            .buffers
            .registry
            .name_of(pane.buffer_id)
            .unwrap_or_else(|| format!("[terminal #{}]", pane.buffer_id.0));
        let status_text = format!(
            "  {name_label}   R:{}  C:{}",
            snap.cursor_row + 1,
            snap.cursor_col,
        );
        // Diagnostic placeholder while the reader hasn't
        // published its first frame: a string of spaces renders
        // as nothing visible, so the user perceives a fully
        // blank pane and can't tell whether the spawn worked,
        // the reader's still warming up, or the renderer's
        // off-path. Show a single status line so the spawn is
        // visible; the actual cell grid replaces it on the
        // first non-zero seq.
        if snap.seq == 0 {
            let placeholder = div()
                .bg(rgb(theme.background))
                .text_color(rgb(theme.foreground))
                .font_family(theme.font_family.clone())
                .child(format!(
                    "{name_label} — {}×{} — waiting for first output",
                    snap.rows, snap.cols,
                ))
                .into_any_element();
            return (placeholder, status_text);
        }
        // T2 substrate swap (2026-05-25): per-cell SGR colors
        // from alacritty's grid. The xterm-default 16-colour
        // palette is hardcoded here so unthemed terminals look
        // identical to a real xterm; a future slice promotes
        // these to the host theme so users can re-skin the
        // terminal palette without recompiling.
        use lattice_terminal::{CellAttrs, NamedColor as TermNamed, TerminalColor};
        const ANSI_PALETTE: [u32; 16] = [
            0x000000, 0xcd0000, 0x00cd00, 0xcdcd00, 0x0000ee, 0xcd00cd, 0x00cdcd, 0xe5e5e5,
            0x7f7f7f, 0xff0000, 0x00ff00, 0xffff00, 0x5c5cff, 0xff00ff, 0x00ffff, 0xffffff,
        ];
        let default_fg = theme.foreground;
        let default_bg = theme.background;
        // Map `TerminalColor::Indexed(16..=255)` (the xterm
        // 256-colour palette beyond the 16 named entries) to its
        // RGB approximation per the xterm spec: indices 16..=231
        // form a 6×6×6 cube; 232..=255 a 24-step grayscale ramp.
        fn indexed_to_rgb(i: u8) -> u32 {
            if (i as usize) < ANSI_PALETTE.len() {
                return ANSI_PALETTE[i as usize];
            }
            if i >= 232 {
                let lvl = 8 + 10 * (i - 232) as u32;
                return (lvl << 16) | (lvl << 8) | lvl;
            }
            let n = (i - 16) as u32;
            let r = (n / 36) % 6;
            let g = (n / 6) % 6;
            let b = n % 6;
            let scale = |v: u32| if v == 0 { 0 } else { 55 + 40 * v };
            (scale(r) << 16) | (scale(g) << 8) | scale(b)
        }
        let term_to_rgb = move |c: TerminalColor, is_fg: bool| -> u32 {
            match c {
                TerminalColor::Default => {
                    if is_fg { default_fg } else { default_bg }
                }
                TerminalColor::Named(n) => {
                    let idx = match n {
                        TermNamed::Black => 0,
                        TermNamed::Red => 1,
                        TermNamed::Green => 2,
                        TermNamed::Yellow => 3,
                        TermNamed::Blue => 4,
                        TermNamed::Magenta => 5,
                        TermNamed::Cyan => 6,
                        TermNamed::White => 7,
                        TermNamed::BrightBlack => 8,
                        TermNamed::BrightRed => 9,
                        TermNamed::BrightGreen => 10,
                        TermNamed::BrightYellow => 11,
                        TermNamed::BrightBlue => 12,
                        TermNamed::BrightMagenta => 13,
                        TermNamed::BrightCyan => 14,
                        TermNamed::BrightWhite => 15,
                    };
                    ANSI_PALETTE[idx]
                }
                TerminalColor::Indexed(i) => indexed_to_rgb(i),
                TerminalColor::Rgb(r, g, b) => {
                    ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
                }
            }
        };
        // Coalesce adjacent cells with identical (fg, bg, attrs)
        // into a single styled `div` per run. Saves the GPU
        // shaping engine from running once per cell when the
        // shell paints in uniform blocks (which is the common
        // case — the prompt, output text, etc.).
        // 2026-05-25: nav_cursor overrides the PTY cursor in
        // Normal-in-terminal so the user sees where j / k / etc.
        // is moving. Matches the TUI peer's logic.
        // 2026-05-27: only paint the terminal cursor when the pane
        // is active. Document panes already do this (cursor is
        // active-only); the terminal kept its PTY cursor visible on
        // every inactive split, breaking the "inactive panes don't
        // own input" visual cue.
        let (cursor_row, cursor_col, cursor_visible) = if !is_active {
            (0, 0, false)
        } else if let Some((nav_l, nav_c)) = nav_cursor {
            let off = snap.scroll_offset as i32;
            let row = nav_l + off;
            if (0..snap.rows as i32).contains(&row) && nav_c < snap.cols {
                (row as u16, nav_c, true)
            } else {
                (0, 0, false)
            }
        } else {
            let r = snap.cursor_row;
            let c = snap.cursor_col;
            let v = snap.cursor_visible && r < snap.rows && c < snap.cols;
            (r, c, v)
        };
        #[derive(Clone, Copy, PartialEq, Eq)]
        struct CellStyle {
            fg: u32,
            bg: u32,
            attrs: CellAttrs,
            cursor: bool, // true = cursor cell (forces its own run)
            highlight: bool, // T3.b.3: search-match cell
        }
        // T3.b.3: translate the current_match's alacritty grid
        // line into the visible-window row coords. Same shape
        // as the TUI peer's `match_overlay`.
        let match_overlay = current_match.and_then(|h| {
            let row = h.line + snap.scroll_offset as i32;
            if (0..snap.rows as i32).contains(&row) {
                let c_start = h.column;
                let c_end = h
                    .column
                    .saturating_add(h.len.min(u16::MAX as u32) as u16)
                    .min(snap.cols);
                Some((row as u16, c_start, c_end))
            } else {
                None
            }
        });
        // T3.b.2 / T3.b.2.b: Visual selection predicate. Same
        // shape as the TUI peer.
        let visual_state = visual;
        let mut rows: Vec<gpui::Div> = Vec::with_capacity(snap.rows as usize);
        for r in 0..snap.rows {
            // 2026-05-27: lock each terminal row to the editor's
            // row_px metric (font_size × 1.3). Without this, default
            // GPUI text rendering used a larger line-height (~20px
            // for text-sm) per row; `snap.rows × 20px` exceeded the
            // pane's allocated height and pushed the modeline /
            // cmdline siblings off-screen when terminal was one of
            // a vsplit pair. `.flex_shrink_0()` prevents flex from
            // squishing the row below `row_px`.
            let mut row_div = div()
                .flex()
                .flex_row()
                .h(px(row_px))
                .flex_shrink_0();
            let mut run_text = String::with_capacity(snap.cols as usize);
            let mut run_style: Option<CellStyle> = None;
            let flush =
                |row_div: gpui::Div,
                 text: &mut String,
                 style: Option<CellStyle>|
                 -> gpui::Div {
                    if text.is_empty() {
                        return row_div;
                    }
                    let style = style.unwrap_or(CellStyle {
                        fg: default_fg,
                        bg: default_bg,
                        attrs: CellAttrs::default(),
                        cursor: false,
                        highlight: false,
                    });
                    // Build the styled run div. Cursor cells get
                    // shape-specific treatment (block vs beam);
                    // every other run honours fg/bg/attrs. Match
                    // highlights paint a fg/bg swap (same as the
                    // block-cursor look) — vim's match colour
                    // would be cleaner once the host theme grows
                    // a `match_background` slot.
                    let mut span = div();
                    let attrs = style.attrs;
                    if style.cursor && insert_active {
                        // Beam cursor: vertical bar on the left edge
                        // of the cell.
                        span = span.border_l_2().border_color(rgb(style.fg));
                    } else if style.cursor {
                        // Block cursor: swap fg/bg.
                        span = span.bg(rgb(style.fg)).text_color(rgb(style.bg));
                    } else if style.highlight {
                        // T3.b.3: search match — invert fg/bg.
                        span = span.bg(rgb(style.fg)).text_color(rgb(style.bg));
                    } else {
                        span = span.text_color(rgb(style.fg));
                        if style.bg != default_bg {
                            span = span.bg(rgb(style.bg));
                        }
                    }
                    // Attribute-style approximations. GPUI's
                    // div doesn't expose every text decoration
                    // yet; map what's there.
                    if attrs.underline {
                        span = span.border_b_1().border_color(rgb(style.fg));
                    }
                    let final_div = span.child(SharedString::from(std::mem::take(text)));
                    row_div.child(final_div)
                };
            for c in 0..snap.cols {
                let cell = snap.cell_at(r, c);
                let is_cursor = cursor_visible && r == cursor_row && c == cursor_col;
                let mut fg_color = term_to_rgb(cell.fg, true);
                let mut bg_color = term_to_rgb(cell.bg, false);
                if cell.attrs.reverse {
                    std::mem::swap(&mut fg_color, &mut bg_color);
                }
                let highlight_match = match_overlay
                    .map(|(m_row, c_start, c_end)| r == m_row && c >= c_start && c < c_end)
                    .unwrap_or(false);
                // T3.b.3 hlsearch: any of the all_matches whose
                // grid line maps to this visible row. Softer
                // highlight than `current_match` — we use a
                // less aggressive bg tint (currently same as
                // match for v1; theme-slot polish later).
                let highlight_hlsearch = !all_matches.is_empty() && {
                    let off = snap.scroll_offset as i32;
                    let cell_line = r as i32 - off;
                    all_matches.iter().any(|h| {
                        if h.line != cell_line {
                            return false;
                        }
                        let c_start = h.column;
                        let c_end = h
                            .column
                            .saturating_add(h.len.min(u16::MAX as u32) as u16);
                        c >= c_start && c < c_end
                    })
                };
                let highlight_visual = visual_state
                    .map(|v| {
                        use lattice_terminal::VisualKind as Vk;
                        let off = snap.scroll_offset as i32;
                        let cell_line = r as i32 - off;
                        match v.kind {
                            Vk::Line => {
                                let (lo, hi) = v.line_range();
                                cell_line >= lo && cell_line <= hi
                            }
                            Vk::Block => {
                                let (lo, hi) = v.line_range();
                                let (lo_c, hi_c) = v.block_col_range();
                                cell_line >= lo
                                    && cell_line <= hi
                                    && c >= lo_c
                                    && c <= hi_c
                            }
                            Vk::Char => {
                                let ((sl, sc), (el, ec)) = v.char_endpoints();
                                if sl == el {
                                    cell_line == sl && c >= sc && c <= ec
                                } else if cell_line == sl {
                                    c >= sc
                                } else if cell_line == el {
                                    c <= ec
                                } else {
                                    cell_line > sl && cell_line < el
                                }
                            }
                        }
                    })
                    .unwrap_or(false);
                let highlight = highlight_match || highlight_visual || highlight_hlsearch;
                let style = CellStyle {
                    fg: fg_color,
                    bg: bg_color,
                    attrs: cell.attrs,
                    cursor: is_cursor,
                    highlight,
                };
                if Some(style) == run_style {
                    run_text.push(cell.ch);
                } else {
                    row_div = flush(row_div, &mut run_text, run_style);
                    run_text.push(cell.ch);
                    run_style = Some(style);
                }
            }
            row_div = flush(row_div, &mut run_text, run_style);
            rows.push(row_div);
        }
        let mut col = div()
            .flex()
            .flex_col()
            .flex_grow()
            .overflow_hidden()
            .bg(rgb(theme.background))
            .text_color(rgb(theme.foreground))
            // Inherit the GPUI theme's font_family (the root
            // sets it too). Hardcoding "monospace" was wrong:
            // GPUI resolves font_family against the app's
            // registered font set; "monospace" is not a
            // CSS-style generic — it's an exact family lookup.
            .font_family(theme.font_family.clone());
        for row in rows {
            col = col.child(row);
        }
        (col.into_any_element(), status_text)
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
        #[cfg(feature = "profile-frames")]
        let frame_start = std::time::Instant::now();
        // Read `picker.display` early so the viewport-height
        // recompute below can subtract the picker strip's rows
        // from the buffer area when the picker is open in
        // minibuffer mode. Re-derived later in the same render
        // body for the overlay-vs-strip branch.
        let picker_use_minibuffer = picker_display_is_minibuffer(&self.app);
        // 5.8.T: per-frame viewport-height recompute from the
        // window's current pixel bounds. The row height MUST
        // match what `EditorElement` actually paints with
        // (`font_size * 1.3`) — using a smaller estimate
        // overcounts visible rows, making `viewport_height` too
        // large, which lets the cursor scroll past the visible
        // area before the host's clamp triggers (Phase 5.8.AF.6
        // QoL fix: cursor disappearing off the bottom edge).
        //
        // `text_sm` in GPUI is 0.875 rem and EditorElement uses
        // a 1.3 line-height multiplier; deriving from
        // `window.rem_size()` keeps the estimate in sync if the
        // user later changes the rem base.
        //
        // `chrome_rows` shrinks the buffer area by however many
        // non-buffer rows the column carries: the status row at
        // the bottom is always 1; the picker minibuffer strip
        // (prompt + candidate band) adds more rows when the
        // user has `picker.display = "minibuffer"` and the
        // picker is open.
        let viewport_px = window.viewport_size();
        let rem = f32::from(window.rem_size());
        let font_size_px = rem * 0.875; // text_sm()
        let estimated_row_px = font_size_px * 1.3; // matches EditorElement::line_height
        let total_rows = (f32::from(viewport_px.height) / estimated_row_px).floor() as i32;
        // 2026-05-27 popup geometry. Locked to a window-relative
        // size so the popup container never grows with content. Both
        // dimensions and the integer row count derived from the
        // inner body area are used in three places this frame:
        //   1. The viewport-height override below (popup-focused
        //      motion clamps against `popup_inner_rows`).
        //   2. `.take(MAX_POPUP_LINES)` in the popup overlay paint
        //      (caps how many body rows the flex_col emits).
        //   3. The popup container's `.min_w()/.max_w()` +
        //      `.min_h()/.max_h()` lock so width never jumps when
        //      a long line scrolls into view.
        let (popup_w_px, popup_h_px) = popup_outer_dims_px(
            f32::from(viewport_px.width),
            f32::from(viewport_px.height),
        );
        let popup_inner_rows =
            popup_inner_height_rows(popup_h_px, rem, estimated_row_px);
        // 2026-05-27: lock the body div's height too. With only the
        // outer popup container size locked, the body's flex-grown
        // content could (under-estimated chrome) render more rows
        // than visually fit, and the popup's overflow_hidden would
        // clip the bottom — the cursor's last visible row would be
        // physically painted but invisible. Setting min_h == max_h
        // on the body forces flex to size it exactly to
        // `popup_inner_rows × row_px` so the row count is the
        // single source of truth for both painting and cursor
        // clamping.
        let popup_body_h_px =
            popup_body_h_px(popup_h_px, rem, estimated_row_px);
        // Issue #17 (2026-05-22): the previous calc subtracted
        // exactly 1 row for the modeline/cmdline bottom strip and
        // ignored every other piece of non-buffer chrome — `.p_3()`
        // on the pane content (1.5rem top+bottom), `.py_1()` on
        // the per-pane status row (~0.5rem), the status text line
        // itself (~1 row), and `.py_1()` on the global bottom row
        // (~0.5rem). Net: ~4 rows of chrome were billed as ~1.
        // The buffer area over-claimed ~3 rows, so cursor jumps
        // past G/}/etc. parked the cursor on rows actually painted
        // under the modeline / pane status.
        //
        // Compute chrome in pixels (the honest unit) and convert
        // to a row-equivalent via ceil() so we round up to a
        // safe-but-snug viewport. Per-pane chrome scales with
        // pane count via `flex_grow()` — each split pane carries
        // its own .p_3 + status — but the global bottom row is
        // single. For multi-pane, each pane's own chrome shrinks
        // its rendered buffer area; this calculation reserves
        // chrome for the active pane's view height.
        let pane_padding_v_px = rem * 0.75 * 2.0; // .p_3() top + bottom = 1.5rem
        let pane_padding_h_px = rem * 0.75 * 2.0; // .p_3() left + right = 1.5rem
        let pane_status_padding_px = rem * 0.25 * 2.0; // .py_1() = 0.5rem
        let pane_status_row_px = estimated_row_px; // status text line
        let global_bottom_padding_px = rem * 0.25 * 2.0; // .py_1() = 0.5rem
        let global_bottom_row_px = estimated_row_px; // modeline / cmdline content
        let per_leaf_v_chrome_px =
            pane_padding_v_px + pane_status_padding_px + pane_status_row_px;
        let per_leaf_h_chrome_px = pane_padding_h_px;
        let global_chrome_v_px = global_bottom_padding_px + global_bottom_row_px;
        // Slice 3c.final.B (group 3): picker read via published
        // substate. Bind the Arc so the `as_deref()` borrow lives
        // for the closure.
        let picker_substate = self.app.render_state.load().picker.clone();
        // Slice 3c.gpui-cmdline-completion: cmdline-completion strip
        // shares the same screen area as the picker minibuffer and
        // honors the same `picker.display` setting. The two are
        // mutually exclusive (picker doesn't activate during `:`
        // typing). The strip has NO separate prompt row — the
        // cmdline itself (bottom row) is the prompt.
        let completion_substate = self.app.render_state.load().completion.clone();
        let picker_strip_rows: i32 = if picker_use_minibuffer {
            picker_substate
                .state
                .as_deref()
                .map(|p| 1 + p.candidates.len().min(10) as i32) // 1 prompt + up to 10 cands
                .unwrap_or(0)
        } else {
            0
        };
        let cmdline_completion_strip_rows: i32 = if picker_use_minibuffer {
            completion_substate
                .state
                .as_deref()
                .filter(|s| !s.candidates.is_empty())
                .map(|s| s.candidates.len().min(10) as i32) // candidates only; cmdline is the prompt
                .unwrap_or(0)
        } else {
            0
        };
        // Issue #25 (2026-05-22): per-pane viewport_height +
        // viewport_width via `collect_pane_geometries`. Replaces
        // the prior single-global `set_viewport_height` call.
        // Each leaf gets its own geometry; the host mirrors the
        // active leaf's height into `Editor::viewport_height`
        // for cursor-clamp + highlights worker.
        //
        // Available height for the pane tree subtracts BOTH the
        // global bottom chrome (modeline + py_1) AND the picker /
        // cmdline-completion strips (which sit above the
        // modeline when minibuffer-mode picker is open).
        // Measure the actual cell advance by shaping a reference
        // character; GPUI's LineLayoutCache makes it O(1) after the
        // first frame. Used both for pane column geometry and
        // cursor-anchored popup placement so the two are consistent.
        // Clone font family before the block so the immutable borrow of
        // self.app drops before the mutable borrows below.
        let font_family_for_advance = self.app.theme.font_family.clone();
        let glyph_advance_px = {
            let ref_font = font(font_family_for_advance);
            let ref_run = TextRun {
                len: 1,
                font: ref_font,
                color: gpui::Rgba::default().into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            f32::from(
                window
                    .text_system()
                    .shape_line(SharedString::from("M"), px(font_size_px), &[ref_run], None)
                    .width,
            )
        };
        let strip_rows_px = (picker_strip_rows + cmdline_completion_strip_rows) as f32
            * estimated_row_px;
        // Issue #29 (2026-05-22): tabline claims one row at the
        // top when visible — subtract from available so per-pane
        // geometries see the correct buffer height.
        let tabline_visible = self.app.render_state.load().tabs.visible;
        let tabline_h_px = if tabline_visible {
            estimated_row_px
        } else {
            0.0
        };
        let avail_h_px = (f32::from(viewport_px.height)
            - global_chrome_v_px
            - strip_rows_px
            - tabline_h_px)
            .max(0.0);
        let avail_w_px = f32::from(viewport_px.width);
        let pane_tree_root = self.app.render_state.load().panes.tree.root().clone();
        let mut pane_geometries: Vec<(usize, u32, u32)> = Vec::new();
        collect_pane_geometries(
            &pane_tree_root,
            avail_w_px,
            avail_h_px,
            per_leaf_v_chrome_px,
            per_leaf_h_chrome_px,
            estimated_row_px,
            glyph_advance_px,
            &mut pane_geometries,
        );
        // Issue #27 (2026-05-22): fire `set_pane_viewport` per
        // leaf ONLY when the geometry actually changes vs. what's
        // already published. The unconditional per-frame fire
        // (the prior shape) sent N actor RPCs per frame, each
        // calling `publish_render_state`. At 60fps with 1 pane
        // that's 60 publishes/sec for no reason; with 4 panes,
        // 240/sec. The flood blocked the GPUI main thread —
        // mouse click no longer focused the window, drag /
        // maximize stopped working, alt+tab was the only way
        // in. Diff-then-send fixes it: in steady state (no
        // resize) the loop fires 0 commands.
        let current_rs = self.app.render_state.load();
        let current_leaves = current_rs.panes.tree.leaves();
        for (idx, rows, cols) in &pane_geometries {
            let needs_update = current_leaves
                .get(*idx)
                .map(|l| l.viewport_height != *rows || l.viewport_width != *cols)
                .unwrap_or(true);
            if needs_update {
                self.app.set_pane_viewport(*idx, *rows, *cols);
            }
        }
        // total_rows + the old chrome_rows / new_viewport
        // arithmetic retires; `set_pane_viewport` carries the
        // per-pane truth.
        let _ = total_rows;
        #[cfg(feature = "profile-frames")]
        let after_viewport = std::time::Instant::now();
        // 2026-05-27: per-frame viewport override matching what the
        // motion engine should clamp against.
        //
        // When a help/hover popup is focused (State B), motion writes
        // against `Editor::cursor` and scroll clamp reads
        // `Editor::viewport_height`. The pane-viewport loop above
        // sets that height to the active pane's row count, but the
        // popup overlay only paints `POPUP_INNER_HEIGHT_ROWS` body
        // rows — anything past that is invisible, so `j` would walk
        // the cursor off the popup without bumping scroll. Override
        // to the popup's inner height while focused so
        // `ensure_cursor_in_viewport` (next call) scrolls correctly.
        //
        // On the way back (popup dismissed): the diff-then-send loop
        // skips `set_pane_viewport` when the pane's leaf rows are
        // unchanged, so `editor.viewport_height` would stay stuck at
        // the popup's inner height. Restore from the active leaf's
        // published row count here too. Mirrors TUI's
        // `App::active_pane_content_height` -> `set_viewport_height`
        // per-frame update; diff-then-send keeps churn down.
        let rs_for_popup = self.app.render_state.load();
        let popup_focused = matches!(
            rs_for_popup.active_document.buffer_kind,
            lattice_core::BufferKind::Help
        );
        let target_height = if popup_focused {
            popup_inner_rows
        } else {
            rs_for_popup.panes.tree.active().viewport_height.max(1)
        };
        if rs_for_popup.active_document.viewport_height != target_height {
            drop(rs_for_popup);
            self.app.set_viewport_height(target_height);
        }
        // 5.8.O: keep the cursor inside the viewport before any
        // paint reads `editor.scroll`. Auto-scrolls if the cursor
        // moved past the visible window since the last frame.
        // `set_viewport_height` above already ran one round of
        // `ensure_cursor_visible`, but this also covers the case
        // where the viewport size didn't change but the cursor
        // moved past the existing window.
        //
        // Perf plan A.3 cursor_snap gate: skip the dispatch when
        // the inputs (cursor, scroll, viewport_height, active
        // buffer kind) haven't changed since the post-dispatch
        // state we last cached. The stored key is the POST-
        // dispatch value so the cache settles in one frame
        // after a snap mutates `scroll`. First frame always runs
        // (cache starts at `None`).
        let pre_ad = self.app.render_state.load().active_document.clone();
        let cursor_key = (
            pre_ad.cursor,
            pre_ad.scroll,
            pre_ad.viewport_height,
            pre_ad.buffer_kind,
        );
        if self.ensure_gate.cursor_snap_key != Some(cursor_key) {
            self.app.ensure_cursor_in_viewport();
            let post_ad = self.app.render_state.load().active_document.clone();
            self.ensure_gate.cursor_snap_key = Some((
                post_ad.cursor,
                post_ad.scroll,
                post_ad.viewport_height,
                post_ad.buffer_kind,
            ));
        }
        // 2026-05-27 viewport-invariant probe. Opt-in via
        //   RUST_LOG=lattice_gpui::viewport=debug
        // Fires every frame, logging the chrome math + the
        // viewport_height vs. cursor-state values needed to
        // triangulate "cursor goes past last visible row" reports.
        // Compares the row count motion clamps against
        // (`viewport_height`) with the painted pixel area
        // (`leaf_h_px = leaf rows * estimated_row_px`) and the
        // global geometry that fed `collect_pane_geometries`.
        if tracing::enabled!(target: "lattice_gpui::viewport", tracing::Level::DEBUG) {
            let rs_probe = self.app.render_state.load();
            let active_leaf = rs_probe.panes.tree.active();
            let cursor = rs_probe.active_document.cursor;
            let scroll = rs_probe.active_document.scroll;
            let vh = rs_probe.active_document.viewport_height;
            let bot_visible = scroll.saturating_add(vh.saturating_sub(1));
            let cursor_past_bot = cursor.line > bot_visible;
            let leaf_h_px = active_leaf.viewport_height as f32 * estimated_row_px;
            tracing::debug!(
                target: "lattice_gpui::viewport",
                viewport_h_px = f32::from(viewport_px.height),
                avail_h_px,
                global_chrome_v_px,
                tabline_h_px,
                strip_rows_px,
                per_leaf_v_chrome_px,
                pane_padding_v_px,
                pane_status_row_px,
                pane_status_padding_px,
                estimated_row_px,
                rem,
                leaf_rows = active_leaf.viewport_height,
                leaf_h_px,
                ad_viewport_height = vh,
                cursor_line = cursor.line,
                scroll,
                bot_visible,
                cursor_past_bot,
                active_buffer_kind = ?rs_probe.active_document.buffer_kind,
                "viewport-invariant probe"
            );
        }
        #[cfg(feature = "profile-frames")]
        let after_ensure = std::time::Instant::now();
        // Phase 5.8.AF.5 / Slice X2.5: the per-frame
        // `self.app.refresh_highlights()` call has been removed.
        // Active-pane highlights are now produced by the
        // background highlights worker
        // (`lattice_host::highlights_worker`) which subscribes to
        // `Editor::highlight_wake` and publishes results into
        // `render_state.syntax.visible_spans`. `paint_pane` reads
        // those spans through `rs_guard.syntax.visible_spans.load()`.
        // Pre-X2 cost: ~178µs at 80 lines per frame; post-X2: zero
        // UI-thread parse cost. Goal #1 violation B1 closed for the
        // GPUI peer.
        // 5.8.R: rebuild the per-pane cache for inactive Document
        // panes whose buffer differs from the active pane's. The
        // host method handles the same-doc short-circuit + reparse
        // gating; this peer just makes the call so paint_pane can
        // read `editor.pane_highlights[idx]` for the inactive case.
        // Slice 3c.final.C: pane-highlight refresh via dispatch.
        //
        // Perf plan A.3 inactive_pane_refresh gate: skip when the
        // pane-tree identity, active pane index, and active doc id
        // are all unchanged. `Arc::as_ptr` is a cheap identity probe
        // — `publish_render_state` rebuilds the pane-tree Arc on any
        // pane-state change (split/close/scroll/buffer-id swap), so
        // ptr equality is a sound "nothing relevant changed" gate.
        // Trade-off documented on `EnsureGateCache::pane_refresh_key`.
        let rs_for_pane = self.app.render_state.load();
        let pane_key = (
            std::sync::Arc::as_ptr(&rs_for_pane.panes.tree) as usize,
            rs_for_pane.panes.tree.active_index(),
            rs_for_pane.active_document.document_buffer_id,
        );
        if self.ensure_gate.pane_refresh_key != Some(pane_key) {
            self.app
                .dispatch_action(lattice_host::action::Action::RefreshPaneHighlights);
            self.ensure_gate.pane_refresh_key = Some(pane_key);
        }
        #[cfg(feature = "profile-frames")]
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
        #[cfg(feature = "profile-frames")]
        let after_tick = std::time::Instant::now();
        // Phase 5.8.AA.p/r/t: every per-tick drain (hover,
        // definitions, code-actions, live-picker, ...) is now
        // folded into `run_tick_pending` above; no per-paint
        // catch-up calls remain.
        // 3c.atomic.H: modeline label read through the published
        // render-state. Paint-time read; the apply loop above
        // has already published any modal change.
        let ad = self.app.ad();
        let modal = ad.modal;

        // 2026-05-25: terminal panes surface `TERMINAL-INSERT` /
        // `TERMINAL-VISUAL` / `TERMINAL` on the bottom row in
        // place of the underlying modal (which stays `Normal`
        // while terminal-insert-mode owns input). The running
        // program basename is rendered as the buffer *name*
        // by `pane_status_label` (registry lookup); we don't
        // repeat it here.
        let modal_label: &str = if matches!(
            ad.buffer_kind,
            lattice_core::BufferKind::Terminal,
        ) {
            if ad.terminal_insert_active {
                "TERMINAL-INSERT"
            } else if ad.terminal_visual_active {
                "TERMINAL-VISUAL"
            } else {
                "TERMINAL"
            }
        } else {
            match modal {
                ModalState::Normal => "NORMAL",
                ModalState::Insert => "INSERT",
                ModalState::Visual(_) => "VISUAL",
                ModalState::OperatorPending => "PENDING",
                ModalState::Command => "COMMAND",
                ModalState::Search(_) => "SEARCH",
                ModalState::Replace => "REPLACE",
            }
        };
        drop(ad);
        // 5.8.C / 5.8.H: bottom global row. In Command/Search
        // modes it shows the in-progress `:cmd` / `/pattern`
        // minibuffer; otherwise it shows the global modal label.
        // Per-pane path + cursor coords now live inside each
        // pane's own status line (built in `paint_pane`).
        // Slice 3c.final.B.7: cmdline + search-line via published
        // `modeline()` sub-state — wait-free Arc clones.
        let modeline = self.app.modeline();
        let bottom_row: String = match modal {
            ModalState::Command => format!(":{}", modeline.cmdline_text),
            ModalState::Search(dir) => {
                let prefix = match dir {
                    lattice_grammar::SearchDirection::Forward => '/',
                    lattice_grammar::SearchDirection::Backward => '?',
                };
                let pattern = modeline.search_pattern.as_deref().unwrap_or("");
                format!("{prefix}{pattern}")
            }
            _ => format!("  {modal_label}"),
        };
        let bottom_is_minibuffer = matches!(modal, ModalState::Command | ModalState::Search(_));

        let theme = self.app.theme.clone();
        // 5.8.H: render the pane tree. `paint_pane_tree` walks
        // `rs.panes.tree.root()` recursively; each leaf paints
        // via `paint_pane` with active/inactive style. The active
        // leaf gets the refreshed `visible_highlights` cache + a
        // visible cursor; inactive leaves show plain text + no
        // cursor (their own stashed `PaneState::cursor` is read
        // for the per-pane status coords but no visible marker is
        // painted).
        //
        // Slice 3c.final.B (group 1): pane tree read through
        // the published `RenderState` instead of `editor.pane_tree`
        // directly. `paint_pane` loads its own `rs_guard` for
        // per-pane access; this load drives the recursion entry
        // point and shares the same Arc across the render body.
        // Slice 3c.final.E.5j: render-state load via App's own Arc
        // (cloned from `editor.render_state` at construction time).
        let render_state = self.app.render_state.load_full();
        let active_idx = render_state.panes.tree.active_index();
        let document_area = self
            .paint_pane_tree(
                render_state.panes.tree.root(),
                &theme,
                active_idx,
                estimated_row_px,
            )
            .flex_grow();
        #[cfg(feature = "profile-frames")]
        let after_paint = std::time::Instant::now();

        // Slice 3c.final.E.5j: insert-completion via the published
        // `completion().insert` sub-state (Arc-bump clone).
        let insert_completion = self.app.render_state.load().completion.insert.clone();
        let completion_overlay: Option<gpui::Div> = insert_completion
            .as_deref()
            .filter(|ic| !ic.rendered.is_empty())
            .map(|ic| {
                let max_visible = 10usize;
                let total = ic.rendered.len();
                let window_start = ic
                    .selected
                    .saturating_sub(max_visible / 2)
                    .min(total.saturating_sub(max_visible.min(total)));
                let window_end = (window_start + max_visible).min(total);
                // 2026-05-27: insert-completion now routes through
                // the shared `paint_candidate_row` so the kind
                // glyph + right-aligned annotation column behaviour
                // matches the picker and cmdline-completion paths.
                // Previously had its own inline row builder that
                // ignored `cand.annotations` entirely — column data
                // (LSP detail, doc snippet, etc.) wasn't shown.
                // 2026-05-27: column-align annotations. Compute
                // widest display across the visible candidates so
                // every row's annotation lands at the same x.
                let display_col_chars = ic.rendered[window_start..window_end]
                    .iter()
                    .map(|c| c.raw.display.chars().count())
                    .max()
                    .unwrap_or(0);
                let visible: Vec<gpui::Div> = ic.rendered[window_start..window_end]
                    .iter()
                    .enumerate()
                    .map(|(i, cand)| {
                        let abs_idx = window_start + i;
                        paint_candidate_row(
                            cand,
                            abs_idx == ic.selected,
                            &theme,
                            false,
                            display_col_chars,
                        )
                    })
                    .collect();
                // 2026-05-27: filter-chord footer mirrors the
                // TUI peer. Width budget approximated from the
                // popup max_w (360px ≈ 45 cells at 8px/char).
                // Adaption: full form → compact `[b]uf` → prune.
                // Also surface the active filter when set.
                let approx_cols: u16 = 45;
                let footer_text = if let Some(active) = ic.source_filter.as_deref() {
                    let label = gpui_source_display_label(active);
                    let raw = format!(" source: {label} │ <C-Space> all ");
                    if raw.chars().count() > approx_cols as usize {
                        raw.chars().take(approx_cols as usize).collect::<String>()
                    } else {
                        raw
                    }
                } else {
                    let sources_present: std::collections::BTreeSet<&str> = ic
                        .raw
                        .iter()
                        .filter_map(|r| r.source.as_ref().map(|s| s.as_str()))
                        .collect();
                    let entries = gpui_filter_chord_entries(&sources_present);
                    gpui_render_filter_chord_footer(&entries, approx_cols)
                };
                let nav_hint = format!(
                    " {} of {} │ <Tab>/<CR> accept │ <Esc> cancel ",
                    if total == 0 { 0 } else { ic.selected + 1 },
                    total,
                );
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
                            .child(nav_hint),
                    )
                    .child(
                        div()
                            .text_color(rgb(theme.popup_border))
                            .child(footer_text),
                    )
            });

        // `picker.display` selects the picker UI shape. `"popup"`
        // floats a centred overlay over the buffer area; the
        // default `"minibuffer"` mirrors the TUI vertico layout:
        // the candidate band sits at the very bottom with the
        // prompt row directly above it, both *below* the status
        // row (the GPUI equivalent of the TUI mode-line / cmdline
        // pair). The two modes are mutually exclusive -- only the
        // one matching the config gets built and inserted into
        // the root. `picker_use_minibuffer` was already computed
        // at the top of `render` so the viewport-height
        // recompute could subtract the strip's rows.
        // Slice 3c.final.B (group 3): picker via published
        // substate; reuse `picker_substate` bound at top of render.
        let picker_overlay: Option<gpui::Div> = (!picker_use_minibuffer)
            .then(|| picker_substate.state.as_deref())
            .flatten()
            .map(|picker| {
            let max_visible = 30usize;
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
            // Slice 3c.unify.gpui-annotation-render: rows now flow
            // through the shared `paint_candidate_row` helper that
            // also paints the right-aligned annotations column.
            // `padded: false` — the overlay container below
            // applies its own `.p_2()`.
            let display_col_chars = picker.candidates[window_start..window_end]
                .iter()
                .map(|c| c.raw.display.chars().count())
                .max()
                .unwrap_or(0);
            let visible_candidates: Vec<gpui::Div> = picker.candidates[window_start..window_end]
                .iter()
                .enumerate()
                .map(|(i, cand)| {
                    let abs_idx = window_start + i;
                    paint_candidate_row(
                        cand,
                        abs_idx == picker.selected,
                        &theme,
                        false,
                        display_col_chars,
                    )
                })
                .collect();
            // Width sizing: file pickers carry long paths (often
            // 60+ chars) and `:picker grep` adds a path:line
            // prefix column. The previous 720px cap clipped both.
            // Targeting ~80% of typical window width with a
            // generous min_w keeps annotation columns (kind /
            // detail) visible without dominating the screen on
            // ultrawides.
            div()
                .flex()
                .flex_col()
                .min_w(px(720.0))
                .w(px(1200.0))
                .max_w(px(1400.0))
                .max_h(px(640.0))
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

        // Vertico-style minibuffer strip (matches TUI when
        // `picker.display = "minibuffer"`). Two rows below the
        // status line: prompt on top, candidate band below.
        // Selected candidate sits at the top of the band so the
        // eye path matches the TUI's prompt → selection
        // adjacency. Built as a normal flex_col child (not an
        // absolute overlay) so it claims layout space rather
        // than floating over the buffer.
        // Slice 3c.final.B (group 3): picker via published substate.
        let picker_minibuffer: Option<gpui::Div> = picker_use_minibuffer
            .then(|| picker_substate.state.as_deref())
            .flatten()
            .map(|picker| {
                const MAX_VISIBLE: usize = 10;
                let total = picker.candidates.len();
                let visible_count = total.min(MAX_VISIBLE).max(1);
                let scroll = if picker.selected < visible_count {
                    0
                } else {
                    picker.selected + 1 - visible_count
                };
                let window_end = (scroll + visible_count).min(total);
                // Slice 3c.unify.gpui-annotation-render: minibuffer
                // strip rows go through the shared
                // `paint_candidate_row` helper. `padded: true` —
                // strip has no surrounding container with its own
                // horizontal padding.
                let cand_rows: Vec<gpui::Div> = if total == 0 {
                    vec![div()
                        .px_2()
                        .text_color(rgb(theme.popup_border))
                        .child("  (no matches)".to_string())]
                } else {
                    let display_col_chars = picker.candidates[scroll..window_end]
                        .iter()
                        .map(|c| c.raw.display.chars().count())
                        .max()
                        .unwrap_or(0);
                    picker.candidates[scroll..window_end]
                        .iter()
                        .enumerate()
                        .map(|(i, cand)| {
                            let abs_idx = scroll + i;
                            paint_candidate_row(
                                cand,
                                abs_idx == picker.selected,
                                &theme,
                                true,
                                display_col_chars,
                            )
                        })
                        .collect()
                };

                let count = format!(
                    "  ({} / {})",
                    if total == 0 { 0 } else { picker.selected + 1 },
                    total,
                );
                let prompt_row = div()
                    .px_2()
                    .flex()
                    .flex_row()
                    .bg(rgb(theme.background))
                    .text_color(rgb(theme.foreground))
                    .child(
                        div()
                            .text_color(rgb(theme.cursor_background))
                            .child(format!("{}> ", picker.title)),
                    )
                    .child(div().child(picker.query.clone()))
                    .child(
                        div()
                            .border_l_2()
                            .border_color(rgb(theme.cursor_background))
                            .child(" "),
                    )
                    .child(
                        div()
                            .text_color(rgb(theme.popup_border))
                            .child(count),
                    );

                div()
                    .flex()
                    .flex_col()
                    .child(prompt_row)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .bg(rgb(theme.background))
                            .children(cand_rows),
                    )
            });

        // Slice 3c.gpui-cmdline-completion: cmdline-completion
        // minibuffer strip. Mirrors the picker minibuffer's shape
        // (vertico-style candidate band) but omits the title /
        // prompt row — the cmdline (`:cmd`) is the prompt. Honors
        // the same `picker.display` setting so the two completion
        // surfaces feel like one UI family.
        let cmdline_completion_minibuffer: Option<gpui::Div> = picker_use_minibuffer
            .then(|| completion_substate.state.as_deref())
            .flatten()
            .filter(|s| !s.candidates.is_empty())
            .map(|state| {
                const MAX_VISIBLE: usize = 10;
                let total = state.candidates.len();
                let visible_count = total.min(MAX_VISIBLE).max(1);
                // Picker keeps the selected row at the top of the
                // band; cmdline completion uses the same convention
                // so the eye path is identical across the two
                // surfaces.
                let scroll = if state.selected < visible_count {
                    0
                } else {
                    state.selected + 1 - visible_count
                };
                let window_end = (scroll + visible_count).min(total);
                // Slice 3c.unify.gpui-annotation-render: shared row
                // builder. `padded: true` for the minibuffer
                // strip variant.
                let display_col_chars = state.candidates[scroll..window_end]
                    .iter()
                    .map(|c| c.raw.display.chars().count())
                    .max()
                    .unwrap_or(0);
                let cand_rows: Vec<gpui::Div> = state.candidates[scroll..window_end]
                    .iter()
                    .enumerate()
                    .map(|(i, cand)| {
                        let abs_idx = scroll + i;
                        paint_candidate_row(
                            cand,
                            abs_idx == state.selected,
                            &theme,
                            true,
                            display_col_chars,
                        )
                    })
                    .collect();
                div()
                    .flex()
                    .flex_col()
                    .bg(rgb(theme.background))
                    .children(cand_rows)
            });

        // Slice 3c.gpui-cmdline-completion: cmdline-completion
        // popup overlay. Mirrors the picker overlay's centered
        // float; activates when `picker.display = "popup"`. The
        // band content is identical to the minibuffer variant.
        let cmdline_completion_overlay: Option<gpui::Div> = (!picker_use_minibuffer)
            .then(|| completion_substate.state.as_deref())
            .flatten()
            .filter(|s| !s.candidates.is_empty())
            .map(|state| {
                const MAX_VISIBLE: usize = 30;
                let total = state.candidates.len();
                let window_start = state
                    .selected
                    .saturating_sub(MAX_VISIBLE / 2)
                    .min(total.saturating_sub(MAX_VISIBLE.min(total)));
                let window_end = (window_start + MAX_VISIBLE).min(total);
                // Slice 3c.unify.gpui-annotation-render: shared row
                // builder. `padded: false` — overlay container
                // applies its own `.p_2()`.
                let display_col_chars = state.candidates[window_start..window_end]
                    .iter()
                    .map(|c| c.raw.display.chars().count())
                    .max()
                    .unwrap_or(0);
                let visible_candidates: Vec<gpui::Div> = state.candidates[window_start..window_end]
                    .iter()
                    .enumerate()
                    .map(|(i, cand)| {
                        let abs_idx = window_start + i;
                        paint_candidate_row(
                            cand,
                            abs_idx == state.selected,
                            &theme,
                            false,
                            display_col_chars,
                        )
                    })
                    .collect();
                div()
                    .flex()
                    .flex_col()
                    .min_w(px(320.0))
                    .max_w(px(720.0))
                    .max_h(px(480.0))
                    .p_2()
                    .bg(rgb(theme.popup_background))
                    .border_1()
                    .border_color(rgb(theme.popup_border))
                    .children(visible_candidates)
            });

        // Phase 5.8.AE + Slice 3c.final.B (group 3): read popup
        // state via the published substate. The Arc-wrapped
        // `HelpBuffer` + `help_highlights` slice live on
        // `render_state.popup.X`; bind locally so the borrows live
        // for the closure.
        let popup_substate = self.app.render_state.load().popup.clone();
        let popup_overlay: Option<gpui::Div> = popup_substate.help.as_deref().map(|buf| {
            let title = buf.title.clone();
            let body_text = buf.content.as_string();
            // M.3.2.c.5: highlights live in buffer-locals keyed by the
            // popup buffer id; published as `popup.help_highlights`.
            let highlights_owned: Vec<Vec<lattice_syntax::StyledSpan>> =
                popup_substate.help_highlights.to_vec();
            let line_highlights: &[Vec<lattice_syntax::StyledSpan>] =
                highlights_owned.as_slice();
            let body_lines: Vec<&str> = body_text.split('\n').collect();

            // When the popup is focused (State B), ad().scroll and
            // ad().cursor describe the popup buffer's scroll/cursor so
            // we can show the right content window and a cursor indicator.
            let ad = self.app.ad();
            let popup_focused = ad.buffer_kind == lattice_core::BufferKind::Help;
            // 2026-05-27: max visible body rows derived from the
            // per-frame popup geometry (`popup_inner_rows` computed
            // up top from window dimensions minus popup chrome). The
            // viewport-height override above uses the same value so
            // motion clamps and the painted body stay in lockstep.
            let max_popup_lines: usize = popup_inner_rows as usize;
            let popup_scroll = if popup_focused { ad.scroll as usize } else { 0 };
            let cursor_doc_line = if popup_focused {
                Some(ad.cursor.line as usize)
            } else {
                None
            };
            // Byte offset within the cursor line (for a char-wide block cursor).
            let cursor_byte = if popup_focused {
                Some(ad.cursor.byte as usize)
            } else {
                None
            };

            // 2026-05-27 popup wrap. Each source line emits ONE OR
            // MORE visible rows: when `popup.wrap` is true and the
            // source line is wider than `inner_cols` chars, split
            // it into char-count chunks. The cursor block appears
            // on the wrap segment whose char-range contains
            // `cursor_byte`; other segments of the same source
            // line render without the cursor highlight.
            let inner_cols = popup_inner_cols(popup_w_px, rem, glyph_advance_px) as usize;
            let wrap_on = popup_wrap_enabled(&self.app);
            // 2026-05-27 popup-wrap probe. One log per frame the
            // popup is open. Enable with:
            //   RUST_LOG=lattice_gpui::popup_wrap=debug
            // Helpful when "wrap doesn't fire" — compare inner_cols
            // against the longest body line. If `wrap_on=false`,
            // the user has `popup.wrap=false` or the option didn't
            // register. If `inner_cols` is larger than the longest
            // line, wrap doesn't activate (everything fits).
            let longest_line_chars = body_lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
            tracing::debug!(
                target: "lattice_gpui::popup_wrap",
                wrap_on,
                inner_cols,
                popup_w_px,
                glyph_advance_px,
                longest_line_chars,
                line_count = body_lines.len(),
                "popup-wrap probe"
            );
            let mut popup_lines: Vec<gpui::Div> = Vec::new();
            'outer: for (idx, line) in body_lines.iter().enumerate().skip(popup_scroll) {
                if popup_lines.len() >= max_popup_lines {
                    break;
                }
                let is_cursor_line = cursor_doc_line == Some(idx);
                let spans: &[lattice_syntax::StyledSpan] =
                    line_highlights.get(idx).map(Vec::as_slice).unwrap_or(&[]);
                let cb_line = if is_cursor_line {
                    cursor_byte.unwrap_or(0)
                } else {
                    usize::MAX
                };
                // Char-index list lets us slice the line at char
                // boundaries (the grapheme question is deferred —
                // help / hover text is overwhelmingly ASCII).
                let char_indices: Vec<(usize, char)> = line.char_indices().collect();
                let total_chars = char_indices.len();
                // Empty line: one blank visible row to preserve row
                // height (flex_row collapses if no children).
                if total_chars == 0 {
                    let mut cells: Vec<gpui::Div> = Vec::new();
                    if is_cursor_line && cb_line == 0 {
                        cells.push(
                            div()
                                .w(px(glyph_advance_px))
                                .flex_shrink_0()
                                .bg(rgb(theme.cursor_background))
                                .text_color(rgb(theme.cursor_foreground))
                                .child(" ".to_string()),
                        );
                    } else {
                        cells.push(
                            div()
                                .w(px(glyph_advance_px))
                                .flex_shrink_0()
                                .child(" ".to_string()),
                        );
                    }
                    popup_lines.push(
                        div()
                            .flex()
                            .flex_row()
                            .h(px(estimated_row_px))
                            .flex_shrink_0()
                            .children(cells),
                    );
                    continue;
                }
                // Determine chunk size. Without wrap, one chunk
                // spanning the whole line (cells overflow the
                // popup's `overflow_hidden` and clip at the right
                // edge — pre-wrap behaviour).
                let chunk_chars = if wrap_on && total_chars > inner_cols {
                    inner_cols.max(1)
                } else {
                    total_chars.max(1)
                };
                let mut chunk_start_char = 0;
                while chunk_start_char < total_chars {
                    if popup_lines.len() >= max_popup_lines {
                        break 'outer;
                    }
                    let chunk_end_char = (chunk_start_char + chunk_chars).min(total_chars);
                    // Byte range for this chunk's text.
                    let byte_start = char_indices[chunk_start_char].0;
                    let byte_end = if chunk_end_char < total_chars {
                        char_indices[chunk_end_char].0
                    } else {
                        line.len()
                    };
                    let cursor_in_chunk = is_cursor_line
                        && cb_line >= byte_start
                        && cb_line < byte_end;
                    let cursor_past_end = is_cursor_line
                        && chunk_end_char == total_chars
                        && cb_line >= line.len();
                    // 2026-05-27 cell-width lock. Without an
                    // explicit width per cell, the row's actual
                    // pixel width was `sum(cell content widths)`
                    // which diverged from `N × glyph_advance_px`
                    // (sub-pixel kerning, font metrics for non-"M"
                    // glyphs). For wrap the break point was
                    // `inner_cols = (popup_w - chrome) / adv`, so
                    // the sum could exceed the popup edge and the
                    // tail of each wrap row clipped past the right
                    // border. Locking `.w(px(adv)) +
                    // .flex_shrink_0()` per cell makes a row of
                    // N cells exactly N × adv wide — the same
                    // budget the wrap math uses.
                    let cell_w_px = glyph_advance_px;
                    let mut cells: Vec<gpui::Div> = char_indices
                        [chunk_start_char..chunk_end_char]
                        .iter()
                        .map(|(byte_idx, c)| {
                            let style = style_at(spans, *byte_idx);
                            let base = div()
                                .w(px(cell_w_px))
                                .flex_shrink_0()
                                .overflow_hidden();
                            if cursor_in_chunk && *byte_idx == cb_line {
                                base.bg(rgb(theme.cursor_background))
                                    .text_color(rgb(theme.cursor_foreground))
                                    .child(c.to_string())
                            } else {
                                base.text_color(rgb(syntax_color(style)))
                                    .child(c.to_string())
                            }
                        })
                        .collect();
                    if cursor_past_end {
                        cells.push(
                            div()
                                .w(px(cell_w_px))
                                .flex_shrink_0()
                                .bg(rgb(theme.cursor_background))
                                .text_color(rgb(theme.cursor_foreground))
                                .child(" ".to_string()),
                        );
                    }
                    popup_lines.push(
                        div()
                            .flex()
                            .flex_row()
                            .h(px(estimated_row_px))
                            .flex_shrink_0()
                            .children(cells),
                    );
                    chunk_start_char = chunk_end_char;
                }
            }

            let border_color = if popup_focused {
                rgb(theme.cursor_background)
            } else {
                rgb(theme.popup_border)
            };
            let header_hint = if popup_focused {
                " (j/k scroll · q/Esc dismiss)"
            } else {
                " (K to focus · Esc dismiss)"
            };
            // 2026-05-27: lock the popup to the per-frame computed
            // outer dimensions. min == max prevents the flex layout
            // from shrinking on short content or growing on long
            // lines (the prior `max_w(px(900))` with no `min_w`
            // caused the popup width to visibly jump when a long
            // line scrolled into view).
            div()
                .flex()
                .flex_col()
                .min_w(px(popup_w_px))
                .max_w(px(popup_w_px))
                .min_h(px(popup_h_px))
                .max_h(px(popup_h_px))
                .overflow_hidden()
                .p_4()
                .bg(rgb(theme.popup_background))
                .text_color(rgb(theme.foreground))
                .border_2()
                .border_color(border_color)
                .child(
                    // 2026-05-27: lock title + separator rows to
                    // exactly `estimated_row_px` so the chrome math
                    // is precise. Default text rendering uses
                    // `text-sm` line-height (~20px @ 16px rem); the
                    // editor row_px is ~18.2px. Without the lock,
                    // header height drifted, eating space the body
                    // needed.
                    div()
                        .flex()
                        .flex_col()
                        .text_color(rgb(theme.popup_border))
                        .pb_2()
                        .child(
                            div()
                                .h(px(estimated_row_px))
                                .flex_shrink_0()
                                .child(format!(" {title}{header_hint} ")),
                        )
                        .child(
                            div()
                                .h(px(estimated_row_px))
                                .flex_shrink_0()
                                .child("───────────────".to_string()),
                        ),
                )
                .child(
                    // 2026-05-27: body height locked to
                    // `popup_inner_rows × row_px` so flex can't
                    // oversize the body. Combined with
                    // `.take(max_popup_lines)` this guarantees the
                    // cursor's last reachable row is always painted
                    // inside the popup's visible bottom edge.
                    div()
                        .flex()
                        .flex_col()
                        .min_h(px(popup_body_h_px))
                        .max_h(px(popup_body_h_px))
                        .overflow_hidden()
                        .children(popup_lines),
                )
        });

        // Issue #29 (2026-05-22): tabline strip. Visibility is
        // resolved by the publisher's `build_tabs_render_state`
        // based on `tabline.show` × tabs.len(). When visible,
        // the strip sits ABOVE document_area; the row's vertical
        // cost is accounted for in `collect_pane_geometries`
        // via `tabline_h_px`.
        let tabs_rs = self.app.render_state.load().tabs.clone();
        let tabline_element = if tabs_rs.visible {
            let mut row = div()
                .flex()
                .flex_row()
                .bg(rgb(theme.status_background))
                .text_color(rgb(theme.status_foreground));
            for (idx, item) in tabs_rs.items.iter().enumerate() {
                let label = format!(" {} {} ", idx + 1, item.label);
                // Slice 3 (2026-05-22): mouse click → switch
                // tab. The click handler dispatches
                // `Action::GoToTab(idx + 1)` so the same
                // semantic path used by `{N}gt` flows through
                // the host's `do_goto_tab`. Per-cell click
                // listener — no global tabline event-routing
                // needed.
                let target_n = (idx + 1) as u32;
                let mut cell = if idx == tabs_rs.active {
                    div()
                        .bg(rgb(theme.cursor_background))
                        .text_color(rgb(theme.cursor_foreground))
                } else {
                    div()
                };
                cell = cell.child(label).on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |this, _event, _window, cx| {
                        this.app.dispatch_action(
                            lattice_host::action::Action::GoToTab(target_n),
                        );
                        cx.notify();
                    }),
                );
                row = row.child(cell);
            }
            Some(row)
        } else {
            None
        };

        let mut root = div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(theme.background))
            .text_color(rgb(theme.foreground))
            .text_sm()
            .font_family(theme.font_family.clone());
        if let Some(tabline) = tabline_element {
            root = root.child(tabline);
        }
        root = root
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

        if let Some(strip) = picker_minibuffer {
            root = root.child(strip);
        }
        // Slice 3c.gpui-cmdline-completion: minibuffer strip + overlay.
        // Mutually exclusive with the picker (the picker doesn't
        // activate while typing in `:`).
        if let Some(strip) = cmdline_completion_minibuffer {
            root = root.child(strip);
        }

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
        if let Some(overlay) = cmdline_completion_overlay {
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
            // Issue #18 (2026-05-22): respect
            // `popup_substate.placement` — the host already publishes
            // Centered vs CursorAnchored per-popup (hover/sigHelp
            // use CursorAnchored; `:describe-*` / `:lsp-status`
            // etc. use Centered). The old code unconditionally
            // centered every popup, which broke hover (`K`) UX:
            // the docs appeared in the middle of the screen
            // instead of next to the symbol under the cursor.
            //
            // For CursorAnchored: compute pixel position from the
            // cursor's (line - scroll, byte) screen coordinates +
            // the pane's `.p_3()` padding. Uses the same
            // `glyph_advance_px` measured above so popup x aligns
            // with the character columns EditorElement paints.
            let placement = popup_substate.placement;
            use lattice_core::ui::popup::PopupPlacement;
            root = match placement {
                PopupPlacement::Centered => root.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .justify_center()
                        .items_center()
                        .child(overlay),
                ),
                PopupPlacement::CursorAnchored => {
                    // 2026-05-22 popup-anchor: use the cursor
                    // SNAPSHOT captured at popup-open time
                    // (`popup_substate.anchor`) instead of the
                    // live `ad.cursor`. Otherwise the popup
                    // follows cursor motions (regression
                    // reported in third triage round). Fall
                    // back to `ad.cursor` defensively for the
                    // pre-anchor-field state (None when boot is
                    // still mid-publish).
                    let ad = self.app.ad();
                    let anchor = popup_substate.anchor.unwrap_or(ad.cursor);
                    // Use doc_scroll_at_anchor (fixed at popup-open time)
                    // rather than ad.scroll: in State B, ad.scroll is the
                    // POPUP's scroll, causing a frame-jump on second K.
                    let cursor_screen_row =
                        anchor.line.saturating_sub(popup_substate.doc_scroll_at_anchor) as f32;
                    let cursor_byte_col = anchor.byte as f32;
                    // 2026-05-27: the popup is positioned relative
                    // to the WINDOW (root container's top-left),
                    // not relative to the document area. Account
                    // for everything between the window top and
                    // the first painted document row: the tabline
                    // (when visible) and the pane's `.p_3()` top
                    // padding. The horizontal anchor uses the
                    // pane's left padding only.
                    let pane_pad_v = rem * 0.75;
                    let pane_pad_h = rem * 0.75;
                    let top_origin_px = tabline_h_px + pane_pad_v;
                    let cursor_row_top =
                        top_origin_px + cursor_screen_row * estimated_row_px;
                    // Prefer placing the popup right below the
                    // cursor row; flip above when the locked popup
                    // height wouldn't fit below; if neither side
                    // fits cleanly, pin to whichever has more room
                    // and let the popup clip against the window
                    // (the lock means we can't shrink).
                    let viewport_h = f32::from(viewport_px.height);
                    let below_top = cursor_row_top + estimated_row_px;
                    let above_top = cursor_row_top - popup_h_px;
                    let fits_below = below_top + popup_h_px <= viewport_h;
                    let fits_above = above_top >= 0.0;
                    let top_px = if fits_below {
                        below_top
                    } else if fits_above {
                        above_top
                    } else {
                        // Neither side fits — pin to the side with
                        // more room, clamped to the viewport.
                        let room_below = viewport_h - below_top;
                        let room_above = cursor_row_top;
                        if room_below >= room_above {
                            (viewport_h - popup_h_px).max(0.0)
                        } else {
                            0.0
                        }
                    };
                    let left_px = pane_pad_h + cursor_byte_col * glyph_advance_px;
                    // Keep the popup within the window's right edge.
                    let viewport_w = f32::from(viewport_px.width);
                    let left_px = left_px.min((viewport_w - popup_w_px).max(0.0));
                    root.child(
                        div()
                            .absolute()
                            .top(px(top_px))
                            .left(px(left_px))
                            .child(overlay),
                    )
                }
            };
        }
        // Phase 5.8.AF.5 / Slice 3c.atomic.L: per-frame budget log.
        // `after_paint` was captured immediately after
        // `paint_pane_tree` returned; the remaining work (overlay
        // assembly + return) is folded into the `tail_us` bucket.
        //
        // Perf plan A.4: the timing captures + the `tracing::debug!`
        // emission are gated behind the `profile-frames` cargo
        // feature so default release builds skip both the
        // `clock_gettime` syscalls and the format machinery. Build
        // with `--features profile-frames` and
        // `RUST_LOG=lattice_gpui::perf=debug` when capturing a trace.
        #[cfg(feature = "profile-frames")]
        {
            let frame_us = frame_start.elapsed().as_micros() as u64;
            tracing::debug!(
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
        }
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
