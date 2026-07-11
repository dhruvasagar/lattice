//! Custom GPUI `Element` rendering pane text via direct cosmic-text
//! shaping (`WindowTextSystem::shape_line` + `ShapedLine::paint`).
//!
//! Phase 5.8.AF.5 / Slice X3.full.1 -> .2.
//!
//! ## Why this exists
//!
//! Pre-X3.full, `window::paint_pane` built an element tree of one
//! `Div` per styled character run, per visible line. Even after X3's
//! run-collapsing (~8x reduction vs one-Div-per-char), ~100-200
//! Divs per pane per frame is the dominant cost: `paint_us` in the
//! 1300-3000µs band at 60Hz, 85-95% of frame time, downstream of
//! GPUI's flex layout + composition pass. Paramount goal #1
//! (sub-frame input latency: keystroke -> glyph within the one-frame ceiling, <= 8.3 ms at 120Hz)
//! cannot be met while the element tree has that fan-out.
//!
//! This element collapses the entire pane body into ONE element-tree
//! node. `prepaint` calls `shape_line` once per visible line (text)
//! and once per visible line (gutter); `paint` calls
//! `ShapedLine::paint` accordingly + `Window::paint_quad` for the
//! pane background and cursor. The shaping itself was already
//! happening inside the per-Div text -- the saving is the upstream
//! flex layout + element composition pass.
//!
//! ## Slice scope
//!
//! - **X3.full.1 ✅**: pane background quad, per-line shaped text,
//!   syntax colouring. (display-line B-series: the colour source
//!   migrated from the worker's `VisibleSpans` to the cells /
//!   `DisplayMatrix` substrate; B4.2 deleted the dead span cache.)
//! - **X3.full.2 (this slice)**: cursor (block / bar / underline
//!   shapes painted via `paint_quad`); gutter (fold marker + severity
//!   sign + line number, one ShapedLine per visible row); legacy
//!   per-cell row construction in `window::paint_pane` deleted.
//! - **X3.full.3 (next)**: decoration backgrounds — visual
//!   selection, hlsearch, current_match, doc_highlight, cursorline,
//!   substitute preview.
//! - **X3.full.4**: inlay-hint virtual text + per-cell diagnostic
//!   underline + GPUI frame-budget bench.
//!
//! ## Cursor positioning (monospace approximation)
//!
//! Slice X3.full.2 positions the cursor at
//! `gutter_width_px + char_col * glyph_advance`. `glyph_advance`
//! is a monospace approximation (`font_size * 0.6`); the editor's
//! default font is monospace so this is visually correct for code
//! text. Proportional-font support requires reading per-glyph X
//! advances via `layout_line` — deferred to a follow-on slice when
//! the use case appears.
//!
//! ## Indexing contract
//!
//! display-line B4.2: the active-pane span-grid indexing contract
//! (`VisibleSpans.spans[i]` ↔ absolute buffer line) was retired with
//! the dead span cache. The element now sources per-row style runs
//! from the `DisplayMatrix` (`display_matrix.row_at_source_line`),
//! falling back to default-styled text for rows the cells worker
//! hasn't built yet — a better visual fail-mode than a panic.
//!
//! See `docs/dev/operations/render-thread-discipline-remediation.md`
//! §X3.full and `docs/dev/architecture/display-line.md`.

#![cfg(feature = "window")]

use std::sync::Arc;

use gpui::{
    App, Bounds, DefiniteLength, Element, ElementId, FontFeatures, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Length, Pixels, ShapedLine, SharedString, Style,
    TextRun, Window, fill, point, px, rgb, size,
};
use lattice_cells::CellMatrix;
use lattice_host::cursor_shape::CursorShape;
use lattice_host::display_matrix::DisplayMatrix;
use lattice_syntax::{Style as SyntaxStyle, StyledSpan};

use crate::GpuiTheme;
use crate::cells_paint::display_line_to_text_runs;
use crate::glyph_resolver::GlyphResolver;

/// Adapter: host-canonical syntax style -> packed 24-bit `0xRRGGBB`.
/// T.5.b: resolves `style` through the active theme's resolved table
/// (`resolved` + `ids`) via `resolve_syntax_style`, the replacement
/// for the retired `Theme::syntax_style`. Threaded from the caller's
/// render-state-derived theme handles so the legacy `build_line_with_inlays`
/// fallback path renders identical colours to the display-line path.
fn syntax_color(
    style: SyntaxStyle,
    resolved: &lattice_host::ui::theme::ResolvedTheme,
    ids: &lattice_host::ui::theme::BuiltinElementIds,
) -> u32 {
    let host_style = lattice_host::ui::theme::resolve_syntax_style(resolved, ids, style);
    host_style
        .fg
        .map(|c| c.to_rgb_u32(0xcdd6f4))
        .unwrap_or(0xcdd6f4)
}

fn style_at(spans: &[StyledSpan], byte: usize) -> SyntaxStyle {
    for span in spans {
        if byte >= span.start && byte < span.end {
            return span.style;
        }
    }
    SyntaxStyle::Default
}

/// Per-visible-line gutter metadata. Pre-resolved by the caller so
/// the element doesn't reach into LSP / fold caches at paint time.
pub(crate) struct GutterLineMeta {
    /// Absolute buffer-line index this gutter row decorates
    /// (`scroll..scroll+viewport_height`, skipping folded lines).
    pub(crate) line_idx: u32,
    /// K.4.6 follow-up (2026-06-02): the line number to RENDER
    /// in the gutter cell. For regular Documents this equals
    /// `line_idx` (identity — composed row IS the source line).
    /// For Multibuffer views this is the SOURCE line number in
    /// the originating file, looked up via the substrate's
    /// `display_line_numbers` map at meta-construction time
    /// (window.rs). Decoupling these two lets cursor / fold /
    /// click handling stay on `line_idx` (composed coords) while
    /// the user sees the meaningful source-file numbers.
    pub(crate) display_line: u32,
    /// Fold-start marker for this head row: `Some((glyph, colour))`
    /// where `glyph` is `▾` (open/expanded fold) or `▸` (closed/collapsed
    /// fold) and `colour` is the resolved `gutter.fold.open` /
    /// `gutter.fold.closed` theme colour. `None` when no fold starts on
    /// this row — a blank cell keeps the column aligned. Painted AFTER
    /// the line number (a separator space, the glyph, then a trailing
    /// gap: `… 99 ▸ code`), mirroring the TUI gutter, not at column 0.
    pub(crate) fold_marker: Option<(char, u32)>,
    /// Pre-resolved diagnostic severity (glyph, colour). `None`
    /// renders a blank space in the severity column so alignment
    /// stays stable.
    pub(crate) severity: Option<(char, u32)>,
    /// D.3.d.2 (2026-05-29): pre-resolved diff sign (glyph,
    /// colour). `None` renders a blank space in the diff-sign
    /// column so alignment stays stable when no session is
    /// open or no hunk touches the row. Resolved by the
    /// caller (`window.rs paint_pane`) from
    /// `rs.diff.sign_map.sign_at(line_idx)`.
    pub(crate) diff_sign: Option<(char, u32)>,
    /// D.3.b.1.gpui (2026-05-29): marks this row as a virtual
    /// row (deletion block from `DiffOverlayVirtualRowProvider`,
    /// or future multibuffer excerpt header). `format_gutter_text`
    /// returns a blank-padded string for virtual rows so the
    /// gutter column stays aligned but shows nothing — the
    /// row's red backdrop quad + content text are the visible
    /// surface.
    pub(crate) is_virtual: bool,
}

/// Active-pane cursor state. `None` on inactive panes.
pub(crate) struct CursorState {
    /// 0-based buffer-line index.
    pub(crate) line: u32,
    /// 0-based utf-8 byte offset into that line's text.
    pub(crate) byte: u32,
    /// Modal-shape (Block / Bar / Underline). Resolved by the
    /// caller via `CursorShape::for_mode`.
    pub(crate) shape: CursorShape,
    /// 2026-05-26: the cursor's source-line text. The caller
    /// (window.rs `paint_pane`) reads it from the document
    /// snapshot at the cursor's line. Used by the cursor-layout
    /// block to compute `char_col` via `byte_to_combined_col` —
    /// `self.text` was zeroed in slice A.4 so the previous
    /// `raw_lines.get(c.line)` lookup returned `""` for any
    /// `c.line != 0` and pinned the horizontal cursor at
    /// column 0.
    pub(crate) line_text: String,
}

/// One inlay-hint row (slice X3.full.4). Caller flattens the LSP
/// `InlayHintLabel` to a plain string and pre-applies `padding_left`
/// / `padding_right` spacing; the element splices `text` into the
/// shaped line at `byte` (utf-8 byte offset into the original
/// line's text). Sorting by `(line, byte)` happens in the element
/// per visible row -- the input list ordering is irrelevant.
pub(crate) struct InlayHintRow {
    pub(crate) line: u32,
    pub(crate) byte: u32,
    pub(crate) text: String,
}

/// One diagnostic underline range (slice X3.full.4). Caller converts
/// the LSP utf-16 range to a utf-8-byte `Range` against the buffer's
/// line text. `color` is `0xRRGGBB` resolved via
/// `diagnostic_glyph_and_color`. Painted as a 2px `paint_quad` along
/// the bottom of the row(s) the range covers.
pub(crate) struct DiagnosticUnderline {
    pub(crate) range: lattice_core::protocol::position::Range,
    pub(crate) color: u32,
}

/// L4a.3 (lsp-architecture.md §15): the resolved inline cursor-line
/// diagnostic summary for one pane. `line` is the absolute buffer
/// line; `text` already carries the leading gap; `color` is
/// `0xRRGGBB` from the severity's host-theme style. Resolved in
/// `window.rs` from `render_state.diagnostics.inline_summary` on the
/// ACTIVE pane only — it tracks the focused cursor, so it is painted
/// per-frame (NOT spliced into the cells cache, which would churn on
/// every cursor move).
pub(crate) struct InlineDiagSummary {
    pub(crate) line: u32,
    pub(crate) text: String,
    pub(crate) color: u32,
}

/// Pane element. One instance per pane per frame.
///
/// Caller (`window::paint_pane`) extracts all referenced values up
/// front so the element holds only owned data; no borrow against
/// `GpuiApp` survives across layout / prepaint / paint method
/// boundaries.
pub(crate) struct EditorElement {
    /// Pane index inside the active pane tree. Used for
    /// `ElementId` so GPUI tracks the same element across frames.
    pub(crate) pane_idx: usize,
    /// Cached theme colours (bg, fg, cursor_bg, cursor_fg, ...).
    pub(crate) theme: GpuiTheme,
    /// Full document text (pre-extracted via `snapshot.text()`).
    /// `shape_line` panics on embedded newlines so the element
    /// splits on `\n` inside `prepaint`.
    pub(crate) text: Arc<String>,
    // display-line B4.2: the dead `visible_spans` field was deleted.
    // It carried the overlay worker's per-line styled-span output,
    // which B4.1 already severed every read of (the `build_runs`
    // else-branch renders default-styled now). The host-side
    // `VisibleSpans` cache it cloned from was deleted in the same
    // slice. Syntax colour flows through the cells / `DisplayMatrix`
    // substrate (`cell_matrix` / `display_matrix` below).
    /// Pane scroll (top visible doc line index, 0-based).
    pub(crate) scroll: u32,
    /// Horizontal scroll: first visible display column (0-based).
    /// Drives the body's left clip when `wrap` is off; the host pins
    /// it to 0 under wrap. Mirrors the TUI `leftcol` offset so both
    /// renderers pan identically (HS.1b).
    pub(crate) leftcol: u32,
    /// Visible viewport height in lines.
    pub(crate) viewport_height: u32,
    /// Per-visible-row gutter metadata (after fold filtering).
    /// Empty when the caller elects to render text-only (e.g. a
    /// future inactive-pane mode); `prepaint` falls back to a
    /// gutter-less walk in that case.
    pub(crate) gutter: Vec<GutterLineMeta>,
    /// Line-number column width in chars (max digits in
    /// `total_lines`).
    pub(crate) gutter_width: usize,
    /// DB.4: extra leading gutter cells to horizontally centre the buffer's
    /// content (dashboard). Added to `gutter_chars` (document lines + cursor)
    /// and to the virtual-row gutter (branding), so the whole buffer shifts
    /// right — centred with no text mutation. `0` for non-centred buffers.
    pub(crate) content_left_pad: u32,
    /// PU.1b-1a (`signcolumn`): whether to reserve the gutter sign
    /// columns (diagnostics severity + diff sign). `true` (default)
    /// keeps both cells — byte-identical to the prior render; `false`
    /// (`signcolumn=no`) drops them so content abuts the line-number
    /// column. TUI peer: `FrameView::sign_column` / `sign_columns_width`.
    pub(crate) sign_column: bool,
    /// PU.1b-1a (`number`): whether the gutter shows line numbers. `false`
    /// (help / dashboard / `:set nonumber`) omits the digits so the gutter is
    /// fold + trail only. TUI peer: `FrameView::show_line_numbers`.
    pub(crate) show_line_numbers: bool,
    /// Active-pane cursor state. `None` => inactive pane (no
    /// cursor marker painted).
    pub(crate) cursor: Option<CursorState>,
    /// True when this is the active pane. Drives whether
    /// cursorline / visual / hlsearch / substitute / doc-highlight
    /// backgrounds paint (inactive panes paint only the bg + text +
    /// gutter -- they don't carry a visual selection or search
    /// state).
    pub(crate) is_active: bool,
    /// Visual-mode selection range in utf-8 byte coordinates.
    /// `None` outside Visual mode. Caller-resolved; element paints
    /// the layered background. (Slice X3.full.3.)
    ///
    /// Used for Charwise / Linewise visual. Blockwise mode
    /// publishes `visual_block_extents` instead — a linear `Range`
    /// can't express the per-line column band a block needs.
    pub(crate) visual_range: Option<lattice_core::protocol::position::Range>,
    /// Visual(Blockwise) rectangle in utf-8 byte coordinates;
    /// `None` outside Visual(Blockwise). When `Some`, the element
    /// paints a per-line column band on each line in
    /// `[start_line, end_line]` and ignores `visual_range`.
    /// Mirrors
    /// [`lattice_host::render_state::ActiveDocumentRenderState::visual_block_extents`].
    pub(crate) visual_block_extents: Option<lattice_host::visual::BlockExtents>,
    /// `current_match`: the primary search hit the cursor sits on.
    /// Painted with the strongest match colour (yellow bg).
    pub(crate) current_match: Option<lattice_core::protocol::position::Range>,
    /// All search matches in the doc (`hlsearch`). Each painted
    /// with the softer match bg.
    pub(crate) all_matches: Vec<lattice_core::protocol::position::Range>,
    /// Substitute-preview ranges (`:s/pattern/replacement/`). Use
    /// the destructive-preview colour.
    pub(crate) substitute_matches: Vec<lattice_core::protocol::position::Range>,
    /// LSP document-highlight ranges, already converted to utf-8
    /// byte ranges by the caller (utf-16→utf-8 happens at the
    /// boundary).
    pub(crate) doc_highlights: Vec<lattice_core::protocol::position::Range>,
    /// Perf plan B.2 slice B.2.a: worker-produced per-row
    /// pre-bucketed static-overlay quads (doc_highlight,
    /// all_matches, substitute). `Some` for the active pane
    /// (consumed in prepaint as the base of
    /// `overlay_quads_per_row`); `None` for inactive panes
    /// (which fall through to the legacy per-frame
    /// `push_range_quads` walk for the only static layer they
    /// paint — doc_highlight).
    pub(crate) worker_static_overlay_quads:
        Option<std::sync::Arc<lattice_host::render_state::StaticOverlayQuads>>,
    /// D.3.b.1.gpui (2026-05-29): published `VirtualRowMatrix`
    /// snapshot. Cloned at construction time from
    /// `rs.virtual_rows.matrix`. The prepaint walk consults
    /// the matrix per visible doc line to interleave Above-
    /// and Below-anchored virtual rows (deletion blocks today,
    /// multibuffer headers in future) into the row stream.
    pub(crate) virtual_rows: std::sync::Arc<lattice_cells::VirtualRowMatrix>,
    /// D.3.e (2026-05-29): per-visible-row diff line-tint
    /// colour. `Some(rgb)` when a hunk's current side touches
    /// this row (Add → faint green, Change → faint yellow);
    /// `None` otherwise. Pre-resolved by the caller
    /// (`window.rs paint_pane`) from `rs.diff.sign_map.sign_at`
    /// for each visible buffer line, in the same order as
    /// `gutter` so `rel_row` indexes both arrays. Tints paint
    /// as full-row quads at the BOTTOM of `overlay_quads_per_row`
    /// — every cursor / search / selection overlay paints
    /// over them, so they read as a backdrop rather than
    /// competing with foreground emphasis.
    pub(crate) diff_tint_per_row: Vec<Option<u32>>,
    /// Background colour for the cursor line
    /// (`host_theme.cursor_line_bg` resolved by the caller, fallback
    /// Catppuccin surface0).
    pub(crate) cursorline_bg: u32,
    /// Whether the cursor-line tint is enabled for the active buffer
    /// (`:set cursorline` / `current-line-highlight`, default off).
    /// Mirrors the TUI's `option_cache.current_line_highlight` gate so
    /// both renderers agree — without it the quad paints unconditionally
    /// and `:set nocursorline` is a no-op in the GPUI peer.
    pub(crate) cursorline_enabled: bool,
    /// D.3.b.3 (2026-05-29): backdrop colour for deletion-
    /// block virtual rows. Resolved at construction time
    /// from `host_theme.diff_deletion_block_bg.to_rgb_u32(0)`
    /// so the paint pass doesn't need to hold a Theme.
    pub(crate) diff_deletion_block_bg: u32,
    /// LSP inlay hints for the buffer (slice X3.full.4). Caller
    /// flattens labels and pre-applies padding; the element
    /// splices `text` at `byte` into each affected visible row's
    /// shaped line. Empty when the buffer has no hints / no LSP
    /// attachment.
    pub(crate) inlay_hints: Vec<InlayHintRow>,
    /// Per-diagnostic underline overlays (slice X3.full.4). Caller
    /// resolves utf16→utf8 and severity→color at the boundary.
    /// Empty when no diagnostics exist for the buffer.
    pub(crate) diagnostic_underlines: Vec<DiagnosticUnderline>,
    /// `0xRRGGBB` for inlay virtual-text. `host_theme` resolved by
    /// the caller, Catppuccin overlay1 (0x7f849c) fallback.
    pub(crate) inlay_color: u32,
    /// L4a.3 (lsp-architecture.md §15): inline cursor-line diagnostic
    /// summary — trailing eol virtual text on the active pane's cursor
    /// line. `None` on inactive panes / when the host idle gate is
    /// disarmed. Painted per-frame at the row's end-of-content x by
    /// `paint` (shaped in `prepaint`), NOT spliced into the cells
    /// cache — it is cursor-transient, like the cursor / underline
    /// overlays.
    pub(crate) inline_diag_summary: Option<InlineDiagSummary>,
    /// S4.1 (2026-05-27): cell-grid substrate snapshot. Populated
    /// from `render_state.cells.matrix.load_full()` for the active
    /// pane; `None` for inactive panes (the cells worker publishes
    /// only for the active document).
    ///
    /// When `Some` and the row's source line is covered by the
    /// matrix, `prepaint` shapes from cells (S4.0 converter →
    /// `shape_line`) and skips the legacy
    /// `build_line_with_inlays` walk. When `None` or the row is
    /// folded / out-of-matrix (boot frame, buffer-switch gap),
    /// dispatch falls through to `shape_row`. The intermediate
    /// `shape_row_from_prepaint` branch (the overlay worker's old
    /// `RowPrepaint`) retired in S4.3; display-line B4.2 then deleted
    /// the `RowPrepaint` / `visible_rows` prepaint cache entirely.
    pub(crate) cell_matrix: Option<Arc<CellMatrix>>,
    /// B3 (2026-06-04): canonical `DisplayMatrix` snapshot — the GPU's
    /// primary shaping source. Populated from
    /// `render_state.cells.display_matrix` for the active pane (guarded on
    /// `version.text == snapshot.text_version`, like `cell_matrix`),
    /// `None` for inactive panes. `prepaint` shapes each covered row via
    /// `display_line_to_text_runs` (style-tagged runs → resolved
    /// `TextRun`s); folded / out-of-window / stale rows fall through to the
    /// legacy `shape_row`. B2.3 makes this text-current synchronously, so
    /// the stale guard no longer fires per keystroke — that retires the
    /// GPU whole-viewport flicker. `cell_matrix` is a DERIVED PROJECTION
    /// of `display_matrix` (`display_matrix_to_cell_matrix`) and is the
    /// PRODUCTION per-glyph source: `paint_cells_row` (the default
    /// active-pane glyph path, S4.final.f) reads it. Per the display-line
    /// B4 re-slice (approach A, 2026-06-20) it is RETAINED as that
    /// projection — not deleted; B4 deletes only the legacy highlight
    /// cache (`visible_spans` etc.). See architecture/display-line.md.
    pub(crate) display_matrix: Option<Arc<DisplayMatrix>>,
    /// T.5.b (theme-system): the resolved theme table + builtin
    /// element ids the display-line path resolves `DisplayRun`
    /// syntax-style tags through (`display_line_to_text_runs` →
    /// `resolve_syntax_style`). Replaces the `host_theme.syntax_style`
    /// read; populated in `window.rs` from the render-state's
    /// `resolved_theme` / `theme_ids` (T.4) — the same locals the
    /// editor element already binds.
    pub(crate) resolved_theme: std::sync::Arc<lattice_host::ui::theme::ResolvedTheme>,
    pub(crate) theme_ids: lattice_host::ui::theme::BuiltinElementIds,
    /// S4.final.b (2026-05-27): per-window glyph-id cache.
    /// `EditorElement::paint`'s body loop uses
    /// `crate::paint_cells::paint_cells_row` (which consumes
    /// this resolver) to emit per-cell `paint_glyph` calls — the
    /// DEFAULT active-pane glyph path (S4.final.f; the old
    /// `LATTICE_PAINT_CELLS` env-gate is now a no-op,
    /// `paint_cells.rs`). The `ShapedLine::paint` path is the
    /// fallback (inactive / folded / ligatures-on). Always populated from
    /// `EditorView.glyph_resolver` so the cache survives across
    /// paints + across panes within the same window. Mutex
    /// because the resolve path mutates the cache on miss and
    /// `EditorElement::paint` takes `&self`.
    pub(crate) glyph_resolver: Arc<std::sync::Mutex<GlyphResolver>>,
}

