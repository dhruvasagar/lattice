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
    FontFeatures, InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Render,
    SharedString, Styled, TextRun, Window, WindowBounds, WindowOptions, div, font, px, rgb, size,
};
use lattice_core::Document;
use lattice_core::ui::pane::{PaneNode, PaneState};
use lattice_grammar::ModalState;
use lattice_host::cursor_shape::CursorShape;
use lattice_host::per_buffer_cache::PerBufferCacheExt;

use crate::{GpuiApp, GpuiTheme};

// Phase 5.8.AF.5 / Slice X3.full.2: `CellStyle` + `run_to_cell`
// (per-cell styled-Div construction) deleted -- replaced by the
// shaping-layer logic in `crate::editor_element::build_text_runs`.
// PU.2: `syntax_color` + `style_at` deleted — the popup-overlay's
// manual per-cell Div walk they fed is gone (the popup interior now
// renders through `EditorElement` reading the synthetic POPUP matrix,
// which resolves syntax styles via the shared `display_line_to_text_runs`
// path like every pane).

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
/// PU.5d: the completion-docs side popup is a fixed-width box placed to the
/// left of GPUI's top-right candidate popup. Width matches the candidate
/// popup (`max_w(px(360.0))`); height grows to the doc body, capped.
pub(crate) const COMPLETION_DOCS_W_PX: f32 = 360.0;
pub(crate) const COMPLETION_DOCS_MAX_ROWS: u32 = 16;

pub(crate) const POPUP_MAX_W_PX: f32 = 900.0;
pub(crate) const POPUP_MAX_H_PX: f32 = 600.0;
pub(crate) const POPUP_MIN_W_PX: f32 = 480.0;
pub(crate) const POPUP_MIN_H_PX: f32 = 240.0;
pub(crate) const POPUP_W_RATIO: f32 = 0.70;
pub(crate) const POPUP_H_RATIO: f32 = 0.60;
/// Popup TITLE is rendered at this multiple of the body font size (bold).
/// The title row's locked height is `row_px * POPUP_TITLE_SCALE` (the larger
/// font's line height), which `popup_chrome_v_px` reserves so the body
/// geometry stays exact. Shared by the chrome math AND the header paint.
pub(crate) const POPUP_TITLE_SCALE: f32 = 1.2;

/// Compute the popup's outer pixel dimensions from the window's
/// viewport pixels. Window-relative with hard min/max caps so the
/// popup is readable on small windows and not absurd on large ones.
pub(crate) fn popup_outer_dims_px(viewport_w_px: f32, viewport_h_px: f32) -> (f32, f32) {
    let w = (viewport_w_px * POPUP_W_RATIO).clamp(POPUP_MIN_W_PX, POPUP_MAX_W_PX);
    let h = (viewport_h_px * POPUP_H_RATIO).clamp(POPUP_MIN_H_PX, POPUP_MAX_H_PX);
    (w, h)
}