/// Per-frame layout state. Slice X3.full.2 holds nothing.
pub(crate) struct EditorElementLayoutState;

/// F.2/F.3 (Thread F): a display row whose columns are NOT uniformly the
/// base font size — one or more contiguous column runs render at a larger
/// scale, all sharing ONE baseline (the emacs markdown-heading model: the
/// `#`/`##` markers stay base-size, the title scales). F.2 introduced this
/// for headings as a fixed 2-piece split; F.3 generalizes it to N pieces
/// so **virtual rows** (the dashboard branding wordmark) get the same
/// per-token scaling.
///
/// A heading is the 2-piece case `[base markers][scaled title]`; a
/// branding row is `[base mark blocks][base gap][scaled wordmark]`; any
/// `VirtualRow::scales` pattern maps to its coalesced runs. `None` for
/// ordinary uniform rows (the untouched fast path).
///
/// Both paint paths read this: the active cell path paints each piece's
/// column range at its scaled advance sharing one baseline; the fallback
/// (inactive / folded / virtual-row) path paints each piece's pre-shaped
/// line side by side. A `ScaledLine` always holds ≥ 1 piece and at least
/// one piece with `scale > 1.0` (a fully-base row is the `None` fast path).
struct ScaledLine {
    /// Pieces in column order, contiguous, covering the whole row.
    pieces: Vec<ScaledPiece>,
    /// The tallest piece scale — the row's height multiplier
    /// (`row_scale`) and the shared-baseline ascent factor.
    max_scale: f32,
}

/// One contiguous run of display columns at a single scale within a
/// [`ScaledLine`].
struct ScaledPiece {
    /// First display column of this piece.
    start_col: u32,
    /// Number of display columns the piece spans.
    cols: u32,
    /// Font scale (`1.0` = base). Shaped at `font_size * scale`.
    scale: f32,
    /// The piece's text shaped at `font_size * scale` — fallback path.
    shaped: ShapedLine,
}

impl ScaledLine {
    /// The piece containing display column `col`, if any.
    fn piece_at(&self, col: u32) -> Option<&ScaledPiece> {
        self.pieces
            .iter()
            .find(|p| col >= p.start_col && col < p.start_col + p.cols)
    }

    /// The font scale at display column `col` (`1.0` past the last piece).
    fn scale_at(&self, col: u32) -> f32 {
        self.piece_at(col).map(|p| p.scale).unwrap_or(1.0)
    }

    /// The x offset (in `advance` units, from the row's text origin) of
    /// display column `col`, summing each preceding piece's scaled
    /// advance plus the partial advance within the piece holding `col`.
    /// `col` at/after the row end returns the full scaled width.
    fn x_offset(&self, col: u32, advance: Pixels) -> Pixels {
        let mut x = Pixels::ZERO;
        for p in &self.pieces {
            let end = p.start_col + p.cols;
            if col >= end {
                x += advance * p.scale * (p.cols as f32);
            } else {
                x += advance * p.scale * (col.saturating_sub(p.start_col) as f32);
                return x;
            }
        }
        x
    }
}

/// State produced in `prepaint`, consumed by `paint`.
pub(crate) struct EditorElementPrepaintState {
    /// One `ShapedLine` per visible doc row (top-of-viewport
    /// first).
    shaped_text: Vec<ShapedLine>,
    /// One `ShapedLine` per visible doc row for the gutter (fold
    /// marker + severity sign + line number). Length matches
    /// `shaped_text`. Empty when `self.gutter` was empty.
    shaped_gutter: Vec<ShapedLine>,
    /// Pre-shaped cursor char for block cursors (re-stamped on top
    /// of the cursor quad in `cursor_foreground`). `None` for
    /// bar/underline cursors or when cursor is off-screen.
    shaped_cursor_char: Option<ShapedLine>,
    /// Cursor (char_column, row_in_viewport). `None` when cursor
    /// is `None` or off-screen.
    cursor_layout: Option<(u32, u32)>,
    /// Pixel height per shaped line.
    line_height: Pixels,
    /// Pixel width per monospace glyph (`font_size * 0.6`).
    glyph_advance: Pixels,
    /// Pixel width of the gutter column. Text + cursor anchor at
    /// `bounds.origin.x + gutter_width_px`.
    gutter_width_px: Pixels,
    /// Per-visible-row (absolute_buffer_line_idx, line_text_clone).
    /// Slice X3.full.3 reads this in `paint` to convert decoration
    /// byte ranges → char columns for `paint_quad` placement,
    /// without re-splitting `self.text` on the hot path.
    /// One entry per row; length matches `shaped_text`.
    row_meta: Vec<(u32, String)>,
    /// W.5 (soft-wrap): per-display-row wrap-segment index. `0` for
    /// the first display row of a source line (and for every row when
    /// wrapping is off); `1, 2, …` for continuation rows. `paint`
    /// reads this with `wrap_width` to paint the cell sub-slice
    /// (`CellRow::segment(seg, wrap_width)`) for the active pane.
    /// Length matches `shaped_text`.
    row_segment: Vec<u32>,
    /// Per-visible-row inlay-hint metadata (slice X3.full.4).
    /// Each row carries a sorted `Vec<(orig_byte, char_width)>`:
    /// orig_byte is the utf-8 byte offset INTO THE ORIGINAL LINE
    /// where the hint was inserted; char_width is the number of
    /// chars the inlay text occupies in the spliced combined line.
    /// Used by `byte_to_combined_col` to remap cursor + decoration
    /// byte coordinates onto the post-splice column space (so the
    /// cursor block stays under the right glyph, etc.). Empty
    /// vector when the row has no inlays. Length matches
    /// `shaped_text`.
    ///
    /// Currently only read inside `prepaint` (cursor + diagnostic +
    /// overlay segments all derive their column-remap from these
    /// offsets and pre-bake the final column tuples). The field is
    /// preserved on the prepaint state so future per-row read paths
    /// (e.g. hover-on-inlay, click-to-jump) can access it without
    /// changing the struct shape.
    #[allow(dead_code)]
    inlay_offsets_per_row: Vec<Vec<(u32, u32)>>,
    /// Per-visible-row diagnostic underline segments (slice
    /// X3.full.4). Each entry is `(col_start, col_end_exclusive,
    /// color)` in combined-column space (after inlay splicing).
    /// Pre-computed in `prepaint` so `paint` is a simple walk +
    /// `paint_quad`. Length matches `shaped_text`.
    diagnostic_segments_per_row: Vec<Vec<(u32, u32, u32)>>,
    /// Per-visible-row overlay quads (perf-plan slice E.1). Each
    /// entry is `(col_start, col_end_exclusive, color)` in
    /// combined-column space — same shape as
    /// `diagnostic_segments_per_row`. Built in `prepaint` by
    /// walking the five overlay layers in fixed precedence
    /// (doc_highlights → all_matches → current_match → visual →
    /// substitute) so `paint` is a single allocation-free quad
    /// emit per row. `paint_quad` overwrites (no blending in gpui
    /// 0.2.2), so later quads in each row's `Vec` win visually —
    /// preserving the layering order the previous per-row × per-
    /// layer loop encoded. Inactive panes only carry doc-highlight
    /// quads (the other layers are active-pane state). Length
    /// matches `shaped_text`.
    overlay_quads_per_row: Vec<Vec<(u32, u32, u32)>>,
    /// S4.final.b (2026-05-27): the body font as captured in
    /// `prepaint` (from `window.text_style().font()`). Re-used
    /// by `paint_cells_row` for the resolve path's `layout_line`
    /// run; identical to the font `shape_line` was called with
    /// so cache keys line up across paths.
    font: gpui::Font,
    /// S4.final.b (2026-05-27): the body font size (`Pixels`) as
    /// captured in `prepaint`. Passed straight to
    /// `Window::paint_glyph` and to the resolver's
    /// `layout_line` call.
    font_size: Pixels,
    /// S4.final.b (2026-05-27): the typographic ascent of the
    /// body font at `font_size`. `Window::paint_glyph` takes
    /// its origin as the *baseline*; `paint_cells_row` derives
    /// the baseline from `line_y + text_ascent`.
    text_ascent: Pixels,
    /// W.5 (soft-wrap): the active matrix's wrap column width (`0`
    /// when wrapping is off / inactive pane / no matrix). `paint`
    /// uses it with `row_segment[i]` to slice the cell row
    /// (`CellRow::segment`) so each display row paints only its
    /// segment's columns. Read once from `display_matrix` /
    /// `cell_matrix` so the renderer and the host scroll model
    /// (which counts `segment_count`) agree on segment geometry.
    wrap_width: u32,
    /// F.2 (Thread F): per-display-row font-size / row-height multiplier
    /// (`syntax.heading.N` → `scale`; `1.0` for body + virtual rows).
    /// Length matches `shaped_text`. `paint` cumulative-sums it into
    /// per-row tops (variable row height) and scales the per-row glyph
    /// advance + font metrics, so a heading renders bigger AND wider on
    /// GPUI. The TUI peer has no analogue (a cell grid cannot vary font
    /// size); it degrades to the resolved bold/weight/underline.
    row_scale: Vec<f32>,
    /// F.2 (Thread F): per-row heading split (base marker prefix + scaled
    /// title). `Some` only for scaled heading rows; `None` for ordinary
    /// rows (1:1 with `shaped_text`). Drives the title-only scaling in
    /// both paint paths so the leading `#` markers stay base-size.
    row_split: Vec<Option<ScaledLine>>,
    /// L4a.3 (lsp-architecture.md §15): the inline cursor-line
    /// diagnostic summary, pre-shaped, as `(viewport_row, shaped)`.
    /// `Some` only when `self.inline_diag_summary` is set and its line
    /// is a visible row, as `(row, end_col, shaped)`. `end_col` is the
    /// source line's TRUE painted column count (source + inlay cells)
    /// from the cell matrix — the same width the cursor uses at EOL —
    /// when wrap is off; `None` for wrapped rows (paint falls back to
    /// the row's shaped width, which is segment-local). `paint` puts
    /// the summary at `text_origin_x + advance*end_col` (or the shaped
    /// width). Using the cell column count avoids landing mid-line when
    /// the cell + combined column models differ (inlay edge cases).
    inline_diag_overlay: Option<(usize, Option<u32>, ShapedLine)>,
    /// DB.4-gpui: the parsed dashboard branding composition, if the
    /// viewport holds a `BrandingBlock` virtual-row group. `paint` draws
    /// it as a 2-D composition (quad mark + shaped wordmark) over the
    /// blanked branding rows. `None` for every non-dashboard buffer.
    branding: Option<BrandingPaint>,
}

/// DB.4-gpui: the dashboard branding composition, parsed from a
/// `VirtualRowKind::BrandingBlock` row group's cells and laid out by the
/// GPUI peer as a 2-D image the flat cell grid can't express — the mark
/// as crisp square quads (corner cuts preserved), the "Lattice" wordmark
/// shaped large and vertically centred beside it. The TUI peer paints the
/// same rows as cells (its terminal-art treatment); this struct is
/// GPUI-only. Tunable geometry lives in `paint` (tile size, gap, wordmark
/// scale) so the look can be dialled in-app.
struct BrandingPaint {
    /// Display-row index of the first branding row (its `y` = `row_top`).
    first_row: usize,
    /// Number of branding rows in the group (defines the vertical box).
    row_count: usize,
    /// Mark grid extent (rows × cols), for square-tile layout.
    mark_rows: u32,
    mark_cols: u32,
    /// Each mark block as `(local_row, col, rgb)` — logo blue or cursor
    /// amber, straight from the cell fg. Absent grid cells (the cut
    /// corners) simply have no tile, so the negative space is preserved.
    tiles: Vec<(u32, u32, u32)>,
    /// The wordmark + tagline + version as `(text, rgb)`, parsed from the
    /// row group. The version is shaped at base font size (not 3.7x).
    wordmark: (String, u32),
    tagline: (String, u32),
    version: (String, u32),
}

/// The full-block glyph the branding provider uses for a mark cell (both
/// the logo bracket and the amber cursor bar). GPUI turns each such cell
/// into a square quad.
const BRANDING_MARK_GLYPH: u32 = 0x2588; // █

/// DB.4-gpui: parse a `BrandingBlock` row group (each entry is
/// `(display_row_index, cells)`) into a [`BrandingPaint`]. Block-glyph
/// cells become mark tiles (keyed by their fg colour); the remaining
/// printable cells accumulate per row into the wordmark (first text row)
/// and tagline (second). Returns `None` if there are no mark tiles.
fn build_branding_paint(
    rows: &[(usize, std::sync::Arc<[lattice_cells::Cell]>)],
) -> Option<BrandingPaint> {
    let first_row = rows.first()?.0;
    let mut tiles: Vec<(u32, u32, u32)> = Vec::new();
    // Text segments keyed by (row_index, segment_index) — a segment
    // covers a run of printable cells with the same FG colour. Color
    // changes (e.g. the brand-blue wordmark → default-fg version) produce
    // adjacent segments rather than one merged string, so each one
    // retains its own theme-resolved colour.
    let mut segments: Vec<(usize, String, u32)> = Vec::new();
    for (gi, (_, cells)) in rows.iter().enumerate() {
        let mut seg_text = String::new();
        let mut seg_color = 0u32;
        for (col, cell) in cells.iter().enumerate() {
            if cell.codepoint == BRANDING_MARK_GLYPH {
                tiles.push((gi as u32, col as u32, cell.fg));
            } else if let Some(ch) = char::from_u32(cell.codepoint) {
                if ch == ' ' {
                    if !seg_text.is_empty() {
                        seg_text.push(' ');
                    }
                } else if !ch.is_control() {
                    // Colour changed from the current segment?  Finalise
                    // the old segment and start a new one with the new
                    // colour so each themed run stays distinct.
                    if seg_color != cell.fg && !seg_text.is_empty() && seg_color != 0 {
                        let trimmed = seg_text.trim_end().to_string();
                        if !trimmed.is_empty() {
                            segments.push((gi, trimmed, seg_color));
                        }
                        seg_text.clear();
                        seg_color = cell.fg;
                    }
                    if seg_color == 0 && cell.fg != 0 {
                        seg_color = cell.fg;
                    }
                    seg_text.push(ch);
                }
            }
        }
        let trimmed = seg_text.trim_end();
        if !trimmed.is_empty() {
            segments.push((gi, trimmed.to_string(), seg_color));
        }
    }
    if tiles.is_empty() {
        return None;
    }

    // DB.4-tui-double: the TUI dashboard mark doubles its column count
    // (10 vs 5 logical) to compensate for the terminal cell aspect ratio
    // (~2:1 height:width). The GPUI peer renders with square tiles, so
    // halve the column resolution — keep only even columns and divide by
    // 2 — to restore the logical 5×6 proportion. A width >= 8 cols is
    // well beyond the original 5-col mark, so it serves as the detector.
    if tiles.iter().any(|t| t.1 >= 8) {
        tiles.retain(|t| t.1 % 2 == 0);
        for t in tiles.iter_mut() {
            t.1 /= 2;
        }
    }

    let min_row = tiles.iter().map(|t| t.0).min().unwrap_or(0);
    let max_row = tiles.iter().map(|t| t.0).max().unwrap_or(0);
    let max_col = tiles.iter().map(|t| t.1).max().unwrap_or(0);
    for t in tiles.iter_mut() {
        t.0 -= min_row;
    }
    // Segments are already in row-then-column order (produced by the
    // cell scan above). Wordmark = first segment; version = second
    // segment on the same row (if any); tagline = first segment on the
    // next row (if any).
    let wordmark = segments
        .first()
        .map(|s| (s.1.clone(), s.2))
        .unwrap_or_default();
    let first_seg_row = segments.first().map(|s| s.0);
    let version = first_seg_row
        .and_then(|row| segments.get(1).filter(|s| s.0 == row))
        .map(|s| (s.1.clone(), s.2))
        .unwrap_or_default();
    let tagline = first_seg_row
        .and_then(|row| segments.iter().find(|s| s.0 != row))
        .map(|s| (s.1.clone(), s.2))
        .unwrap_or_default();
    Some(BrandingPaint {
        first_row,
        row_count: rows.len(),
        mark_rows: max_row - min_row + 1,
        mark_cols: max_col + 1,
        tiles,
        wordmark,
        tagline,
        version,
    })
}

impl IntoElement for EditorElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for EditorElement {
    type RequestLayoutState = EditorElementLayoutState;
    type PrepaintState = EditorElementPrepaintState;

    fn id(&self) -> Option<ElementId> {
        Some(ElementId::Name(SharedString::from(format!(
            "lattice-editor-pane-{}",
            self.pane_idx
        ))))
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.flex_grow = 1.0;
        style.size.width = Length::Definite(DefiniteLength::Fraction(1.0));
        style.size.height = Length::Definite(DefiniteLength::Fraction(1.0));
        let layout_id = window.request_layout(style, [], cx);
        (layout_id, EditorElementLayoutState)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        let raw_lines: Vec<&str> = self.text.split('\n').collect();

        let text_style = window.text_style();
        let mut font = text_style.font();
        if !self.theme.ligatures {
            font.features = FontFeatures::disable_ligatures();
        }
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let line_height: Pixels = font_size * 1.3;
        // S4.final.b (2026-05-27): typographic ascent of the
        // body font at `font_size`. `Window::paint_glyph`
        // expects its origin to be the *baseline*; paint_cells
        // derives the per-row baseline as `line_y + text_ascent`.
        // Computed once here (LineLayoutCache caches the
        // FontMetrics lookup behind it) and stashed on the
        // prepaint state.
        let text_ascent: Pixels = {
            let font_id = window.text_system().resolve_font(&font);
            window.text_system().ascent(font_id, font_size)
        };
        // Measure the actual advance width of one monospace cell by
        // shaping a reference character. GPUI's LineLayoutCache
        // memoises the result, so this shape call costs one hash
        // lookup after the first frame. Fallback to the 0.6
        // approximation if shaping fails (shouldn't happen for any
        // real font, but keeps the renderer non-panicking).
        let glyph_advance: Pixels = {
            let ref_run = TextRun {
                len: 1,
                font: font.clone(),
                color: gpui::Rgba::default().into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            window
                .text_system()
                .shape_line(SharedString::from("M"), font_size, &[ref_run], None)
                .width
        };
        let gutter_chars: usize = self.content_left_pad as usize
            + if self.gutter.is_empty() {
                0
            } else {
                // PU.1b-1a: the leading `2` is the severity + diff sign
                // cells — dropped when `signcolumn=no` so the px width
                // tracks `format_gutter_text`'s gated output.
                //
                // The trailing `3` is the separator space after the digits
                // (1) + the fold-marker cell (1) + the trailing gap after the
                // glyph (1) — the `… 99 ▸ code` layout. This MUST match
                // `format_gutter_text`'s full width (`{sev}{diff}{num:>width$}
                // {fold} ` = sign + digits + 3) and the virtual-row
                // reservation (`gutter_width + 3 + sign_cells`). If it
                // undercounts, the code's first column lands inside the
                // gutter and the line number runs flush against the code.
                let sign_cells = if self.sign_column { 2 } else { 0 };
                sign_cells + self.gutter_width + 3
            };
        let gutter_width_px: Pixels = glyph_advance * (gutter_chars as f32);

        let row_capacity = self.gutter.len().max(self.viewport_height as usize);
        let mut shaped_text = Vec::with_capacity(row_capacity);
        let mut shaped_gutter = Vec::with_capacity(self.gutter.len());
        let mut row_meta: Vec<(u32, String)> = Vec::with_capacity(row_capacity);
        // W.5 (soft-wrap): per-display-row wrap-segment index, 1:1 with
        // `shaped_text`. `paint` reads it with `wrap_width` to paint
        // the right cell sub-slice for each display row.
        let mut row_segment: Vec<u32> = Vec::with_capacity(row_capacity);
        // F.2 (Thread F): per-display-row font-size / row-height
        // multiplier, 1:1 with `shaped_text`. `1.0` for body + virtual
        // rows; `> 1.0` for scaled heading rows. `paint` cumulative-sums
        // it into per-row tops (variable row height) + scales the per-row
        // glyph advance / font metrics. The TUI peer has no analogue.
        let mut row_scale: Vec<f32> = Vec::with_capacity(row_capacity);
        // F.2: per-row heading split (None for ordinary rows).
        let mut row_split: Vec<Option<ScaledLine>> = Vec::with_capacity(row_capacity);
        let mut inlay_offsets_per_row: Vec<Vec<(u32, u32)>> = Vec::with_capacity(row_capacity);
        let mut diagnostic_segments_per_row: Vec<Vec<(u32, u32, u32)>> =
            Vec::with_capacity(row_capacity);
        let mut overlay_quads_per_row: Vec<Vec<(u32, u32, u32)>> = Vec::with_capacity(row_capacity);
        // DB.4-gpui: `(display_row, cells)` for each BrandingBlock virtual
        // row emitted this frame; parsed into a `BrandingPaint` below.
        let mut branding_rows: Vec<(usize, std::sync::Arc<[lattice_cells::Cell]>)> = Vec::new();
        // D.3.b.1.gpui (2026-05-29): for each entry in
        // `self.gutter`, the shaped_text row index of the
        // corresponding doc row after virtual-row interleaving.
        // Cursor lookup remaps through this Vec.
        let mut doc_to_shaped_row_local: Vec<u32> = Vec::with_capacity(self.gutter.len());

        // Slice X3.full.4: precompute per-line inlay-hint lists,
        // sorted by byte offset, so the per-row shaping loop just
        // filters by line. Typical doc has 0-3 inlays per line; the
        // sort is over the buffer's full hint set (also typically
        // small -- hundreds at worst).
        let mut sorted_inlays: Vec<&InlayHintRow> = self.inlay_hints.iter().collect();
        sorted_inlays.sort_by_key(|h| (h.line, h.byte));

        // W.5 (soft-wrap): build a source line's (combined, runs,
        // inlay_offsets) WITHOUT shaping — `push_wrapped_doc_row`
        // shapes per wrap segment. Prefers the canonical
        // `DisplayMatrix` row (style-tagged runs resolved → `TextRun`s
        // via the host theme, replacing the retired
        // `shape_row_from_cells`); folded / out-of-window / stale rows
        // fall through to the `build_line_with_inlays` walk over
        // default-styled spans + LSP inlay hints. (For the active pane
        // the body glyphs come from `paint_cells_row`; this shape is
        // the fallback for inactive panes / boot frames / folded
        // rows. display-line B4.2: the worker's `visible_spans` /
        // `visible_rows` prepaint cache the fallback used to read was
        // deleted — the fallback now renders default-styled text.)
        let build_runs =
            |line: &str, _rel: usize, line_idx: u32| -> (String, Vec<TextRun>, Vec<(u32, u32)>) {
                // 2026-06-02: parity with TUI cells-empty fallback. A
                // display row may exist but be empty (transient
                // doc-switch / new-buffer publish race); in that case
                // the rope line is the source of truth.
                let display_row = self
                    .display_matrix
                    .as_ref()
                    .and_then(|m| m.row_at_source_line(line_idx))
                    .filter(|dl| !dl.text.is_empty() || line.is_empty());
                if let Some(dl) = display_row {
                    display_line_to_text_runs(dl, &self.resolved_theme, &self.theme_ids, &font)
                } else {
                    // B4.1 (2026-06-20): rows the canonical `DisplayMatrix`
                    // doesn't cover (boot / stale / out-of-window /
                    // transient post-split) render DEFAULT-styled — empty
                    // spans, mirroring the TUI's plain-text fallback (DR.3).
                    // B4.1 severed the GPU peer's last `visible_spans` read;
                    // B4.2 deleted the cache. Covered rows (the steady
                    // state) get full syntax colour from `display_matrix`
                    // above. (`build_line_with_inlays` still splices LSP
                    // inlays; with empty spans the line text is default-fg.)
                    let line_spans: &[StyledSpan] = &[];
                    let inlays_on_line: Vec<(usize, &str)> = sorted_inlays
                        .iter()
                        .filter(|h| h.line == line_idx)
                        .map(|h| (h.byte as usize, h.text.as_str()))
                        .collect();
                    build_line_with_inlays(
                        line,
                        line_spans,
                        &inlays_on_line,
                        &font,
                        self.inlay_color,
                        &self.resolved_theme,
                        &self.theme_ids,
                    )
                }
            };

        // W.5: the active matrix's wrap column width (0 = wrapping
        // off / inactive pane / no matrix → every line is one segment,
        // a byte-identical non-wrapping render). Read from the
        // canonical `DisplayMatrix` first, then the `CellMatrix`, so
        // the renderer and the host scroll model (which counts
        // `segment_count`) agree on display-row geometry.
        let wrap_width: u32 = self
            .display_matrix
            .as_ref()
            .map(|m| m.wrap_width)
            .or_else(|| self.cell_matrix.as_ref().map(|m| m.wrap_width))
            .unwrap_or(0);

        // Per-row diagnostic-segment computation. Walks
        // `self.diagnostic_underlines` against (line_idx, line_text,
        // inlay_offsets); returns (col_start, col_end_excl, color)
        // tuples in combined-column space.
        let diag_segments_for_row = |line_idx: u32,
                                     line_text: &str,
                                     inlay_offsets: &[(u32, u32)]|
         -> Vec<(u32, u32, u32)> {
            let mut segs = Vec::new();
            let line_len = line_text.len();
            for d in &self.diagnostic_underlines {
                let r = &d.range;
                if line_idx < r.start.line || line_idx > r.end.line {
                    continue;
                }
                let start_byte = if line_idx == r.start.line {
                    (r.start.byte as usize).min(line_len)
                } else {
                    0
                };
                let end_byte = if line_idx == r.end.line {
                    (r.end.byte as usize).min(line_len)
                } else {
                    line_len
                };
                if end_byte <= start_byte {
                    continue;
                }
                let col_start = byte_to_combined_col(line_text, start_byte, inlay_offsets) as u32;
                let col_end = byte_to_combined_col(line_text, end_byte, inlay_offsets) as u32;
                if col_end <= col_start {
                    continue;
                }
                segs.push((col_start, col_end, d.color));
            }
            segs
        };

        // Perf-plan slice E.1: per-row overlay quads. Walks the
        // five overlay layers in fixed precedence and emits
        // `(col_start, col_end, color)` tuples — same shape as
        // `diagnostic_segments_per_row`. Layering is encoded by
        // push order: `paint_quad` overwrites, so the last quad
        // pushed for a given (col, row) wins visually. Doc-
        // highlights paint on every pane that shares the active
        // buffer; the other four layers are active-pane state.
        // Replaces the prior per-row × per-layer
        // `paint_range_overlay` calls that ran on the hot paint
        // loop.
        let is_active = self.is_active;
        let doc_highlights = &self.doc_highlights;
        let all_matches = &self.all_matches;
        let current_match = &self.current_match;
        let visual_range = &self.visual_range;
        let visual_block_extents = self.visual_block_extents;
        let substitute_matches = &self.substitute_matches;
        // Perf plan B.2 slice B.2.a: active pane consumes the
        // worker's pre-bucketed static quads as the base of
        // `overlay_quads_per_row` and only walks the cursor-coupled
        // layers (`current_match`, `visual_range`) per frame.
        // Inactive panes fall through to the legacy per-frame walk
        // for the only static layer they paint — doc_highlight.
        let worker_static_overlay_quads: Option<
            &[Vec<lattice_host::render_state::RowOverlayQuad>],
        > = self
            .worker_static_overlay_quads
            .as_ref()
            .map(|q| q.quads.as_ref());
        // T.6: overlay layer colors resolve from the registered theme
        // elements (shared with the TUI peer; closes the prior drift).
        // `DocHighlight` maps to `doc_highlight.read`: the `OverlayLayer`
        // enum carries no read/write/text distinction, so the worker-
        // emitted single layer renders as read. TODO(T.6): OverlayLayer
        // lacks doc-hl kind; renders as read — the worker-side kind
        // split is out of scope (TUI keeps its 3-way via its own `kind`
        // param on `document_highlight_style`).
        let resolved_theme = &self.resolved_theme;
        let theme_ids = self.theme_ids;
        let color_for_layer = |layer: lattice_host::render_state::OverlayLayer| -> u32 {
            let (id, fallback) = match layer {
                lattice_host::render_state::OverlayLayer::DocHighlight => {
                    (theme_ids.doc_highlight_read, 0x585b70)
                }
                lattice_host::render_state::OverlayLayer::AllMatches => {
                    (theme_ids.search_match, 0x6c7086)
                }
                lattice_host::render_state::OverlayLayer::Substitute => {
                    (theme_ids.substitute_preview, 0xf38ba8)
                }
            };
            resolved_theme
                .get(id)
                .bg
                .map(|c| c.to_rgb_u32(fallback))
                .unwrap_or(fallback)
        };
        // T.6: cursor-coupled overlay colors resolve from the registered
        // `search.current` / `selection` elements (shared with the TUI
        // peer's `match_style` / `visual_style`). Resolved once here, not
        // per row (paramount #1).
        let current_match_color: u32 = resolved_theme
            .get(theme_ids.search_current)
            .bg
            .map(|c| c.to_rgb_u32(0xf9e2af))
            .unwrap_or(0xf9e2af);
        let selection_color: u32 = resolved_theme
            .get(theme_ids.selection)
            .bg
            .map(|c| c.to_rgb_u32(0x45475a))
            .unwrap_or(0x45475a);
        let diff_tint_per_row = &self.diff_tint_per_row;
        let overlay_quads_for_row = |line_idx: u32,
                                     rel_row: usize,
                                     line_text: &str,
                                     inlay_offsets: &[(u32, u32)]|
         -> Vec<(u32, u32, u32)> {
            let mut quads: Vec<(u32, u32, u32)> = Vec::new();
            // D.3.e: full-row diff tint, painted FIRST so
            // every cursor / selection / search overlay
            // composites OVER it. Width is the line's
            // total combined columns (source chars +
            // inlay-virtual-text chars). Zero-width rows
            // (empty lines) get a 1-column tint so the
            // backdrop is still visible.
            if let Some(&Some(tint_color)) = diff_tint_per_row.get(rel_row) {
                let source_cols = line_text.chars().count() as u32;
                let inlay_cols: u32 = inlay_offsets.iter().map(|(_, w)| *w).sum();
                let total_cols = source_cols + inlay_cols;
                let width = total_cols.max(1);
                quads.push((0, width, tint_color));
            }
            if is_active {
                // Worker bucket carries doc_highlight + all_matches +
                // substitute already in combined-column space.
                // Splice cursor-coupled layers in between AllMatches
                // and Substitute to preserve the original precedence
                // (doc_highlight → all_matches → current_match →
                // visual → substitute).
                if let Some(rows) = worker_static_overlay_quads
                    && let Some(row) = rows.get(rel_row)
                {
                    for q in row {
                        match q.layer {
                            lattice_host::render_state::OverlayLayer::DocHighlight
                            | lattice_host::render_state::OverlayLayer::AllMatches => {
                                let cs = byte_to_combined_col(
                                    line_text,
                                    q.source_byte_start as usize,
                                    inlay_offsets,
                                ) as u32;
                                let ce = byte_to_combined_col(
                                    line_text,
                                    q.source_byte_end as usize,
                                    inlay_offsets,
                                ) as u32;
                                if ce > cs {
                                    quads.push((cs, ce, color_for_layer(q.layer)));
                                }
                            }
                            lattice_host::render_state::OverlayLayer::Substitute => {
                                // Defer substitute until after the
                                // cursor-coupled layers are pushed.
                            }
                        }
                    }
                } else {
                    // Worker bucket missing (boot before first
                    // recompute, or buffer mismatch). Fall back to
                    // the legacy per-frame walk so static overlays
                    // still paint correctly.
                    push_range_quads(
                        &mut quads,
                        doc_highlights,
                        line_idx,
                        line_text,
                        inlay_offsets,
                        color_for_layer(lattice_host::render_state::OverlayLayer::DocHighlight),
                    );
                    push_range_quads(
                        &mut quads,
                        all_matches,
                        line_idx,
                        line_text,
                        inlay_offsets,
                        color_for_layer(lattice_host::render_state::OverlayLayer::AllMatches),
                    );
                }
                if let Some(r) = current_match {
                    push_range_quads(
                        &mut quads,
                        std::slice::from_ref(r),
                        line_idx,
                        line_text,
                        inlay_offsets,
                        current_match_color,
                    );
                }
                // Blockwise visual: per-line column band
                // [start_col, end_col]. Both ends are inclusive
                // byte columns — match TUI's
                // `apply_match_overlay` semantics (`end + 1`
                // exclusive). The band paints on every line in
                // [start_line, end_line]; lines short of
                // `start_col` paint nothing. Blockwise
                // suppresses the linear `visual_range` overlay
                // since the host still publishes a charwise-
                // shaped fallback for it.
                if let Some(b) = visual_block_extents {
                    if line_idx >= b.start_line && line_idx <= b.end_line {
                        let line_len = line_text.len();
                        let start = (b.start_col as usize).min(line_len);
                        let end = ((b.end_col as usize) + 1).min(line_len);
                        if start < end {
                            let cs = byte_to_combined_col(line_text, start, inlay_offsets) as u32;
                            let ce = byte_to_combined_col(line_text, end, inlay_offsets) as u32;
                            if ce > cs {
                                quads.push((cs, ce, selection_color));
                            }
                        }
                    }
                } else if let Some(r) = visual_range {
                    push_range_quads(
                        &mut quads,
                        std::slice::from_ref(r),
                        line_idx,
                        line_text,
                        inlay_offsets,
                        selection_color,
                    );
                }
                // Push the deferred substitute layer last so it
                // sits on top of cursor + visual per the original
                // precedence. Worker bucket again preferred; legacy
                // walk fallback if no bucket exists.
                if let Some(rows) = worker_static_overlay_quads
                    && let Some(row) = rows.get(rel_row)
                {
                    for q in row {
                        if matches!(
                            q.layer,
                            lattice_host::render_state::OverlayLayer::Substitute
                        ) {
                            let cs = byte_to_combined_col(
                                line_text,
                                q.source_byte_start as usize,
                                inlay_offsets,
                            ) as u32;
                            let ce = byte_to_combined_col(
                                line_text,
                                q.source_byte_end as usize,
                                inlay_offsets,
                            ) as u32;
                            if ce > cs {
                                quads.push((cs, ce, color_for_layer(q.layer)));
                            }
                        }
                    }
                } else {
                    push_range_quads(
                        &mut quads,
                        substitute_matches,
                        line_idx,
                        line_text,
                        inlay_offsets,
                        color_for_layer(lattice_host::render_state::OverlayLayer::Substitute),
                    );
                }
            } else {
                // Inactive pane: only doc_highlight is painted
                // (the other static layers + cursor-coupled
                // layers are active-pane state). Bucket isn't
                // available for inactive panes; per-frame walk
                // stays on the cheap N path (doc_highlight is
                // capped tiny by the LSP response).
                push_range_quads(
                    &mut quads,
                    doc_highlights,
                    line_idx,
                    line_text,
                    inlay_offsets,
                    color_for_layer(lattice_host::render_state::OverlayLayer::DocHighlight),
                );
            }
            quads
        };

        if self.gutter.is_empty() {
            // Slice 1 fallback (no gutter metadata supplied):
            // walk the visible window directly. Folds aren't
            // applied here because the caller would need to
            // pre-filter the rows for fold-skipping; without that,
            // the fallback paints every line in
            // `[scroll, scroll+viewport_height)`.
            //
            // PU.2: `self.text` is the VISIBLE WINDOW starting at
            // `scroll` (caller-supplied), so `raw_lines` is indexed by
            // the window-relative offset `rel` (0-based) while the
            // ABSOLUTE source line is `scroll + rel` — the latter keys
            // the `DisplayMatrix` lookup in `build_runs`. Iterate `rel`
            // directly so the window is correct at ANY scroll: the prior
            // `[scroll, scroll+vh).min(raw_lines.len())` math clamped the
            // END to the window LENGTH, dropping `scroll`-many rows once
            // `scroll > 0` (only correct for `scroll == 0`). No
            // production caller passed an empty gutter before PU.2's
            // floating popup, so this fix is regression-free.
            let visible_count = (self.viewport_height.max(1) as usize).min(raw_lines.len());
            for rel in 0..visible_count {
                // W.5: respect the height cap (wrapped lines can fill
                // the viewport mid-window).
                if shaped_text.len() as u32 >= self.viewport_height {
                    break;
                }
                let line_idx = (self.scroll as usize).saturating_add(rel);
                let line = raw_lines.get(rel).copied().unwrap_or("");
                let (combined, runs, inlay_offsets) = build_runs(line, rel, line_idx as u32);
                let full_diag = diag_segments_for_row(line_idx as u32, line, &inlay_offsets);
                let full_overlay =
                    overlay_quads_for_row(line_idx as u32, rel, line, &inlay_offsets);
                // W.5: source line → `seg_count` display rows. Take the
                // larger of the cell-row and combined column counts so
                // neither the active-pane cells paint nor the
                // ShapedLine fallback drops a trailing segment if the
                // two column models differ (e.g. inlay edge cases).
                let cell_cols = self
                    .cell_matrix
                    .as_ref()
                    .and_then(|m| m.row_at_source_line(line_idx as u32))
                    .map(|r| r.col_count())
                    .unwrap_or(0);
                let body_cols = cell_cols.max(combined.chars().count() as u32);
                let seg_count = if wrap_width == 0 {
                    1
                } else {
                    lattice_cells::wrap_segments(body_cols, wrap_width).max(1)
                };
                // F.2: the heading split for this line (base marker
                // prefix cols + title scale), or `None` for ordinary text.
                let heading_scale = self
                    .display_matrix
                    .as_ref()
                    .and_then(|m| m.row_at_source_line(line_idx as u32))
                    .and_then(|dl| {
                        crate::cells_paint::heading_scale_split(
                            dl,
                            &self.resolved_theme,
                            &self.theme_ids,
                        )
                    });
                push_wrapped_doc_row(
                    line_idx as u32,
                    line,
                    &combined,
                    &runs,
                    inlay_offsets,
                    &full_diag,
                    &full_overlay,
                    seg_count,
                    wrap_width,
                    None,
                    self.gutter_width,
                    self.sign_column,
                    &font,
                    font_size,
                    heading_scale,
                    self.viewport_height,
                    window,
                    &mut shaped_text,
                    &mut shaped_gutter,
                    &mut row_meta,
                    &mut row_segment,
                    &mut row_scale,
                    &mut row_split,
                    &mut inlay_offsets_per_row,
                    &mut diagnostic_segments_per_row,
                    &mut overlay_quads_per_row,
                );
            }
        } else {
            // Gutter-driven walk: caller already pre-filtered the
            // visible rows (skipping folded lines) and built the
            // gutter metadata; the text rows mirror that filter.
            //
            // D.3.b.1.gpui (2026-05-29): around each visible doc
            // row, interleave Above- and Below-anchored virtual
            // rows from `self.virtual_rows`. The interleaver
            // tracks the shaped_text row index for each gutter
            // entry via `doc_to_shaped_row_local` so the cursor
            // lookup below remaps from doc-row index (position
            // in self.gutter) to shaped_text row.
            // 2026-06-03: cap interleaved (doc + virtual) display
            // rows at `viewport_height`, mirroring the TUI's
            // `compose_visible_lines_inner` `while out.len() <
            // height` loop. Without this, the gutter walk emits
            // `viewport_height` DOC rows and then stacks virtual
            // rows (excerpt headers) on top, overflowing the pane
            // and painting the surplus behind the modeline. The
            // host's `bottom_anchored_scroll` keeps the cursor
            // within this budget, so the cap only ever drops rows
            // below the cursor that wouldn't fit anyway. Parity
            // with the TUI peer per [[feedback_tui_gpui_parity]].
            // Sticky pre-pass: render fixed-top rows before scrollable content.
            // These are excluded from virtual_rows_at_gpui to avoid double-paint.
            for vrow in self.virtual_rows.sticky_rows() {
                if shaped_text.len() as u32 >= self.viewport_height {
                    break;
                }
                push_virtual_row(
                    vrow,
                    self.gutter_width,
                    &font,
                    font_size,
                    self.theme.foreground,
                    vrow.bg.unwrap_or(0),
                    window,
                    &mut shaped_text,
                    &mut shaped_gutter,
                    &mut row_meta,
                    &mut row_segment,
                    &mut row_scale,
                    &mut row_split,
                    &mut inlay_offsets_per_row,
                    &mut diagnostic_segments_per_row,
                    &mut overlay_quads_per_row,
                    &mut branding_rows,
                );
            }
            'rows: for meta in &self.gutter {
                if shaped_text.len() as u32 >= self.viewport_height {
                    break;
                }
                let line_idx = meta.line_idx as usize;
                let rel = line_idx.saturating_sub(self.scroll as usize);
                // 2026-05-26: raw_lines indexed by visible-row
                // offset (see fallback branch above).
                let line = raw_lines.get(rel).copied().unwrap_or("");
                // D.3.b.1.gpui: emit Above-anchored virtual rows
                // for this doc line first.
                for vrow in virtual_rows_at_gpui(
                    &self.virtual_rows,
                    meta.line_idx,
                    lattice_cells::AnchorPosition::Above,
                ) {
                    if shaped_text.len() as u32 >= self.viewport_height {
                        break 'rows;
                    }
                    push_virtual_row(
                        vrow,
                        self.gutter_width,
                        &font,
                        font_size,
                        self.theme.foreground,
                        self.diff_deletion_block_bg,
                        window,
                        &mut shaped_text,
                        &mut shaped_gutter,
                        &mut row_meta,
                        &mut row_segment,
                        &mut row_scale,
                        &mut row_split,
                        &mut inlay_offsets_per_row,
                        &mut diagnostic_segments_per_row,
                        &mut overlay_quads_per_row,
                        &mut branding_rows,
                    );
                }
                // The doc row itself must also respect the budget:
                // if Above-rows just filled the viewport, stop
                // before pushing it (and its gutter entry) so the
                // per-row vecs stay 1:1 and nothing paints past the
                // pane.
                if shaped_text.len() as u32 >= self.viewport_height {
                    break;
                }
                // Record the shaped_text row index of this doc row's
                // FIRST display segment for the cursor remap below.
                // The cursor adds its own segment index on top.
                doc_to_shaped_row_local.push(shaped_text.len() as u32);
                // W.5: build the line's (combined, runs, inlay_offsets)
                // un-shaped (see `build_runs`), its full-width overlay /
                // diagnostic quads, and the gutter for segment 0, then
                // expand into `seg_count` display rows via
                // `push_wrapped_doc_row`. The gutter-driven walk already
                // pre-filters folded lines, so coverage gaps only occur
                // on boot / buffer-switch (handled inside `build_runs`).
                let (combined, runs, inlay_offsets) = build_runs(line, rel, meta.line_idx);
                let full_diag = diag_segments_for_row(meta.line_idx, line, &inlay_offsets);
                let full_overlay = overlay_quads_for_row(meta.line_idx, rel, line, &inlay_offsets);
                let cell_cols = self
                    .cell_matrix
                    .as_ref()
                    .and_then(|m| m.row_at_source_line(meta.line_idx))
                    .map(|r| r.col_count())
                    .unwrap_or(0);
                let body_cols = cell_cols.max(combined.chars().count() as u32);
                let seg_count = if wrap_width == 0 {
                    1
                } else {
                    lattice_cells::wrap_segments(body_cols, wrap_width).max(1)
                };
                let gutter_text = format_gutter_text(
                    meta,
                    self.gutter_width,
                    self.sign_column,
                    self.show_line_numbers,
                );
                let gutter_runs =
                    build_gutter_runs(&gutter_text, meta, font.clone(), self.sign_column);
                let shaped_g = window.text_system().shape_line(
                    SharedString::from(gutter_text),
                    font_size,
                    &gutter_runs,
                    None,
                );
                // F.2: the heading split for this line (base marker
                // prefix cols + title scale), or `None` for ordinary text.
                let heading_scale = self
                    .display_matrix
                    .as_ref()
                    .and_then(|m| m.row_at_source_line(meta.line_idx))
                    .and_then(|dl| {
                        crate::cells_paint::heading_scale_split(
                            dl,
                            &self.resolved_theme,
                            &self.theme_ids,
                        )
                    });
                let capped = push_wrapped_doc_row(
                    meta.line_idx,
                    line,
                    &combined,
                    &runs,
                    inlay_offsets,
                    &full_diag,
                    &full_overlay,
                    seg_count,
                    wrap_width,
                    Some(shaped_g),
                    self.gutter_width,
                    self.sign_column,
                    &font,
                    font_size,
                    heading_scale,
                    self.viewport_height,
                    window,
                    &mut shaped_text,
                    &mut shaped_gutter,
                    &mut row_meta,
                    &mut row_segment,
                    &mut row_scale,
                    &mut row_split,
                    &mut inlay_offsets_per_row,
                    &mut diagnostic_segments_per_row,
                    &mut overlay_quads_per_row,
                );
                if capped {
                    break 'rows;
                }

                // D.3.b.1.gpui: emit Below-anchored virtual
                // rows after the doc row.
                for vrow in virtual_rows_at_gpui(
                    &self.virtual_rows,
                    meta.line_idx,
                    lattice_cells::AnchorPosition::Below,
                ) {
                    if shaped_text.len() as u32 >= self.viewport_height {
                        break 'rows;
                    }
                    push_virtual_row(
                        vrow,
                        self.gutter_width,
                        &font,
                        font_size,
                        self.theme.foreground,
                        self.diff_deletion_block_bg,
                        window,
                        &mut shaped_text,
                        &mut shaped_gutter,
                        &mut row_meta,
                        &mut row_segment,
                        &mut row_scale,
                        &mut row_split,
                        &mut inlay_offsets_per_row,
                        &mut diagnostic_segments_per_row,
                        &mut overlay_quads_per_row,
                        &mut branding_rows,
                    );
                }
            }
        }

        // Cursor layout + char pre-shaping.
        let (cursor_layout, shaped_cursor_char) = match &self.cursor {
            None => (None, None),
            Some(c) => {
                // Cursor row inside the visible viewport: linear
                // search over `self.gutter` (the source of truth
                // for "which absolute line is at viewport row R"
                // once folds are involved). If gutter is empty
                // (fallback), compute directly from `scroll`.
                let row_in_viewport = if self.gutter.is_empty() {
                    let row = (c.line as i64) - (self.scroll as i64);
                    if (0..self.viewport_height as i64).contains(&row) {
                        Some(row as u32)
                    } else {
                        None
                    }
                } else {
                    // D.3.b.1.gpui: position in self.gutter
                    // gives the doc-row index; remap through
                    // doc_to_shaped_row_local to the shaped_text
                    // row index (which includes interleaved
                    // virtual rows).
                    self.gutter
                        .iter()
                        .position(|m| m.line_idx == c.line)
                        .and_then(|p| doc_to_shaped_row_local.get(p).copied())
                };
                match row_in_viewport {
                    None => (None, None),
                    Some(row) => {
                        // 2026-05-26 (rev 2): `self.text` was zeroed
                        // in slice A.4 (`1e1da8d`) and `row_meta`
                        // is populated from the same empty source,
                        // so neither carries the cursor's line
                        // text. The caller (window.rs `paint_pane`)
                        // reads the cursor's source line from the
                        // document snapshot and passes it via
                        // `CursorState.line_text`.
                        let line: &str = c.line_text.as_str();
                        // Floor to a char boundary: a cursor byte that
                        // lands inside a multi-byte char (em-dash etc.)
                        // would panic the `line[byte..]` slice below.
                        // The cursor sits ON the char containing this
                        // byte, so the char's start is the right anchor.
                        let mut byte = (c.byte as usize).min(line.len());
                        while byte > 0 && !line.is_char_boundary(byte) {
                            byte -= 1;
                        }
                        // Slice X3.full.4: remap byte → combined col
                        // via the cursor row's inlay offsets so the
                        // block cursor stays under the right glyph
                        // when inlays are present earlier on the line.
                        let cursor_row_inlays: &[(u32, u32)] = inlay_offsets_per_row
                            .get(row as usize)
                            .map(Vec::as_slice)
                            .unwrap_or(&[]);
                        let char_col = byte_to_combined_col(line, byte, cursor_row_inlays) as u32;
                        // W.5 (soft-wrap): the combined column splits
                        // into a segment index (which display row of the
                        // wrapped line the cursor sits on) and the column
                        // within that segment. `row` is the line's first
                        // display segment (recorded in
                        // `doc_to_shaped_row_local`); the cursor adds its
                        // own segment. `wrap_width == 0` ⇒ the whole
                        // column, no extra row (pre-W.5 behaviour). This
                        // mirrors the TUI peer's
                        // `display_col / wrap_width` + `% wrap_width`.
                        let (own_segment, body_col) = if wrap_width > 0 {
                            (char_col / wrap_width, char_col % wrap_width)
                        } else {
                            (0, char_col)
                        };
                        let cursor_row = row + own_segment;
                        if cursor_row >= self.viewport_height {
                            // The cursor's wrapped segment fell past the
                            // viewport budget (its continuation rows were
                            // capped). Treat as off-screen — the host
                            // scroll model keeps the cursor in budget, so
                            // this only guards the transient overflow.
                            (None, None)
                        } else {
                            let shaped =
                                if matches!(c.shape, CursorShape::Block) && byte < line.len() {
                                    let rest = &line[byte..];
                                    let ch = rest.chars().next().unwrap_or(' ');
                                    let runs = vec![TextRun {
                                        len: ch.len_utf8(),
                                        font: font.clone(),
                                        color: rgb(self.theme.cursor_foreground).into(),
                                        background_color: None,
                                        underline: None,
                                        strikethrough: None,
                                    }];
                                    // F.2/F.3: a block cursor on a scaled row
                                    // shapes its covered char at that column's
                                    // scale — base over the markers, scaled
                                    // over the title — so the re-stamped glyph
                                    // matches the underlying text.
                                    let cur_scale = match row_split.get(cursor_row as usize) {
                                        Some(Some(sl)) => sl.scale_at(body_col),
                                        _ => 1.0,
                                    };
                                    Some(window.text_system().shape_line(
                                        SharedString::from(ch.to_string()),
                                        font_size * cur_scale,
                                        &runs,
                                        None,
                                    ))
                                } else {
                                    None
                                };
                            (Some((body_col, cursor_row)), shaped)
                        }
                    }
                }
            }
        };

        // L4a.3: shape the inline cursor-line diagnostic summary (when
        // armed + its line is a visible row) into its own ShapedLine,
        // painted at the row's end-of-content x in `paint`. Active pane
        // only — `self.inline_diag_summary` is `None` on inactive panes.
        // Italic + severity colour mirror the TUI peer's eol style.
        let inline_diag_overlay = self.inline_diag_summary.as_ref().and_then(|sum| {
            // LAST visible row for the source line (the only row when
            // wrap is off; the final wrap segment otherwise) so the
            // summary trails the whole line, not the first segment.
            let row = row_meta.iter().rposition(|(l, _)| *l == sum.line)?;
            // True painted end column (source + inlay cells) from the
            // cell matrix — matches the cursor's EOL x. Only reliable
            // with wrap off (one row per line); wrapped rows use the
            // shaped width fallback in paint.
            let end_col = if wrap_width == 0 {
                self.cell_matrix
                    .as_ref()
                    .and_then(|m| m.row_at_source_line(sum.line))
                    .map(|r| r.col_count())
            } else {
                None
            };
            let mut italic = font.clone();
            italic.style = gpui::FontStyle::Italic;
            let runs = vec![TextRun {
                len: sum.text.len(),
                font: italic,
                color: rgb(sum.color).into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            }];
            let shaped = window.text_system().shape_line(
                SharedString::from(sum.text.clone()),
                font_size,
                &runs,
                None,
            );
            Some((row, end_col, shaped))
        });

        EditorElementPrepaintState {
            shaped_text,
            shaped_gutter,
            shaped_cursor_char,
            cursor_layout,
            line_height,
            glyph_advance,
            gutter_width_px,
            row_meta,
            row_segment,
            inlay_offsets_per_row,
            diagnostic_segments_per_row,
            overlay_quads_per_row,
            font: font.clone(),
            font_size,
            text_ascent,
            wrap_width,
            row_scale,
            row_split,
            inline_diag_overlay,
            branding: build_branding_paint(&branding_rows),
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        // Pane background.
        window.paint_quad(fill(bounds, rgb(self.theme.background)));
        let line_height = prepaint.line_height;
        let advance = prepaint.glyph_advance;
        let text_origin_x = bounds.origin.x + prepaint.gutter_width_px;
        // HS.1b: horizontal scroll. With wrap off, the body pans left
        // by `leftcol` display columns — `col_x` subtracts it (cursor
        // + selection/decoration quads) and the body-glyph paint
        // slices the first `leftcol` cells off each ordinary row, so
        // column `leftcol` lands at `text_origin_x` (never over the
        // gutter, no content mask needed). Pinned to 0 under wrap.
        // Heading-split rows are left unoffset for now (they rarely
        // pan); full coverage is a follow-up.
        let leftcol = if prepaint.wrap_width == 0 {
            self.leftcol
        } else {
            0
        };

        // F.2 (Thread F): per-display-row vertical metrics for variable
        // row height. A row's height is `line_height * row_scale[i]`
        // (1.0 for body + virtual rows; > 1.0 for scaled headings) and
        // `row_top(i)` is its cumulative top y. Built once here, O(rows);
        // every paint site below reads by index instead of the old
        // uniform `bounds.origin.y + line_height * i`. With all-1.0
        // scales this reduces to the prior arithmetic exactly.
        let row_count = prepaint.shaped_text.len();
        let mut row_tops: Vec<Pixels> = Vec::with_capacity(row_count);
        let mut row_heights: Vec<Pixels> = Vec::with_capacity(row_count);
        {
            let mut y = bounds.origin.y;
            for i in 0..row_count {
                let s = prepaint.row_scale.get(i).copied().unwrap_or(1.0);
                row_tops.push(y);
                let h = line_height * s;
                row_heights.push(h);
                y += h;
            }
        }
        // Index helpers with a uniform-height fallback for any row index
        // beyond the built vecs (defensive; the loops below stay in range).
        let row_top = |i: usize| -> Pixels {
            row_tops
                .get(i)
                .copied()
                .unwrap_or(bounds.origin.y + line_height * (i as f32))
        };
        let row_h = |i: usize| -> Pixels { row_heights.get(i).copied().unwrap_or(line_height) };
        // F.2/F.3: per-token scaling makes the glyph advance NON-uniform
        // within a scaled row — base over the base pieces, scaled over the
        // scaled ones. `col_x` maps a display column to its x pixel;
        // `col_scale` gives that column's font scale. Ordinary rows (no
        // split) reduce to the uniform `text_origin_x + advance * col`.
        let row_split = &prepaint.row_split;
        let col_scale = |i: usize, col: u32| -> f32 {
            match row_split.get(i) {
                Some(Some(sl)) => sl.scale_at(col),
                _ => 1.0,
            }
        };
        let col_x = |i: usize, col: u32| -> Pixels {
            match row_split.get(i) {
                // Scaled rows: sum each preceding piece's scaled advance.
                // No h-scroll offset on scaled content yet (follow-up).
                Some(Some(sl)) => text_origin_x + sl.x_offset(col, advance),
                // Ordinary rows pan left by `leftcol` (wrap off).
                _ => text_origin_x + advance * (col.saturating_sub(leftcol) as f32),
            }
        };
        // F.2: with variable row height the cumulative stack of
        // `viewport_height` rows can exceed the pane (the host still
        // estimates capacity at the uniform row height — see the deferred
        // F.1 follow-on). Clip rows whose top is at/below the pane bottom
        // so a tall heading never bleeds over the modeline below. With
        // all-1.0 scales no row is ever clipped (exactly `viewport_height`
        // uniform rows fit), so this is a no-op for ordinary buffers.
        let pane_bottom = bounds.origin.y + bounds.size.height;

        // Slice X3.full.3: per-row decoration backgrounds. Layered
        // bottom -> top so the strongest signal wins visually:
        //   cursorline (full row) -> doc_highlight -> hlsearch ->
        //   current_match -> visual -> substitute.
        // `paint_quad` overwrites (no blending in gpui 0.2.2), so
        // the last paint at a given (col, row) is what the user
        // sees. Inactive panes paint only the doc-highlight layer
        // (search / selection / substitute / cursorline are active-
        // pane state) so a tagged-but-inactive pane still hints at
        // symbol relations.
        if self.is_active && !prepaint.row_meta.is_empty() {
            // Cursorline: paint a full-row quad on whichever
            // visible row hosts the cursor — but only when
            // `:set cursorline` is on (`current-line-highlight`).
            // Mirrors the TUI gate so `:set nocursorline` works in
            // both renderers; default-off means no quad until opt-in.
            if self.cursorline_enabled
                && let Some((_, cur_row)) = prepaint.cursor_layout
            {
                let row_y = row_top(cur_row as usize);
                let pane_width = bounds.size.width;
                let row_bounds = Bounds::new(
                    point(bounds.origin.x, row_y),
                    gpui::size(pane_width, row_h(cur_row as usize)),
                );
                window.paint_quad(fill(row_bounds, rgb(self.cursorline_bg)));
            }
        }
        // Per-line range overlays (perf-plan slice E.1). Quads
        // were pre-built in `prepaint` (see
        // `overlay_quads_per_row` + `push_range_quads`); paint
        // here is an allocation-free walk that mirrors the
        // diagnostic-underline loop below. Layering order is
        // encoded by push order in the inner Vec — `paint_quad`
        // overwrites, so later quads win visually.
        for (row_idx, quads) in prepaint.overlay_quads_per_row.iter().enumerate() {
            if quads.is_empty() {
                continue;
            }
            let row_y = row_top(row_idx);
            if row_y >= pane_bottom {
                break;
            }
            for (col_start, col_end, color) in quads {
                let quad_x = col_x(row_idx, *col_start);
                let quad_w = col_x(row_idx, *col_end) - quad_x;
                let quad_bounds = Bounds::new(point(quad_x, row_y), size(quad_w, row_h(row_idx)));
                window.paint_quad(fill(quad_bounds, rgb(*color)));
            }
        }

        // Gutter.
        for (i, shaped_g) in prepaint.shaped_gutter.iter().enumerate() {
            // F.2: the gutter (line number) stays at the base font size,
            // vertically centered within the (possibly taller) row by
            // passing the row's height as the line box.
            let line_y = row_top(i);
            if line_y >= pane_bottom {
                break;
            }
            // DB.4: the shaped gutter text is `gutter_chars − content_left_pad`
            // cells wide (`format_gutter_text` omits the centring pad), while
            // the body starts at the full `gutter_width_px`. Paint the gutter
            // shifted right by the pad so its RIGHT edge meets the body —
            // otherwise the gutter (and its fold marker) strands at the pane's
            // left edge while the centred content sits far to the right. The
            // TUI peer gets this for free by folding the pad into `gutter_w`
            // and right-aligning the glyph. `content_left_pad` is 0 for
            // non-centred buffers, so this is a no-op off the dashboard.
            let gutter_x = bounds.origin.x + prepaint.glyph_advance * self.content_left_pad as f32;
            let origin = point(gutter_x, line_y);
            if let Err(err) = shaped_g.paint(origin, row_h(i), window, cx) {
                tracing::warn!(
                    target: "lattice_gpui::editor_element",
                    row = i,
                    pane = self.pane_idx,
                    error = ?err,
                    "gutter ShapedLine::paint failed"
                );
            }
        }

        // Text body. S4.final.f: `paint_cells_row` is the default
        // for active-pane document bodies when `ui.ligatures=false`.
        // When `ui.ligatures=true` (LG.1), bg quads still come from
        // the cell matrix (for syntax-token backgrounds) but glyphs
        // are emitted by `ShapedLine::paint` — which shapes the full
        // multi-char TextRun so OpenType ligature sequences form.
        // Folded rows / boot frames / buffer-switch gaps / inactive
        // panes (`cell_matrix == None`) fall through to
        // `ShapedLine::paint` — same behaviour regardless of the
        // ligatures flag.
        let ligatures = self.theme.ligatures;
        let use_paint_cells = self.cell_matrix.is_some();
        for (i, shaped_line) in prepaint.shaped_text.iter().enumerate() {
            let line_y = row_top(i);
            if line_y >= pane_bottom {
                break;
            }
            let origin = point(text_origin_x, line_y);
            let painted_via_cells = if use_paint_cells {
                let row_meta_entry = prepaint.row_meta.get(i);
                match (self.cell_matrix.as_ref(), row_meta_entry) {
                    (Some(matrix), Some((line_idx, line_text))) => {
                        match matrix.row_at_source_line(*line_idx) {
                            // 2026-06-03 fix: mirror prepaint's
                            // cell-row filter
                            // (`!cells.is_empty() || line.is_empty()`).
                            // An empty cells row over a NON-empty line
                            // means the cells worker hasn't produced
                            // content for this composed row yet (the
                            // multibuffer search view's composed lines
                            // sit empty in the matrix until covered).
                            // prepaint already built the correct legacy
                            // `ShapedLine` for it; painting the empty
                            // cells row here draws zero glyphs AND sets
                            // `painted_via_cells = true`, suppressing
                            // that ShapedLine — which blanked the first
                            // match line on the active pane (inactive
                            // panes have `cell_matrix == None`, so they
                            // always took the ShapedLine path and looked
                            // correct). Fall through to ShapedLine when
                            // the cells row can't cover the line.
                            Some(cell_row)
                                if !cell_row.cells.is_empty() || line_text.is_empty() =>
                            {
                                // W.5 (soft-wrap): paint only this
                                // display row's wrap segment. `segment`
                                // slices `[seg·w, (seg+1)·w)`; with
                                // `wrap_width == 0` segment 0 is the
                                // whole row (byte-identical to pre-W.5).
                                let seg = prepaint.row_segment.get(i).copied().unwrap_or(0);
                                let seg_cells = cell_row.segment(seg, prepaint.wrap_width);
                                // HS.1b: pan ordinary rows left by
                                // `leftcol` cells (wrap off) so column
                                // `leftcol` paints at `text_origin_x`.
                                // Heading-split rows keep their prefix
                                // math and are not offset yet.
                                let seg_cells = if leftcol > 0
                                    && row_split.get(i).is_none_or(|s| s.is_none())
                                {
                                    &seg_cells[(leftcol as usize).min(seg_cells.len())..]
                                } else {
                                    seg_cells
                                };
                                // A continuation segment with no cells but
                                // whose fallback ShapedLine carries text
                                // (column models diverged) falls through so
                                // the ShapedLine segment paints it.
                                if seg_cells.is_empty() && seg > 0 && !line_text.is_empty() {
                                    false
                                } else {
                                    // F.2/F.3 (Thread F): a scaled row paints
                                    // in N pieces sharing ONE baseline — each
                                    // piece's column run at its own scale
                                    // (base markers, scaled title; base mark
                                    // blocks, scaled wordmark). Ordinary rows
                                    // paint once at base (byte-identical to
                                    // pre-F.2). LG.1 ligatures-on emits bg-only
                                    // here + glyphs via the ShapedLine
                                    // fallback.
                                    match row_split.get(i) {
                                        Some(Some(sl)) => {
                                            // Shared baseline = the tallest
                                            // piece's ascent, so base pieces
                                            // sit on the scaled baseline.
                                            let shared_ascent = prepaint.text_ascent * sl.max_scale;
                                            for p in &sl.pieces {
                                                let start =
                                                    (p.start_col as usize).min(seg_cells.len());
                                                let end = ((p.start_col + p.cols) as usize)
                                                    .min(seg_cells.len());
                                                let piece_cells = &seg_cells[start..end];
                                                let piece_origin = point(
                                                    origin.x + sl.x_offset(p.start_col, advance),
                                                    line_y,
                                                );
                                                if ligatures {
                                                    crate::paint_cells::paint_cells_row_bg_only(
                                                        piece_cells,
                                                        piece_origin,
                                                        advance * p.scale,
                                                        row_h(i),
                                                        window,
                                                    );
                                                } else {
                                                    crate::paint_cells::paint_cells_row(
                                                        piece_cells,
                                                        piece_origin,
                                                        advance * p.scale,
                                                        row_h(i),
                                                        shared_ascent,
                                                        &prepaint.font,
                                                        prepaint.font_size * p.scale,
                                                        self.theme.foreground,
                                                        &self.glyph_resolver,
                                                        window,
                                                    );
                                                }
                                            }
                                            // ligatures: glyphs still come
                                            // from the ShapedLine fallback.
                                            !ligatures
                                        }
                                        _ => {
                                            if ligatures {
                                                crate::paint_cells::paint_cells_row_bg_only(
                                                    seg_cells,
                                                    origin,
                                                    advance,
                                                    row_h(i),
                                                    window,
                                                );
                                                false
                                            } else {
                                                crate::paint_cells::paint_cells_row(
                                                    seg_cells,
                                                    origin,
                                                    advance,
                                                    row_h(i),
                                                    prepaint.text_ascent,
                                                    &prepaint.font,
                                                    prepaint.font_size,
                                                    self.theme.foreground,
                                                    &self.glyph_resolver,
                                                    window,
                                                );
                                                true
                                            }
                                        }
                                    }
                                }
                            }
                            _ => false,
                        }
                    }
                    _ => false,
                }
            } else {
                false
            };
            if !painted_via_cells {
                // F.2/F.3: fallback (inactive / folded / ligatures-glyph /
                // virtual-row) path. A scaled row paints its N pre-shaped
                // pieces side by side, all sharing one baseline so base
                // pieces stay base-size — kept consistent with the active
                // cell path above so a focus change never resizes anything
                // ([[feedback_decorations_update_in_place]]).
                match row_split.get(i) {
                    Some(Some(sl)) => {
                        // Align baselines: gpui paints a line's baseline at
                        // `origin.y + (line_height + ascent - descent)/2`
                        // (text_system/line.rs). The tallest piece defines
                        // the baseline (painted at `line_y`); every shorter
                        // piece shifts DOWN so its baseline matches.
                        let h = row_h(i);
                        let max_ad = sl
                            .pieces
                            .iter()
                            .map(|p| p.shaped.ascent - p.shaped.descent)
                            .fold(Pixels::ZERO, |a, b| if b > a { b } else { a });
                        for p in &sl.pieces {
                            let piece_ad = p.shaped.ascent - p.shaped.descent;
                            let piece_y = line_y + (max_ad - piece_ad) * 0.5;
                            let piece_x = text_origin_x + sl.x_offset(p.start_col, advance);
                            let _ = p.shaped.paint(point(piece_x, piece_y), h, window, cx);
                        }
                    }
                    _ => {
                        if let Err(err) = shaped_line.paint(origin, row_h(i), window, cx) {
                            tracing::warn!(
                                target: "lattice_gpui::editor_element",
                                line_index = self.scroll as usize + i,
                                pane = self.pane_idx,
                                error = ?err,
                                "text ShapedLine::paint failed"
                            );
                        }
                    }
                }
            }
        }

        // Slice X3.full.4: per-cell diagnostic underlines. Painted
        // after the text body so the underline sits visually under
        // the glyphs (2px quad along the row bottom). Pre-computed
        // in `prepaint` so this loop is allocation-free; segments
        // are already in combined-column space.
        for (row_idx, segs) in prepaint.diagnostic_segments_per_row.iter().enumerate() {
            if segs.is_empty() {
                continue;
            }
            let row_y = row_top(row_idx);
            if row_y >= pane_bottom {
                break;
            }
            let underline_y = row_y + row_h(row_idx) - px(2.0);
            for (col_start, col_end, color) in segs {
                if col_end <= col_start {
                    continue;
                }
                let quad_x = col_x(row_idx, *col_start);
                let quad_w = col_x(row_idx, *col_end) - quad_x;
                let quad_bounds = Bounds::new(point(quad_x, underline_y), size(quad_w, px(2.0)));
                window.paint_quad(fill(quad_bounds, rgb(*color)));
            }
        }

        // L4a.3 (lsp-architecture.md §15): inline cursor-line diagnostic
        // summary. Painted at the row's end-of-content x (after inlays),
        // per-frame — no cells-cache involvement (the summary is
        // cursor-transient interaction state, like the cursor / underline
        // overlays). Mirrors the TUI peer's `splice_virtual_text_into_spans`
        // eol splice ([[feedback_tui_gpui_parity]]).
        if let Some((row, end_col, shaped)) = &prepaint.inline_diag_overlay {
            let line_y = row_top(*row);
            if line_y < pane_bottom {
                // Prefer the cell matrix's painted column count
                // (source + inlay cells) × advance — the true end of
                // the line, matching the cursor's EOL x. Fall back to
                // the row's shaped width for wrapped (segment-local)
                // rows where the column count isn't a flat line width.
                let eol_x = match end_col {
                    Some(c) => text_origin_x + advance * (*c as f32),
                    None => text_origin_x + prepaint.shaped_text[*row].width,
                };
                if let Err(err) = shaped.paint(point(eol_x, line_y), row_h(*row), window, cx) {
                    tracing::warn!(
                        target: "lattice_gpui::editor_element",
                        row = *row,
                        pane = self.pane_idx,
                        error = ?err,
                        "inline diagnostic summary ShapedLine::paint failed"
                    );
                }
            }
        }

        // Cursor (painted on top for bar/underline; block re-stamps
        // the covered char in cursor_foreground via shaped_cursor_char).
        if let (Some(cursor), Some((char_col, row))) = (&self.cursor, prepaint.cursor_layout) {
            // F.2: a cursor on a heading row uses that column's scale +
            // the row height so the block/bar/underline matches the glyph
            // it covers — base over the markers, scaled over the title.
            let advance = prepaint.glyph_advance * col_scale(row as usize, char_col);
            let row_height = row_h(row as usize);
            let cursor_x = col_x(row as usize, char_col);
            let cursor_y = row_top(row as usize);
            // F.2: cursor clipped past the pane bottom (variable-height
            // overflow) — nothing more to paint (this is the last block).
            if cursor_y >= pane_bottom {
                return;
            }
            let origin = point(cursor_x, cursor_y);
            match cursor.shape {
                CursorShape::Block => {
                    let cell = Bounds::new(origin, size(advance, row_height));
                    window.paint_quad(fill(cell, rgb(self.theme.cursor_background)));
                    if let Some(shaped) = &prepaint.shaped_cursor_char {
                        if let Err(err) = shaped.paint(origin, row_height, window, cx) {
                            tracing::warn!(
                                target: "lattice_gpui::editor_element",
                                pane = self.pane_idx,
                                error = ?err,
                                "cursor char ShapedLine::paint failed"
                            );
                        }
                    }
                }
                CursorShape::Bar => {
                    let bar = Bounds::new(origin, size(px(2.0), row_height));
                    window.paint_quad(fill(bar, rgb(self.theme.cursor_background)));
                }
                CursorShape::Underline => {
                    let underline_origin = point(origin.x, origin.y + row_height - px(2.0));
                    let underline = Bounds::new(underline_origin, size(advance, px(2.0)));
                    window.paint_quad(fill(underline, rgb(self.theme.cursor_background)));
                }
            }
        }

        // DB.4-gpui: the dashboard branding, painted as a 2-D composition
        // over the (blanked) BrandingBlock rows — the mark as crisp square
        // quads (absent grid cells = cut corners preserved), TOP-ALIGNED
        // with the "Lattice" wordmark so the mark's top edge lines up
        // precisely with the wordmark's cap-height top (not vertically
        // centred against the whole title+subtitle block — that visibly
        // sinks the mark below the title). The subtitle sits on its own
        // line below, slightly larger than body text so it stands out,
        // starting at the SAME pen origin as the wordmark. The knobs below
        // (mark↔text gap, title↔subtitle gap, subtitle scale) are
        // deliberately in one place so the look can be dialled in-app.
        if let Some(b) = &prepaint.branding {
            let clamp0 = |p: Pixels| if p < Pixels::ZERO { Pixels::ZERO } else { p };
            let word_scale = 3.7_f32; // "Lattice" font scale vs base

            // Shape the wordmark (scaled) + tagline (slightly-larger-than-
            // body). Empty strings degrade to a single space so
            // `shape_line` never sees a zero-length input.
            let word_fs = prepaint.font_size * word_scale;
            let word_color = if b.wordmark.1 == 0 {
                self.theme.foreground
            } else {
                b.wordmark.1
            };
            let word_text = if b.wordmark.0.is_empty() {
                " ".to_string()
            } else {
                b.wordmark.0.clone()
            };
            let word_run = TextRun {
                len: word_text.len(),
                font: prepaint.font.clone(),
                color: rgb(word_color).into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let word_shaped = window.text_system().shape_line(
                SharedString::from(word_text),
                word_fs,
                &[word_run],
                None,
            );
            let tag_scale = 1.15_f32; // subtitle: slightly bigger than body, to stand out
            let tag_fs = prepaint.font_size * tag_scale;
            let tag_color = if b.tagline.1 == 0 {
                self.theme.foreground
            } else {
                b.tagline.1
            };
            let tag_text = if b.tagline.0.is_empty() {
                " ".to_string()
            } else {
                b.tagline.0.clone()
            };
            let tag_run = TextRun {
                len: tag_text.len(),
                font: prepaint.font.clone(),
                color: rgb(tag_color).into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let tag_shaped = window.text_system().shape_line(
                SharedString::from(tag_text),
                tag_fs,
                &[tag_run],
                None,
            );

            // Shape the version string at base font size (not the 3.7×
            // wordmark scale) so it reads as regular body-sized text.
            let ver_color = if b.version.1 == 0 {
                self.theme.foreground
            } else {
                b.version.1
            };
            let ver_text = if b.version.0.is_empty() {
                " ".to_string()
            } else {
                b.version.0.clone()
            };
            let ver_run = TextRun {
                len: ver_text.len(),
                font: prepaint.font.clone(),
                color: rgb(ver_color).into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let ver_shaped = window.text_system().shape_line(
                SharedString::from(ver_text),
                prepaint.font_size,
                &[ver_run],
                None,
            );

            // Tight (no built-in leading padding) glyph extents: painting
            // at `line_height == ascent - descent` makes `ShapedLine::paint`
            // put the glyph top EXACTLY at the given origin (zero padding),
            // so "mark top == wordmark top" is exact, not approximate.
            // "Lattice" has no descenders, so its VISIBLE height (top of
            // "L" to baseline) is `ascent` alone — `ascent - descent` is the
            // full glyph BOX (including the empty space reserved below the
            // baseline for descenders like g/y), which is why deriving the
            // mark from it made the mark overshoot past "Lattice"'s
            // baseline. `ascent` is the anatomically precise reference.
            let word_h = word_shaped.ascent;
            let tag_h = tag_shaped.ascent - tag_shaped.descent;
            let title_subtitle_gap = word_h * 0.4; // push the subtitle clearly onto its own line

            // The mark is scaled up 1.5× from the wordmark cap height so the
            // logo tiles are visibly larger and balanced against the large
            // 3.7× wordmark. Square tiles; no gap between them.
            let mark_h = word_h * 1.5;
            let tile_h = mark_h * (1.0 / b.mark_rows.max(1) as f32);
            let tile_w = tile_h;
            let mark_w = tile_w * (b.mark_cols as f32);
            let mark_text_gap = tile_w * 1.4;

            let block_h = mark_h.max(word_h + title_subtitle_gap + tag_h);

            // The blanked branding rows reserve this vertical box; centre
            // the composition's bounding box within it (vertical only).
            let box_top = row_top(b.first_row);
            let box_h = line_height * (b.row_count as f32);
            let block_top = box_top + clamp0((box_h - block_h) * 0.5);

            // Horizontal placement: start at `text_origin_x`, the SAME
            // left edge every other dashboard row uses. The host already
            // centres the whole dashboard's content column in the pane via
            // `content_left_pad` (baked into `gutter_width_px` — see
            // `crates/lattice-host/src/dispatch.rs` `rebuild_option_cache`
            // and `dashboard.md` §5.3 "rows are left-aligned within the
            // gutter, so the mark's columns line up"). Re-centring here
            // against the raw pane width — what an earlier version of this
            // code did — computes a DIFFERENT reference than the rest of
            // the page, which is exactly what made the branding look
            // off-centre relative to the body text below it.
            let mark_x = text_origin_x;
            let mark_y = block_top;
            for (r, c, color) in &b.tiles {
                let tx = mark_x + tile_w * (*c as f32);
                let ty = mark_y + tile_h * (*r as f32);
                window.paint_quad(fill(
                    Bounds::new(point(tx, ty), size(tile_w, tile_h)),
                    rgb(*color),
                ));
            }

            // Wordmark — glyph top exactly at `block_top` (tight line
            // height `word_h` means zero built-in padding, so this is
            // exact, not approximate).
            let text_x = mark_x + mark_w + mark_text_gap;
            let _ = word_shaped.paint(point(text_x, block_top), word_h, window, cx);

            // Version — base font size, painted after the wordmark with
            // a small gap, baselines aligned so the text sits on the same
            // invisible line as the bottom of the brand name.
            if !b.version.0.is_empty() {
                let ver_gap = px(6.0);
                let ver_x = text_x + word_shaped.width + ver_gap;
                let ver_h = ver_shaped.ascent - ver_shaped.descent;
                let ver_y = block_top + word_shaped.ascent - ver_shaped.ascent;
                let _ = ver_shaped.paint(point(ver_x, ver_y), ver_h, window, cx);
            }

            // Subtitle — same pen origin `text_x` is not quite enough: a
            // larger font's left side-bearing (the gap between the pen
            // origin and the glyph's visible ink) is proportionally larger
            // in absolute pixels than a smaller font's, so pinning both to
            // the same pen origin leaves the subtitle's ink start slightly
            // LEFT of the wordmark's ink start. `bearing_frac` is an
            // approximation of that side-bearing as a fraction of font
            // size (gpui doesn't expose per-glyph ink bounds here); nudge
            // it if the "A" and "L" ink still don't line up exactly.
            let bearing_frac = 0.055_f32;
            let tag_indent = (word_fs - tag_fs) * bearing_frac;
            let tag_top = block_top + word_h + title_subtitle_gap;
            let _ = tag_shaped.paint(point(text_x + tag_indent, tag_top), tag_h, window, cx);
        }
    }
}

/// Convert a utf-8 byte offset within `line` to a char column in
/// the inlay-spliced combined line. `inlay_offsets` is a sorted
/// slice of `(orig_byte, char_width)`: every inlay whose
/// `orig_byte <= byte` shifts the column by its `char_width` (the
/// element splices inlay text BEFORE the char at `orig_byte`).
///
/// Empty `inlay_offsets` reduces this to a plain
/// `line[..byte].chars().count()` -- the slice-X3.full.3 behaviour
/// before inlay support landed.
///
/// Saturating: `byte >= line.len()` returns the line's char count
/// plus the sum of every inlay's `char_width`.
pub fn byte_to_combined_col(line: &str, byte: usize, inlay_offsets: &[(u32, u32)]) -> usize {
    // Defensive (2026-06-03): a `byte` that lands inside a
    // multi-byte char (e.g. an em-dash `—`, 3 bytes) would panic
    // the `line[..byte]` slice below. Floor to the containing
    // char's start — the column of the char this byte belongs
    // to. Never panic on the paint hot path (CLAUDE.md).
    let byte = {
        let mut b = byte.min(line.len());
        while b > 0 && !line.is_char_boundary(b) {
            b -= 1;
        }
        b
    };
    let base = if byte >= line.len() {
        line.chars().count()
    } else {
        line[..byte].chars().count()
    };
    let inlay_shift: usize = inlay_offsets
        .iter()
        .filter(|(orig, _)| (*orig as usize) <= byte)
        .map(|(_, w)| *w as usize)
        .sum();
    base + inlay_shift
}

/// Push a `(col_start, col_end_exclusive, color)` tuple for every
/// `ranges[i]` that intersects `line_idx`'s row, in combined-column
/// space (i.e. after inlay-offset splicing). Same shape as
/// `diagnostic_segments_per_row`. `line_text` + `inlay_offsets`
/// drive the utf-8 byte → combined-char-column conversion
/// (monospace advance assumption matches the cursor + gutter maths).
///
/// Perf-plan slice E.1 moves this work from the hot paint loop
/// (formerly `paint_range_overlay`) into `prepaint`, so the
/// painter just walks pre-built tuples without re-computing
/// intersections or byte-to-column conversions per frame.
///
/// Slice X3.full.3 paints BACKGROUNDS only — the underlying
/// syntax colours of the text remain unchanged. Vim's classic
/// "current_match inverts fg" is deferred until a slice that
/// re-shapes the covered text with a different `TextRun`; the bg
/// alone is enough to make matches visible against the syntax
/// palette.
pub fn push_range_quads(
    out: &mut Vec<(u32, u32, u32)>,
    ranges: &[lattice_core::protocol::position::Range],
    line_idx: u32,
    line_text: &str,
    inlay_offsets: &[(u32, u32)],
    color: u32,
) {
    let line_len = line_text.len();
    for r in ranges {
        if line_idx < r.start.line || line_idx > r.end.line {
            continue;
        }
        let start_byte = if line_idx == r.start.line {
            (r.start.byte as usize).min(line_len)
        } else {
            0
        };
        let end_byte = if line_idx == r.end.line {
            (r.end.byte as usize).min(line_len)
        } else {
            line_len
        };
        if end_byte <= start_byte {
            continue;
        }
        let col_start = byte_to_combined_col(line_text, start_byte, inlay_offsets) as u32;
        let col_end = byte_to_combined_col(line_text, end_byte, inlay_offsets) as u32;
        if col_end <= col_start {
            continue;
        }
        out.push((col_start, col_end, color));
    }
}

/// Slice X3.full.4 helper: build a `(combined_text, runs,
/// inlay_offsets)` triple for one visible line, splicing inlay
/// virtual text into the line at each hint's byte offset.
///
/// - `combined_text` is the line text with inlay strings inserted
///   at each `(orig_byte, _)` position; passed to
///   `WindowTextSystem::shape_line` as-is.
/// - `runs` colours every byte of `combined_text`. Adjacent chars
///   with the same colour merge into one run (the analogue of
///   `build_text_runs`'s collapse); inlay chars always carry
///   `inlay_color` and so break the merge across their boundary.
/// - `inlay_offsets` is `[(orig_byte, char_width)]` sorted by
///   `orig_byte`. Each entry records that an inlay of `char_width`
///   chars was inserted BEFORE byte `orig_byte` of the original
///   line -- so cursor + decoration code can apply the shift via
///   `byte_to_combined_col`.
///
/// `inlays` MUST be sorted by `orig_byte` ascending. Trailing
/// hints whose `orig_byte >= line.len()` paint at end-of-line.
pub fn build_line_with_inlays(
    line: &str,
    spans: &[StyledSpan],
    inlays: &[(usize, &str)],
    font: &gpui::Font,
    inlay_color: u32,
    resolved: &lattice_host::ui::theme::ResolvedTheme,
    ids: &lattice_host::ui::theme::BuiltinElementIds,
) -> (String, Vec<TextRun>, Vec<(u32, u32)>) {
    let inlay_byte_total: usize = inlays.iter().map(|(_, t)| t.len()).sum();
    let mut b = LineRunBuilder::new(font, line.len() + inlay_byte_total);

    let mut inlay_idx = 0usize;
    for (orig_byte, ch) in line.char_indices() {
        while inlay_idx < inlays.len() && inlays[inlay_idx].0 <= orig_byte {
            let (off, text) = inlays[inlay_idx];
            let char_width = text.chars().count() as u32;
            b.inlay_offsets.push((off as u32, char_width));
            b.emit(text, inlay_color);
            inlay_idx += 1;
        }
        let color = syntax_color(style_at(spans, orig_byte), resolved, ids);
        let mut buf = [0u8; 4];
        b.emit(ch.encode_utf8(&mut buf), color);
    }
    // Trailing inlays at or past EOL.
    while inlay_idx < inlays.len() {
        let (off, text) = inlays[inlay_idx];
        let char_width = text.chars().count() as u32;
        b.inlay_offsets.push((off as u32, char_width));
        b.emit(text, inlay_color);
        inlay_idx += 1;
    }
    b.finish()
}

/// Internal builder for `build_line_with_inlays`. Tracks the
/// in-progress text run so adjacent same-colour chars merge.
struct LineRunBuilder<'a> {
    combined: String,
    runs: Vec<TextRun>,
    inlay_offsets: Vec<(u32, u32)>,
    font: &'a gpui::Font,
    current_color: u32,
    current_len: usize,
    started: bool,
}

impl<'a> LineRunBuilder<'a> {
    fn new(font: &'a gpui::Font, capacity: usize) -> Self {
        Self {
            combined: String::with_capacity(capacity),
            runs: Vec::new(),
            inlay_offsets: Vec::new(),
            font,
            current_color: 0,
            current_len: 0,
            started: false,
        }
    }

    fn emit(&mut self, s: &str, color: u32) {
        if !self.started {
            self.current_color = color;
            self.current_len = s.len();
            self.started = true;
        } else if color == self.current_color {
            self.current_len += s.len();
        } else {
            self.runs.push(make_run_with_color(
                self.current_color,
                self.current_len,
                self.font,
            ));
            self.current_color = color;
            self.current_len = s.len();
        }
        self.combined.push_str(s);
    }

    fn finish(mut self) -> (String, Vec<TextRun>, Vec<(u32, u32)>) {
        if self.started && self.current_len > 0 {
            self.runs.push(make_run_with_color(
                self.current_color,
                self.current_len,
                self.font,
            ));
        }
        (self.combined, self.runs, self.inlay_offsets)
    }
}

/// Build a flat fg-only [`TextRun`] for `len` utf-8 bytes —
/// the shape `build_line_with_inlays`'s [`LineRunBuilder`]
/// emits. Cell-derived runs go through
/// [`crate::cells_paint::cell_row_to_text_runs`] instead, which
/// carries modifier bits (S4.2). Kept `pub(crate)` for symmetry
/// with the rest of the legacy path; demote to `fn` once the
/// legacy path retires (S4.final).
pub(crate) fn make_run_with_color(color: u32, len: usize, font: &gpui::Font) -> TextRun {
    TextRun {
        len,
        font: font.clone(),
        color: rgb(color).into(),
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

/// Catppuccin Mocha overlay2 -- gutter line-number colour for
/// non-cursor lines.
const GUTTER_NORMAL_COLOR: u32 = 0x9399b2;
/// Fold-marker glyphs, shared with the TUI peer (`fold_glyph_for`):
/// `▾` on an open (expanded) foldable head, `▸` on a closed (collapsed)
/// one. Small triangles that render in any monospace font — no Nerd-Font
/// dependency. The colour is resolved per-marker from the theme
/// (`gutter.fold.open` / `gutter.fold.closed`) and carried in
/// `GutterLineMeta::fold_marker`, so these consts are glyph-only.
pub(crate) const FOLD_GLYPH_OPEN: char = '▾';
pub(crate) const FOLD_GLYPH_CLOSED: char = '▸';

/// W.5 (soft-wrap): continuation-row gutter marker. U+21AA
/// (rightwards arrow with hook); no Nerd-Font dependency, so it
/// renders in any monospace font. Matches the TUI peer's
/// `WRAP_CONT_MARKER` for cross-renderer parity.
const WRAP_CONT_MARKER: &str = "↪";
/// W.5: dim colour for the continuation marker (Catppuccin Mocha
/// surface2). Reads as "this row continues the line above" without
/// competing with the real line numbers.
const WRAP_CONT_GUTTER_COLOR: u32 = 0x585b70;

/// W.5 (soft-wrap): char range `[start, end)` of display segment
/// `seg` for a line of `total_chars` columns wrapped at `wrap_width`.
/// `wrap_width == 0` (wrapping off) returns the whole line on segment
/// 0. Segment boundaries match `lattice_cells::wrap_segments`, so the
/// renderer and the host scroll model agree on display-row counts.
fn segment_char_range(total_chars: usize, seg: u32, wrap_width: u32) -> (usize, usize) {
    if wrap_width == 0 {
        return (0, total_chars);
    }
    let w = wrap_width as usize;
    let start = (seg as usize).saturating_mul(w).min(total_chars);
    let end = start.saturating_add(w).min(total_chars);
    (start, end)
}

/// W.5 (soft-wrap): slice a shaped line's `(combined, runs)` to the
/// char range `[char_start, char_end)` for one wrap segment,
/// preserving each run's style. `runs` are byte-length-keyed and
/// contiguous over `combined`; the slice keeps the per-run overlap
/// with the segment's byte window. Returns `(segment_text,
/// segment_runs)` whose run lengths sum to `segment_text.len()`,
/// ready for `shape_line`.
fn slice_runs_to_char_range(
    combined: &str,
    runs: &[TextRun],
    char_start: usize,
    char_end: usize,
) -> (String, Vec<TextRun>) {
    let byte_start = combined
        .char_indices()
        .nth(char_start)
        .map(|(b, _)| b)
        .unwrap_or(combined.len());
    let byte_end = combined
        .char_indices()
        .nth(char_end)
        .map(|(b, _)| b)
        .unwrap_or(combined.len());
    let seg_text = combined.get(byte_start..byte_end).unwrap_or("").to_string();
    let mut out: Vec<TextRun> = Vec::new();
    let mut pos = 0usize;
    for r in runs {
        let r_start = pos;
        let r_end = pos + r.len;
        pos = r_end;
        let lo = r_start.max(byte_start);
        let hi = r_end.min(byte_end);
        if hi > lo {
            let mut nr = r.clone();
            nr.len = hi - lo;
            out.push(nr);
        }
    }
    (seg_text, out)
}

/// W.5 (soft-wrap): project a source line's full-width column quads
/// (overlay backgrounds / diagnostic underlines, in combined-column
/// space) onto one wrap segment's local column window `[lo, hi)`. A
/// quad `[cs, ce)` is intersected with the window and re-based to the
/// segment's column 0. GPUI paints these as positioned `paint_quad`s
/// (unlike the TUI, which bakes overlay styles into the spans it
/// splits), so the renderer must re-bucket the quads per segment.
fn quads_for_segment(full: &[(u32, u32, u32)], lo: u32, hi: u32) -> Vec<(u32, u32, u32)> {
    full.iter()
        .filter_map(|&(cs, ce, color)| {
            let s = cs.max(lo);
            let e = ce.min(hi);
            // Lazy `then` (not `then_some`): the subtraction must not
            // be evaluated when the quad doesn't overlap the segment
            // (`e` may be < `lo`, which would underflow `e - lo`).
            (e > s).then(|| (s - lo, e - lo, color))
        })
        .collect()
}

/// W.5 (soft-wrap): shape the gutter for a wrapped continuation row —
/// a dim `↪` right-aligned in the line-number column, with the
/// severity / diff / fold columns blank. Same total width as
/// `format_gutter_text` (`sign_cells + gutter_width + 3`) so
/// continuation rows align with their source line's gutter.
fn shaped_continuation_gutter(
    gutter_width: usize,
    font: &gpui::Font,
    font_size: Pixels,
    sign_column: bool,
    window: &mut Window,
) -> ShapedLine {
    // Leading blanks: [severity + diff when signcolumn=yes] +
    // right-aligned marker in the number column + 3 trailing blanks
    // (separator space + blank fold slot + trailing gap) — the fold
    // marker now sits AFTER the digits, so it is a trailing blank here.
    // PU.1b-1a: drop the two sign blanks when `signcolumn=no` so the
    // continuation gutter matches `format_gutter_text`'s gated width.
    let lead = if sign_column { "  " } else { "" };
    let text = format!("{lead}{WRAP_CONT_MARKER:>gutter_width$}   ");
    let run = TextRun {
        len: text.len(),
        font: font.clone(),
        color: rgb(WRAP_CONT_GUTTER_COLOR).into(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_line(SharedString::from(text), font_size, &[run], None)
}

/// W.5 (soft-wrap): push one source line as `seg_count` display rows.
/// Segment 0 carries the real gutter (`gutter_seg0`); continuation
/// rows get a dim `↪`. The body text and the overlay / diagnostic
/// quads are sliced to each segment's local column window, and every
/// parallel per-row array is pushed so the row participates in paint.
/// Returns `true` if the viewport-height cap was hit mid-line (the
/// caller should stop emitting further rows for the viewport).
///
/// `gutter_seg0 == None` selects the gutter-less fallback (the
/// `self.gutter.is_empty()` path): no `shaped_gutter` entries are
/// pushed, matching the pre-W.5 behaviour.
///
/// `seg_count <= 1` or `wrap_width == 0` (wrapping off, or the line
/// fits) pushes the full `combined` / `runs` / quads verbatim, so a
/// non-wrapping render is byte-identical to the pre-W.5 single push.
#[allow(clippy::too_many_arguments)]
fn push_wrapped_doc_row(
    line_idx: u32,
    line_text: &str,
    combined: &str,
    runs: &[TextRun],
    inlay_offsets: Vec<(u32, u32)>,
    full_diag: &[(u32, u32, u32)],
    full_overlay: &[(u32, u32, u32)],
    seg_count: u32,
    wrap_width: u32,
    gutter_seg0: Option<ShapedLine>,
    gutter_width: usize,
    sign_column: bool,
    font: &gpui::Font,
    font_size: Pixels,
    heading_scale: Option<(u32, f32)>,
    viewport_height: u32,
    window: &mut Window,
    shaped_text: &mut Vec<ShapedLine>,
    shaped_gutter: &mut Vec<ShapedLine>,
    row_meta: &mut Vec<(u32, String)>,
    row_segment: &mut Vec<u32>,
    row_scale: &mut Vec<f32>,
    row_split: &mut Vec<Option<ScaledLine>>,
    inlay_offsets_per_row: &mut Vec<Vec<(u32, u32)>>,
    diagnostic_segments_per_row: &mut Vec<Vec<(u32, u32, u32)>>,
    overlay_quads_per_row: &mut Vec<Vec<(u32, u32, u32)>>,
) -> bool {
    let total_chars = combined.chars().count();
    let single = seg_count <= 1 || wrap_width == 0;
    // F.2 (Thread F): only a SINGLE-segment row is eligible for the
    // heading split (markers base-size + title scaled). A wrapped heading
    // (rare — wrapping is usually off) renders at base size; the split is
    // skipped. `shaped_text` is ALWAYS shaped at BASE size — the scaled
    // rendering for a heading row comes from the `HeadingSplit` built
    // below, read by both paint paths. Ordinary rows: byte-identical to
    // pre-F.2.
    let heading = if single { heading_scale } else { None };
    let has_gutter = gutter_seg0.is_some();
    let mut gutter_seg0 = gutter_seg0;
    for seg in 0..seg_count.max(1) {
        // Segment 0 always fits (the caller checked the budget before
        // calling); continuations stop at the height cap so the
        // per-row vecs stay 1:1 and nothing paints past the pane.
        if seg > 0 && shaped_text.len() as u32 >= viewport_height {
            return true;
        }
        let (seg_text, seg_runs) = if single {
            (combined.to_string(), runs.to_vec())
        } else {
            let (lo, hi) = segment_char_range(total_chars, seg, wrap_width);
            slice_runs_to_char_range(combined, runs, lo, hi)
        };
        let shaped = window.text_system().shape_line(
            SharedString::from(seg_text),
            font_size,
            &seg_runs,
            None,
        );
        shaped_text.push(shaped);
        if has_gutter {
            if seg == 0 {
                shaped_gutter.push(
                    gutter_seg0
                        .take()
                        .expect("gutter_seg0 present on segment 0"),
                );
            } else {
                shaped_gutter.push(shaped_continuation_gutter(
                    gutter_width,
                    font,
                    font_size,
                    sign_column,
                    window,
                ));
            }
        }
        row_meta.push((line_idx, line_text.to_string()));
        row_segment.push(seg);
        // F.2/F.3: build the scaled line for an eligible (single-segment)
        // heading row — the markers shaped at base, the title at
        // `font_size * title_scale`, both for the fallback paint path. This
        // is the 2-piece case of the general N-piece `ScaledLine` (virtual
        // rows build N pieces from `VirtualRow::scales`). The row's height
        // multiplier (`row_scale`) is the tallest piece scale. Ordinary
        // rows push `None` + `1.0` (fast path).
        match heading {
            Some((prefix_cols, title_scale)) => {
                let mut pieces: Vec<ScaledPiece> = Vec::with_capacity(2);
                if prefix_cols > 0 {
                    let (pre_text, pre_runs) =
                        slice_runs_to_char_range(combined, runs, 0, prefix_cols as usize);
                    let prefix_shaped = window.text_system().shape_line(
                        SharedString::from(pre_text),
                        font_size,
                        &pre_runs,
                        None,
                    );
                    pieces.push(ScaledPiece {
                        start_col: 0,
                        cols: prefix_cols,
                        scale: 1.0,
                        shaped: prefix_shaped,
                    });
                }
                let (title_text, title_runs) =
                    slice_runs_to_char_range(combined, runs, prefix_cols as usize, total_chars);
                let title_shaped = window.text_system().shape_line(
                    SharedString::from(title_text),
                    font_size * title_scale,
                    &title_runs,
                    None,
                );
                pieces.push(ScaledPiece {
                    start_col: prefix_cols,
                    cols: (total_chars as u32).saturating_sub(prefix_cols),
                    scale: title_scale,
                    shaped: title_shaped,
                });
                row_scale.push(title_scale);
                row_split.push(Some(ScaledLine {
                    pieces,
                    max_scale: title_scale,
                }));
            }
            None => {
                row_scale.push(1.0);
                row_split.push(None);
            }
        }
        // The full inlay offsets live on segment 0 (the cursor
        // base-row lookup reads them there); continuations don't need
        // them (their overlay/diag quads are already pre-bucketed).
        inlay_offsets_per_row.push(if seg == 0 {
            inlay_offsets.clone()
        } else {
            Vec::new()
        });
        if single {
            diagnostic_segments_per_row.push(full_diag.to_vec());
            overlay_quads_per_row.push(full_overlay.to_vec());
        } else {
            let lo = seg.saturating_mul(wrap_width);
            let hi = lo.saturating_add(wrap_width);
            diagnostic_segments_per_row.push(quads_for_segment(full_diag, lo, hi));
            overlay_quads_per_row.push(quads_for_segment(full_overlay, lo, hi));
        }
    }
    false
}

/// Format a gutter row's text content: 1 char fold marker + 1
/// char severity sign + N-char right-aligned line number + 1
/// space. Total width = `2 + gutter_width + 1`.
/// D.3.b.1.gpui (2026-05-29): iterate `VirtualRowMatrix` rows
/// anchored at `line` with the given `position`. Mirrors the
/// TUI helper of the same name.
fn virtual_rows_at_gpui<'a>(
    matrix: &'a lattice_cells::VirtualRowMatrix,
    line: u32,
    position: lattice_cells::AnchorPosition,
) -> impl Iterator<Item = &'a lattice_cells::VirtualRow> + 'a {
    let start = matrix.first_row_at_or_after(line) as usize;
    matrix.rows[start..]
        .iter()
        .take_while(move |r| r.anchor_line == line)
        .filter(move |r| r.position == position)
        // Pinned rows (sticky headerlines + the branding masthead) are
        // rendered at the pane top in the pre-pass; skip them here so they
        // don't double-paint in the content loop.
        .filter(|r| !r.kind.is_pinned())
}

/// F.3 (Thread F): build the N-piece [`ScaledLine`] for a virtual row
/// from its per-column [`lattice_cells::VirtualRow::scales`], or `None`
/// when the row is uniformly base size (the fast path). Coalesces the
/// scales into contiguous runs — exactly as the shaping loop coalesces
/// per-cell `fg` — and shapes each run's text slice at `font_size *
/// scale` for the fallback paint path. The dashboard branding wordmark
/// is the first consumer: its "Lattice" run scales while the mark blocks
/// stay base.
fn build_virtual_row_scaled_line(
    vrow: &lattice_cells::VirtualRow,
    content: &str,
    content_cols: u32,
    runs: &[TextRun],
    font_size: Pixels,
    window: &mut Window,
) -> Option<ScaledLine> {
    let scales = vrow.scales.as_ref()?;
    let scale_runs = lattice_cells::coalesce_scales(scales, content_cols);
    // Fast path: every run is base size ⇒ no ScaledLine (uniform row).
    if !scale_runs
        .iter()
        .any(|r| r.scale != lattice_cells::BASE_SCALE)
    {
        return None;
    }
    let mut pieces: Vec<ScaledPiece> = Vec::with_capacity(scale_runs.len());
    let mut max_scale = 1.0f32;
    for r in &scale_runs {
        let scale = r.scale as f32 / lattice_cells::BASE_SCALE as f32;
        if scale > max_scale {
            max_scale = scale;
        }
        let (piece_text, piece_runs) = slice_runs_to_char_range(
            content,
            runs,
            r.start_col as usize,
            (r.start_col + r.cols) as usize,
        );
        let shaped = window.text_system().shape_line(
            SharedString::from(piece_text),
            font_size * scale,
            &piece_runs,
            None,
        );
        pieces.push(ScaledPiece {
            start_col: r.start_col,
            cols: r.cols,
            scale,
            shaped,
        });
    }
    Some(ScaledLine { pieces, max_scale })
}

/// D.3.b.1.gpui (2026-05-29): shape a virtual row's content
/// and push it + placeholder entries into every parallel
/// per-row array so the row participates in the paint pass.
/// The gutter renders as fully blank (alignment placeholder);
/// the body's deletion-block backdrop is added as a
/// full-row overlay quad. Uses sentinel `line_idx = u32::MAX`
/// in `row_meta` so the cells-fast-path lookup in `paint`
/// returns `None` and falls back to `ShapedLine::paint`.
#[allow(clippy::too_many_arguments)]
fn push_virtual_row(
    vrow: &lattice_cells::VirtualRow,
    gutter_width: usize,
    font: &gpui::Font,
    font_size: Pixels,
    body_color: u32,
    backdrop_color: u32,
    window: &mut Window,
    shaped_text: &mut Vec<ShapedLine>,
    shaped_gutter: &mut Vec<ShapedLine>,
    row_meta: &mut Vec<(u32, String)>,
    row_segment: &mut Vec<u32>,
    row_scale: &mut Vec<f32>,
    row_split: &mut Vec<Option<ScaledLine>>,
    inlay_offsets_per_row: &mut Vec<Vec<(u32, u32)>>,
    diagnostic_segments_per_row: &mut Vec<Vec<(u32, u32, u32)>>,
    overlay_quads_per_row: &mut Vec<Vec<(u32, u32, u32)>>,
    branding_rows: &mut Vec<(usize, std::sync::Arc<[lattice_cells::Cell]>)>,
) {
    // DB.4-gpui: a BrandingBlock row is recorded (with its display index +
    // cells) for the 2-D branding pass and then rendered BLANK here — the
    // GPUI peer paints the quad mark + shaped wordmark over it, so the raw
    // mark/wordmark cells must not double-paint. (The TUI peer paints the
    // cells directly; it never reaches this function.)
    if vrow.kind == lattice_cells::VirtualRowKind::BrandingBlock {
        branding_rows.push((shaped_text.len(), vrow.cells.clone()));
        let blank = window.text_system().shape_line(
            SharedString::from(" "),
            font_size,
            &[TextRun {
                len: 1,
                font: font.clone(),
                color: rgb(body_color).into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            None,
        );
        let blank_gutter: String = " ".repeat(gutter_width + 5);
        let shaped_g = window.text_system().shape_line(
            SharedString::from(blank_gutter.clone()),
            font_size,
            &[TextRun {
                len: blank_gutter.len(),
                font: font.clone(),
                color: rgb(GUTTER_NORMAL_COLOR).into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            None,
        );
        shaped_text.push(blank);
        shaped_gutter.push(shaped_g);
        row_meta.push((u32::MAX, String::new()));
        row_segment.push(0);
        row_scale.push(1.0);
        row_split.push(None);
        inlay_offsets_per_row.push(Vec::new());
        diagnostic_segments_per_row.push(Vec::new());
        overlay_quads_per_row.push(Vec::new());
        return;
    }
    // D.3.b.2 (2026-05-29): build the body text + a parallel
    // `Vec<TextRun>` keyed by per-cell `fg`. Adjacent cells
    // sharing the same fg coalesce into a single TextRun so
    // an 80-char line produces ~5 runs, not 80. `cell.fg = 0`
    // is the "use default" sentinel; fall back to
    // `body_color` (the theme foreground passed by the
    // caller). For empty rows we still emit one space so
    // `shape_line` doesn't fail on a zero-length input.
    let mut content = String::with_capacity(vrow.cells.len());
    let mut runs: Vec<TextRun> = Vec::new();
    let mut current_color: Option<u32> = None;
    let mut current_len: usize = 0;
    let flush = |color: Option<u32>, len: usize, runs: &mut Vec<TextRun>, font: &gpui::Font| {
        if len == 0 {
            return;
        }
        let resolved = color.filter(|c| *c != 0).unwrap_or(body_color);
        runs.push(TextRun {
            len,
            font: font.clone(),
            color: rgb(resolved).into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        });
    };
    for cell in vrow.cells.iter() {
        let Some(ch) = char::from_u32(cell.codepoint) else {
            continue;
        };
        let cell_color = Some(cell.fg);
        if current_color.is_none() {
            current_color = cell_color;
        }
        if current_color != cell_color {
            flush(current_color, current_len, &mut runs, font);
            current_color = cell_color;
            current_len = 0;
        }
        let ch_bytes = ch.len_utf8();
        content.push(ch);
        current_len += ch_bytes;
    }
    flush(current_color, current_len, &mut runs, font);
    if content.is_empty() {
        content.push(' ');
        runs.push(TextRun {
            len: 1,
            font: font.clone(),
            color: rgb(body_color).into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        });
    }
    let content_cols = content.chars().count() as u32;
    // F.3 (Thread F): per-token scaling for virtual rows. Coalesce the
    // row's per-column `scales` into contiguous runs (mirroring the fg
    // coalescing above) and, if any run is larger than base, build the
    // N-piece `ScaledLine` the paint path reads — each piece shaped at
    // `font_size * scale` on a shared baseline. A row with no scaled run
    // stays on the fast path (`None` + `row_scale = 1.0`). Built from
    // `&content` before it is moved into the base `shaped_body`.
    let scaled_line =
        build_virtual_row_scaled_line(vrow, &content, content_cols, &runs, font_size, window);
    let row_scale_val = scaled_line.as_ref().map(|s| s.max_scale).unwrap_or(1.0);
    let shaped_body =
        window
            .text_system()
            .shape_line(SharedString::from(content), font_size, &runs, None);
    // Gutter: fully blank-padded to match
    // `format_gutter_text`'s virtual-row width.
    let blank_gutter: String = " ".repeat(gutter_width + 5);
    let gutter_run = TextRun {
        len: blank_gutter.len(),
        font: font.clone(),
        color: rgb(GUTTER_NORMAL_COLOR).into(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let shaped_g = window.text_system().shape_line(
        SharedString::from(blank_gutter),
        font_size,
        &[gutter_run],
        None,
    );
    // D.6.i (2026-05-31): per-virtual-row-kind backdrop
    // selection. Deletion blocks (D.3) paint with the
    // `diff_deletion_block_bg` quad so the user sees
    // "this content existed in baseline but is gone."
    // Filler rows (D.4.c / D.6.b) are visual padding for
    // side-by-side alignment — they paint with no
    // backdrop, otherwise the deletion-block red would
    // mis-read them as deleted lines. Generic (the
    // default for any other virtual-row source) gets the
    // deletion-block backdrop for back-compat with
    // pre-D.6.i providers.
    let quads = match vrow.kind {
        lattice_cells::VirtualRowKind::DeletionBlock | lattice_cells::VirtualRowKind::Generic => {
            let backdrop_width = content_cols.max(1);
            // T.7 (2026-06-18): honor `vrow.bg` first, matching the TUI
            // peer (render.rs `vrow.bg.map(...).or_else(kind default)`).
            // A multibuffer excerpt header is a Generic row carrying a
            // baked `bg`; without this it would paint the deletion-block
            // red. `bg: None` Generic rows still fall back to the
            // deletion-block backdrop (D.6.i back-compat).
            let quad_color = vrow.bg.unwrap_or(backdrop_color);
            vec![(0u32, backdrop_width, quad_color)]
        }
        lattice_cells::VirtualRowKind::Sticky => {
            // backdrop_color is set to vrow.bg by the sticky pre-pass caller.
            if backdrop_color != 0 {
                let backdrop_width = content_cols.max(1);
                vec![(0u32, backdrop_width, backdrop_color)]
            } else {
                Vec::new()
            }
        }
        // Filler: no backdrop (blank padding). BrandingBlock: no cell
        // backdrop either — the GPUI branding pass (DB.4-gpui) paints the
        // 2-D composition over these rows itself.
        lattice_cells::VirtualRowKind::Filler | lattice_cells::VirtualRowKind::BrandingBlock => {
            Vec::new()
        }
    };
    shaped_text.push(shaped_body);
    shaped_gutter.push(shaped_g);
    row_meta.push((u32::MAX, String::new()));
    // W.5: virtual rows are a single display row each (segment 0).
    row_segment.push(0);
    // F.3: virtual rows honor per-token scaling — `row_scale_val` grows
    // the row height to the tallest piece; `scaled_line` carries the
    // pieces (or `None` for an all-base row, the fast path).
    row_scale.push(row_scale_val);
    row_split.push(scaled_line);
    inlay_offsets_per_row.push(Vec::new());
    diagnostic_segments_per_row.push(Vec::new());
    overlay_quads_per_row.push(quads);
}

fn format_gutter_text(
    meta: &GutterLineMeta,
    gutter_width: usize,
    sign_column: bool,
    show_line_numbers: bool,
) -> String {
    // The line-number digits are shown only when `number` is set; otherwise the
    // digit slot is blank (gutter_width is 0 in that case anyway). Matches the
    // TUI `show_line_numbers` gate.
    let num: String = if show_line_numbers {
        (meta.display_line as usize + 1).to_string()
    } else {
        String::new()
    };
    // PU.1b-1a: the two sign cells (severity + diff) are present only
    // when `signcolumn=yes` (default). `signcolumn=no` drops them so
    // the gutter is fold + line-number + trail only — content abuts
    // the digits, exactly the geometry `gutter_chars` reserves.
    if meta.is_virtual {
        // D.3.b.1.gpui: virtual rows render a fully-blank gutter so
        // the column stays the same width as document rows. Width =
        // [1 (sev) + 1 (diff) when reserved] + gutter_width + 3 (the
        // separator space + fold-marker slot + trailing gap).
        let sign_cells = if sign_column { 2 } else { 0 };
        return " ".repeat(gutter_width + 3 + sign_cells);
    }
    // Fold marker now sits AFTER the line number (a separator space,
    // the glyph, then a trailing gap: `… 99 ▸ code`), mirroring the
    // TUI gutter — not at column 0. A blank glyph keeps non-fold rows
    // the same width.
    let fold = meta.fold_marker.map(|(g, _)| g).unwrap_or(' ');
    if !sign_column {
        return format!(
            "{num:>width$} {fold} ",
            num = num,
            width = gutter_width,
            fold = fold,
        );
    }
    let sev = meta.severity.map(|(g, _)| g).unwrap_or(' ');
    // D.3.d.2: diff-sign column sits LEFT of the line number
    // (between severity and the digits) — matches editor
    // convention (Vim signcolumn, Helix, Zed, VSCode,
    // JetBrains). LSP severity and diff occupy adjacent
    // dedicated columns so the two decoration types don't
    // compete (Helix-style). Same blank-space-when-absent
    // discipline so the layout never shifts on :diff /
    // :diffoff.
    let diff = meta.diff_sign.map(|(g, _)| g).unwrap_or(' ');
    format!(
        "{sev}{diff}{num:>width$} {fold} ",
        sev = sev,
        diff = diff,
        num = num,
        width = gutter_width,
        fold = fold,
    )
}

/// Build the `TextRun`s for a gutter row. The text is
/// `{sev}{diff}{num} {fold} ` (severity + diff omitted when
/// `signcolumn=no`): severity, diff sign, then the line-number column +
/// separator space in the normal gutter colour, the fold glyph in its
/// own resolved colour, and a trailing gap. Colouring the fold glyph
/// separately is why it gets its own run rather than riding along with
/// the digits.
fn build_gutter_runs(
    text: &str,
    meta: &GutterLineMeta,
    font: gpui::Font,
    sign_column: bool,
) -> Vec<TextRun> {
    let mut runs = Vec::with_capacity(5);
    let mut bytes_consumed = 0usize;
    let push = |runs: &mut Vec<TextRun>, len: usize, color: u32| {
        if len == 0 {
            return;
        }
        runs.push(TextRun {
            len,
            font: font.clone(),
            color: rgb(color).into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        });
    };

    // PU.1b-1a: the severity + diff sign runs exist only when
    // `signcolumn=yes` — `format_gutter_text` omitted those two chars
    // for `no`, so consuming them here would mis-slice the digits.
    if sign_column {
        // Run: severity sign.
        let sev_color = meta.severity.map(|(_, c)| c).unwrap_or(GUTTER_NORMAL_COLOR);
        let sev_len = text[bytes_consumed..]
            .chars()
            .next()
            .unwrap_or(' ')
            .len_utf8();
        push(&mut runs, sev_len, sev_color);
        bytes_consumed += sev_len;

        // Run: D.3.d.2 diff sign (left of line number, between
        // severity and digits — Vim/Helix/Zed/VSCode convention).
        let diff_color = meta
            .diff_sign
            .map(|(_, c)| c)
            .unwrap_or(GUTTER_NORMAL_COLOR);
        let diff_len = text[bytes_consumed..]
            .chars()
            .next()
            .unwrap_or(' ')
            .len_utf8();
        push(&mut runs, diff_len, diff_color);
        bytes_consumed += diff_len;
    }

    // Tail layout: `{num:>width} {fold} ` — the fold glyph is the
    // second-to-last char and the trailing gap is the last. Slice from
    // the END so a multi-byte glyph (`▸`/`▾`, 3 bytes) or a blank space
    // are handled uniformly.
    let trailing_len = text.chars().next_back().map(char::len_utf8).unwrap_or(0);
    let before_trailing = &text[..text.len() - trailing_len];
    let fold_len = before_trailing
        .chars()
        .next_back()
        .map(char::len_utf8)
        .unwrap_or(0);
    // Line number + separator space (normal colour), up to the glyph.
    let mid_len = before_trailing.len() - fold_len - bytes_consumed;
    push(&mut runs, mid_len, GUTTER_NORMAL_COLOR);
    // The fold glyph in its resolved colour (falls back to the normal
    // gutter tone for a blank non-fold row).
    let fold_color = meta
        .fold_marker
        .map(|(_, c)| c)
        .unwrap_or(GUTTER_NORMAL_COLOR);
    push(&mut runs, fold_len, fold_color);
    // Trailing gap.
    let tail_len = trailing_len;
    if tail_len > 0 {
        runs.push(TextRun {
            len: tail_len,
            font,
            color: rgb(GUTTER_NORMAL_COLOR).into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        });
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DB.4-gpui: the branding parser turns a `BrandingBlock` row group's
    /// cells into mark tiles (by fg colour) + wordmark/tagline text, and —
    /// critically — leaves absent grid cells (the cut corners) tile-less so
    /// the interlocking negative space is preserved.
    #[test]
    fn build_branding_paint_parses_tiles_and_text_preserving_corners() {
        use lattice_cells::Cell;
        let block = BRANDING_MARK_GLYPH;
        let blue = 0x1f6febu32;
        let amber = 0xf59e0bu32;
        let white = 0xffffffu32;
        let cell = |cp: u32, fg: u32| Cell::new(cp, fg, 0, 0);
        let sp = cell(' ' as u32, 0);
        // Row at display index 3: blocks at cols 0 and 2 (col 1 is a CUT
        // corner — a space), then the wordmark "Hi".
        let row_a = vec![
            cell(block, blue),
            sp,
            cell(block, blue),
            sp,
            cell('H' as u32, white),
            cell('i' as u32, white),
        ];
        // Row at index 4: amber cursor tile at col 1, then tagline "yo".
        let row_b = vec![
            sp,
            cell(block, amber),
            sp,
            sp,
            cell('y' as u32, 0x999999),
            cell('o' as u32, 0x999999),
        ];
        let rows = vec![
            (3usize, std::sync::Arc::from(row_a.into_boxed_slice())),
            (4usize, std::sync::Arc::from(row_b.into_boxed_slice())),
        ];
        let b = build_branding_paint(&rows).expect("branding parses");
        assert_eq!(b.first_row, 3);
        assert_eq!(b.row_count, 2);
        assert_eq!(b.mark_cols, 3, "widest mark col is 2 → 3 cols");
        assert_eq!(b.mark_rows, 2);
        // Tiles present with their colours; the cut corner (row 0, col 1)
        // has NO tile — negative space preserved.
        assert!(b.tiles.contains(&(0, 0, blue)));
        assert!(b.tiles.contains(&(0, 2, blue)));
        assert!(b.tiles.contains(&(1, 1, amber)), "amber cursor tile");
        assert!(
            !b.tiles.iter().any(|t| t.0 == 0 && t.1 == 1),
            "cut corner stays tile-less"
        );
        assert_eq!(b.wordmark, ("Hi".to_string(), white));
        assert_eq!(b.tagline.0, "yo");
    }

    /// The rendered gutter text MUST be exactly the width the element
    /// reserves for `gutter_width_px` (`sign_cells + gutter_width + 3`,
    /// where the `+3` is the separator space + fold-marker cell + the
    /// trailing gap — the `… 99 ▸ code` layout). If the reservation
    /// mismatches, the code's first column lands inside the gutter and the
    /// line number runs flush against the code. Pins both the document and
    /// the virtual-row branch against the reservation formula.
    #[test]
    fn gutter_text_width_matches_reserved_chars() {
        let doc = GutterLineMeta {
            line_idx: 41,
            display_line: 41,
            fold_marker: None,
            severity: None,
            diff_sign: None,
            is_virtual: false,
        };
        let virt = GutterLineMeta {
            line_idx: 41,
            display_line: 41,
            fold_marker: None,
            severity: None,
            diff_sign: None,
            is_virtual: true,
        };
        let gutter_width = 3usize;
        for &sign in &[false, true] {
            let sign_cells = if sign { 2 } else { 0 };
            let reserved = sign_cells + gutter_width + 3;
            assert_eq!(
                format_gutter_text(&doc, gutter_width, sign, true)
                    .chars()
                    .count(),
                reserved,
                "document gutter must fill the reserved width (sign={sign})"
            );
            assert_eq!(
                format_gutter_text(&virt, gutter_width, sign, true)
                    .chars()
                    .count(),
                reserved,
                "virtual gutter must match the reserved width (sign={sign})"
            );
        }
    }

    /// T.5.b: the resolved table + builtin ids the
    /// `build_line_with_inlays` / `syntax_color` tests resolve syntax
    /// colours through — the default registry, same construction the
    /// renderer uses at boot.
    fn theme_defaults() -> (
        std::sync::Arc<lattice_host::ui::theme::ResolvedTheme>,
        lattice_host::ui::theme::BuiltinElementIds,
    ) {
        use lattice_host::ui::theme::ThemeRegistry as _;
        let reg = lattice_host::ui::theme::InMemoryThemeRegistry::with_defaults();
        let resolved = reg.resolved();
        let ids = lattice_host::ui::theme::BuiltinElementIds::capture(&reg);
        (resolved, ids)
    }

    // ----- W.5 soft-wrap segment helpers -----

    #[test]
    fn w5_segment_char_range_splits_on_wrap_width() {
        // Wrap off → the whole line lands on segment 0.
        assert_eq!(segment_char_range(10, 0, 0), (0, 10));
        assert_eq!(segment_char_range(10, 5, 0), (0, 10));
        // Width 4 over 10 chars → [0,4) [4,8) [8,10).
        assert_eq!(segment_char_range(10, 0, 4), (0, 4));
        assert_eq!(segment_char_range(10, 1, 4), (4, 8));
        assert_eq!(segment_char_range(10, 2, 4), (8, 10));
        // Out-of-range segment clamps to the end (empty).
        assert_eq!(segment_char_range(10, 3, 4), (10, 10));
    }

    #[test]
    fn w5_quads_for_segment_rebuckets_into_local_columns() {
        // A full-line quad spanning cols [2, 9), wrapped at width 4.
        let full = vec![(2u32, 9u32, 0xff0000u32)];
        // Segment 0 covers [0,4): overlap [2,4) → local [2,4).
        assert_eq!(quads_for_segment(&full, 0, 4), vec![(2, 4, 0xff0000)]);
        // Segment 1 covers [4,8): overlap [4,8) → local [0,4).
        assert_eq!(quads_for_segment(&full, 4, 8), vec![(0, 4, 0xff0000)]);
        // Segment 2 covers [8,12): overlap [8,9) → local [0,1).
        assert_eq!(quads_for_segment(&full, 8, 12), vec![(0, 1, 0xff0000)]);
        // A segment with no overlap drops the quad.
        assert!(quads_for_segment(&full, 12, 16).is_empty());
    }

    #[test]
    fn w5_slice_runs_to_char_range_preserves_styles_per_segment() {
        let font = gpui::font("monospace");
        // "aabbbb" — run A (red, 2 chars) + run B (green, 4 chars).
        let combined = "aabbbb";
        let runs = vec![
            make_run_with_color(0xff0000, 2, &font),
            make_run_with_color(0x00ff00, 4, &font),
        ];
        // Segment 0 = chars [0,4) = "aabb": run A full (2) + run B partial (2).
        let (text0, runs0) = slice_runs_to_char_range(combined, &runs, 0, 4);
        assert_eq!(text0, "aabb");
        assert_eq!(runs0.len(), 2);
        assert_eq!(runs0[0].len, 2);
        assert_eq!(runs0[1].len, 2);
        let red: gpui::Hsla = rgb(0xff0000).into();
        let green: gpui::Hsla = rgb(0x00ff00).into();
        assert_eq!(runs0[0].color, red);
        assert_eq!(runs0[1].color, green);
        // Run lengths sum to the segment byte length (ascii: 1 byte/char).
        assert_eq!(runs0.iter().map(|r| r.len).sum::<usize>(), text0.len());
        // Segment 1 = chars [4,6) = "bb": only run B (green, 2 chars).
        let (text1, runs1) = slice_runs_to_char_range(combined, &runs, 4, 6);
        assert_eq!(text1, "bb");
        assert_eq!(runs1.len(), 1);
        assert_eq!(runs1[0].len, 2);
        assert_eq!(runs1[0].color, green);
    }

    #[test]
    fn w5_slice_runs_to_char_range_cuts_on_char_boundaries() {
        let font = gpui::font("monospace");
        // "→→ab" — 2 arrows (3 bytes each) + 2 ascii, one run over all.
        let combined = "→→ab";
        let runs = vec![make_run_with_color(0x123456, combined.len(), &font)];
        // Chars [0,2) = "→→" (6 bytes) — must not split a codepoint.
        let (text, sliced) = slice_runs_to_char_range(combined, &runs, 0, 2);
        assert_eq!(text, "→→");
        assert_eq!(sliced.len(), 1);
        assert_eq!(sliced[0].len, "→→".len());
        assert_eq!(sliced[0].len, text.len());
    }

    #[test]
    fn text_runs_no_spans_one_default_run() {
        let (resolved, ids) = theme_defaults();
        let line = "let x = 1;";
        let (combined, runs, offsets) = build_line_with_inlays(
            line,
            &[],
            &[],
            &gpui::font("monospace"),
            0x7f849c,
            &resolved,
            &ids,
        );
        assert!(offsets.is_empty());
        assert_eq!(combined, line);
        assert_eq!(runs.len(), 1, "no-span line collapses to a single run");
        assert_eq!(runs[0].len, line.len());
        let expected: gpui::Hsla = rgb(syntax_color(SyntaxStyle::Default, &resolved, &ids)).into();
        assert_eq!(runs[0].color, expected);
    }

    #[test]
    fn text_runs_collapse_same_style_split_on_change() {
        let line = "abcde";
        let spans = vec![
            StyledSpan {
                start: 0,
                end: 2,
                style: SyntaxStyle::Keyword,
            },
            StyledSpan {
                start: 2,
                end: 3,
                style: SyntaxStyle::String,
            },
            StyledSpan {
                start: 3,
                end: 5,
                style: SyntaxStyle::Keyword,
            },
        ];
        let (resolved, ids) = theme_defaults();
        let (_, runs, _) = build_line_with_inlays(
            line,
            &spans,
            &[],
            &gpui::font("monospace"),
            0x7f849c,
            &resolved,
            &ids,
        );
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0].len, 2);
        assert_eq!(runs[1].len, 1);
        assert_eq!(runs[2].len, 2);
        let total: usize = runs.iter().map(|r| r.len).sum();
        assert_eq!(total, line.len());
    }

    #[test]
    fn text_runs_empty_line_no_runs() {
        let (resolved, ids) = theme_defaults();
        let (combined, runs, offsets) = build_line_with_inlays(
            "",
            &[],
            &[],
            &gpui::font("monospace"),
            0x7f849c,
            &resolved,
            &ids,
        );
        assert!(combined.is_empty());
        assert!(runs.is_empty());
        assert!(offsets.is_empty());
    }

    #[test]
    fn gutter_text_format_renders_padding_and_default_chars() {
        let meta = GutterLineMeta {
            line_idx: 0,
            display_line: 0,
            fold_marker: None,
            severity: None,
            diff_sign: None,
            is_virtual: false,
        };
        // {sev}{diff}{num:>3} {fold} = "  " + "  1" + " " + " " + " "
        // = "    1   " (8 chars): fold marker now trails the number.
        assert_eq!(format_gutter_text(&meta, 3, true, true), "    1   ");
    }

    #[test]
    fn gutter_text_format_fold_marker() {
        let meta = GutterLineMeta {
            line_idx: 41,
            display_line: 41,
            fold_marker: Some((FOLD_GLYPH_CLOSED, 0x9399b2)),
            severity: None,
            diff_sign: None,
            is_virtual: false,
        };
        // {sev}{diff}{num:>3} {fold} = "  " + " 42" + " " + "▸" + " "
        // = "   42 ▸ " (8 chars): the glyph sits AFTER the number now.
        assert_eq!(format_gutter_text(&meta, 3, true, true), "   42 ▸ ");
    }

    #[test]
    fn gutter_text_format_severity_glyph() {
        let meta = GutterLineMeta {
            line_idx: 9,
            display_line: 9,
            fold_marker: None,
            severity: Some(('E', 0xff0000)),
            diff_sign: None,
            is_virtual: false,
        };
        // 'E' + ' ' (diff) + "10" + ' ' + ' ' (fold) + ' ' = "E 10   ".
        assert_eq!(format_gutter_text(&meta, 2, true, true), "E 10   ");
    }

    #[test]
    fn gutter_text_format_diff_sign_left_of_line_number() {
        let meta = GutterLineMeta {
            line_idx: 9,
            display_line: 9,
            fold_marker: None,
            severity: None,
            diff_sign: Some(('+', 0x33aa33)),
            is_virtual: false,
        };
        // D.3.d.2: ' ' (sev) + '+' (diff) + "10" + ' ' + ' ' (fold) + ' '
        // = " +10   ".
        assert_eq!(format_gutter_text(&meta, 2, true, true), " +10   ");
    }

    #[test]
    fn gutter_text_signcolumn_no_drops_severity_and_diff_cells() {
        // PU.1b-1a: with `signcolumn=no` the severity + diff cells are
        // gone even when a diagnostic + hunk touch the line. Gutter is
        // line-number + separator + fold + trail: "10" + ' ' + ' ' + ' '
        // = "10   ".
        let meta = GutterLineMeta {
            line_idx: 9,
            display_line: 9,
            fold_marker: None,
            severity: Some(('E', 0xff0000)),
            diff_sign: Some(('+', 0x33aa33)),
            is_virtual: false,
        };
        assert_eq!(format_gutter_text(&meta, 2, false, true), "10   ");
        // The reserved (default) form keeps both sign cells.
        assert_eq!(format_gutter_text(&meta, 2, true, true), "E+10   ");
        // build_gutter_runs must not mis-slice the gated text: with no
        // sign cells the runs are (line-number+separator), (fold slot),
        // (trailing gap) = 3 runs summing to the full text length.
        let runs = build_gutter_runs("10   ", &meta, gpui::font("monospace"), false);
        assert_eq!(runs.len(), 3);
        assert_eq!(runs.iter().map(|r| r.len).sum::<usize>(), "10   ".len());
    }

    #[test]
    fn gutter_runs_colour_the_fold_glyph_separately() {
        // The fold glyph rides its own run in its resolved colour, sliced
        // out from between the line-number run and the trailing gap even
        // though the glyph is multi-byte (`▸` = 3 bytes).
        let meta = GutterLineMeta {
            line_idx: 41,
            display_line: 41,
            fold_marker: Some((FOLD_GLYPH_CLOSED, 0xabcdef)),
            severity: None,
            diff_sign: None,
            is_virtual: false,
        };
        let text = format_gutter_text(&meta, 3, true, true); // "   42 ▸ "
        let runs = build_gutter_runs(&text, &meta, gpui::font("monospace"), true);
        // Runs sum to the whole text (no bytes dropped / double-counted).
        assert_eq!(runs.iter().map(|r| r.len).sum::<usize>(), text.len());
        // Exactly one run carries the fold colour, and it is glyph-wide.
        let fold_runs: Vec<_> = runs
            .iter()
            .filter(|r| r.color == rgb(0xabcdef).into())
            .collect();
        assert_eq!(fold_runs.len(), 1, "one run should carry the fold colour");
        assert_eq!(fold_runs[0].len, FOLD_GLYPH_CLOSED.len_utf8());
    }

    #[test]
    fn byte_to_combined_col_no_inlays_matches_char_count() {
        // 'café' is 5 bytes utf-8 (c=1, a=1, f=1, é=2). Char-col at
        // byte 3 (start of é) is 3; the EOL byte (5) is 4 chars.
        assert_eq!(byte_to_combined_col("café", 3, &[]), 3);
        assert_eq!(byte_to_combined_col("café", 5, &[]), 4); // EOL
        // ASCII fast path: char-col == byte for the whole prefix.
        assert_eq!(byte_to_combined_col("abc", 2, &[]), 2);
    }

    #[test]
    fn byte_to_combined_col_shifts_by_inlay_widths_before_or_at_byte() {
        // Line "let x = 1;", inlay ": i32" (5 chars) spliced at
        // byte 5 (just after "let x"). Cursor at byte 5 ("=") sits
        // AFTER the inlay -- the inlay's orig_byte (5) is <= cursor
        // byte (5), so col shifts by 5.
        let line = "let x = 1;";
        let inlays = vec![(5u32, 5u32)];
        assert_eq!(byte_to_combined_col(line, 5, &inlays), 5 + 5);
        // Cursor at byte 4 ("x") sits BEFORE the inlay -- no shift.
        assert_eq!(byte_to_combined_col(line, 4, &inlays), 4);
        // Cursor at EOL: shift still applies.
        assert_eq!(
            byte_to_combined_col(line, line.len(), &inlays),
            line.chars().count() + 5
        );
    }

    #[test]
    fn build_line_with_inlays_no_inlays_matches_build_text_runs() {
        let line = "let x = 1;";
        let spans = vec![StyledSpan {
            start: 0,
            end: 3,
            style: SyntaxStyle::Keyword,
        }];
        let font = gpui::font("monospace");
        let (resolved, ids) = theme_defaults();
        let (combined, runs, offsets) =
            build_line_with_inlays(line, &spans, &[], &font, 0x7f849c, &resolved, &ids);
        assert_eq!(combined, line, "no inlays => combined == line");
        assert!(offsets.is_empty(), "no inlays => no offsets");
        let total_len: usize = runs.iter().map(|r| r.len).sum();
        assert_eq!(total_len, line.len(), "runs must cover the line");
    }

    #[test]
    fn build_line_with_inlays_splices_text_and_records_offset() {
        let line = "let x = 1;";
        let spans: Vec<StyledSpan> = Vec::new();
        let inlays: Vec<(usize, &str)> = vec![(5, ": i32")];
        let font = gpui::font("monospace");
        let (resolved, ids) = theme_defaults();
        let (combined, runs, offsets) =
            build_line_with_inlays(line, &spans, &inlays, &font, 0x7f849c, &resolved, &ids);
        assert_eq!(
            combined, "let x: i32 = 1;",
            "inlay text spliced before byte 5 of orig line"
        );
        assert_eq!(offsets, vec![(5, 5)], "one inlay, 5 chars wide");
        let total_len: usize = runs.iter().map(|r| r.len).sum();
        assert_eq!(total_len, combined.len(), "runs cover combined text");
        // 3 runs: orig prefix (default color), inlay (overlay1),
        // orig suffix (default color again).
        assert_eq!(runs.len(), 3);
        let expected_inlay_color: gpui::Hsla = rgb(0x7f849c).into();
        assert_eq!(runs[1].color, expected_inlay_color);
    }

    #[test]
    fn build_line_with_inlays_trailing_inlay_appended_at_eol() {
        let line = "fn foo()";
        let inlays: Vec<(usize, &str)> = vec![(line.len(), " -> i32")];
        let (resolved, ids) = theme_defaults();
        let (combined, _, offsets) = build_line_with_inlays(
            line,
            &[],
            &inlays,
            &gpui::font("monospace"),
            0x7f849c,
            &resolved,
            &ids,
        );
        assert_eq!(combined, "fn foo() -> i32");
        assert_eq!(offsets, vec![(line.len() as u32, 7)]);
    }

    // --- Slice E.1 — push_range_quads (overlay-quad pre-bucket) ---

    use lattice_core::protocol::position::{Position, Range};

    fn range(start_line: u32, start_byte: u32, end_line: u32, end_byte: u32) -> Range {
        Range {
            start: Position::new(start_line, start_byte),
            end: Position::new(end_line, end_byte),
        }
    }

    #[test]
    fn push_range_quads_skips_rows_outside_range() {
        let line_text = "hello world";
        let mut out = Vec::new();
        let ranges = [range(5, 0, 5, 5)];
        push_range_quads(&mut out, &ranges, 3, line_text, &[], 0xaaaaaa);
        push_range_quads(&mut out, &ranges, 7, line_text, &[], 0xaaaaaa);
        assert!(out.is_empty(), "rows outside the range emit no quads");
    }

    #[test]
    fn push_range_quads_single_line_range_clipped_to_bytes() {
        let line_text = "hello world";
        let mut out = Vec::new();
        // Range covers bytes 6..11 ("world").
        push_range_quads(&mut out, &[range(2, 6, 2, 11)], 2, line_text, &[], 0xbeef);
        assert_eq!(out, vec![(6, 11, 0xbeef)]);
    }

    #[test]
    fn push_range_quads_multi_line_range_uses_full_line_in_middle() {
        let line_text = "abcdef";
        let mut out = Vec::new();
        // Range spans lines 4..=6; line 5 is fully covered.
        push_range_quads(&mut out, &[range(4, 2, 6, 3)], 5, line_text, &[], 1);
        assert_eq!(out, vec![(0, 6, 1)], "middle row gets [0, line_len) cols");
    }

    #[test]
    fn push_range_quads_multi_line_range_start_row_uses_start_byte() {
        let line_text = "abcdef";
        let mut out = Vec::new();
        push_range_quads(&mut out, &[range(4, 2, 6, 3)], 4, line_text, &[], 2);
        assert_eq!(
            out,
            vec![(2, 6, 2)],
            "start row spans [start_byte, line_len)"
        );
    }

    #[test]
    fn push_range_quads_multi_line_range_end_row_uses_end_byte() {
        let line_text = "abcdef";
        let mut out = Vec::new();
        push_range_quads(&mut out, &[range(4, 2, 6, 3)], 6, line_text, &[], 3);
        assert_eq!(out, vec![(0, 3, 3)], "end row spans [0, end_byte)");
    }

    #[test]
    fn push_range_quads_inlay_shifts_columns() {
        // Line "ab|cd" where a 2-char inlay is spliced before
        // byte 2; range 1..3 → cols 1..5 in combined space
        // (1 + 2 inlay chars before the closing byte).
        let line_text = "abcd";
        let inlay_offsets = [(2u32, 2u32)];
        let mut out = Vec::new();
        push_range_quads(
            &mut out,
            &[range(0, 1, 0, 3)],
            0,
            line_text,
            &inlay_offsets,
            0x42,
        );
        assert_eq!(out, vec![(1, 5, 0x42)]);
    }

    #[test]
    fn push_range_quads_empty_range_skipped() {
        let line_text = "abc";
        let mut out = Vec::new();
        push_range_quads(&mut out, &[range(0, 2, 0, 2)], 0, line_text, &[], 9);
        assert!(out.is_empty(), "end_byte == start_byte emits nothing");
    }

    #[test]
    fn push_range_quads_layering_preserved_by_push_order() {
        // Two layers stacked on the same row in order A, B; the
        // walk emits A then B so the painter (which overwrites
        // last-wins) draws B on top of A. The pre-bucket
        // preserves this contract by appending without sorting.
        let line_text = "hello";
        let mut out = Vec::new();
        push_range_quads(&mut out, &[range(0, 0, 0, 5)], 0, line_text, &[], 0xa);
        push_range_quads(&mut out, &[range(0, 1, 0, 4)], 0, line_text, &[], 0xb);
        assert_eq!(out, vec![(0, 5, 0xa), (1, 4, 0xb)]);
    }
}