/// Pixel cost of the popup's vertical chrome (border + .p_4 padding
/// top+bottom + the bold/larger title row + .pb_2 header gap).
/// Subtract from the popup's outer height to get the inner body area.
///
/// 2026-06: the `───` separator row was removed (the bold, larger title
/// + the `.pb_2()` gap separate the header from the body); the title row
/// is now `row_px * POPUP_TITLE_SCALE` tall (the larger font's line
/// height), locked in the paint so this stays exact.
pub(crate) fn popup_chrome_v_px(rem: f32, row_px: f32) -> f32 {
    let border_v = 2.0 * 2.0; // .border_2() top + bottom
    let p4_v = rem * 1.0 * 2.0; // .p_4() top + bottom = 2rem
    let title_row = row_px * POPUP_TITLE_SCALE; // bold/larger title row
    let pb_2_v = rem * 0.5; // header .pb_2() = 0.5rem
    border_v + p4_v + title_row + pb_2_v
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

/// Pixel height the popup body div is locked to. Uses the FULL available
/// inner height (`popup_h_px − chrome`), NOT `inner_rows × row_px`.
///
/// The body paints exactly [`popup_inner_height_rows`] rows via its
/// `EditorElement`, but that element lays each row out at its OWN
/// `line_height` (`font_size × 1.3`), which GPUI rounds to physical pixels.
/// Flooring the body to `inner_rows × estimated_row_px` left ZERO vertical
/// slack, so the accumulated rounding pushed the last row a sub-pixel past
/// the locked body and `overflow_hidden` clipped it — the last line rendered
/// "partially behind" the bottom edge once `G` parked the cursor on it.
/// Claiming the full inner height keeps the floor remainder (< one row) as
/// harmless slack below the last row, absorbing the rounding. The row COUNT
/// (scroll / matrix / cursor clamp) is still `popup_inner_height_rows`, so
/// the content fills the popup; only the body div's lock loosens.
pub(crate) fn popup_body_h_px(popup_h_px: f32, rem: f32, row_px: f32) -> f32 {
    (popup_h_px - popup_chrome_v_px(rem, row_px)).max(row_px)
}

/// The real line height GPUI resolves for plain UI text — the per-pane
/// modeline/status row and the global cmdline row — at the `text_sm` font
/// size (0.875rem). Those elements are `div().child(text)` and never call
/// `.line_height(...)`, so they get GPUI's DEFAULT `TextStyle::line_height`
/// (`phi()`, the golden ratio, ≈1.618× font_size) — NOT the `1.3×`
/// multiplier `EditorElement` uses for its own content rows (an explicit,
/// unrelated override at `editor_element.rs:618`). A prior version of this
/// file reused the `1.3×` content-row estimate for these UI rows too,
/// undercounting their real height by ~25%: the per-pane chrome budget came
/// out too small, the computed row count one too many, and the extra row
/// was silently clipped by `overflow_hidden` right where the modeline
/// starts ("last line behind the modeline," independent of the tabline).
/// Measured via GPUI's own `TextStyle::line_height_in_pixels` so this can't
/// silently drift from GPUI's actual default again.
pub(crate) fn default_ui_row_px(rem_size: gpui::Pixels) -> f32 {
    f32::from(
        gpui::TextStyle {
            font_size: gpui::rems(0.875).into(),
            ..Default::default()
        }
        .line_height_in_pixels(rem_size),
    )
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

/// Whether the global cmdline/echo bottom row should be painted this
/// frame. It is suppressed in exactly one case: a picker open in
/// minibuffer mode, where the picker carries its OWN prompt row that
/// claims the bottom slot — mirroring the TUI peer's `draw_picker_prompt`,
/// which draws into the cmdline row instead of `draw_command_or_echo`.
/// Painting both would wedge an empty cmdline row between the modeline and
/// the picker prompt (the GPUI gap this guards against). cmdline-completion
/// keeps the row (the `:cmd` line IS its prompt); picker popup-mode keeps
/// it (echo stays visible under the floating overlay). Pure, so the truth
/// table is unit-testable without a gpui render context.
fn global_bottom_row_visible(picker_use_minibuffer: bool, picker_has_state: bool) -> bool {
    !(picker_use_minibuffer && picker_has_state)
}

/// Select the global bottom-row content (vim's shared cmdline / echo
/// line) plus the echo level for colouring. While typing `:` / `/` it's
/// the in-progress minibuffer; otherwise it's the last echo message — a
/// `:set foo?` value, a command error, or the `-- INSERT --` showmode
/// (ML.5d) — restoring the echo the TUI peer shows (GPUI previously left
/// the row blank outside Command/Search). Pure, so it is unit-testable
/// without a gpui render context.
fn bottom_row_content(
    modal: lattice_grammar::ModalState,
    modeline: &lattice_host::render_state::ModelineRenderState,
    messages: &lattice_host::render_state::MessagesRenderState,
) -> (String, Option<lattice_host::action::EchoLevel>) {
    use lattice_grammar::ModalState;
    // MB.2: the expanded tier-2 band grows the one-row prompt into a
    // full-modal mini-buffer. The `:` prefix is already included in
    // the published cmdline_full_text (command_line_full_text prepends
    // it); the renderer only adds a prefix for the search line (`/` or
    // `?`). Continuation lines (typed <CR> in the band) are plain.
    if modeline.cmdline_expanded {
        // Search line: the published text needs a prefix.
        if modeline.search_direction.is_some() {
            let prefix: char = match modeline.search_direction {
                Some(lattice_grammar::SearchDirection::Forward) => '/',
                Some(lattice_grammar::SearchDirection::Backward) => '?',
                _ => '/',
            };
            let full: &str = &modeline.cmdline_full_text;
            let mut result = String::with_capacity(full.len() + full.matches('\n').count());
            for (i, l) in full.split('\n').enumerate() {
                if i > 0 { result.push('\n'); }
                if i == 0 { result.push(prefix); }
                result.push_str(l);
            }
            return (result, None);
        }
        // Command line: the `:` is already in the published text.
        return (modeline.cmdline_full_text.to_string(), None);
    }
    match modal {
        ModalState::Command => (format!(":{}", modeline.cmdline_text), None),
        ModalState::Search(dir) => {
            let prefix = match dir {
                lattice_grammar::SearchDirection::Forward => '/',
                lattice_grammar::SearchDirection::Backward => '?',
            };
            let pattern = modeline.search_pattern.as_deref().unwrap_or("");
            (format!("{prefix}{pattern}"), None)
        }
        // Any non-minibuffer mode shows the last echo message (until the
        // next command / mode change overwrites or clears it).
        _ => match messages.last.as_deref() {
            Some(msg) => (msg.text.clone(), Some(msg.level)),
            None => (String::new(), None),
        },
    }
}

/// Upper bound on rows materialised for entry-list panes (oil /
/// file-tree) when the host hasn't yet published a per-pane
/// `viewport_height` (e.g. the very first frame). Keeps paint O(viewport)
/// instead of O(directory-size) on huge listings (paramount goal #1).
const VIEWPORT_ROWS_FALLBACK: usize = 200;

/// Map a renderer-neutral [`lattice_core::ui::icons::IconColor`] to a
/// packed `0xRRGGBB` for GPUI's `rgb()`. Mirrors the TUI peer's
/// `to_ratatui_color`; the named colours resolve to the default
/// (Catppuccin Mocha) palette so file-type icons read the same hue across
/// renderers. `Reset` falls back to the document foreground. Pure, so the
/// mapping is unit-testable without a gpui render context.
fn icon_color_to_rgb(c: lattice_core::ui::icons::IconColor, default_fg: u32) -> u32 {
    use lattice_core::ui::icons::IconColor;
    match c {
        IconColor::Rgb(rgb) => rgb,
        IconColor::Reset => default_fg,
        IconColor::Yellow => 0x00f9_e2af,
        IconColor::DarkGray => 0x006c_7086,
        IconColor::Blue => 0x0089_b4fa,
        IconColor::Cyan => 0x0094_e2d5,
        IconColor::Green => 0x00a6_e3a1,
        IconColor::White => 0x00cd_d6f4,
    }
}

/// Foreground colour for a file-entry row (oil / file-tree), matching the
/// TUI peer's `icon_for_entry`: **directories** and **dotfiles** take their
/// themeable `file_tree.dir` / `file_tree.hidden` registry roles — so the
/// built-in themes style them and both renderers resolve the SAME colour —
/// while every other file keeps the fixed devicon brand hue from
/// `entry_visual`. The roles already live in the shared `lattice-theme`
/// registry (`register_builtins`); this just consumes them, exactly as the
/// TUI side does via `ids.file_tree_*`.
fn entry_fg(
    rs_guard: &lattice_host::render_state::RenderState,
    is_dir: bool,
    is_hidden: bool,
    icol: lattice_core::ui::icons::IconColor,
    default_fg: u32,
) -> u32 {
    let role = |id| {
        rs_guard
            .resolved_theme
            .get(id)
            .fg
            .map(|c| c.to_rgb_u32(default_fg))
            .unwrap_or(default_fg)
    };
    if is_dir {
        role(rs_guard.theme_ids.file_tree_dir)
    } else if is_hidden {
        role(rs_guard.theme_ids.file_tree_hidden)
    } else {
        icon_color_to_rgb(icol, default_fg)
    }
}

/// First grid row to paint for a terminal of `snap_rows` shown in a pane that
/// can hold `pane_rows` rows (`0` = not yet published ⇒ no cap). Returns the
/// START row so the BOTTOM `min(snap_rows, pane_rows)` rows render — recent
/// output + the cursor — and the pane NEVER paints more rows than it has. A
/// terminal grid is sized by the PTY, not the pane, so a horizontal split
/// (halved height) leaves the grid taller than its pane; without this bound
/// the `flex_shrink_0` rows overflow `pane_chrome` and push the modeline +
/// cmdline off-screen. Pure, so the clipping is unit-testable.
fn terminal_row_start(snap_rows: u16, pane_rows: u32) -> u16 {
    let max_rows: u16 = if pane_rows > 0 {
        (pane_rows as u16).min(snap_rows)
    } else {
        snap_rows
    };
    snap_rows.saturating_sub(max_rows)
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

// PU.2: `popup_wrap_enabled` (read of the `popup.wrap` option) deleted
// — the floating help popup now always wraps via the synthetic POPUP
// matrix's `wrap_width` (the host builds it with `wrap = true`, help-mode's
// declared `Wrap`), matching the TUI peer (PU.1b-3). No renderer-side
// wrap branch remains.

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
        out.push(GpuiFilterChordEntry {
            key: "b",
            label: "buf",
        });
    }
    if sources_present.contains(lattice_completion::insert::LSP_COMPLETION_SOURCE_ID) {
        out.push(GpuiFilterChordEntry {
            key: "o",
            label: "lsp",
        });
    }
    if sources_present.contains(lattice_completion::insert::PATH_SOURCE_ID) {
        out.push(GpuiFilterChordEntry {
            key: "f",
            label: "path",
        });
    }
    if sources_present.contains(lattice_completion::insert::TREE_SITTER_SYMBOL_SOURCE_ID) {
        out.push(GpuiFilterChordEntry {
            key: "t",
            label: "ts",
        });
    }
    if sources_present.contains(lattice_completion::insert::SNIPPET_SOURCE_ID) {
        out.push(GpuiFilterChordEntry {
            key: "s",
            label: "snip",
        });
    }
    out
}

fn gpui_render_filter_chord_footer(entries: &[GpuiFilterChordEntry], width_cols: u16) -> String {
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
                    // `^` is the standard Ctrl indicator
                    // (e.g. `^C` = Ctrl-C). `[^b]uf` reads as
                    // "Ctrl-b → buf" in half the chars of the
                    // full form.
                    format!("[^{}]{}", e.key, e.label)
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
#[allow(clippy::too_many_arguments)]
fn paint_candidate_row(
    cand: &lattice_completion::RenderedCandidate,
    selected: bool,
    theme: &GpuiTheme,
    padded: bool,
    display_col_chars: usize,
    columns: &lattice_completion::AnnotationColumns,
    // T.6: the resolved theme table + ids so annotation base colors
    // read from the registered `completion.annotation.*` elements.
    resolved: &lattice_host::ui::theme::ResolvedTheme,
    ids: &lattice_host::ui::theme::BuiltinElementIds,
) -> gpui::Div {
    // Issue #35 (2026-05-22): match highlight now uses
    // `picker_match_highlight` (Catppuccin peach by default,
    // distinct from `foreground`). Previously used
    // `cursor_background` which is identical to `foreground`
    // in the Catppuccin Mocha defaults — match highlights
    // were invisible. PH.1: the u32 is passed straight to
    // `preview_char_color_rgb` (no `rgb()` wrap needed here).
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
    // highlighting + PH.1 syntax-highlight overlay. Fast path:
    // no match ranges AND no syntax spans → single child
    // (empty-query "show all" with a plain preview hits this
    // every row).
    let display_div: gpui::Div =
        if cand.match_ranges.is_empty() && cand.raw.display_spans.is_empty() {
            div().child(display.clone()).text_color(row_fg)
        } else {
            // PH.1: per-char composition — fuzzy-match highlight over
            // the `display_spans` syntax overlay (match wins on
            // overlap, picker-preview-highlight.md §5). Decision is a
            // pure helper shared with the parity test; mirrors the TUI
            // peer's `push_preview_run`.
            let row_fg_u32 = if selected {
                theme.status_foreground
            } else {
                theme.foreground
            };
            let cells: Vec<gpui::Div> = display
                .char_indices()
                .map(|(byte_idx, c)| {
                    let fg = preview_char_color_rgb(
                        byte_idx,
                        &cand.match_ranges,
                        &cand.raw.display_spans,
                        resolved,
                        ids,
                        row_fg_u32,
                        theme.picker_match_highlight,
                    );
                    div().child(c.to_string()).text_color(rgb(fg))
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
    // MARG.3 (2026-06-03): per-variant annotation rendering —
    // each annotation gets its own child div coloured by its
    // category. Catppuccin palette below matches the design's
    // intent (yellow for keybinding, cyan for doc, etc.); the
    // TUI peer uses ratatui named colours for the same five
    // categories, palette parity. See
    // `docs/dev/architecture/marginalia.md` §5. Theme-driven
    // slot lookup remains a queued follow-up (same slice as
    // the matcher-highlight theme TODO at the top of
    // `lattice-ui-tui::render::candidate_to_line`); for now
    // each peer hardcodes its own palette tuned to its
    // baseline (Catppuccin here, ratatui 16-colour there).
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
        .child(
            div()
                .text_color(marginalia_fg)
                .flex_shrink_0()
                .child(kind_glyph),
        )
        .child(display_div.flex_shrink_0());
    // MARG.5 (2026-06-03): per-category column-aligned
    // annotations. Walk `columns` (pre-computed per-visible-
    // set max widths) in display order; render this
    // candidate's matching annotation per column or a blank
    // cell of the column width. Mirrors the TUI peer's
    // shape (`candidate_to_line`).
    if !columns.is_empty() {
        // Pad spaces so the first annotation column lands at
        // the same x as every other row. `+ 2` leaves a small
        // gap between candidate text and the column band.
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
        for (i, (category, col_width)) in columns.iter().enumerate() {
            if i > 0 {
                row = row.child(div().text_color(marginalia_fg).flex_shrink_0().child("  "));
            }
            let ann_for_col = cand.annotations.iter().find(|a| a.category() == category);
            match ann_for_col {
                Some(ann) => {
                    let text_chars = ann.display_text().chars().count();
                    let cell_pad = col_width.saturating_sub(text_chars);
                    match ann {
                        // MR.2: a styled cell paints one child div per
                        // segment, each resolved from its own theme slot
                        // (per-bit permission coloring). Selection shows via
                        // the row background, so segments read the slot fg in
                        // both states (no per-segment brightening). Parity
                        // with the TUI peer's `candidate_to_line`.
                        lattice_completion::Annotation::Styled { segments, .. } => {
                            for seg in segments {
                                let segfg = styled_segment_color_rgb(&seg.slot, resolved, ids);
                                row = row.child(
                                    div()
                                        .text_color(rgb(segfg))
                                        .flex_shrink_0()
                                        .child(seg.text.to_string()),
                                );
                            }
                        }
                        _ => {
                            let fg = rgb(annotation_color_rgb(ann, selected, resolved, ids));
                            row = row.child(
                                div()
                                    .text_color(fg)
                                    .flex_shrink_0()
                                    .child(ann.display_text().into_owned()),
                            );
                        }
                    }
                    if cell_pad > 0 {
                        row = row.child(
                            div()
                                .text_color(marginalia_fg)
                                .flex_shrink_0()
                                .child(" ".repeat(cell_pad)),
                        );
                    }
                }
                None => {
                    // Blank cell — keeps downstream columns
                    // aligned vertically across rows.
                    if col_width > 0 {
                        row = row.child(
                            div()
                                .text_color(marginalia_fg)
                                .flex_shrink_0()
                                .child(" ".repeat(col_width)),
                        );
                    }
                }
            }
        }
    }
    if let Some(bg) = row_bg {
        row.bg(bg)
    } else {
        row
    }
}

/// MR.2 (2026-06-30): resolve one [`lattice_completion::AnnotationSegment`]'s
/// foreground from its theme slot, as a `0xRRGGBB`. `ids.annotation_slot`
/// maps the slot key to its element (unknown → custom annotation); the
/// resolved fg falls back to the custom-blue literal only if even that
/// slot is unset. Shares the slot→id resolver with the TUI peer so a
/// styled marginalia cell colors identically across renderers.
fn styled_segment_color_rgb(
    slot: &str,
    resolved: &lattice_host::ui::theme::ResolvedTheme,
    ids: &lattice_host::ui::theme::BuiltinElementIds,
) -> u32 {
    resolved
        .get(ids.annotation_slot(slot))
        .fg
        .map(|c| c.to_rgb_u32(0x89b4fa))
        .unwrap_or(0x89b4fa)
}

/// PH.1: resolve one display char's foreground, composing the
/// fuzzy-match highlight over the `display_spans` syntax overlay.
/// **Match wins on overlap** (picker-preview-highlight.md §5): a
/// char inside any `match_ranges` paints `match_fg`; otherwise
/// the syntax color from the covering `display_spans` entry
/// (resolved via `resolve_syntax_style`, so `:colorscheme`
/// recolors live); otherwise `row_fg` (today's plain preview).
/// Pure so the paint path and the parity test share one source
/// of truth — the GPUI mirror of the TUI peer's
/// `push_preview_run`.
fn preview_char_color_rgb(
    byte_idx: usize,
    match_ranges: &[std::ops::Range<usize>],
    display_spans: &[lattice_completion::DisplaySpan],
    resolved: &lattice_host::ui::theme::ResolvedTheme,
    ids: &lattice_host::ui::theme::BuiltinElementIds,
    row_fg: u32,
    match_fg: u32,
) -> u32 {
    if match_ranges
        .iter()
        .any(|r| byte_idx >= r.start && byte_idx < r.end)
    {
        return match_fg;
    }
    display_spans
        .iter()
        .find(|ds| byte_idx >= ds.range.start && byte_idx < ds.range.end)
        .and_then(|ds| {
            lattice_host::ui::theme::resolve_syntax_style(resolved, ids, ds.style)
                .fg
                .map(|c| c.to_rgb_u32(row_fg))
        })
        .unwrap_or(row_fg)
}

/// MARG.3 (2026-06-03): map each [`lattice_completion::Annotation`]
/// variant to a Catppuccin-palette `0xRRGGBB`. `selected` lifts the
/// shade so it stays legible against the selected-row background
/// (`theme.status_background`). Mirrors the TUI peer's
/// `annotation_color` helper; the two peers maintain palette parity
/// even though one uses ratatui named colours and the other uses
/// concrete RGB. Theme-slot lookup is the queued follow-up that
/// would unify both.
///
/// Palette intent (matches `docs/dev/architecture/marginalia.md` §5):
/// - kind        → overlay2 grey (subtle, doesn't compete)
/// - doc snippet → sky (cyan family)
/// - keybinding  → yellow (Catppuccin Yellow; matches help-mode
///   chord highlight)
/// - source      → mauve (purple/magenta)
/// - custom      → blue (fallback for plugin annotations)
/// T.6: resolve a completion-annotation color. The *base*
/// (unselected) color now reads from the registered
/// `completion.annotation.{kind,doc,keybinding,source,custom}`
/// elements (shared with the host's theme registry). The selected-row
/// BRIGHTENING stays renderer logic: a selected row returns the
/// pre-existing brightened literal so the contrast against the
/// status-background stays intact (the brightening is NOT a separate
/// element). Fallbacks reproduce the legacy base literals.
fn annotation_color_rgb(
    ann: &lattice_completion::Annotation,
    selected: bool,
    resolved: &lattice_host::ui::theme::ResolvedTheme,
    ids: &lattice_host::ui::theme::BuiltinElementIds,
) -> u32 {
    use lattice_completion::Annotation;
    let (id, base_fallback, brightened) = match ann {
        Annotation::Kind(_) => (ids.completion_annotation_kind, 0x9399b2, 0xcdd6f4),
        Annotation::DocSnippet(_) => (ids.completion_annotation_doc, 0x89dceb, 0xbfeaf5),
        Annotation::Keybinding(_) => (ids.completion_annotation_keybinding, 0xf9e2af, 0xfff0b8),
        Annotation::Source(_) => (ids.completion_annotation_source, 0xcba6f7, 0xe2cfff),
        // Plugin / extension fallback. Unknown `slot` strings
        // all resolve to this colour pre-Phase-4; the typed
        // theme registry that resolves slot keys to colours
        // lands with the WASM plugin host.
        Annotation::Custom { .. } => (ids.completion_annotation_custom, 0x89b4fa, 0xb6d3ff),
        // `Styled` cells are painted per-segment by the caller (each
        // segment resolves its own slot via `ids.annotation_slot`), so
        // this whole-cell path is never taken for them; map to the custom
        // fallback for exhaustiveness.
        Annotation::Styled { .. } => (ids.completion_annotation_custom, 0x89b4fa, 0xb6d3ff),
    };
    if selected {
        // Brightened form preserved as renderer logic on top of the base.
        brightened
    } else {
        resolved
            .get(id)
            .fg
            .map(|c| c.to_rgb_u32(base_fallback))
            .unwrap_or(base_fallback)
    }
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
/// T.6.t: read a single-char `ui.diagnostic-*-glyph` typed option,
/// falling back to `dflt` (matches the deleted host `Theme::default()`
/// glyph literals: `■▲●·`). The TUI peer reads the same options via its
/// native `Theme` cache (`build_tui_theme`).
fn diagnostic_glyph_option<D>(config: &lattice_config::ConfigRegistry, dflt: char) -> char
where
    D: lattice_config::OptionDecl<Value = String>,
{
    config
        .get_typed::<D>()
        .and_then(|s| s.chars().next())
        .unwrap_or(dflt)
}

fn diagnostic_glyph_and_color(
    config: &lattice_config::ConfigRegistry,
    resolved: &lattice_host::ui::theme::ResolvedTheme,
    ids: &lattice_host::ui::theme::BuiltinElementIds,
    severity: lattice_lsp::DiagnosticSeverity,
) -> (char, u32) {
    // T.6.t: the severity glyph reads from the `ui.diagnostic-*-glyph`
    // typed options (was the deleted host `Theme.*_glyph` char); the
    // *style* reads from the resolved table.
    let (glyph, style) = match severity {
        lattice_lsp::DiagnosticSeverity::ERROR => (
            diagnostic_glyph_option::<lattice_host::ui::theme_options::UiDiagnosticErrorGlyph>(
                config, '■',
            ),
            resolved.get(ids.diagnostic_error),
        ),
        lattice_lsp::DiagnosticSeverity::WARNING => (
            diagnostic_glyph_option::<lattice_host::ui::theme_options::UiDiagnosticWarningGlyph>(
                config, '▲',
            ),
            resolved.get(ids.diagnostic_warning),
        ),
        lattice_lsp::DiagnosticSeverity::INFORMATION => (
            diagnostic_glyph_option::<lattice_host::ui::theme_options::UiDiagnosticInfoGlyph>(
                config, '●',
            ),
            resolved.get(ids.diagnostic_info),
        ),
        lattice_lsp::DiagnosticSeverity::HINT => (
            diagnostic_glyph_option::<lattice_host::ui::theme_options::UiDiagnosticHintGlyph>(
                config, '·',
            ),
            resolved.get(ids.diagnostic_hint),
        ),
        _ => (
            diagnostic_glyph_option::<lattice_host::ui::theme_options::UiDiagnosticInfoGlyph>(
                config, '●',
            ),
            resolved.get(ids.diagnostic_info),
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
/// (rebuilding every sub-state `Arc`, notifying the background
/// workers, nudging `paint_request`) regardless of whether the underlying
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
/// DR.2 (decoration-retention) removed the `pane_refresh_key` gate
/// along with the per-frame `RefreshPaneHighlights` dispatch it
/// guarded: inactive panes now read their own retained per-pane
/// `DisplayMatrix`, so there is no per-frame pane-highlight refresh
/// left to gate.
#[derive(Default)]
struct EnsureGateCache {
    cursor_snap_key: Option<(
        lattice_core::protocol::position::Position,
        u32,
        u32,
        lattice_core::BufferKind,
    )>,
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
    /// PU.2: last floating-popup inner `(rows, cols)` pushed to the host
    /// via `App::set_popup_viewport`. Diff-then-send across frames (the
    /// GPUI peer of the TUI runtime's `last_popup_dims` local) so a
    /// steady-state popup fires zero actor RPCs; `None` once on dismiss.
    last_popup_dims: Option<(u32, u32)>,
    /// PU.5d: diff-then-send cache for the completion-docs popup geometry
    /// (`set_completion_docs_viewport`), peer of `last_popup_dims`.
    last_completion_docs_dims: Option<(u32, u32)>,
}

impl EditorView {
    fn new(document: Document, cx: &mut Context<Self>) -> Self {
        let app = GpuiApp::new(document);
        // X1b: spawn the worker-paint-request bridge. The background
        // workers (cells / display-matrix, overlay) fire
        // `editor.paint_request.notify_one()` after every
        // content-changing `WorkerDecision::Recomputed`; this future
        // awaits each wake and calls `cx.notify()` so GPUI schedules
        // a paint even when no user input is in flight. Without this
        // bridge, an async worker recompute that finishes while the
        // user is idle (e.g. final reparse after a held-key burst
        // settles) would publish fresh matrix / overlay output that
        // nothing reads until the next keystroke -- breaking goal-#4
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
        // AW.3: this bridge is a PURE repaint forwarder — `paint_request`
        // → `cx.notify()`. It no longer drains `run_tick_pending` itself.
        // The editor actor's `async_landed` arm (`editor_actor.rs`) is the
        // single, renderer-agnostic drain chokepoint: every async result
        // (LSP action requests, picker init / live-query, event-bus
        // arrivals, worker recomputes) fires `async_landed`, and the actor
        // runs `run_tick_pending` + `publish_render_state` + fires
        // `paint_request` ON THE ACTOR THREAD before this bridge ever wakes.
        // So by the time we get here the result is already in the published
        // `RenderState`; we only need to schedule a GPUI paint. This makes
        // the GPUI peer a pure consumer of published state, exactly like the
        // TUI's `Wake::Repaint` arm — the drain is no longer duplicated in a
        // renderer-specific path. (Previously the bridge ran its own
        // `run_tick_pending` to close the idle-hover gap; that gap is now
        // closed uniformly by `async_landed` — see lsp-architecture §12.)
        let paint_request = app.paint_request.clone();
        cx.spawn(async move |this, cx| {
            loop {
                paint_request.notified().await;
                if this.update(cx, |_view, cx| cx.notify()).is_err() {
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
            last_popup_dims: None,
            last_completion_docs_dims: None,
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
        pane_rows: &std::collections::HashMap<usize, u32>,
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
            PaneNode::Leaf(idx) => {
                self.paint_pane(*idx, theme, *idx == active_idx, row_px, pane_rows)
            }
            PaneNode::HorizontalSplit { top, bottom, ratio } => {
                let ratio = ratio.clamp(0.05, 0.95);
                div()
                    .flex()
                    .flex_col()
                    .flex_grow()
                    .child(
                        self.paint_pane_tree(top, theme, active_idx, row_px, pane_rows)
                            .flex_grow()
                            .flex_basis(px(ratio * RATIO_SCALE))
                            .min_h(px(0.0))
                            .border_b_1()
                            .border_color(rgb(theme.popup_border)),
                    )
                    .child(
                        self.paint_pane_tree(bottom, theme, active_idx, row_px, pane_rows)
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
                        self.paint_pane_tree(left, theme, active_idx, row_px, pane_rows)
                            .flex_grow()
                            .flex_basis(px(ratio * RATIO_SCALE))
                            .min_w(px(0.0))
                            .border_r_1()
                            .border_color(rgb(theme.popup_border)),
                    )
                    .child(
                        self.paint_pane_tree(right, theme, active_idx, row_px, pane_rows)
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
    /// ML.2: build the per-pane modeline as a zone / per-`Span` flex row,
    /// reusing the SAME host resolver the TUI uses
    /// (`lattice_host::modeline`) so the *content* is identical across
    /// peers — only this paint differs. Three zones via `justify_between`
    /// (Left flush-left, Right flush-right, Center between); flexbox gives
    /// the zone layout natively. Per-role colours are adapted inline from
    /// the resolved theme (GPUI keeps no style cache;
    /// `feedback_renderer_cache_protects_ux`). Active panes compose the
    /// per-role fg over the `modeline.active` bar bg; inactive panes use
    /// the uniform muted `modeline.inactive` bar.
    ///
    /// `provider_label` is `None`: the GPUI peer has no M.4 pane-render
    /// provider registry yet (Document → path, Terminal → registry name
    /// slot, both via the resolver). A GPUI provider registry mirrors the
    /// TUI's M.4 later; until then file-tree/oil aren't GPUI pane-rendered.
    fn modeline_row(
        pane: &PaneState,
        is_active: bool,
        rs: &lattice_host::render_state::RenderState,
    ) -> gpui::Div {
        const FALLBACK_FG: u32 = 0x00cd_d6f4; // palette `text`
        const FALLBACK_BAR: u32 = 0x001e_1e2e; // palette `base`
        let snap = &rs.modeline_elements;
        let resolved = &rs.resolved_theme;
        let ids = &rs.theme_ids;

        // Bar background — active (`surface1`) vs inactive (`surface0`).
        let bar = if is_active {
            resolved.get(ids.modeline_active)
        } else {
            resolved.get(ids.modeline_inactive)
        };
        let bar_bg = bar
            .bg
            .map(|c| c.to_rgb_u32(FALLBACK_BAR))
            .unwrap_or(FALLBACK_BAR);

        // ML.5: the `ui.modeline.{left,center,right}` config drives zone
        // membership + order; `resolve_layout` returns the per-zone
        // descriptor lists (Auto = descriptor placement) + the configured
        // separator. Content per descriptor is still resolved here
        // (built-ins host-side, pushed from the snapshot) — same shape as
        // the TUI's `resolve_zone`, parity in lockstep.
        let layout = lattice_host::modeline::resolve_layout(&snap.registry, &rs.options.config);
        let sep = layout.separator.clone();
        let zone_runs = |els: &[&lattice_mode::ModelineElement]| -> Vec<(String, Option<lattice_mode::ModelineRole>)> {
            let mut runs: Vec<(String, Option<lattice_mode::ModelineRole>)> = Vec::new();
            for el in els {
                // §7: Global-scope elements (e.g. the diff summary) render
                // only on the active pane; PaneLocal is the default.
                if matches!(el.scope, lattice_mode::Scope::Global) && !is_active {
                    continue;
                }
                let id = el.id.as_str();
                let content = if id.starts_with("core.") {
                    lattice_host::modeline::resolve_builtin_content(id, pane, is_active, rs, None)
                } else {
                    // Pushed elements (modes / plugins, ML.3): resolved
                    // per the descriptor's scope against this pane's
                    // buffer (PaneLocal) or the global slot.
                    snap.resolve(el, pane.buffer_id).cloned().unwrap_or_default()
                };
                if content.is_empty() {
                    continue;
                }
                // Configured separator between elements within a zone
                // (`ui.modeline.separator`, default a single space).
                if !runs.is_empty() && !sep.is_empty() {
                    runs.push((sep.clone(), None));
                }
                for span in content.spans {
                    if !span.text.is_empty() {
                        runs.push((span.text, Some(span.role)));
                    }
                }
            }
            runs
        };

        let mut left = zone_runs(&layout.left);
        let center = zone_runs(&layout.center);
        let mut right = zone_runs(&layout.right);
        // `ui.modeline.padding`: blank margin at the row's start / end,
        // expressed as content spaces so it matches the TUI peer exactly
        // (the old `px_2` chrome in `pane_chrome` is dropped in favour of
        // this configurable, cell-uniform margin).
        if layout.padding > 0 {
            let pad = (" ".repeat(layout.padding), None);
            left.insert(0, pad.clone());
            right.push(pad);
        }

        // Style one run: inactive → uniform muted; active → per-role fg.
        let styled_run =
            move |text: String, role: Option<lattice_mode::ModelineRole>| -> gpui::Div {
                use lattice_host::modeline as ml;
                let style = if is_active {
                    // Per-role element id (inferred type; inline to avoid
                    // naming `lattice_theme::ElementId` here).
                    let id = match role.as_ref().map(|r| r.as_str()) {
                        Some(ml::ROLE_MODE) => ids.modeline_mode,
                        Some(ml::ROLE_PATH) => ids.modeline_path,
                        Some(ml::ROLE_POSITION) => ids.modeline_position,
                        Some(ml::ROLE_LANG) => ids.modeline_lang,
                        Some(ml::ROLE_MODE_ITEM) => ids.modeline_mode_item,
                        // Padding / unknown: text-ish on the bar.
                        _ => ids.modeline_path,
                    };
                    resolved.get(id)
                } else {
                    resolved.get(ids.modeline_inactive)
                };
                let fg = style
                    .fg
                    .map(|c| c.to_rgb_u32(FALLBACK_FG))
                    .unwrap_or(FALLBACK_FG);
                let mut span = div().text_color(rgb(fg)).child(text);
                if style.modifiers.bold {
                    span = span.font_weight(gpui::FontWeight::BOLD);
                }
                span
            };
        let zone_div = |runs: Vec<(String, Option<lattice_mode::ModelineRole>)>| -> gpui::Div {
            let mut z = div().flex().flex_row();
            for (text, role) in runs {
                z = z.child(styled_run(text, role));
            }
            z
        };

        div()
            .flex()
            .flex_row()
            .w_full()
            .justify_between()
            .bg(rgb(bar_bg))
            .child(zone_div(left))
            .child(zone_div(center))
            .child(zone_div(right))
    }

    /// Wrap a pane's `inner` content with the per-pane modeline row
    /// beneath it. The row is the styled flex element built by
    /// [`Self::modeline_row`] (it already carries the bar bg + per-role
    /// spans); this just stacks content + row and dims inactive content.
    fn pane_chrome(
        inner: AnyElement,
        status_row: gpui::Div,
        render_active: bool,
        inactive_opacity: f32,
    ) -> gpui::Div {
        // 2026-05-27: dim the buffer content (NOT the status row) when
        // this pane is inactive. `inactive_opacity` is user-configurable
        // via `:set ui.inactive_pane_opacity=N`. The status row stays at
        // full opacity (its own muted styling marks it inactive).
        let content_opacity: f32 = if render_active { 1.0 } else { inactive_opacity };
        div()
            .flex()
            .flex_col()
            .flex_grow()
            .min_h(px(0.0))
            .overflow_hidden()
            .child(
                // `min_h(0)` lets the content area shrink below its content's
                // intrinsic min-size (a terminal's `flex_shrink_0` rows) so it
                // is bounded by the pane and the per-pane modeline (the sibling
                // below) is never pushed out — `overflow_hidden` then clips the
                // excess. Without it, taffy keeps the content's full min-size.
                div()
                    .flex_grow()
                    .min_h(px(0.0))
                    .p_3()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .opacity(content_opacity)
                    .child(inner),
            )
            // Horizontal margin is now content-level (`ui.modeline.padding`,
            // applied in `modeline_row` as leading/trailing spaces) so it
            // matches the TUI peer exactly; only the vertical chrome stays.
            .child(status_row.py_1())
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
        pane_rows: &std::collections::HashMap<usize, u32>,
    ) -> gpui::Div {
        // Slice 3c.final.E.swap: paint reads route through the
        // App's own `render_state` Arc (cloned from
        // `editor.render_state` at construction). No `&Editor`
        // borrow held across the function body.
        let ad = self.app.ad();
        let rs_guard = self.app.render_state.load();
        // display-line B4.2: the `visible_spans` / `visible_rows`
        // prepaint loads retired here. `EditorElement`'s active-pane
        // shaping reads from `rs_guard.cells.matrix` /
        // `rs_guard.cells.display_matrix`, falling back to
        // default-styled text for boot frames / folded rows.
        // Perf plan B.2 slice B.2.a: worker's per-row pre-bucketed
        // static-overlay quads (doc_highlight / all_matches /
        // substitute). Active pane consumes this directly; inactive
        // panes fall through to the legacy per-frame bucket (only
        // doc_highlight is painted for them and N is small).
        let active_overlay_quads_guard = rs_guard.syntax.static_overlay_quads.load();
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
            // Bound to THIS frame's fresh pane-row budget (falls back to the
            // published `viewport_height` if absent) so the terminal never
            // overflows a height-reduced (horizontal-split) pane.
            let fresh_rows = pane_rows
                .get(&pane_idx)
                .copied()
                .unwrap_or(pane.viewport_height);
            let inner = self.build_terminal_inner(
                pane,
                &rs_guard,
                theme,
                insert_active,
                is_active,
                row_px,
                fresh_rows,
            );
            // ML.2: terminal panes get the same zone/per-Span modeline as
            // every other kind (shared resolver) — no kind-specific status.
            let status_row = Self::modeline_row(pane, is_active, &rs_guard);
            return Self::pane_chrome(
                inner,
                status_row,
                is_active,
                inactive_pane_opacity(&self.app),
            );
        }
        // Oil + file-tree are non-`Document` buffer kinds: their content
        // lives in `BufferData::Oil` / `BufferData::FileTree`, not behind
        // `document_handle`, so they need their own inner builders (the
        // TUI peer renders them via the M.4 pane-render providers
        // `oil_pane_render` / `file_tree_pane_render`). Handled inline here
        // for the same reason Terminal is — the GPUI M.4 provider registry
        // is a later slice (see `pane_chrome` doc). Both flow through the
        // shared `pane_chrome` wrapper so the modeline row is reserved
        // uniformly [[feedback_buffers_no_special_case]].
        if matches!(pane.buffer, lattice_core::BufferKind::Oil) {
            let inner = self.build_oil_inner(pane, &rs_guard, theme, is_active);
            let status_row = Self::modeline_row(pane, is_active, &rs_guard);
            return Self::pane_chrome(
                inner,
                status_row,
                is_active,
                inactive_pane_opacity(&self.app),
            );
        }
        if matches!(pane.buffer, lattice_core::BufferKind::FileTree) {
            let inner = self.build_file_tree_inner(pane, &rs_guard, theme, is_active);
            let status_row = Self::modeline_row(pane, is_active, &rs_guard);
            return Self::pane_chrome(
                inner,
                status_row,
                is_active,
                inactive_pane_opacity(&self.app),
            );
        }
        // Resolve the buffer's document handle. Inactive panes may
        // reference buffers different from `editor.document`; the
        // registry clone on `rs_guard.buffers` shares the editor's
        // `Arc<Mutex<...>>` so the lookup sees the latest state.
        // 2026-06-02: pull both the snapshot AND the per-pane
        // `display_line_numbers` mapping from the SAME handle.
        // Previously the gutter meta below cloned
        // `rs_guard.active_document.load().display_line_numbers`, which
        // gave inactive multibuffer panes the active doc's
        // mapping (None for regular files → identity numbering
        // 1,2,3 instead of source line numbers). Per "active
        // state only affects the active pane" — TUI/GPUI parity
        // [[feedback_tui_gpui_parity]].
        let handle_opt = rs_guard.buffers.registry.document_handle(pane.buffer_id);
        let Some(handle) = handle_opt else {
            return div()
                .p_3()
                .child(format!("(buffer {:?} unavailable)", pane.buffer_id));
        };
        let snapshot = handle.snapshot();
        let pane_display_line_numbers: Option<std::sync::Arc<[u32]>> =
            handle.display_line_numbers();
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
        // MB.1 (rich minibuffer): while the `:` line is open, `self.document`
        // is the synthetic `*command-line*` buffer. The active pane must keep
        // its own buffer's cursor / scroll (the pane body is already
        // registry-keyed) rather than adopting the command-line cursor — the
        // same treatment as a focus-stealing popup.
        // MB.5: same for the `/`·`?` search line.
        let popup_owns_active =
            ad.popup_focused || ad.command_line_active || ad.search_line_active;
        // When the popup has focus the document pane should look
        // inactive — no cursorline, no selection, no active status
        // bar — the same appearance it has when a different pane has
        // focus.
        // PI.3/PI.4: a pane that is *previewing* another buffer renders as an
        // isolated projection — it reads the DISPLAYED buffer's snapshot
        // (via `pane.buffer_id`, already substituted in the published leaf)
        // plus the preview cursor / scroll baked into the leaf, NOT the
        // active document's (`ad.*`). So the focused pane is "render active"
        // only when showing its committed buffer.
        let render_active = is_active && !popup_owns_active && !pane.is_previewing();
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
        // HS.1b: horizontal scroll mirrors the same active-vs-stashed
        // rule as `pane_scroll`. `ad.leftcol` is the active pane's
        // live value; an inactive (or popup-backgrounded) pane reads
        // its stashed `PaneState::leftcol`.
        let pane_leftcol = if render_active {
            ad.leftcol
        } else {
            pane.leftcol
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
        let mut raw_lines: Vec<String> =
            Vec::with_capacity(visible_end.saturating_sub(visible_start));
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
        // PU.1b-1a: reserve line-number digits only when `number` is set;
        // gutterless buffers (help / dashboard) get 0 (matches the TUI gate).
        let show_line_numbers = rs_guard
            .active_document
            .load()
            .option_cache
            .show_line_numbers;
        let gutter_width = if show_line_numbers {
            total_lines_for_gutter.to_string().len()
        } else {
            0
        };

        // MO.4.a: URI + render_state used by the gutter-decoration
        // pre-loop below to inject LspDiagnosticsData service.
        let uri = rs_guard.buffers.uris.get(&pane.buffer_id);
        // Slice 3c.final.E.swap: render_state via App's own Arc.
        let render_state = self.app.render_state.load_full();

        // T.6.t: the severity glyph reads from the published
        // typed-options registry (`ui.diagnostic-*-glyph`); `:set`
        // overrides flow through identically for both renderer peers.
        // The deleted host `Theme` carried the glyph char; the *style*
        // still resolves through the table below.
        let config = render_state.options.config.clone();
        // T.4 (theme-system): the resolved read table + builtin ids,
        // snapshotted into `RenderState`. GPUI has no native theme
        // cache — it adapts inline via `to_rgb_u32` per read — so the
        // migrated diagnostic styles read `resolved.get(ids.x)` here.
        let resolved_theme = rs_guard.resolved_theme.clone();
        let theme_ids = rs_guard.theme_ids;

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
        // Fold-bleed fix (2026-06-30): fold elision must use THIS pane's
        // buffer's folds, not the active doc's. Previously every pane built
        // its index from `active_document.folds`, so folding buffer A elided
        // lines in an inactive pane showing buffer B. `folds_for_buffer`
        // resolves the per-buffer list (active → live `self.folds`; other →
        // published `cells.panes` entry). Shared with the TUI peer.
        let (pane_folds, pane_foldenable) = rs_guard.folds_for_buffer(pane.buffer_id);
        let fold_index = lattice_host::folds::FoldIndex::from_folds(&pane_folds, pane_foldenable);
        // K.4.6 follow-up (2026-06-02): cache the
        // display_line_numbers map outside the per-row closure.
        // None for regular Documents (gutter shows composed-row
        // identity); Some(arr) for Multibuffer where
        // arr[composed_row] is the source line in the original
        // file. Substrate-published via the Document trait
        // method; renderer just consumes. TUI/GPUI parity per
        // [[feedback_tui_gpui_parity]].
        // 2026-06-02: per-pane mapping, not active-doc mapping.
        // Loaded above from the same handle that backed
        // `snapshot`. None for regular Documents (gutter shows
        // composed-row identity); Some(arr) for Multibuffer.
        let display_line_numbers_for_meta = pane_display_line_numbers.clone();
        // MO.4.a: gutter-decoration pre-loop. Walk active modes for this
        // pane's buffer once; accumulate GutterDecoration contributions into
        // per-line maps. Replaces per-line render_state reads inside gutter_meta.
        let (diff_gutter, severity_gutter) = {
            use lattice_mode::{
                DecorationCtx, GutterDecoration, GutterDiffKind, GutterSeverityLevel,
                ServiceRegistry,
            };
            let mut services = ServiceRegistry::new();
            // D-fix.3b: per-pane gutter signs — register THIS pane's buffer's
            // sign map (proposed→current-side, baseline→baseline-side) so both
            // panes of a side-by-side diff show signs, not just the active one.
            if let Some(sign_map) = rs_guard.diff.sign_maps.get(&pane.buffer_id) {
                services.register(lattice_host::diff::mode::DiffDecorationData {
                    sign_map: sign_map.clone(),
                });
            }
            // LspDiagnosticsData: inject when URI resolves (lsp-mode gate is
            // implicit — LspMode::gutter_decorations returns empty when service absent).
            {
                let diagnostics =
                    uri.and_then(|u| render_state.diagnostics.layer.diagnostics_arc(u));
                services.register(lattice_lsp::modes::LspDiagnosticsData { diagnostics });
            }
            // CM.3c: inject the `*compilation*` buffer's severity index (the
            // off-thread compilation drain → `render_state.compilation_severity`
            // slot → here). Lockstep with the TUI peer: the renderer only reads
            // the slot and registers the carrier — no `lattice-compilation`
            // dependency, no paint-time scan. `CompilationMode::gutter_decorations`
            // maps it to `Severity` marks in the same gutter column as LSP.
            if let Some(entries) = rs_guard.compilation_severity.get(&pane.buffer_id) {
                services.register(lattice_mode::CompilationSeverityData {
                    entries: entries.clone(),
                });
            }
            let deco_ctx = DecorationCtx::new(pane.buffer_id, &services);
            let mut diff_map: std::collections::HashMap<u32, GutterDiffKind> = Default::default();
            let mut sev_map: std::collections::HashMap<u32, GutterSeverityLevel> =
                Default::default();
            if let Some(active) = rs_guard.modes.map.get(&pane.buffer_id) {
                let registry = &rs_guard.modes.mode_registry;
                let mut all_ids: Vec<lattice_mode::ModeId> = Vec::new();
                if let Some(major) = active.major() {
                    all_ids.push(major);
                }
                all_ids.extend_from_slice(active.minors());
                for id in all_ids {
                    if let Some(mode) = registry.get(id) {
                        for deco in mode.gutter_decorations(&deco_ctx) {
                            match deco {
                                GutterDecoration::Diff { line, kind } => {
                                    diff_map.entry(line).or_insert(kind);
                                }
                                GutterDecoration::Severity { line, level } => {
                                    sev_map
                                        .entry(line)
                                        .and_modify(|e| {
                                            if level > *e {
                                                *e = level;
                                            }
                                        })
                                        .or_insert(level);
                                }
                            }
                        }
                    }
                }
            }
            // PL8.E: merge WASM plugin gutter decorations (host-cached, read
            // wait-free) into the SAME partition. Never runs WASM at paint
            // time — the producer wrote this cache off the render path; the
            // renderer only reads it, so plugin marks paint through the
            // identical glyph/tint mapping below. Lockstep with the TUI peer.
            {
                use lattice_host::per_buffer_cache::PerBufferCacheExt;
                if let Some(cache) = rs_guard.wasm_gutter_decorations.get_for(pane.buffer_id) {
                    for deco in &cache.decorations {
                        match deco {
                            GutterDecoration::Diff { line, kind } => {
                                diff_map.entry(*line).or_insert(*kind);
                            }
                            GutterDecoration::Severity { line, level } => {
                                sev_map
                                    .entry(*line)
                                    .and_modify(|e| {
                                        if *level > *e {
                                            *e = *level;
                                        }
                                    })
                                    .or_insert(*level);
                            }
                        }
                    }
                }
            }
            (diff_map, sev_map)
        };
        // T.6.t: hoist the four severity glyphs out of the per-line
        // closure — one typed-option read each instead of O(viewport)
        // lookups. The *style* still resolves per-line from the table.
        let glyph_error = diagnostic_glyph_option::<
            lattice_host::ui::theme_options::UiDiagnosticErrorGlyph,
        >(&config, '■');
        let glyph_warning = diagnostic_glyph_option::<
            lattice_host::ui::theme_options::UiDiagnosticWarningGlyph,
        >(&config, '▲');
        let glyph_info = diagnostic_glyph_option::<
            lattice_host::ui::theme_options::UiDiagnosticInfoGlyph,
        >(&config, '●');
        let glyph_hint = diagnostic_glyph_option::<
            lattice_host::ui::theme_options::UiDiagnosticHintGlyph,
        >(&config, '·');
        // Fold-marker colours, resolved once per pane from the theme.
        // Muted by cross-editor convention (open dimmer than closed);
        // the defaults mirror the `overlay` / `subtext` palette tones so
        // a theme with an unset element still reads sensibly.
        let fold_open_color = resolved_theme
            .get(theme_ids.gutter_fold_open)
            .fg
            .map(|c| c.to_rgb_u32(0x6c7086))
            .unwrap_or(0x6c7086);
        let fold_closed_color = resolved_theme
            .get(theme_ids.gutter_fold_closed)
            .fg
            .map(|c| c.to_rgb_u32(0x9399b2))
            .unwrap_or(0x9399b2);
        let gutter_meta: Vec<crate::editor_element::GutterLineMeta> = (visible_start..visible_end)
            .filter(|line_idx| !fold_index.line_inside_closed_fold(*line_idx as u32))
            .map(|line_idx| {
                // Show a marker on every foldable head (open or closed)
                // when foldenable is on, matching the TUI peer — `▾`
                // expanded, `▸` collapsed — each in its themed colour.
                let fold_marker = fold_index.fold_start_kind_at(line_idx as u32).map(|kind| {
                    use lattice_host::folds::FoldMarker;
                    match kind {
                        FoldMarker::Open => {
                            (crate::editor_element::FOLD_GLYPH_OPEN, fold_open_color)
                        }
                        FoldMarker::Closed => {
                            (crate::editor_element::FOLD_GLYPH_CLOSED, fold_closed_color)
                        }
                    }
                });
                // MO.4.a: read from pre-built mode-walk map.
                let severity = severity_gutter
                    .get(&(line_idx as u32))
                    .copied()
                    .map(|level| {
                        use lattice_mode::GutterSeverityLevel;
                        // T.6.t: glyph from the hoisted `ui.diagnostic-*-glyph`
                        // option chars; style from the resolved table.
                        let (glyph, style) = match level {
                            GutterSeverityLevel::Error => {
                                (glyph_error, resolved_theme.get(theme_ids.diagnostic_error))
                            }
                            GutterSeverityLevel::Warning => (
                                glyph_warning,
                                resolved_theme.get(theme_ids.diagnostic_warning),
                            ),
                            GutterSeverityLevel::Info => {
                                (glyph_info, resolved_theme.get(theme_ids.diagnostic_info))
                            }
                            GutterSeverityLevel::Hint => {
                                (glyph_hint, resolved_theme.get(theme_ids.diagnostic_hint))
                            }
                        };
                        let color = style.fg.map(|c| c.to_rgb_u32(0x9399b2)).unwrap_or(0x9399b2);
                        (glyph, color)
                    });
                let display_line = display_line_numbers_for_meta
                    .as_ref()
                    .and_then(|m| m.get(line_idx).copied())
                    .unwrap_or(line_idx as u32);
                // MO.4.a: read from pre-built mode-walk map.
                let diff_sign = diff_gutter.get(&(line_idx as u32)).copied().map(|kind| {
                    use lattice_mode::GutterDiffKind;
                    // T.4.b: read sign styles from the resolved table.
                    let style = match kind {
                        GutterDiffKind::Add => resolved_theme.get(theme_ids.diff_add_sign),
                        GutterDiffKind::Change => resolved_theme.get(theme_ids.diff_change_sign),
                        GutterDiffKind::Remove => resolved_theme.get(theme_ids.diff_remove_sign),
                        GutterDiffKind::Conflict => {
                            resolved_theme.get(theme_ids.diff_conflict_sign)
                        }
                    };
                    let fg = style.fg.map(|c| c.to_rgb_u32(0xcdd6f4)).unwrap_or(0xcdd6f4);
                    let glyph = match kind {
                        GutterDiffKind::Add => '+',
                        GutterDiffKind::Change => '~',
                        GutterDiffKind::Remove => '-',
                        GutterDiffKind::Conflict => '?',
                    };
                    (glyph, fg)
                });
                crate::editor_element::GutterLineMeta {
                    line_idx: line_idx as u32,
                    display_line,
                    fold_marker,
                    severity,
                    diff_sign,
                    is_virtual: false,
                }
            })
            .collect();

        // D.3.e (2026-05-29): per-visible-row diff tint. Same
        // sign-map source as `diff_sign` above — Add → faint
        // dark green, Change → faint dark yellow, Remove → no
        // current-side row to tint (deletion block is the
        // visual surface). Order parallels `gutter_meta` so
        // `rel_row` indexes both arrays.
        // D-fix.3b: per-pane — tint from THIS pane's buffer's sign map so a
        // side-by-side diff colours both sides. `None` ⇒ no diff session for
        // this buffer ⇒ no tints.
        let pane_sign_map = rs_guard.diff.sign_maps.get(&pane.buffer_id).cloned();
        let diff_tint_per_row: Vec<Option<u32>> = (visible_start..visible_end)
            .filter(|line_idx| !fold_index.line_inside_closed_fold(*line_idx as u32))
            .map(|line_idx| {
                pane_sign_map
                    .as_ref()
                    .and_then(|sm| sm.sign_at(line_idx as u32))
                    .and_then(|kind| {
                        use lattice_host::diff::overlay::DiffSignKind;
                        // T.4.b: read line tint colours from the resolved
                        // table's `bg` channel (`None` ⇒ no tint).
                        match kind {
                            DiffSignKind::Add => resolved_theme
                                .get(theme_ids.diff_add_line)
                                .bg
                                .map(|c| c.to_rgb_u32(0)),
                            DiffSignKind::Change => resolved_theme
                                .get(theme_ids.diff_change_line)
                                .bg
                                .map(|c| c.to_rgb_u32(0)),
                            // D-fix.3b: baseline removed lines tint red.
                            DiffSignKind::Remove => resolved_theme
                                .get(theme_ids.diff_remove_line)
                                .bg
                                .map(|c| c.to_rgb_u32(0)),
                            // D.6.f (2026-05-31): three-way Conflict tint.
                            DiffSignKind::Conflict => resolved_theme
                                .get(theme_ids.diff_conflict_line)
                                .bg
                                .map(|c| c.to_rgb_u32(0)),
                        }
                    })
            })
            .collect();
        // CM.3d (2026-07-22): compilation location-line bg tint,
        // computed from the render-state location-line index.
        // Same shape as diff tint above.
        let compilation_location_tint_per_row: Vec<Option<u32>> =
            (visible_start..visible_end)
                .filter(|line_idx| !fold_index.line_inside_closed_fold(*line_idx as u32))
                .map(|line_idx| {
                    rs_guard
                        .compilation_location_lines
                        .get(&pane.buffer_id)
                        .and_then(|entries| {
                            entries
                                .iter()
                                .find(|(l, _, _)| *l == line_idx as u32)
                        })
                        .map(|_| {
                            let (bg, _fg) = *rs_guard.compilation_theme_colors;
                            bg
                        })
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
            rs_guard.active_document.load().visual_range
        } else {
            None
        };
        // 2026-05-27: Visual(Blockwise) rectangle published by the
        // host. Element-side paints a per-line column band instead
        // of the linear visual_range.
        let visual_block_extents = if render_active {
            rs_guard.active_document.load().visual_block_extents
        } else {
            None
        };
        // MB.5: while the `/`·`?` search line is open, the document pane
        // renders via the inactive path, but hlsearch / current-match
        // overlays must still paint so the user sees live matches.
        let render_overlays = render_active || ad.search_line_active;
        let current_match = if render_overlays {
            rs_guard.active_document.load().current_match
        } else {
            None
        };
        let all_matches: Vec<lattice_core::protocol::position::Range> = if render_overlays {
            rs_guard.active_document.load().all_matches.to_vec()
        } else {
            Vec::new()
        };
        let substitute_matches: Vec<lattice_core::protocol::position::Range> = if render_active {
            rs_guard
                .active_document
                .load()
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
                        let start_text = if (start_line as usize) < total_lines {
                            snapshot.buffer.line(start_line).unwrap_or_default()
                        } else {
                            String::new()
                        };
                        let end_text = if (end_line as usize) < total_lines {
                            snapshot.buffer.line(end_line).unwrap_or_default()
                        } else {
                            String::new()
                        };
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
        // T.4.d: current-line tint from the resolved table's `bg`
        // channel (legacy `Color::Indexed(236)` → the 0x313244
        // fallback, matching prior behaviour since Indexed has no Rgb).
        let cursorline_bg = resolved_theme
            .get(theme_ids.editor_cursor_line)
            .bg
            .map(|c| c.to_rgb_u32(0x313244))
            .unwrap_or(0x313244);
        // Gate the cursorline quad on `:set cursorline`
        // (`current-line-highlight`, default off) — same active-document
        // option-cache seam the TUI reads (`render.rs` cursorline path)
        // and the same seam used for `foldenable` above. Without this the
        // GPUI peer painted the cursorline unconditionally.
        // PI.4: a focused preview pane resolves cursorline from the
        // DISPLAYED buffer through the renderer-agnostic seam (the same
        // `RenderState` method the TUI peer calls), so the previewed
        // buffer keeps its own cursorline; otherwise the active document's
        // resolved value.
        let cursorline_enabled = if pane.is_previewing() {
            rs_guard.current_line_highlight_for(pane.buffer_id)
        } else {
            rs_guard
                .active_document
                .load()
                .option_cache
                .current_line_highlight
        };

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
                        let line_text = if (line_idx as usize) < total_lines {
                            snapshot.buffer.line(line_idx).unwrap_or_default()
                        } else {
                            String::new()
                        };
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
                        let start_text = if (start_line as usize) < total_lines {
                            snapshot.buffer.line(start_line).unwrap_or_default()
                        } else {
                            String::new()
                        };
                        let end_text = if (end_line as usize) < total_lines {
                            snapshot.buffer.line(end_line).unwrap_or_default()
                        } else {
                            String::new()
                        };
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
                            .map(|s| {
                                diagnostic_glyph_and_color(&config, &resolved_theme, &theme_ids, s)
                                    .1
                            })
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

        // L4a.3 (lsp-architecture.md §15): inline cursor-line diagnostic
        // summary. The host idle gate (L4a.2) publishes `(line, summary)`
        // for the ACTIVE buffer's cursor line; resolve it only on the
        // active pane (the summary tracks the focused cursor). The
        // severity rank maps to the same host-theme colour the gutter
        // glyph + underline use, via `diagnostic_glyph_and_color`.
        let diag_mode_on = render_state
            .translator
            .active_minor_modes
            .iter()
            .any(|m| *m == lattice_lsp::modes::LspDiagnosticsMode::mode_id());
        let inline_diag_summary: Option<crate::editor_element::InlineDiagSummary> = if is_active
            && diag_mode_on
        {
            render_state
                .diagnostics
                .inline_summary
                .as_ref()
                .map(|(line, summary)| {
                    let severity = match summary.severity_rank {
                        0 => lattice_lsp::DiagnosticSeverity::ERROR,
                        1 => lattice_lsp::DiagnosticSeverity::WARNING,
                        2 => lattice_lsp::DiagnosticSeverity::INFORMATION,
                        _ => lattice_lsp::DiagnosticSeverity::HINT,
                    };
                    let color =
                        diagnostic_glyph_and_color(&config, &resolved_theme, &theme_ids, severity)
                            .1;
                    crate::editor_element::InlineDiagSummary {
                        line: *line,
                        text: format!("    {}", summary.text),
                        color,
                    }
                })
        } else {
            None
        };

        // T.6: inlay color resolves from the `inlay.hint` element
        // (shared with the TUI peer's `inlay_hint_style`).
        let inlay_color: u32 = resolved_theme
            .get(theme_ids.inlay_hint)
            .fg
            .map(|c| c.to_rgb_u32(0x7f849c))
            .unwrap_or(0x7f849c);

        // ML.2: the per-pane modeline is built by `Self::modeline_row`
        // (zone / per-Span flex row) from the SHARED host resolver
        // (`lattice_host::modeline`) at the `pane_chrome` call below —
        // identical content to the TUI peer, only the paint differs. The
        // old inline modal-label + path + mode-items + lang assembly (and
        // its `PENDING`/`COMMAND` drift from the host `O-PEND`/`CMD`) is
        // gone; the host resolver is the single source.

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
            // display-line B4.2: the `visible_spans` field was deleted
            // from `EditorElement`. Both active and inactive panes
            // render syntax colour from their per-pane `DisplayMatrix`
            // (`cell_matrix` / `display_matrix` below, built by the
            // cells worker for every visible pane); rows the matrix
            // doesn't yet cover render default-styled.
            // Perf plan B.2 slice B.2.a: active pane consumes the
            // worker's static-overlay bucket; inactive panes keep
            // the per-frame `push_range_quads` path (only
            // doc_highlight is painted there and N is small).
            worker_static_overlay_quads: if render_active {
                Some((*active_overlay_quads_guard).clone())
            } else {
                None
            },
            diff_tint_per_row,
            compilation_location_tint_per_row,
            // D.3.b.1.gpui (2026-05-29): snapshot the virtual-
            // row matrix from RenderState — the prepaint walk
            // interleaves Above- and Below-anchored virtual
            // rows around each visible doc line.
            //
            // K.4.6 c.ii (2026-06-02, FIXED 2026-06-02): per-pane
            // matrix lookup, NO fallback to the single-cell
            // virtual_rows.matrix. The fallback (originally
            // intended for transient races during pane teardown)
            // actually leaks the LAST active pane's writes into
            // panes that should have no virtual rows, because
            // virtual_rows.matrix shares Arc identity with the
            // active-pane-at-publish-time cell and is never
            // cleared on activate_document. TUI/GPUI parity per
            // [[feedback_tui_gpui_parity]].
            virtual_rows: rs_guard
                .virtual_rows
                .matrix_for_pane(pane.id)
                .map(|cell| cell.load_full())
                .unwrap_or_else(|| std::sync::Arc::new(lattice_cells::VirtualRowMatrix::empty())),
            scroll: pane_scroll,
            leftcol: pane_leftcol,
            viewport_height,
            gutter: gutter_meta,
            gutter_width,
            // PU.1b-1a (`signcolumn`): reserve the sign columns per the
            // active buffer's resolved option (mirrors how GPUI reads
            // `option_cache.foldenable`). Correct for the active pane;
            // inactive panes inherit the active value — the same
            // pre-existing per-pane-option limitation as the rest of
            // this element. TUI peer: `FrameView::sign_column`.
            sign_column: rs_guard.active_document.load().option_cache.sign_column,
            // PI.0: centring pad follows the *rendered* buffer's
            // `CenterContentWidth` local + this pane's width, not the
            // active-buffer identity — so the dashboard keeps its centring
            // even when a picker preview swaps `document_buffer_id` to the
            // previewed file. Shared resolver with the TUI peer.
            content_left_pad: rs_guard.content_left_pad_for(pane.buffer_id),
            show_line_numbers,
            cursor: cursor_state,
            is_active: render_active,
            visual_range,
            visual_block_extents,
            current_match,
            all_matches,
            substitute_matches,
            doc_highlights,
            cursorline_bg,
            cursorline_enabled,
            // T.4.b: resolve deletion-block backdrop colour from the
            // resolved table's `bg` channel so the paint pass doesn't
            // need to hold a Theme reference.
            diff_deletion_block_bg: resolved_theme
                .get(theme_ids.diff_deletion_block)
                .bg
                .map(|c| c.to_rgb_u32(0))
                .unwrap_or(0),
            inlay_hints,
            diagnostic_underlines,
            inlay_color,
            inline_diag_summary,
            // S4.1 (2026-05-27): active pane consumes the cell
            // matrix published by the cell-builder worker;
            // inactive panes pass `None` (mirrors `visible_rows`
            // — the cells worker only publishes for the active
            // document). The `prepaint` body branches use this
            // as the first try in a `cells → prepaint → legacy`
            // fallback chain; folded rows / boot frames / the
            // brief buffer-switch gap fall through to the
            // existing prepaint and legacy paths.
            // 2026-06-02 stale-matrix guard (parity with TUI): the
            // cells worker is async. After `apply_edit` publishes
            // a new snapshot, the worker rebuilds on a background
            // task. Until it finishes the matrix has cells
            // matching the PRE-edit content. Skip the matrix
            // when `version.text` lags `snapshot.text_version` so
            // EditorElement falls back to the legacy `shape_row`
            // path with `snapshot.buffer.line(...)` — user sees
            // the new char immediately, syntax styling catches
            // up next frame.
            // DR.2 (decoration-retention): read THIS pane's cell matrix
            // by id, for active and inactive alike. The active pane's
            // `pane_matrices` entry shares Arc identity with the
            // top-level `matrix` (dispatch.rs derives both from the same
            // per-buffer registry cell), so this is behaviour-preserving
            // for the active pane while giving inactive panes the same
            // retained, fully-styled matrix instead of the lesser span
            // fallback. `snapshot` is THIS pane's buffer, so the stale
            // guard is correct per pane. No focus-keyed branch
            // [[feedback_buffers_no_special_case]].
            cell_matrix: {
                let cells = rs_guard.cells.load();
                cells
                    .matrix_for_pane(pane.id)
                    .map(|cell| cell.load_full())
                    .filter(|m| m.version.text == snapshot.text_version)
            },
            // B3 (2026-06-04): the canonical DisplayMatrix is the GPU's
            // primary shaping source (cell_matrix above now feeds only the
            // experimental per-glyph paint_cells path). Same active-pane +
            // stale guard as cell_matrix; B2.3 rebuilds version.text
            // synchronously in the publish tail, so on a single-keystroke
            // edit it is already current and the guard does NOT fire — that
            // retires the GPU whole-viewport flicker. The guard still fires
            // for publishes the sync path skips (multi-edit, doc switch),
            // where EditorElement falls back to the legacy shape_row path.
            // DR.2 (decoration-retention): per-pane display matrix, same
            // rationale as `cell_matrix`. The canonical DisplayMatrix is
            // the GPU's primary shaping source; reading it per-pane is
            // what makes inactive panes paint full syntax through the
            // shared path. Same per-pane stale guard.
            display_matrix: {
                let cells = rs_guard.cells.load();
                cells
                    .display_matrix_for_pane(pane.id)
                    .map(|cell| cell.load_full())
                    .filter(|m| m.version.text == snapshot.text_version)
            },
            // T.5.b: the resolved table + builtin ids the display-line
            // path resolves syntax styles through (`resolve_syntax_style`),
            // replacing the `host_theme.syntax_style` read. Reuses the
            // T.4 locals bound from `rs_guard` above.
            resolved_theme: resolved_theme.clone(),
            theme_ids,
            // S4.final.b (2026-05-27): per-window glyph-id
            // cache. Always carries the shared resolver from
            // `EditorView`; consumption is gated on
            // `paint_cells_enabled()` in `EditorElement::paint`.
            // Sharing across panes means a buffer-switch keeps
            // the cache warm.
            glyph_resolver: self.glyph_resolver.clone(),
        };

        let status_row = Self::modeline_row(pane, render_active, &rs_guard);
        Self::pane_chrome(
            editor_element.into_any_element(),
            status_row,
            render_active,
            inactive_pane_opacity(&self.app),
        )
    }

    /// Build the inner content of an **oil** pane (flat editable directory
    /// listing). Parity with the TUI peer's `draw_oil_pane`: one row per
    /// visible entry, a file-type icon glyph prepended to the bare name,
    /// the cursor row highlighted block-style. Oil content + the
    /// `(name, is_dir)` pairs come straight from the host registry's
    /// `with_oil` accessor (no oil type dep needed); the icon glyph +
    /// colour resolve through the shared `entry_visual` so TUI/GPUI agree.
    ///
    /// Like every other kind, the caller wraps this via
    /// [`Self::pane_chrome`] so the listing can never paint past the
    /// modeline [[feedback_buffers_no_special_case]]. Rows are bounded to
    /// the pane's `viewport_height` so paint stays O(viewport), not
    /// O(directory-size) (paramount goal #1).
    fn build_oil_inner(
        &self,
        pane: &PaneState,
        rs_guard: &lattice_host::render_state::RenderState,
        theme: &GpuiTheme,
        is_active: bool,
    ) -> AnyElement {
        let oil = rs_guard.buffers.registry.with_oil(pane.buffer_id, |o| {
            (
                o.content.as_string(),
                o.snapshot_entries()
                    .iter()
                    .map(|e| (e.name.clone(), e.is_dir))
                    .collect::<Vec<(String, bool)>>(),
            )
        });
        let Some((raw_text, entries)) = oil else {
            return div()
                .bg(rgb(theme.background))
                .text_color(rgb(theme.foreground))
                .child(format!("(oil buffer {:?} unavailable)", pane.buffer_id))
                .into_any_element();
        };
        let (cursor_line, scroll) = if is_active {
            let ad = rs_guard.active_document.load();
            (ad.cursor.line as usize, ad.scroll as usize)
        } else {
            (pane.cursor.line as usize, pane.scroll as usize)
        };
        let nerd_fonts = rs_guard
            .options
            .config
            .get_typed::<lattice_host::ui::theme_options::UiNerdFonts>()
            .map(|v| *v)
            .unwrap_or(false);
        let viewport = if pane.viewport_height > 0 {
            pane.viewport_height as usize
        } else {
            VIEWPORT_ROWS_FALLBACK
        };
        let rows: Vec<gpui::Div> = raw_text
            .split('\n')
            .enumerate()
            .skip(scroll)
            .take(viewport)
            .map(|(i, name_str)| {
                let line_idx = scroll + i;
                let is_cursor = is_active && line_idx == cursor_line;
                // `entry_visual` only inspects `file_name()` / extension, so a
                // bare relative name resolves the same icon as a full path —
                // no need to join the OilDir.
                let (name, is_dir) = entries
                    .get(line_idx)
                    .cloned()
                    .unwrap_or_else(|| (name_str.to_string(), false));
                let (glyph, icol) = lattice_core::ui::icons::entry_visual(
                    std::path::Path::new(&name),
                    is_dir,
                    nerd_fonts,
                );
                let is_hidden = name.starts_with('.');
                let fg = if is_cursor {
                    theme.cursor_foreground
                } else {
                    entry_fg(rs_guard, is_dir, is_hidden, icol, theme.foreground)
                };
                let mut row = div()
                    .text_color(rgb(fg))
                    .child(format!("{glyph}{name_str}"));
                if is_cursor {
                    row = row.bg(rgb(theme.cursor_background));
                }
                row
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .bg(rgb(theme.background))
            .children(rows)
            .into_any_element()
    }

    /// Build the inner content of a **file-tree** pane. Parity with the
    /// TUI peer's `draw_file_tree_pane`: the rope content already carries
    /// the indentation + `▾`/`▸` expansion markers, so each visible line
    /// is rendered verbatim and only *colour-tinted* per entry (the icon
    /// glyph is not prepended — the tree markers already convey kind). The
    /// per-entry path/kind comes from the `FileTreeEntries` buffer-local
    /// (host-populated); the colour resolves through the shared
    /// `entry_visual`. Bounded to `viewport_height` like the oil builder.
    fn build_file_tree_inner(
        &self,
        pane: &PaneState,
        rs_guard: &lattice_host::render_state::RenderState,
        theme: &GpuiTheme,
        is_active: bool,
    ) -> AnyElement {
        let raw_text = rs_guard
            .buffers
            .registry
            .with_file_tree(pane.buffer_id, |t| t.content.as_string());
        let Some(raw_text) = raw_text else {
            return div()
                .bg(rgb(theme.background))
                .text_color(rgb(theme.foreground))
                .child(format!(
                    "(file-tree buffer {:?} unavailable)",
                    pane.buffer_id
                ))
                .into_any_element();
        };
        let entries: Vec<lattice_file_tree::FileTreeEntry> = rs_guard
            .buffer_locals
            .map
            .get(&pane.buffer_id)
            .and_then(|locals| locals.get::<lattice_file_tree::modes::FileTreeEntries>())
            .map(|e| e.0.clone())
            .unwrap_or_default();
        let (cursor_line, scroll) = if is_active {
            let ad = rs_guard.active_document.load();
            (ad.cursor.line as usize, ad.scroll as usize)
        } else {
            (pane.cursor.line as usize, pane.scroll as usize)
        };
        let nerd_fonts = rs_guard
            .options
            .config
            .get_typed::<lattice_host::ui::theme_options::UiNerdFonts>()
            .map(|v| *v)
            .unwrap_or(false);
        let viewport = if pane.viewport_height > 0 {
            pane.viewport_height as usize
        } else {
            VIEWPORT_ROWS_FALLBACK
        };
        let rows: Vec<gpui::Div> = raw_text
            .split('\n')
            .enumerate()
            .zip(entries.iter())
            .skip(scroll)
            .take(viewport)
            .map(|((i, raw_line), entry)| {
                let line_idx = scroll + i;
                let is_cursor = is_active && line_idx == cursor_line;
                let is_dir = matches!(
                    entry.kind,
                    lattice_file_tree::FileTreeEntryKind::Directory { .. }
                );
                let (_glyph, icol) =
                    lattice_core::ui::icons::entry_visual(&entry.path, is_dir, nerd_fonts);
                let is_hidden = entry
                    .path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with('.'));
                let fg = if is_cursor {
                    theme.cursor_foreground
                } else {
                    entry_fg(rs_guard, is_dir, is_hidden, icol, theme.foreground)
                };
                let mut row = div().text_color(rgb(fg)).child(raw_line.to_string());
                if is_cursor {
                    row = row.bg(rgb(theme.cursor_background));
                }
                row
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .bg(rgb(theme.background))
            .children(rows)
            .into_any_element()
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
        pane_rows: u32,
    ) -> AnyElement {
        // ML.2: returns the inner content only; the per-pane modeline row
        // is built uniformly by `Self::modeline_row` at the call site (no
        // kind-specific status, `feedback_buffers_no_special_case`).
        let snap_opt = rs_guard
            .buffers
            .registry
            .with_terminal(pane.buffer_id, |t| {
                (
                    t.snapshot.load_full(),
                    t.current_match,
                    t.visual,
                    t.all_matches.clone(),
                    t.nav_cursor,
                )
            });
        let Some((snap, current_match, mut visual, all_matches, mut nav_cursor)) = snap_opt else {
            return div()
                .bg(rgb(theme.background))
                .text_color(rgb(theme.foreground))
                .child(format!("(terminal #{} unavailable)", pane.buffer_id.0))
                .into_any_element();
        };
        // T-clean-1 Phase A.2 (2026-05-28): active pane reads
        // cursor + visual from the publisher's derived render-
        // state fields (computed from doc-space `self.cursor`
        // + `synthetic.origin_top_line`). Inactive panes keep
        // their `with_terminal` reads — those carry the
        // last-known cell coords from when the pane was last
        // active, which is the intentional cross-pane
        // preservation behaviour.
        if is_active {
            if rs_guard
                .active_document
                .load()
                .terminal_nav_cursor
                .is_some()
            {
                nav_cursor = rs_guard.active_document.load().terminal_nav_cursor;
            }
            if rs_guard.active_document.load().terminal_visual.is_some() {
                visual = rs_guard.active_document.load().terminal_visual;
            }
        }
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
        // Used only by the seq==0 placeholder below; the per-pane status
        // bar (terminal name + position) is built by `Self::modeline_row`
        // via the shared resolver (`core.path` reads this same name slot).
        let name_label = rs_guard
            .buffers
            .registry
            .name_of(pane.buffer_id)
            .unwrap_or_else(|| format!("[terminal #{}]", pane.buffer_id.0));
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
            return placeholder;
        }
        // T2 substrate swap (2026-05-25): per-cell SGR colors from
        // alacritty's grid. The 16-colour ANSI palette is sourced from the
        // active theme's `terminal.ansi.*` roles (each maps to a palette
        // accent), so a `:colorscheme` swap recolours the terminal and the
        // colours are readable on dark backgrounds — replacing the old
        // hardcoded dim-VGA xterm palette (`0xcd0000` / `0x0000ee` …).
        use lattice_terminal::{CellAttrs, NamedColor as TermNamed, TerminalColor};
        let default_fg = theme.foreground;
        let default_bg = theme.background;
        let ansi_palette: [u32; 16] = std::array::from_fn(|i| {
            rs_guard
                .resolved_theme
                .get(rs_guard.theme_ids.terminal_ansi[i])
                .fg
                .map(|c| c.to_rgb_u32(default_fg))
                .unwrap_or(default_fg)
        });
        // Map `TerminalColor::Indexed(16..=255)` (the xterm
        // 256-colour palette beyond the 16 named entries) to its
        // RGB approximation per the xterm spec: indices 16..=231
        // form a 6×6×6 cube; 232..=255 a 24-step grayscale ramp.
        // Indices 0..=15 fall back to the themed `ansi` palette.
        fn indexed_to_rgb(i: u8, ansi: &[u32; 16]) -> u32 {
            if (i as usize) < ansi.len() {
                return ansi[i as usize];
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
                    if is_fg {
                        default_fg
                    } else {
                        default_bg
                    }
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
                    ansi_palette[idx]
                }
                TerminalColor::Indexed(i) => indexed_to_rgb(i, &ansi_palette),
                TerminalColor::Rgb(r, g, b) => ((r as u32) << 16) | ((g as u32) << 8) | (b as u32),
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
            cursor: bool,    // true = cursor cell (forces its own run)
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
        // Never paint more rows than the pane allocates. The terminal grid is
        // sized by the PTY, NOT the pane, so a pane shorter than the grid (the
        // classic case: a HORIZONTAL split halves the height while the grid
        // still has its full row count) would otherwise paint `flex_shrink_0`
        // rows past `pane_chrome`'s clip and shove the per-pane modeline — then
        // the global cmdline + sibling panes — off-screen. Bound to the pane's
        // published row budget and show the BOTTOM rows (recent output + the
        // cursor), the same O(viewport) discipline oil / file-tree use. A
        // vertical split keeps full height, so this is a no-op there.
        let row_start = terminal_row_start(snap.rows, pane_rows);
        let mut rows: Vec<gpui::Div> = Vec::with_capacity((snap.rows - row_start) as usize);
        for r in row_start..snap.rows {
            // 2026-05-27: lock each terminal row to the editor's
            // row_px metric (font_size × 1.3). Without this, default
            // GPUI text rendering used a larger line-height (~20px
            // for text-sm) per row; `snap.rows × 20px` exceeded the
            // pane's allocated height and pushed the modeline /
            // cmdline siblings off-screen when terminal was one of
            // a vsplit pair. `.flex_shrink_0()` prevents flex from
            // squishing the row below `row_px`.
            let mut row_div = div().flex().flex_row().h(px(row_px)).flex_shrink_0();
            let mut run_text = String::with_capacity(snap.cols as usize);
            let mut run_style: Option<CellStyle> = None;
            let flush =
                |row_div: gpui::Div, text: &mut String, style: Option<CellStyle>| -> gpui::Div {
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
                // Width contract: a wide glyph owns its two display columns via
                // GPUI's own shaping; its trailing `wide_spacer` cell must NOT
                // be emitted as a stray space (that pushes the row one column
                // wide per glyph). Skip it so grid col stays 1:1 with display
                // col. See docs/dev/audit/terminal-wide-char-ghosting.md.
                if cell.wide_spacer {
                    continue;
                }
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
                        let c_end = h.column.saturating_add(h.len.min(u16::MAX as u32) as u16);
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
                                cell_line >= lo && cell_line <= hi && c >= lo_c && c <= hi_c
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
        col.into_any_element()
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
        let (popup_w_px, popup_h_px) =
            popup_outer_dims_px(f32::from(viewport_px.width), f32::from(viewport_px.height));
        let popup_inner_rows = popup_inner_height_rows(popup_h_px, rem, estimated_row_px);
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
        let popup_body_h_px = popup_body_h_px(popup_h_px, rem, estimated_row_px);
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
        // 2026-07-02 (regression #3 of the same class as Issue #17 and the
        // popup fix, d7c5f450): see `default_ui_row_px`'s doc — the
        // modeline/status row and the global cmdline row resolve to GPUI's
        // default line height (phi), not `estimated_row_px`'s 1.3x.
        let default_row_px: f32 = default_ui_row_px(window.rem_size());
        let pane_status_row_px = default_row_px; // status text line
        let global_bottom_padding_px = rem * 0.25 * 2.0; // .py_1() = 0.5rem
        let global_bottom_row_px = default_row_px; // cmdline-only content (Option-A: modal moved to per-pane)
        let per_leaf_v_chrome_px = pane_padding_v_px + pane_status_padding_px + pane_status_row_px;
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
        let ligatures_enabled = self.app.theme.ligatures;
        let glyph_advance_px = {
            let mut ref_font = font(font_family_for_advance);
            if !ligatures_enabled {
                ref_font.features = FontFeatures::disable_ligatures();
            }
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
        let strip_rows_px =
            (picker_strip_rows + cmdline_completion_strip_rows) as f32 * estimated_row_px;
        // Issue #29 (2026-05-22): tabline claims one row at the
        // top when visible — subtract from available so per-pane
        // geometries see the correct buffer height.
        let tabline_visible = self.app.render_state.load().tabs.visible;
        let tabline_h_px = if tabline_visible {
            estimated_row_px
        } else {
            0.0
        };
        let avail_h_px =
            (f32::from(viewport_px.height) - global_chrome_v_px - strip_rows_px - tabline_h_px)
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
        let popup_focused = rs_for_popup.active_document.load().popup_focused;
        let target_height = if popup_focused {
            popup_inner_rows
        } else {
            rs_for_popup.panes.tree.active().viewport_height.max(1)
        };
        if rs_for_popup.active_document.load().viewport_height != target_height {
            drop(rs_for_popup);
            self.app.set_viewport_height(target_height);
        }
        // PU.2: floating-popup inner-geometry hand-off — the GPUI peer of
        // the TUI runtime's `popup_feedback_inner_dims` → `set_popup_viewport`.
        // The renderer is the sizing authority, so it pushes the popup's
        // resolved inner `(rows, cols)` to the host; `build_cells_panes`
        // reads `popup_viewport_{height,width}` to build the synthetic
        // `PaneId::POPUP` matrix the interior `EditorElement` paints from.
        // Gated identically to the TUI: a floating popup is open AND help is
        // NOT an in-pane leaf (that case is a real pane the loop above already
        // covers). `popup_inner_rows` / `popup_inner_cols` are the SAME inner
        // dims the chrome locks the body to (`popup_body_h_px`, `inner_cols`),
        // so the matrix width and the painted body agree. Diff-then-send via
        // `self.last_popup_dims` keeps a steady-state popup at zero RPCs and
        // pushes once on dismiss (the gate flips to `None`, but we only send
        // on `Some` — the synthetic pane simply stops being built host-side).
        let popup_dims = {
            let rs = self.app.render_state.load();
            let in_pane_help = matches!(
                rs.panes.tree.active().buffer,
                lattice_core::BufferKind::Help
            );
            if rs.popup.is_open() && !in_pane_help {
                Some((
                    popup_inner_rows,
                    popup_inner_cols(popup_w_px, rem, glyph_advance_px),
                ))
            } else {
                None
            }
        };
        if self.last_popup_dims != popup_dims {
            if let Some((rows, cols)) = popup_dims {
                self.app.set_popup_viewport(rows, cols);
            }
            self.last_popup_dims = popup_dims;
        }
        // PU.5d: completion-docs popup geometry hand-off (peer of the
        // floating-popup feedback above). Shown when completion is open with
        // a resolved non-empty doc body AND the host has created the
        // ephemeral docs buffer. GPUI's candidate popup is a fixed top-right
        // box, so the docs popup is a fixed-width box to its left; its inner
        // cols drive the `PaneId::COMPLETION_DOCS` matrix wrap, rows the
        // viewport (grown to the body, capped). Diff-then-send.
        let completion_docs_dims = {
            let rs = self.app.render_state.load();
            let comp = &rs.completion;
            let body_lines = comp
                .insert
                .as_deref()
                .and_then(|ic| ic.doc_popup.as_ref())
                .and_then(|d| d.body.as_ref())
                .filter(|b| !b.is_empty())
                .map(|b| b.lines().count().max(1) as u32);
            match (body_lines, comp.docs_buffer_id) {
                (Some(lines), Some(_)) => Some((
                    lines.min(COMPLETION_DOCS_MAX_ROWS).max(1),
                    popup_inner_cols(COMPLETION_DOCS_W_PX, rem, glyph_advance_px),
                )),
                _ => None,
            }
        };
        if self.last_completion_docs_dims != completion_docs_dims {
            if let Some((rows, cols)) = completion_docs_dims {
                self.app.set_completion_docs_viewport(rows, cols);
            }
            self.last_completion_docs_dims = completion_docs_dims;
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
        let pre_ad = self.app.render_state.load().active_document.load_full();
        let cursor_key = (
            pre_ad.cursor,
            pre_ad.scroll,
            pre_ad.viewport_height,
            pre_ad.buffer_kind,
        );
        if self.ensure_gate.cursor_snap_key != Some(cursor_key) {
            self.app.ensure_cursor_in_viewport();
            let post_ad = self.app.render_state.load().active_document.load_full();
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
            let cursor = rs_probe.active_document.load().cursor;
            let scroll = rs_probe.active_document.load().scroll;
            let vh = rs_probe.active_document.load().viewport_height;
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
                active_buffer_kind = ?rs_probe.active_document.load().buffer_kind,
                "viewport-invariant probe"
            );
        }
        #[cfg(feature = "profile-frames")]
        let after_ensure = std::time::Instant::now();
        // Phase 5.8.AF.5 / Slice X2.5: the per-frame
        // `self.app.refresh_highlights()` call has been removed.
        // display-line B-series: active-pane syntax colour is now
        // produced by the cells worker into the `DisplayMatrix`
        // substrate; overlay backgrounds by the
        // `lattice_host::overlay_worker` (woken via
        // `Editor::overlay_wake`). `paint_pane` reads the matrix via
        // `rs_guard.cells.display_matrix`; B4.2 deleted the old
        // worker span cell (`visible_spans`). Pre-X2 cost: ~178µs at
        // 80 lines per frame; now zero UI-thread parse cost. Goal #1
        // violation B1 closed for the GPUI peer.
        // DR.2 (decoration-retention): the per-frame
        // `RefreshPaneHighlights` dispatch is gone. Inactive panes now
        // read their own retained per-pane `DisplayMatrix` (built by the
        // cells worker for every visible pane), so there is nothing to
        // re-slice on focus change — the redundant `pane_highlights`
        // producer is retired. Removing this dispatch is the perf half
        // of DR.2: zero decoration recompute on a pure focus toggle.
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

        drop(ad);
        // 5.8.C / 5.8.H: bottom global row. In Command/Search modes
        // it shows the in-progress `:cmd` / `/pattern` minibuffer;
        // otherwise it is empty — the modal indicator moved to each
        // pane's own status footer (Option-A modeline overhaul, MO.4.b).
        // Slice 3c.final.B.7: cmdline + search-line via published
        // `modeline()` sub-state — wait-free Arc clones.
        let modeline = self.app.modeline();
        let messages = self.app.messages();
        // Bottom global row: the in-progress `:`/`/` minibuffer while
        // typing, otherwise the last echo message — a `:set foo?` value,
        // a command error, or the `-- INSERT --` showmode (ML.5d). This
        // is vim's shared bottom line; GPUI previously left it blank
        // outside Command/Search, so echoes the TUI peer shows were
        // invisible here. `bottom_echo_level` colours errors / warnings.
        let (bottom_row, bottom_echo_level) = bottom_row_content(modal, &modeline, &messages);
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
        // T.6: the resolved theme table + ids for completion-annotation
        // base colors (`paint_candidate_row` → `annotation_color_rgb`).
        let resolved_theme = render_state.resolved_theme.clone();
        let theme_ids = render_state.theme_ids;
        let active_idx = render_state.panes.tree.active_index();
        // THIS frame's freshly-computed per-pane row budget, keyed by leaf
        // index. `pane.viewport_height` (published) lags by a frame because
        // `set_pane_viewport` rides the actor — so a terminal pane reading it
        // during paint would size against the PRE-split height and never
        // converge. The fresh map bounds the terminal to its current pane.
        let pane_row_map: std::collections::HashMap<usize, u32> = pane_geometries
            .iter()
            .map(|(idx, rows, _)| (*idx, *rows))
            .collect();
        let document_area = self
            .paint_pane_tree(
                render_state.panes.tree.root(),
                &theme,
                active_idx,
                estimated_row_px,
                &pane_row_map,
            )
            .flex_grow()
            // Defensive: never let pane content (a terminal's flex_shrink_0
            // rows) force the document area to grow and push the global
            // cmdline off-screen.
            .min_h(px(0.0));
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
                // MARG.5: per-category column layout across the
                // visible candidate set — each row's annotation
                // cells render against this so columns align
                // vertically even when some rows have keybindings
                // and others don't.
                let columns = lattice_completion::AnnotationColumns::from_visible(
                    ic.rendered[window_start..window_end].iter(),
                );
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
                            &columns,
                            &resolved_theme,
                            &theme_ids,
                        )
                    })
                    .collect();
                // 2026-05-27: filter-chord footer mirrors the
                // TUI peer. Width budget approximated from the
                // popup max_w (360px ≈ 45 cells at 8px/char).
                // Adaption: full form → compact `[b]uf` → prune.
                // Also surface the active filter when set.
                let approx_cols: u16 = 45;
                let footer_text = if let Some(active) = ic.source_filter.as_ref() {
                    let label = gpui_source_display_label(active.as_str());
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
                    .child(div().text_color(rgb(theme.popup_border)).child(footer_text))
            });

        // PU.5d: completion-docs side popup. GPUI had no docs popup before
        // this slice; it now renders the focused candidate's documentation
        // through the SAME `EditorElement` + `PaneId::COMPLETION_DOCS` matrix
        // as the floating popup (markdown colour + link styling + soft-wrap),
        // in a fixed-width box placed to the left of the top-right candidate
        // popup. Shown only once the host created the ephemeral docs buffer
        // (`reconcile_completion_docs_buffer`) and fed back geometry; one
        // un-sized frame falls back to the matrix-less plain text (eventual
        // consistency, same as the floating popup).
        let docs_resolved = self.app.render_state.load().resolved_theme.clone();
        let docs_ids = self.app.render_state.load().theme_ids;
        let completion_docs_overlay: Option<gpui::Div> = {
            let rs = self.app.render_state.load();
            let comp = rs.completion.clone();
            let doc_popup = comp.insert.as_deref().and_then(|ic| ic.doc_popup.as_ref());
            let body_present = doc_popup
                .and_then(|d| d.body.as_ref())
                .is_some_and(|b| !b.is_empty());
            match (comp.docs_buffer_id, doc_popup) {
                (Some(docs_id), Some(doc_popup)) if body_present => {
                    let scroll = doc_popup.scroll as u32;
                    let content_snap = rs
                        .buffers
                        .registry
                        .document_handle(docs_id)
                        .map(|h| h.snapshot());
                    let text_version = content_snap.as_ref().map(|s| s.text_version).unwrap_or(0);
                    let body_string = content_snap
                        .as_ref()
                        .map(|s| s.buffer.as_string())
                        .unwrap_or_default();
                    let inner_rows = (body_string.split('\n').count().max(1) as u32)
                        .min(COMPLETION_DOCS_MAX_ROWS);
                    // Visible window from `scroll` (the gutter-less walk
                    // indexes `text` by `line_idx - scroll`).
                    let window_text: String = body_string
                        .split('\n')
                        .skip(scroll as usize)
                        .collect::<Vec<_>>()
                        .join("\n");
                    let cells = rs.cells.load();
                    let pane = lattice_core::ui::pane::PaneId::COMPLETION_DOCS;
                    let display_matrix = cells
                        .display_matrix_for_pane(pane)
                        .map(|c| c.load_full())
                        .filter(|m| m.version.text == text_version);
                    let cell_matrix = cells
                        .matrix_for_pane(pane)
                        .map(|c| c.load_full())
                        .filter(|m| m.version.text == text_version);
                    let editor_element = crate::editor_element::EditorElement {
                        // `usize::MAX - 1`: distinct ElementId from the
                        // floating popup (`usize::MAX`) and every real pane.
                        pane_idx: usize::MAX - 1,
                        theme: theme.clone(),
                        text: std::sync::Arc::new(window_text),
                        scroll,
                        leftcol: 0,
                        viewport_height: inner_rows,
                        // help-flavoured ephemeral buffer ⇒ nonu /
                        // signcolumn=no ⇒ empty gutter (text-only walk).
                        gutter: Vec::new(),
                        gutter_width: 0,
                        content_left_pad: 0,
                        show_line_numbers: false,
                        sign_column: false,
                        // Docs popup is never focused — no cursor / overlays.
                        cursor: None,
                        is_active: false,
                        visual_range: None,
                        visual_block_extents: None,
                        current_match: None,
                        all_matches: Vec::new(),
                        substitute_matches: Vec::new(),
                        doc_highlights: Vec::new(),
                        worker_static_overlay_quads: None,
                        virtual_rows: std::sync::Arc::new(lattice_cells::VirtualRowMatrix::empty()),
                        diff_tint_per_row: Vec::new(),
                        compilation_location_tint_per_row: Vec::new(),
                        cursorline_bg: 0,
                        cursorline_enabled: false,
                        diff_deletion_block_bg: 0,
                        inlay_hints: Vec::new(),
                        diagnostic_underlines: Vec::new(),
                        inlay_color: 0,
                        inline_diag_summary: None,
                        cell_matrix,
                        display_matrix,
                        resolved_theme: docs_resolved.clone(),
                        theme_ids: docs_ids,
                        glyph_resolver: self.glyph_resolver.clone(),
                    };
                    let docs_body_h_px = inner_rows as f32 * estimated_row_px;
                    let docs_h_px = popup_chrome_v_px(rem, estimated_row_px) + docs_body_h_px;
                    Some(
                        div()
                            .flex()
                            .flex_col()
                            .min_w(px(COMPLETION_DOCS_W_PX))
                            .max_w(px(COMPLETION_DOCS_W_PX))
                            .min_h(px(docs_h_px))
                            .max_h(px(docs_h_px))
                            .overflow_hidden()
                            .p_4()
                            .bg(rgb(theme.popup_background))
                            .text_color(rgb(theme.foreground))
                            .border_2()
                            .border_color(rgb(theme.popup_border))
                            .child(
                                // Same header treatment as the main popup:
                                // bold, larger title; no `───` separator.
                                div().flex().flex_row().pb_2().child(
                                    div()
                                        .h(px(estimated_row_px * POPUP_TITLE_SCALE))
                                        .flex_shrink_0()
                                        .text_size(px(font_size_px * POPUP_TITLE_SCALE))
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .text_color(rgb(theme.popup_title))
                                        .child(" docs ".to_string()),
                                ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .min_h(px(docs_body_h_px))
                                    .max_h(px(docs_body_h_px))
                                    .overflow_hidden()
                                    .child(editor_element.into_any_element()),
                            ),
                    )
                }
                _ => None,
            }
        };

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
        let picker_overlay: Option<gpui::Div> =
            (!picker_use_minibuffer)
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
                    // MARG.5: per-category column layout — see the
                    // first call site upstream for full rationale.
                    let columns = lattice_completion::AnnotationColumns::from_visible(
                        picker.candidates[window_start..window_end].iter(),
                    );
                    let visible_candidates: Vec<gpui::Div> = picker.candidates
                        [window_start..window_end]
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
                                &columns,
                                &resolved_theme,
                                &theme_ids,
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
                                    " {} ({} / {}){} ",
                                    picker.title,
                                    if total == 0 { 0 } else { picker.selected + 1 },
                                    total,
                                    if picker.loading { " searching…" } else { "" },
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
                        .child(div().pt_2().text_color(rgb(theme.popup_border)).child(
                            "[ <C-n>/<C-p> navigate · <CR> accept · <Esc> cancel ]".to_string(),
                        ))
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
                    vec![
                        div()
                            .px_2()
                            .text_color(rgb(theme.popup_border))
                            .child("  (no matches)".to_string()),
                    ]
                } else {
                    let display_col_chars = picker.candidates[scroll..window_end]
                        .iter()
                        .map(|c| c.raw.display.chars().count())
                        .max()
                        .unwrap_or(0);
                    // MARG.5: per-category column layout — see the
                    // first call site upstream for full rationale.
                    let columns = lattice_completion::AnnotationColumns::from_visible(
                        picker.candidates[scroll..window_end].iter(),
                    );
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
                                &columns,
                                &resolved_theme,
                                &theme_ids,
                            )
                        })
                        .collect()
                };

                let count = format!(
                    "  ({} / {}){}",
                    if total == 0 { 0 } else { picker.selected + 1 },
                    total,
                    if picker.loading { " searching…" } else { "" },
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
                    .child(div().text_color(rgb(theme.popup_border)).child(count));

                div().flex().flex_col().child(prompt_row).child(
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
                // MARG.5: per-category column layout — see the
                // first call site upstream for full rationale.
                let columns = lattice_completion::AnnotationColumns::from_visible(
                    state.candidates[scroll..window_end].iter(),
                );
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
                            &columns,
                            &resolved_theme,
                            &theme_ids,
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
                // MARG.5: per-category column layout — see the
                // first call site upstream for full rationale.
                let columns = lattice_completion::AnnotationColumns::from_visible(
                    state.candidates[window_start..window_end].iter(),
                );
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
                            &columns,
                            &resolved_theme,
                            &theme_ids,
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

        // Phase 5.8.AE + Slice 3c.final.B (group 3): read popup state via
        // the published substate. PU.1b-4b: the popup is gated on
        // `popup_substate.buffer_id`; its CONTENT / TITLE / line-count come
        // from the registry Document at that id (single source), and its
        // State-A scroll from `popup_substate.scroll`. No popup-side
        // `HelpBuffer` snapshot is published anymore. (Help syntax / link
        // styling rides the live cells-worker `DisplayMatrix`.)
        let popup_substate = self.app.render_state.load().popup.clone();
        // T.5.b: the popup-overlay cell renderer resolves syntax
        // styles through the active theme's resolved table (loaded
        // once; captured by the popup closure below).
        let popup_resolved = self.app.render_state.load().resolved_theme.clone();
        let popup_ids = self.app.render_state.load().theme_ids;
        let popup_overlay: Option<gpui::Div> = popup_substate.buffer_id.map(|popup_id| {
            // PU.2: the popup CONTENT renders through the shared document
            // `EditorElement` reading the synthetic `PaneId::POPUP`
            // `DisplayMatrix` (markdown colour from PU.1b-1, link styling
            // from the PU.1b-2a `ExtraHighlights` merge, soft-wrap from the
            // matrix `wrap_width`, h-scroll / folds for free) — the GPUI peer
            // of the TUI's `draw_help_overlay` compose flip (PU.1b-3). Only
            // the box (border + title + separator) below stays
            // popup-specific chrome. A help popup is now pixel-equivalent to
            // a `:set nonu signcolumn=no wrap` document in a box (K.4 /
            // `feedback_render_is_option_derived`).
            //
            // PU.1b-4b: title + content + line-count are sourced from the
            // registry Document at `popup_id` (single source); State-A
            // scroll from the published `popup_substate.scroll`. No
            // popup-side `HelpBuffer` snapshot.
            let ad = self.app.ad();
            let popup_focused = ad.popup_focused;
            let rs = self.app.render_state.load();
            // Title = the popup buffer's registry name.
            let title = rs.buffers.registry.name_of(popup_id).unwrap_or_default();
            // Snapshot from the registry handle: the popup is a registry
            // Document never `activate_document`'d as `self.document`
            // (PU.1a), so its content must come from the handle — the same
            // source the host builds the POPUP matrix from
            // (`build_one_pane_cells_input`, `is_active_buffer=false`).
            let content_snap = rs
                .buffers
                .registry
                .document_handle(popup_id)
                .map(|h| h.snapshot());
            let text_version = content_snap.as_ref().map(|s| s.text_version).unwrap_or(0);
            let body_string = content_snap
                .as_ref()
                .map(|s| s.buffer.as_string())
                .unwrap_or_default();
            // Scroll anchor by focus state — the SAME choice the host makes
            // when building the POPUP matrix (`build_cells_panes`): State B →
            // live `ad.scroll`; State A → the published popup view scroll.
            // Matching it keeps the matrix rows and the painted window aligned.
            let popup_scroll: u32 = if popup_focused {
                ad.scroll
            } else {
                popup_substate.scroll
            };
            // `EditorElement.text` carries the VISIBLE WINDOW from
            // `scroll` (the gutter-less walk indexes it by
            // `line_idx - scroll`); matrix lookups stay keyed by absolute
            // source line, so colour stays aligned with the text.
            let window_text: String = body_string
                .split('\n')
                .skip(popup_scroll as usize)
                .collect::<Vec<_>>()
                .join("\n");
            // State B paints a block cursor inside the popup; State A
            // none (focus is on the doc beneath). `line_text` comes from
            // the content snapshot (the element's windowed `text` can't
            // recover an arbitrary cursor line — same contract as
            // `paint_pane`).
            let cursor = if popup_focused {
                let line_text = content_snap
                    .as_ref()
                    .and_then(|s| s.buffer.line(ad.cursor.line))
                    .unwrap_or_default();
                Some(crate::editor_element::CursorState {
                    line: ad.cursor.line,
                    byte: ad.cursor.byte,
                    shape: CursorShape::for_mode(ad.modal),
                    line_text,
                })
            } else {
                None
            };
            // The synthetic `PaneId::POPUP` matrices the cells worker
            // built off the geometry fed at the top of `render`
            // (`set_popup_viewport`). Same `version.text` stale guard as
            // every pane (`paint_pane`): a lagging matrix falls back to
            // default-styled windowed text for a frame, colour catches up.
            let cells = rs.cells.load();
            let popup_pane = lattice_core::ui::pane::PaneId::POPUP;
            let display_matrix = cells
                .display_matrix_for_pane(popup_pane)
                .map(|c| c.load_full())
                .filter(|m| m.version.text == text_version);
            let cell_matrix = cells
                .matrix_for_pane(popup_pane)
                .map(|c| c.load_full())
                .filter(|m| m.version.text == text_version);
            let editor_element = crate::editor_element::EditorElement {
                // `usize::MAX`: a stable `ElementId` distinct from every
                // real pane index (0, 1, 2, …) so GPUI tracks the popup
                // element across frames without colliding.
                pane_idx: usize::MAX,
                theme: theme.clone(),
                text: std::sync::Arc::new(window_text),
                scroll: popup_scroll,
                // Help wraps; the host pins `leftcol = 0` under wrap.
                leftcol: 0,
                viewport_height: popup_inner_rows,
                // Empty gutter → the text-only walk. Help-mode is `nonu`
                // + `signcolumn=no`, so there is no gutter to paint.
                gutter: Vec::new(),
                gutter_width: 0,
                content_left_pad: 0,
                show_line_numbers: false,
                sign_column: false,
                cursor,
                is_active: popup_focused,
                // A help overlay carries no selection / search / diff /
                // inlay / diagnostic decoration — all empty / None.
                visual_range: None,
                visual_block_extents: None,
                current_match: None,
                all_matches: Vec::new(),
                substitute_matches: Vec::new(),
                doc_highlights: Vec::new(),
                worker_static_overlay_quads: None,
                virtual_rows: std::sync::Arc::new(lattice_cells::VirtualRowMatrix::empty()),
                diff_tint_per_row: Vec::new(),
                compilation_location_tint_per_row: Vec::new(),
                cursorline_bg: 0,
                cursorline_enabled: false,
                diff_deletion_block_bg: 0,
                inlay_hints: Vec::new(),
                diagnostic_underlines: Vec::new(),
                inlay_color: 0,
                inline_diag_summary: None,
                cell_matrix,
                display_matrix,
                resolved_theme: popup_resolved.clone(),
                theme_ids: popup_ids,
                glyph_resolver: self.glyph_resolver.clone(),
            };

            let border_color = if popup_focused {
                rgb(theme.cursor_background)
            } else {
                rgb(theme.popup_border)
            };
            let header_hint = if popup_focused {
                " Esc to dismiss"
            } else {
                " K to focus · Esc to dismiss"
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
                    // Header: a BOLD, larger (`POPUP_TITLE_SCALE`) title in
                    // the accent colour + a dim hint, on one baseline-aligned
                    // row. No `───` separator — the title styling + the
                    // `.pb_2()` gap separate it from the body. The title
                    // row's height is locked to `estimated_row_px *
                    // POPUP_TITLE_SCALE` (the larger font's line height),
                    // exactly what `popup_chrome_v_px` reserves, so the body
                    // geometry stays precise.
                    div()
                        .flex()
                        .flex_row()
                        .pb_2()
                        .child(
                            div()
                                .h(px(estimated_row_px * POPUP_TITLE_SCALE))
                                .flex_shrink_0()
                                .text_size(px(font_size_px * POPUP_TITLE_SCALE))
                                .font_weight(gpui::FontWeight::BOLD)
                                .text_color(rgb(theme.popup_title))
                                .child(format!(" {title} ")),
                        )
                        .child(
                            // Push the smaller hint DOWN by the title-vs-hint
                            // height difference (`row_px * (scale - 1)`) so
                            // its bottom lines up with the title's bottom
                            // rather than floating up at the top. (gpui's flex
                            // `items_end`/`justify_end` didn't land the text
                            // where its bottom meets the title; an explicit
                            // top offset is deterministic.)
                            div()
                                .flex_shrink_0()
                                .pt(px(estimated_row_px * (POPUP_TITLE_SCALE - 1.0)))
                                .text_color(rgb(theme.popup_hint))
                                .child(header_hint),
                        ),
                )
                .child(
                    // PU.2: body height locked to `popup_inner_rows ×
                    // row_px` so flex can't oversize the body; the
                    // `EditorElement` flex-grows to fill it and paints
                    // exactly `viewport_height` (= `popup_inner_rows`)
                    // display rows from the synthetic POPUP matrix.
                    div()
                        .flex()
                        .flex_col()
                        .min_h(px(popup_body_h_px))
                        .max_h(px(popup_body_h_px))
                        .overflow_hidden()
                        .child(editor_element.into_any_element()),
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
                        this.app
                            .dispatch_action(lattice_host::action::Action::GoToTab(target_n));
                        cx.notify();
                    }),
                );
                row = row.child(cell);
            }
            Some(row)
        } else {
            None
        };

        // When the picker is open in minibuffer mode it carries its OWN
        // prompt row (built into `picker_minibuffer` below). The picker
        // prompt must claim the bottom slot — exactly as the TUI peer's
        // `draw_picker_prompt` draws into the cmdline row instead of
        // `draw_command_or_echo`. Rendering the global cmdline/echo row
        // *and* the picker's prompt would leave an empty cmdline row
        // wedged between the modeline and the picker prompt (the GPUI gap
        // this fixes). cmdline-completion is unaffected: it has no prompt
        // of its own (the `:cmd` bottom row IS its prompt), so the bottom
        // row stays for that path.
        let render_global_bottom_row =
            global_bottom_row_visible(picker_use_minibuffer, picker_substate.state.is_some());

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
        root = root.child(document_area);
        if render_global_bottom_row {
            root = root.child({
                // MB.2: the expanded tier-2 band draws each line of the
                // multi-line command line as its own row (flex-col), so the
                // bottom row grows in place and pushes the panes + their
                // mode-lines up (GPUI peer of the TUI band).
                // Level-coloured echo (errors red + bold, warnings
                // yellow) to match the TUI echo line; cmdline / search /
                // Info fall through to the default status foreground.
                let mut msg = if modeline.cmdline_expanded {
                    let mut col = div().flex().flex_col();
                    for line in bottom_row.split('\n') {
                        col = col.child(div().child(line.to_string()));
                    }
                    col
                } else if matches!(modal, lattice_grammar::ModalState::Command)
                    && modeline
                        .cmdline_decorations
                        .as_ref()
                        .is_some_and(|d| !d.spans.is_empty())
                {
                    // MB.4: colour the `:` line per the published decoration
                    // spans, then the live error / parameter hint (peer of
                    // the TUI `draw_command_or_echo` path). Spans are byte
                    // ranges into `cmdline_text` (the `:` prompt is separate).
                    let d = modeline.cmdline_decorations.as_ref().unwrap();
                    let line = modeline.cmdline_text.as_ref();
                    let default_fg = theme.status_foreground;
                    let mut row = div().flex().flex_row().child(":".to_string());
                    let mut pos = 0usize;
                    for sp in &d.spans {
                        let s = sp.range.start.min(line.len());
                        let e = sp.range.end.min(line.len());
                        if s < pos
                            || e < s
                            || !line.is_char_boundary(s)
                            || !line.is_char_boundary(e)
                        {
                            continue;
                        }
                        if s > pos {
                            row = row.child(line[pos..s].to_string());
                        }
                        let fg = lattice_host::ui::theme::resolve_syntax_style(
                            &resolved_theme,
                            &theme_ids,
                            sp.style,
                        )
                        .fg
                        .map(|c| c.to_rgb_u32(default_fg))
                        .unwrap_or(default_fg);
                        row = row.child(div().text_color(rgb(fg)).child(line[s..e].to_string()));
                        pos = e;
                    }
                    if pos < line.len() {
                        row = row.child(line[pos..].to_string());
                    }
                    if let Some(err) = &d.error {
                        let ec = lattice_host::ui::theme::resolve_syntax_style(
                            &resolved_theme,
                            &theme_ids,
                            lattice_cells::style::Style::DiagnosticError,
                        )
                        .fg
                        .map(|c| c.to_rgb_u32(0x00f3_8ba8))
                        .unwrap_or(0x00f3_8ba8);
                        row = row.child(div().text_color(rgb(ec)).child(format!("  {err}")));
                    } else if let Some(ph) = &d.param_hint {
                        row =
                            row.child(div().text_color(rgb(0x006c_7086)).child(format!("  {ph}")));
                    }
                    row
                } else {
                    div().child(bottom_row)
                };
                match bottom_echo_level {
                    Some(lattice_host::action::EchoLevel::Error) => {
                        let fg = resolved_theme
                            .get(theme_ids.diagnostic_error)
                            .fg
                            .map(|c| c.to_rgb_u32(0x00f3_8ba8))
                            .unwrap_or(0x00f3_8ba8);
                        msg = msg.text_color(rgb(fg)).font_weight(gpui::FontWeight::BOLD);
                    }
                    Some(lattice_host::action::EchoLevel::Warn) => {
                        let fg = resolved_theme
                            .get(theme_ids.diagnostic_warning)
                            .fg
                            .map(|c| c.to_rgb_u32(0x00f9_e2af))
                            .unwrap_or(0x00f9_e2af);
                        msg = msg.text_color(rgb(fg));
                    }
                    _ => {}
                }
                let mut row = div()
                    .bg(rgb(theme.status_background))
                    .text_color(rgb(theme.status_foreground))
                    .px_2()
                    .py_1()
                    .flex()
                    .flex_row()
                    .child(msg);
                // MB.2e: honor `command-line.expand-height` — reserve at
                // least that many rows for the expanded band (peer of the
                // TUI's fixed band), so a short command still opens the
                // configured height and the panes above flex-shrink up. A
                // longer command grows past it (content-driven), matching
                // the TUI band scrolling within its reserved region.
                if modeline.cmdline_expanded {
                    let band_rows = modeline
                        .cmdline_expand_height
                        .rows(total_rows.max(1) as u16);
                    row = row
                        .min_h(px(band_rows as f32 * estimated_row_px))
                        .flex_shrink_0();
                }
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
        }

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
        // PU.5d: docs popup to the LEFT of the candidate popup. The
        // candidate box is `.right_4()` (16px) wide up to 360px, so the docs
        // box sits at right = 16 + 360 + 8(gap) = 384px, same `.top_8()`.
        if let Some(overlay) = completion_docs_overlay {
            root = root.child(div().absolute().top_8().right(px(384.0)).child(overlay));
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
                    let cursor_screen_row = anchor
                        .line
                        .saturating_sub(popup_substate.doc_scroll_at_anchor)
                        as f32;
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
                    let cursor_row_top = top_origin_px + cursor_screen_row * estimated_row_px;
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
        // Resolve `ui.window.*` before open_window. The editor boots inside the
        // builder closure below (too late for WindowOptions), so parse the default
        // config paths into a throwaway registry now. Both are registered scalar
        // options, so no structural prefixes are needed.
        let (decorations, start_maximized) = {
            let reg = lattice_config::ConfigRegistry::new();
            reg.init_from_linkme();
            let root = lattice_host::editor::Editor::workspace_root_from_cwd();
            let _ = lattice_config::load_default_paths(&reg, root.as_deref(), &[]);
            let decorations = reg
                .get_typed::<lattice_config::WindowDecorationsOption>()
                .map(|v| *v)
                .unwrap_or_default();
            let start_maximized = reg
                .get_typed::<lattice_config::StartMaximized>()
                .map(|v| *v)
                .unwrap_or(false);
            (decorations, start_maximized)
        };
        let (titlebar, window_decorations) = crate::window_chrome::window_chrome(decorations);
        let default_bounds = Bounds::centered(None, size(px(720.0), px(480.0)), cx);
        // ui.window.start-maximized: the maximize strategy depends on whether the
        // window is resizable, which depends on `decorations`.
        //
        // - Decorated (`full`) windows ARE resizable, so GPUI's
        //   `WindowBounds::Maximized` (macOS zoom / X11 maximized state / Windows
        //   SW_MAXIMIZE) works — GPUI applies it during window construction.
        // - Borderless (`none`) windows are NON-resizable on macOS: `titlebar:
        //   None` drops `NSResizableWindowMask`, so `zoom()` is a no-op AND even
        //   AX tools (Raycast/yabai) get their `setFrame` rejected. We therefore
        //   cannot rely on a maximize/zoom action; instead we open the window
        //   already sized to the full display, since the creation-time frame is
        //   honored regardless of later resizability. (On Linux/Windows borderless
        //   windows stay resizable, but opening at display size fills the screen
        //   there too, so this branch is correct cross-platform.)
        let window_bounds = if start_maximized {
            if decorations.is_borderless() {
                let full = cx
                    .primary_display()
                    .map(|d| d.bounds())
                    .unwrap_or(default_bounds);
                WindowBounds::Windowed(full)
            } else {
                WindowBounds::Maximized(default_bounds)
            }
        } else {
            WindowBounds::Windowed(default_bounds)
        };
        let window = cx.open_window(
            WindowOptions {
                window_bounds: Some(window_bounds),
                titlebar,
                // XDG app-id for Linux desktop environments (Wayland /
                // X11) so the window groups correctly with any .desktop
                // launcher that references "com.lattice-editor.lattice".
                app_id: Some("com.lattice-editor.lattice".to_string()),
                window_decorations,
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
            // ui.window.decorations = transparent: hide the traffic-light buttons
            // now that the window exists. `setHidden:` (inside hide_traffic_lights)
            // keeps their Accessibility geometry intact, so Raycast/yabai can still
            // drive the window — unlike moving them off-screen. No-op off macOS.
            if matches!(decorations, lattice_config::Decorations::Transparent) {
                crate::window_chrome::hide_traffic_lights(window);
            }
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

#[cfg(test)]
mod popup_geometry_tests {
    use super::{default_ui_row_px, popup_body_h_px, popup_chrome_v_px, popup_inner_height_rows};

    /// The popup body div must hold every inner row WITH slack, and never
    /// overflow the popup. Regression guard for the "last line partially
    /// behind on G" bug: flooring the body to `inner_rows × row_px` left
    /// zero slack, so the EditorElement's per-row pixel rounding pushed the
    /// last row past the locked body (clipped by `overflow_hidden`).
    #[test]
    fn popup_body_carries_row_rounding_slack_without_overflow() {
        let rem = 16.0;
        for &row_px in &[16.0_f32, 18.2, 20.0] {
            for &h in &[240.0_f32, 400.0, 600.0] {
                let rows = popup_inner_height_rows(h, rem, row_px);
                let body = popup_body_h_px(h, rem, row_px);
                // Holds every inner row...
                assert!(
                    body >= rows as f32 * row_px,
                    "body {body} must fit {rows} rows ({}) @ row_px={row_px} h={h}",
                    rows as f32 * row_px
                );
                // ...and never overflows the popup's outer height (body +
                // chrome ≤ popup height).
                assert!(
                    body + popup_chrome_v_px(rem, row_px) <= h + 0.01,
                    "body+chrome must fit popup h={h}"
                );
            }
        }
        // With a non-row-aligned height there is genuine slack (the floor
        // remainder) — what absorbs the EditorElement's rounding.
        let body = popup_body_h_px(600.0, rem, 18.0);
        let rows = popup_inner_height_rows(600.0, rem, 18.0);
        assert!(
            body > rows as f32 * 18.0,
            "expected rounding slack; re-flooring the body would regress the clip"
        );
    }

    /// Regression guard for the "last line behind the modeline" bug
    /// (independent of the tabline, GPUI-only): the modeline/status row
    /// and the global cmdline row resolve to GPUI's DEFAULT `TextStyle`
    /// line height (phi, the golden ratio ≈1.618×), not `EditorElement`'s
    /// own 1.3× content-row multiplier. A prior version of this file
    /// reused the 1.3× estimate for these UI rows, undercounting their
    /// real height and reserving too little chrome — the pane geometry
    /// computed one row too many, and the extra row was clipped by
    /// `overflow_hidden` right under the modeline. This pins the
    /// default-row measurement to the actual golden-ratio formula so a
    /// future edit can't silently reintroduce the 1.3× estimate here.
    #[test]
    fn default_ui_row_px_uses_golden_ratio_not_content_row_multiplier() {
        let rem = gpui::px(16.0);
        let font_size_px = 16.0 * 0.875; // text_sm()
        let content_row_px = font_size_px * 1.3; // EditorElement::line_height
        let ui_row_px = default_ui_row_px(rem);

        // GPUI's default TextStyle::line_height is phi() ≈ 1.618×, which
        // is meaningfully taller than EditorElement's 1.3× — the two must
        // NOT collapse to the same value, or this fix has regressed.
        assert!(
            ui_row_px > content_row_px * 1.1,
            "default UI row height ({ui_row_px}) must exceed the content \
             row estimate ({content_row_px}) by a wide margin — reusing \
             the content-row multiplier here is exactly the bug this \
             guards against"
        );
        // Sanity bound: phi × font_size, rounded to the nearest pixel.
        let expected = (font_size_px * 1.618_034).round();
        assert!(
            (ui_row_px - expected).abs() < 1.0,
            "expected ~{expected}px (phi × font_size), got {ui_row_px}"
        );
    }
}

#[cfg(test)]
mod pane_geometry_split_tests {
    use super::{collect_pane_geometries, default_ui_row_px};
    use lattice_core::ui::pane::PaneNode;

    fn leaf(idx: usize) -> PaneNode {
        PaneNode::Leaf(idx)
    }

    fn hsplit(top: PaneNode, bottom: PaneNode) -> PaneNode {
        PaneNode::HorizontalSplit {
            top: Box::new(top),
            bottom: Box::new(bottom),
            ratio: 0.5,
        }
    }

    fn vsplit(left: PaneNode, right: PaneNode) -> PaneNode {
        PaneNode::VerticalSplit {
            left: Box::new(left),
            right: Box::new(right),
            ratio: 0.5,
        }
    }

    /// Generalises the "last line behind the modeline" fix from "one pane"
    /// to ARBITRARY split trees and ARBITRARY viewport sizes — the
    /// `per_leaf_v_chrome_px` (built from `default_ui_row_px`, not the
    /// content-row estimate) must apply correctly at every leaf regardless
    /// of split depth, and `collect_pane_geometries` must never silently
    /// drop a leaf or double/under-count a split's share.
    ///
    /// `collect_pane_geometries` is a pure recursive function of `(tree,
    /// available px, chrome px, row/col px)` — it has no notion of "the
    /// user resized" or "the user split N times"; every call re-derives
    /// geometry from scratch off whatever the CURRENT tree and CURRENT
    /// viewport pixels are. So this test doesn't assume any particular
    /// window size or split shape reflects real usage — it sweeps a
    /// deliberately wide matrix (1-way through 4-way splits, both split
    /// orientations, viewport sizes from small to large) and asserts the
    /// per-leaf INVARIANT holds identically at every point, rather than
    /// spot-checking one shape and generalising by assumption.
    #[test]
    fn per_leaf_chrome_applies_correctly_across_arbitrary_splits_and_sizes() {
        let trees: Vec<(&str, PaneNode)> = vec![
            ("1-way", leaf(0)),
            ("2-way h", hsplit(leaf(0), leaf(1))),
            ("2-way v", vsplit(leaf(0), leaf(1))),
            ("3-way", hsplit(leaf(0), vsplit(leaf(1), leaf(2)))),
            (
                "4-way",
                hsplit(vsplit(leaf(0), leaf(1)), vsplit(leaf(2), leaf(3))),
            ),
        ];

        // Viewport sizes standing in for "the user resized the GPUI
        // window" — including sizes small enough that some leaves clamp
        // to the `.max(1.0)` floor.
        let viewport_sizes: &[(f32, f32)] = &[
            (1200.0, 800.0),
            (800.0, 600.0),
            (400.0, 300.0),
            (200.0, 120.0),
            (100.0, 60.0),
        ];

        let rem = 16.0_f32;
        let font_size_px = rem * 0.875;
        let row_px = font_size_px * 1.3; // EditorElement's own content row height
        let col_px = font_size_px * 0.6; // monospace glyph advance approximation
        // Mirrors window.rs's real per_leaf_v_chrome_px composition, using
        // the FIXED default_ui_row_px (not the content-row estimate) —
        // this is the exact value under test.
        let pane_padding_v_px = rem * 0.75 * 2.0;
        let pane_status_padding_px = rem * 0.25 * 2.0;
        let default_row_px = default_ui_row_px(gpui::px(rem));
        let per_leaf_v_chrome_px = pane_padding_v_px + pane_status_padding_px + default_row_px;
        let per_leaf_h_chrome_px = rem * 0.75 * 2.0;

        for (tree_name, tree) in &trees {
            let expected_leaf_count = match tree_name {
                &"1-way" => 1,
                &"2-way h" | &"2-way v" => 2,
                &"3-way" => 3,
                &"4-way" => 4,
                _ => unreachable!(),
            };
            for &(avail_w, avail_h) in viewport_sizes {
                let mut out = Vec::new();
                collect_pane_geometries(
                    tree,
                    avail_w,
                    avail_h,
                    per_leaf_v_chrome_px,
                    per_leaf_h_chrome_px,
                    row_px,
                    col_px,
                    &mut out,
                );

                assert_eq!(
                    out.len(),
                    expected_leaf_count,
                    "{tree_name} at ({avail_w}x{avail_h}): every leaf must get a \
                     geometry entry — none dropped, none duplicated"
                );

                // Every leaf gets at least 1 row/col (the `.max(1.0)`
                // floor) even when its allocated split share is smaller
                // than one row/col of chrome + content — never zero,
                // never negative (usize can't go negative, but a
                // mis-derived huge value would also be wrong).
                for (idx, rows, cols) in &out {
                    assert!(
                        *rows >= 1 && *cols >= 1,
                        "{tree_name} leaf {idx} at ({avail_w}x{avail_h}): rows={rows} \
                         cols={cols} must both be >= 1"
                    );
                    // Upper bound sanity: a leaf can never claim more rows
                    // than the WHOLE viewport could hold at this row_px,
                    // regardless of split depth — proves chrome is being
                    // subtracted somewhere in the chain, not lost.
                    let max_possible_rows = (avail_h / row_px).ceil() as u32 + 1;
                    assert!(
                        *rows <= max_possible_rows,
                        "{tree_name} leaf {idx} at ({avail_w}x{avail_h}): rows={rows} \
                         exceeds what the whole viewport could hold ({max_possible_rows}) \
                         — chrome subtraction is being lost across the split recursion"
                    );
                }
            }
        }
    }

    /// Pins the specific regression this whole fix addresses, but at EVERY
    /// leaf of a 4-way split rather than just a single pane: using the
    /// (wrong) content-row estimate for `per_leaf_v_chrome_px` instead of
    /// `default_ui_row_px` under-reserves chrome enough that some
    /// split configurations compute one MORE row than the correct chrome
    /// value would — the off-by-one that clips the last line under the
    /// modeline. This must hold for every leaf, not just an unsplit pane.
    #[test]
    fn wrong_chrome_estimate_would_overcount_rows_at_every_split_leaf() {
        let rem = 16.0_f32;
        let font_size_px = rem * 0.875;
        let row_px = font_size_px * 1.3;
        let col_px = font_size_px * 0.6;
        let pane_padding_v_px = rem * 0.75 * 2.0;
        let pane_status_padding_px = rem * 0.25 * 2.0;

        let correct_chrome =
            pane_padding_v_px + pane_status_padding_px + default_ui_row_px(gpui::px(rem));
        // The bug: reusing the content-row estimate instead of the real
        // (larger, golden-ratio) default UI row height.
        let wrong_chrome = pane_padding_v_px + pane_status_padding_px + row_px;
        assert!(
            wrong_chrome < correct_chrome,
            "sanity: the bug under-reserves chrome relative to the fix"
        );

        let tree = hsplit(vsplit(leaf(0), leaf(1)), vsplit(leaf(2), leaf(3)));
        let avail_w = 1000.0_f32;

        // The correct-vs-wrong chrome delta (~4.8px here) is smaller than
        // one row (~18.2px), so whether it crosses an integer-row boundary
        // depends on the fractional remainder of `usable_h / row_px` at
        // the exact height — it won't trigger at every height. Rather than
        // assume any ONE particular window size happens to expose it
        // (exactly the kind of external-factor assumption to avoid), sweep
        // a dense, deterministic range of viewport heights and require
        // that the invariant (`wrong_rows >= correct_rows` everywhere) holds
        // at EVERY size tried, while confirming the regression is exercised
        // by AT LEAST ONE size in the swept range.
        let mut saw_overcount = false;
        for avail_h_int in (100..=1000).step_by(2) {
            let avail_h = avail_h_int as f32;
            let mut correct_out = Vec::new();
            collect_pane_geometries(
                &tree,
                avail_w,
                avail_h,
                correct_chrome,
                0.0,
                row_px,
                col_px,
                &mut correct_out,
            );
            let mut wrong_out = Vec::new();
            collect_pane_geometries(
                &tree,
                avail_w,
                avail_h,
                wrong_chrome,
                0.0,
                row_px,
                col_px,
                &mut wrong_out,
            );
            correct_out.sort_by_key(|(idx, _, _)| *idx);
            wrong_out.sort_by_key(|(idx, _, _)| *idx);

            for ((idx, correct_rows, _), (_, wrong_rows, _)) in correct_out.iter().zip(&wrong_out) {
                assert!(
                    wrong_rows >= correct_rows,
                    "leaf {idx} at avail_h={avail_h}: the under-reserving estimate must \
                     never compute FEWER rows than the fix (it only ever over-counts or \
                     matches) — got wrong={wrong_rows} correct={correct_rows}"
                );
                if wrong_rows > correct_rows {
                    saw_overcount = true;
                }
            }
        }
        assert!(
            saw_overcount,
            "expected at least one viewport height in the swept range where the wrong \
             estimate over-counts rows by at least one — otherwise this test doesn't \
             exercise the regression"
        );
    }
}

#[cfg(test)]
mod modeline_tests {
    use lattice_host::ui::theme::{BuiltinElementIds, InMemoryThemeRegistry, ThemeRegistry as _};

    /// MR.2: a styled marginalia segment resolves its slot through the
    /// theme on the GPUI peer — perm.write = red, perm.exec = green —
    /// and an unknown slot falls back to the custom annotation color.
    /// Parity with the TUI peer's per-segment resolution.
    #[test]
    fn styled_segment_color_resolves_slot_on_gpui() {
        let reg = InMemoryThemeRegistry::with_defaults();
        let resolved = reg.resolved();
        let ids = BuiltinElementIds::capture(&reg);
        assert_eq!(
            super::styled_segment_color_rgb("completion.annotation.perm.write", &resolved, &ids),
            0xf38ba8, // red
        );
        assert_eq!(
            super::styled_segment_color_rgb("completion.annotation.perm.exec", &resolved, &ids),
            0xa6e3a1, // green
        );
        // Unknown slot → custom annotation element (blue), never a panic.
        let custom = resolved
            .get(ids.completion_annotation_custom)
            .fg
            .map(|c| c.to_rgb_u32(0x89b4fa))
            .unwrap_or(0x89b4fa);
        assert_eq!(
            super::styled_segment_color_rgb("nope.unknown", &resolved, &ids),
            custom,
        );
    }

    /// MR.4: a theme change recolors styled marginalia on the GPUI peer
    /// — overriding the `perm.write` element changes the resolved
    /// segment color through the same path `paint_candidate_row` uses.
    #[test]
    fn styled_segment_color_follows_colorscheme_on_gpui() {
        use lattice_host::ui::theme::{ElementName, StyleSpec, ThemeRegistry as _};
        let reg = InMemoryThemeRegistry::with_defaults();
        let ids = BuiltinElementIds::capture(&reg);
        let before = super::styled_segment_color_rgb(
            "completion.annotation.perm.write",
            &reg.resolved(),
            &ids,
        );
        reg.set_override(
            ElementName::from_static("completion.annotation.perm.write"),
            StyleSpec::new().fg("green"),
        );
        let after = super::styled_segment_color_rgb(
            "completion.annotation.perm.write",
            &reg.resolved(),
            &ids,
        );
        assert_ne!(
            before, after,
            "styled marginalia tracks the active theme on GPUI"
        );
    }

    /// PH.1: GPUI per-char preview composition — match-highlight
    /// wins over the syntax overlay, else syntax color, else row
    /// fg. Parity with the TUI peer's `push_preview_run` (same
    /// `resolve_syntax_style` seam).
    #[test]
    fn preview_char_color_composes_match_over_syntax_on_gpui() {
        let reg = InMemoryThemeRegistry::with_defaults();
        let resolved = reg.resolved();
        let ids = BuiltinElementIds::capture(&reg);
        let spans = vec![lattice_completion::DisplaySpan {
            range: 0..4,
            style: lattice_cells::style::Style::Keyword,
        }];
        let matches = vec![0..2usize];
        const ROW: u32 = 0x0011_1111;
        const MATCH: u32 = 0x00fa_b387;
        let keyword = resolved.get(ids.syntax_keyword).fg.unwrap().to_rgb_u32(ROW);

        // byte 0: in match AND keyword span → match wins.
        assert_eq!(
            super::preview_char_color_rgb(0, &matches, &spans, &resolved, &ids, ROW, MATCH),
            MATCH,
            "match wins on overlap"
        );
        // byte 2: keyword span only → syntax color.
        assert_eq!(
            super::preview_char_color_rgb(2, &matches, &spans, &resolved, &ids, ROW, MATCH),
            keyword,
        );
        // byte 9 (uncovered) → row fg (plain preview).
        assert_eq!(
            super::preview_char_color_rgb(9, &matches, &spans, &resolved, &ids, ROW, MATCH),
            ROW,
        );
    }

    /// PH.1: GPUI preview syntax color tracks `:colorscheme` —
    /// overriding `syntax.keyword` recolors the resolved span.
    #[test]
    fn preview_char_color_follows_colorscheme_on_gpui() {
        use lattice_host::ui::theme::{ElementName, StyleSpec, ThemeRegistry as _};
        let reg = InMemoryThemeRegistry::with_defaults();
        let ids = BuiltinElementIds::capture(&reg);
        let spans = vec![lattice_completion::DisplaySpan {
            range: 0..2,
            style: lattice_cells::style::Style::Keyword,
        }];
        let no_match: Vec<std::ops::Range<usize>> = vec![];
        let before =
            super::preview_char_color_rgb(0, &no_match, &spans, &reg.resolved(), &ids, 0, 0);
        reg.set_override(
            ElementName::from_static("syntax.keyword"),
            StyleSpec::new().fg("green"),
        );
        let after =
            super::preview_char_color_rgb(0, &no_match, &spans, &reg.resolved(), &ids, 0, 0);
        assert_ne!(
            before, after,
            "preview syntax color tracks the active theme on GPUI"
        );
    }

    /// ML.2: the `modeline.*` elements GPUI's `modeline_row` paints through
    /// resolve to the expected u32 colours under the default palette —
    /// pinning the exact `resolved.get(id).fg.to_rgb_u32` adaptation the
    /// row uses. CONTENT parity with the TUI is guaranteed by construction
    /// (both peers call the same `lattice_host::modeline` resolver), so
    /// this covers the GPUI-specific colour path.
    #[test]
    fn modeline_elements_resolve_to_gpui_colours() {
        let reg = InMemoryThemeRegistry::with_defaults();
        let resolved = reg.resolved();
        let ids = BuiltinElementIds::capture(&reg);

        let mode = resolved.get(ids.modeline_mode);
        assert_eq!(
            mode.fg.unwrap().to_rgb_u32(0),
            0x0089_b4fa,
            "mode fg = blue"
        );
        assert!(mode.modifiers.bold, "mode is bold");
        assert_eq!(
            resolved.get(ids.modeline_active).bg.unwrap().to_rgb_u32(0),
            0x0045_475a,
            "active bar bg = surface1"
        );
        let inactive = resolved.get(ids.modeline_inactive);
        assert_eq!(
            inactive.bg.unwrap().to_rgb_u32(0),
            0x0031_3244,
            "inactive bar = surface0"
        );
        assert_eq!(
            inactive.fg.unwrap().to_rgb_u32(0),
            0x006c_7086,
            "inactive fg = overlay"
        );
    }

    /// The bottom global row shows the in-progress `:` minibuffer in
    /// Command mode, but otherwise falls back to the last echo message
    /// (e.g. a `:set wrap?` value) — the parity gap with the TUI peer
    /// this fixes. `super::bottom_row_content` is the pure selector.
    #[test]
    fn bottom_row_falls_back_to_echo_outside_minibuffer() {
        use lattice_grammar::ModalState;
        use lattice_host::action::{EchoLevel, EchoMessage};
        use lattice_host::render_state::{MessagesRenderState, ModelineRenderState};
        use std::sync::Arc;

        let echo = MessagesRenderState {
            last: Some(Arc::new(EchoMessage {
                text: "wrap=false".to_string(),
                level: EchoLevel::Info,
            })),
        };
        let modeline = ModelineRenderState::default();

        // Normal mode → the echo (this is what GPUI used to drop).
        let (row, level) = super::bottom_row_content(ModalState::Normal, &modeline, &echo);
        assert_eq!(row, "wrap=false");
        assert!(matches!(level, Some(EchoLevel::Info)));

        // Command mode → the `:` minibuffer wins over any pending echo.
        let cmd = ModelineRenderState {
            cmdline_text: Arc::from("set wrap?"),
            ..Default::default()
        };
        let (row, level) = super::bottom_row_content(ModalState::Command, &cmd, &echo);
        assert_eq!(row, ":set wrap?");
        assert!(level.is_none(), "cmdline carries no echo level");

        // Normal mode, no message → blank row.
        let empty = MessagesRenderState { last: None };
        let (row, _) = super::bottom_row_content(ModalState::Normal, &modeline, &empty);
        assert!(row.is_empty());

        // MB.2: the expanded band wins over the (real Insert / Normal)
        // modal and shows the full multi-line command line, `:`-prefixed.
        let expanded = ModelineRenderState {
            cmdline_expanded: true,
            cmdline_full_text: Arc::from("e foo\nbar baz"),
            ..Default::default()
        };
        let (row, level) = super::bottom_row_content(ModalState::Insert, &expanded, &echo);
        assert_eq!(row, ":e foo\nbar baz");
        assert!(level.is_none());
    }

    /// The global cmdline/echo row is painted in every state EXCEPT a
    /// picker open in minibuffer mode — there the picker's own prompt row
    /// claims the bottom slot (like the TUI's `draw_picker_prompt`), so
    /// painting the cmdline row too would leave an empty row between the
    /// modeline and the picker prompt (the GPUI gap this guards). This
    /// pins the full truth table.
    #[test]
    fn global_bottom_row_suppressed_only_for_minibuffer_picker() {
        // Picker open in minibuffer mode → suppress (picker prompt is the row).
        assert!(!super::global_bottom_row_visible(true, true));
        // Picker open but in popup mode → keep (echo shows under the overlay).
        assert!(super::global_bottom_row_visible(false, true));
        // No picker, minibuffer display configured (e.g. cmdline-completion,
        // plain editing) → keep (the `:cmd`/echo line IS the bottom row).
        assert!(super::global_bottom_row_visible(true, false));
        // No picker at all → keep.
        assert!(super::global_bottom_row_visible(false, false));
    }

    /// `icon_color_to_rgb` maps the renderer-neutral named `IconColor`s
    /// (the non-`Rgb` devicon fallbacks) to the default GPUI palette, and
    /// passes `Rgb(_)` through untouched. `Reset` defers to the supplied
    /// document foreground. Pins the colour adapter the oil / file-tree
    /// builders use for regular-file devicon hues.
    #[test]
    fn terminal_row_start_clips_to_pane_height_from_the_bottom() {
        // Grid taller than the pane (e.g. a horizontal split halved the
        // height while the PTY still holds 50 rows) → show the BOTTOM 24
        // (start at row 26), so the terminal never overflows its pane and
        // shoves the modeline / cmdline off-screen.
        assert_eq!(super::terminal_row_start(50, 24), 26);
        // Pane ≥ grid → no clip (vertical split keeps full height).
        assert_eq!(super::terminal_row_start(24, 50), 0);
        assert_eq!(super::terminal_row_start(24, 24), 0);
        // Pane height not published yet (0) → no clip (paint the whole grid).
        assert_eq!(super::terminal_row_start(50, 0), 0);
    }

    #[test]
    fn icon_color_to_rgb_maps_named_palette_and_passes_rgb_through() {
        use lattice_core::ui::icons::IconColor;
        assert_eq!(
            super::icon_color_to_rgb(IconColor::Rgb(0xDEA584), 0x111111),
            0xDEA584
        );
        assert_eq!(
            super::icon_color_to_rgb(IconColor::Reset, 0x123456),
            0x123456
        );
        assert_eq!(super::icon_color_to_rgb(IconColor::Blue, 0), 0x0089_b4fa);
        assert_eq!(super::icon_color_to_rgb(IconColor::Green, 0), 0x00a6_e3a1);
        assert_eq!(super::icon_color_to_rgb(IconColor::Yellow, 0), 0x00f9_e2af);
    }

    /// The oil / file-tree builders colour directories and dotfiles from
    /// the shared `file_tree.dir` / `file_tree.hidden` theme roles (so
    /// built-in themes drive them and TUI/GPUI agree). This pins that the
    /// roles exist and are themed under the default palette — dir is blue +
    /// bold, hidden is dim grey, and regular `file_tree.file` carries no
    /// override (regular files keep their devicon hue).
    #[test]
    fn file_tree_entry_roles_are_themed_in_default_palette() {
        let reg = InMemoryThemeRegistry::with_defaults();
        let resolved = reg.resolved();
        let ids = BuiltinElementIds::capture(&reg);

        let dir = resolved.get(ids.file_tree_dir);
        assert!(dir.fg.is_some(), "file_tree.dir must carry a themed fg");
        assert!(dir.modifiers.bold, "file_tree.dir is bold");

        let hidden = resolved.get(ids.file_tree_hidden);
        assert!(
            hidden.fg.is_some(),
            "file_tree.hidden must carry a themed fg"
        );
        assert_ne!(
            dir.fg.unwrap().to_rgb_u32(0),
            hidden.fg.unwrap().to_rgb_u32(0),
            "dir and hidden resolve to distinct hues"
        );

        // Regular files carry no override — the devicon hue shows through.
        assert!(resolved.get(ids.file_tree_file).fg.is_none());
    }
}
