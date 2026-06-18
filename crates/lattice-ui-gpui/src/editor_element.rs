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
//!   syntax colouring from worker-published `VisibleSpans`.
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
//! `VisibleSpans.spans[i]` covers absolute buffer line
//! `scroll_at_compute_time + i`, per
//! `lattice-syntax::Syntax::highlight_lines`. The element reads
//! with `spans.get(line_idx - scroll)`. If the worker hasn't
//! caught up to the current `scroll` (X1b idle-wake gap), `get`
//! returns `None` and the line paints in `SyntaxStyle::Default` —
//! better visual fail-mode than a panic.
//!
//! See `docs/dev/operations/render-thread-discipline-remediation.md`
//! §X3.full.

#![cfg(feature = "window")]

use std::sync::Arc;

use gpui::{
    App, Bounds, DefiniteLength, Element, ElementId, FontFeatures, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Length, Pixels, SharedString, ShapedLine, Style,
    TextRun, Window, fill, point, px, rgb, size,
};
use lattice_cells::CellMatrix;
use lattice_host::cursor_shape::CursorShape;
use lattice_host::display_matrix::DisplayMatrix;
use lattice_host::render_state::VisibleSpans;
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
    /// `true` => render the fold-start marker (►) in column 0.
    pub(crate) fold_start: bool,
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
    /// Worker-published spans. `spans[i]` covers absolute buffer
    /// line `scroll + i`. Fallback path — used when the cell
    /// matrix doesn't have a row for the visible line (boot
    /// frame before the cell-builder's first publish, folded
    /// lines, or out-of-coverage / buffer-switch gaps) and for
    /// inactive panes (which carry `cell_matrix == None` because
    /// the cells worker publishes only for the active document).
    pub(crate) visible_spans: Arc<VisibleSpans>,
    /// Pane scroll (top visible doc line index, 0-based).
    pub(crate) scroll: u32,
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
    /// S4.1 (2026-05-27): cell-grid substrate snapshot. Populated
    /// from `render_state.cells.matrix.load_full()` for the active
    /// pane; `None` for inactive panes (mirrors the
    /// `visible_spans` split, since the cells worker publishes
    /// only for the active document).
    ///
    /// When `Some` and the row's source line is covered by the
    /// matrix, `prepaint` shapes from cells (S4.0 converter →
    /// `shape_line`) and skips the legacy
    /// `build_line_with_inlays` walk. When `None` or the row is
    /// folded / out-of-matrix (boot frame, buffer-switch gap),
    /// dispatch falls through to `shape_row`. The intermediate
    /// `shape_row_from_prepaint` branch (highlights worker's
    /// `RowPrepaint`) retired in S4.3 — the worker still
    /// publishes `visible_rows` for TUI markdown / help /
    /// messages bodies in other render functions.
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
    /// GPU whole-viewport flicker. `cell_matrix` now feeds only the
    /// experimental per-glyph `paint_cells` path until B4.
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
    /// S4.final.b (2026-05-27): per-window glyph-id cache. When
    /// the runtime toggle `LATTICE_PAINT_CELLS=1` is set,
    /// `EditorElement::paint`'s body loop uses
    /// `crate::paint_cells::paint_cells_row` (which consumes
    /// this resolver) to emit per-cell `paint_glyph` calls
    /// instead of `ShapedLine::paint`. Always populated from
    /// `EditorView.glyph_resolver` so the cache survives across
    /// paints + across panes within the same window. Mutex
    /// because the resolve path mutates the cache on miss and
    /// `EditorElement::paint` takes `&self`.
    pub(crate) glyph_resolver: Arc<std::sync::Mutex<GlyphResolver>>,
}

/// Per-frame layout state. Slice X3.full.2 holds nothing.
pub(crate) struct EditorElementLayoutState;

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
        let gutter_chars: usize = if self.gutter.is_empty() {
            0
        } else {
            2 + self.gutter_width + 1
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
        let mut inlay_offsets_per_row: Vec<Vec<(u32, u32)>> =
            Vec::with_capacity(row_capacity);
        let mut diagnostic_segments_per_row: Vec<Vec<(u32, u32, u32)>> =
            Vec::with_capacity(row_capacity);
        let mut overlay_quads_per_row: Vec<Vec<(u32, u32, u32)>> =
            Vec::with_capacity(row_capacity);
        // D.3.b.1.gpui (2026-05-29): for each entry in
        // `self.gutter`, the shaped_text row index of the
        // corresponding doc row after virtual-row interleaving.
        // Cursor lookup remaps through this Vec.
        let mut doc_to_shaped_row_local: Vec<u32> =
            Vec::with_capacity(self.gutter.len());

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
        // fall through to the legacy `build_line_with_inlays` walk
        // over `visible_spans` + LSP inlay hints. (For the active pane
        // the body glyphs come from `paint_cells_row`; this shape is
        // the fallback for inactive panes / boot frames / folded
        // rows. The `visible_rows` highlight publishing stays — it
        // feeds TUI markdown / help / messages bodies elsewhere.)
        let build_runs =
            |line: &str,
             rel: usize,
             line_idx: u32|
             -> (String, Vec<TextRun>, Vec<(u32, u32)>) {
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
                    let line_spans: &[StyledSpan] = self
                        .visible_spans
                        .spans
                        .get(rel)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]);
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
        let diag_segments_for_row =
            |line_idx: u32, line_text: &str, inlay_offsets: &[(u32, u32)]| -> Vec<(u32, u32, u32)> {
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
                    let col_start =
                        byte_to_combined_col(line_text, start_byte, inlay_offsets) as u32;
                    let col_end =
                        byte_to_combined_col(line_text, end_byte, inlay_offsets) as u32;
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
        let worker_static_overlay_quads: Option<&[Vec<lattice_host::render_state::RowOverlayQuad>]> =
            self.worker_static_overlay_quads
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
        let overlay_quads_for_row =
            |line_idx: u32,
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
                if let Some(&Some(tint_color)) =
                    diff_tint_per_row.get(rel_row)
                {
                    let source_cols = line_text.chars().count() as u32;
                    let inlay_cols: u32 =
                        inlay_offsets.iter().map(|(_, w)| *w).sum();
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
                            color_for_layer(
                                lattice_host::render_state::OverlayLayer::DocHighlight,
                            ),
                        );
                        push_range_quads(
                            &mut quads,
                            all_matches,
                            line_idx,
                            line_text,
                            inlay_offsets,
                            color_for_layer(
                                lattice_host::render_state::OverlayLayer::AllMatches,
                            ),
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
                                let cs = byte_to_combined_col(
                                    line_text,
                                    start,
                                    inlay_offsets,
                                ) as u32;
                                let ce = byte_to_combined_col(
                                    line_text,
                                    end,
                                    inlay_offsets,
                                ) as u32;
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
                            color_for_layer(
                                lattice_host::render_state::OverlayLayer::Substitute,
                            ),
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
                        color_for_layer(
                            lattice_host::render_state::OverlayLayer::DocHighlight,
                        ),
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
            let visible_start = (self.scroll as usize).min(raw_lines.len());
            let visible_end = (self.scroll as usize)
                .saturating_add(self.viewport_height.max(1) as usize)
                .min(raw_lines.len());
            for line_idx in visible_start..visible_end {
                // W.5: respect the height cap (wrapped lines can fill
                // the viewport mid-window).
                if shaped_text.len() as u32 >= self.viewport_height {
                    break;
                }
                let rel = line_idx.saturating_sub(self.scroll as usize);
                // 2026-05-26: `self.text` carries the visible-window
                // text (slice A.4 + cursor-line fix follow-up), so
                // raw_lines is indexed by visible-row offset, not
                // by absolute line.
                let line = raw_lines.get(rel).copied().unwrap_or("");
                let (combined, runs, inlay_offsets) =
                    build_runs(line, rel, line_idx as u32);
                let full_diag =
                    diag_segments_for_row(line_idx as u32, line, &inlay_offsets);
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
                    &font,
                    font_size,
                    self.viewport_height,
                    window,
                    &mut shaped_text,
                    &mut shaped_gutter,
                    &mut row_meta,
                    &mut row_segment,
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
                    &mut inlay_offsets_per_row,
                    &mut diagnostic_segments_per_row,
                    &mut overlay_quads_per_row,
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
                        &mut inlay_offsets_per_row,
                        &mut diagnostic_segments_per_row,
                        &mut overlay_quads_per_row,
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
                let (combined, runs, inlay_offsets) =
                    build_runs(line, rel, meta.line_idx);
                let full_diag =
                    diag_segments_for_row(meta.line_idx, line, &inlay_offsets);
                let full_overlay =
                    overlay_quads_for_row(meta.line_idx, rel, line, &inlay_offsets);
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
                let gutter_text = format_gutter_text(meta, self.gutter_width);
                let gutter_runs = build_gutter_runs(&gutter_text, meta, font.clone());
                let shaped_g = window.text_system().shape_line(
                    SharedString::from(gutter_text),
                    font_size,
                    &gutter_runs,
                    None,
                );
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
                    &font,
                    font_size,
                    self.viewport_height,
                    window,
                    &mut shaped_text,
                    &mut shaped_gutter,
                    &mut row_meta,
                    &mut row_segment,
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
                        &mut inlay_offsets_per_row,
                        &mut diagnostic_segments_per_row,
                        &mut overlay_quads_per_row,
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
                        let char_col =
                            byte_to_combined_col(line, byte, cursor_row_inlays) as u32;
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
                            let shaped = if matches!(c.shape, CursorShape::Block)
                                && byte < line.len()
                            {
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
                                Some(window.text_system().shape_line(
                                    SharedString::from(ch.to_string()),
                                    font_size,
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
            // visible row hosts the cursor.
            if let Some((_, cur_row)) = prepaint.cursor_layout {
                let row_y = bounds.origin.y + line_height * (cur_row as f32);
                let pane_width = bounds.size.width;
                let row_bounds = Bounds::new(
                    point(bounds.origin.x, row_y),
                    gpui::size(pane_width, line_height),
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
            let row_y = bounds.origin.y + line_height * (row_idx as f32);
            for (col_start, col_end, color) in quads {
                let quad_x = text_origin_x + advance * (*col_start as f32);
                let quad_w = advance * ((*col_end - *col_start) as f32);
                let quad_bounds =
                    Bounds::new(point(quad_x, row_y), size(quad_w, line_height));
                window.paint_quad(fill(quad_bounds, rgb(*color)));
            }
        }

        // Gutter.
        for (i, shaped_g) in prepaint.shaped_gutter.iter().enumerate() {
            let line_y = bounds.origin.y + line_height * (i as f32);
            let origin = point(bounds.origin.x, line_y);
            if let Err(err) = shaped_g.paint(origin, line_height, window, cx) {
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
            let line_y = bounds.origin.y + line_height * (i as f32);
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
                                let seg = prepaint
                                    .row_segment
                                    .get(i)
                                    .copied()
                                    .unwrap_or(0);
                                let seg_cells =
                                    cell_row.segment(seg, prepaint.wrap_width);
                                // A continuation segment with no cells but
                                // whose fallback ShapedLine carries text
                                // (column models diverged) falls through so
                                // the ShapedLine segment paints it.
                                if seg_cells.is_empty()
                                    && seg > 0
                                    && !line_text.is_empty()
                                {
                                    false
                                } else if ligatures {
                                    // LG.1: ligatures on — bg quads from cell
                                    // matrix, glyphs from ShapedLine::paint so
                                    // multi-char runs are shaped together and
                                    // OpenType ligature sequences form.
                                    crate::paint_cells::paint_cells_row_bg_only(
                                        seg_cells,
                                        origin,
                                        prepaint.glyph_advance,
                                        line_height,
                                        window,
                                    );
                                    false
                                } else {
                                    crate::paint_cells::paint_cells_row(
                                        seg_cells,
                                        origin,
                                        prepaint.glyph_advance,
                                        line_height,
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
                            _ => false,
                        }
                    }
                    _ => false,
                }
            } else {
                false
            };
            if !painted_via_cells {
                if let Err(err) = shaped_line.paint(origin, line_height, window, cx) {
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

        // Slice X3.full.4: per-cell diagnostic underlines. Painted
        // after the text body so the underline sits visually under
        // the glyphs (2px quad along the row bottom). Pre-computed
        // in `prepaint` so this loop is allocation-free; segments
        // are already in combined-column space.
        for (row_idx, segs) in prepaint.diagnostic_segments_per_row.iter().enumerate() {
            if segs.is_empty() {
                continue;
            }
            let row_y = bounds.origin.y + line_height * (row_idx as f32);
            let underline_y = row_y + line_height - px(2.0);
            for (col_start, col_end, color) in segs {
                if col_end <= col_start {
                    continue;
                }
                let quad_x = text_origin_x + advance * (*col_start as f32);
                let quad_w = advance * ((*col_end - *col_start) as f32);
                let quad_bounds =
                    Bounds::new(point(quad_x, underline_y), size(quad_w, px(2.0)));
                window.paint_quad(fill(quad_bounds, rgb(*color)));
            }
        }

        // Cursor (painted on top for bar/underline; block re-stamps
        // the covered char in cursor_foreground via shaped_cursor_char).
        if let (Some(cursor), Some((char_col, row))) = (&self.cursor, prepaint.cursor_layout) {
            let cursor_x = text_origin_x + prepaint.glyph_advance * (char_col as f32);
            let cursor_y = bounds.origin.y + line_height * (row as f32);
            let origin = point(cursor_x, cursor_y);
            let advance = prepaint.glyph_advance;
            match cursor.shape {
                CursorShape::Block => {
                    let cell = Bounds::new(origin, size(advance, line_height));
                    window.paint_quad(fill(cell, rgb(self.theme.cursor_background)));
                    if let Some(shaped) = &prepaint.shaped_cursor_char {
                        if let Err(err) = shaped.paint(origin, line_height, window, cx) {
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
                    let bar = Bounds::new(origin, size(px(2.0), line_height));
                    window.paint_quad(fill(bar, rgb(self.theme.cursor_background)));
                }
                CursorShape::Underline => {
                    let underline_origin = point(origin.x, origin.y + line_height - px(2.0));
                    let underline = Bounds::new(underline_origin, size(advance, px(2.0)));
                    window.paint_quad(fill(underline, rgb(self.theme.cursor_background)));
                }
            }
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
            self.runs
                .push(make_run_with_color(self.current_color, self.current_len, self.font));
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
/// Catppuccin Mocha peach -- fold-start marker colour.
const FOLD_MARKER_COLOR: u32 = 0xfab387;
/// Fold-start glyph (right-pointing triangle).
const FOLD_MARKER_GLYPH: char = '►';

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
/// fold / severity / diff columns blank. Same total width as
/// `format_gutter_text` (`gutter_width + 4`) so continuation rows
/// align with their source line's gutter.
fn shaped_continuation_gutter(
    gutter_width: usize,
    font: &gpui::Font,
    font_size: Pixels,
    window: &mut Window,
) -> ShapedLine {
    // 3 leading blanks (fold + severity + diff) + right-aligned
    // marker in the number column + 1 trailing space.
    let text = format!("   {WRAP_CONT_MARKER:>gutter_width$} ");
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
    font: &gpui::Font,
    font_size: Pixels,
    viewport_height: u32,
    window: &mut Window,
    shaped_text: &mut Vec<ShapedLine>,
    shaped_gutter: &mut Vec<ShapedLine>,
    row_meta: &mut Vec<(u32, String)>,
    row_segment: &mut Vec<u32>,
    inlay_offsets_per_row: &mut Vec<Vec<(u32, u32)>>,
    diagnostic_segments_per_row: &mut Vec<Vec<(u32, u32, u32)>>,
    overlay_quads_per_row: &mut Vec<Vec<(u32, u32, u32)>>,
) -> bool {
    let total_chars = combined.chars().count();
    let single = seg_count <= 1 || wrap_width == 0;
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
                    window,
                ));
            }
        }
        row_meta.push((line_idx, line_text.to_string()));
        row_segment.push(seg);
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
        // Sticky rows are rendered at the pane top in the pre-pass;
        // skip them here so they don't double-paint in the content loop.
        .filter(|r| r.kind != lattice_cells::VirtualRowKind::Sticky)
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
    inlay_offsets_per_row: &mut Vec<Vec<(u32, u32)>>,
    diagnostic_segments_per_row: &mut Vec<Vec<(u32, u32, u32)>>,
    overlay_quads_per_row: &mut Vec<Vec<(u32, u32, u32)>>,
) {
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
    let flush =
        |color: Option<u32>, len: usize, runs: &mut Vec<TextRun>, font: &gpui::Font| {
            if len == 0 {
                return;
            }
            let resolved = color
                .filter(|c| *c != 0)
                .unwrap_or(body_color);
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
    let shaped_body = window.text_system().shape_line(
        SharedString::from(content),
        font_size,
        &runs,
        None,
    );
    // Gutter: fully blank-padded to match
    // `format_gutter_text`'s virtual-row width.
    let blank_gutter: String = " ".repeat(gutter_width + 4);
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
        lattice_cells::VirtualRowKind::DeletionBlock
        | lattice_cells::VirtualRowKind::Generic => {
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
        lattice_cells::VirtualRowKind::Filler => Vec::new(),
    };
    shaped_text.push(shaped_body);
    shaped_gutter.push(shaped_g);
    row_meta.push((u32::MAX, String::new()));
    // W.5: virtual rows are a single display row each (segment 0).
    row_segment.push(0);
    inlay_offsets_per_row.push(Vec::new());
    diagnostic_segments_per_row.push(Vec::new());
    overlay_quads_per_row.push(quads);
}

fn format_gutter_text(meta: &GutterLineMeta, gutter_width: usize) -> String {
    if meta.is_virtual {
        // D.3.b.1.gpui: virtual rows render a fully-blank
        // gutter so the column stays the same width as
        // document rows. Total width = 1 (fold) + 1 (sev) +
        // 1 (diff) + gutter_width + 1 (trail) = gutter_width + 4.
        return " ".repeat(gutter_width + 4);
    }
    let fold = if meta.fold_start {
        FOLD_MARKER_GLYPH
    } else {
        ' '
    };
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
        "{fold}{sev}{diff}{num:>width$} ",
        fold = fold,
        sev = sev,
        diff = diff,
        num = meta.display_line as usize + 1,
        width = gutter_width,
    )
}

/// Build the `TextRun`s for a gutter row. Three runs (fold, sev,
/// linenum+trailing-space) with their respective colours.
fn build_gutter_runs(text: &str, meta: &GutterLineMeta, font: gpui::Font) -> Vec<TextRun> {
    let mut runs = Vec::with_capacity(3);
    let mut bytes_consumed = 0usize;

    // Run 1: fold marker.
    let fold_color = if meta.fold_start {
        FOLD_MARKER_COLOR
    } else {
        GUTTER_NORMAL_COLOR
    };
    let fold_char = text.chars().next().unwrap_or(' ');
    let fold_len = fold_char.len_utf8();
    runs.push(TextRun {
        len: fold_len,
        font: font.clone(),
        color: rgb(fold_color).into(),
        background_color: None,
        underline: None,
        strikethrough: None,
    });
    bytes_consumed += fold_len;

    // Run 2: severity sign.
    let sev_color = meta.severity.map(|(_, c)| c).unwrap_or(GUTTER_NORMAL_COLOR);
    let sev_char = text[bytes_consumed..].chars().next().unwrap_or(' ');
    let sev_len = sev_char.len_utf8();
    runs.push(TextRun {
        len: sev_len,
        font: font.clone(),
        color: rgb(sev_color).into(),
        background_color: None,
        underline: None,
        strikethrough: None,
    });
    bytes_consumed += sev_len;

    // Run 3: D.3.d.2 diff sign (left of line number, between
    // severity and digits — Vim/Helix/Zed/VSCode convention).
    let diff_color = meta.diff_sign.map(|(_, c)| c).unwrap_or(GUTTER_NORMAL_COLOR);
    let diff_char = text[bytes_consumed..].chars().next().unwrap_or(' ');
    let diff_len = diff_char.len_utf8();
    runs.push(TextRun {
        len: diff_len,
        font: font.clone(),
        color: rgb(diff_color).into(),
        background_color: None,
        underline: None,
        strikethrough: None,
    });
    bytes_consumed += diff_len;

    // Run 4: line number + trailing space.
    let tail_len = text.len() - bytes_consumed;
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
        let expected: gpui::Hsla =
            rgb(syntax_color(SyntaxStyle::Default, &resolved, &ids)).into();
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
            fold_start: false,
            severity: None,
            diff_sign: None,
            is_virtual: false,
        };
        // fold + sev + diff + "  1" + trail = "     1 " (7 chars).
        assert_eq!(format_gutter_text(&meta, 3), "     1 ");
    }

    #[test]
    fn gutter_text_format_fold_marker() {
        let meta = GutterLineMeta {
            line_idx: 41,
            display_line: 41,
            fold_start: true,
            severity: None,
            diff_sign: None,
            is_virtual: false,
        };
        // ► + ' ' + ' ' + " 42" + ' ' = "►   42 " (7 chars).
        assert_eq!(format_gutter_text(&meta, 3), "►   42 ");
    }

    #[test]
    fn gutter_text_format_severity_glyph() {
        let meta = GutterLineMeta {
            line_idx: 9,
            display_line: 9,
            fold_start: false,
            severity: Some(('E', 0xff0000)),
            diff_sign: None,
            is_virtual: false,
        };
        // ' ' + 'E' + ' ' + "10" + ' ' = " E 10 ".
        assert_eq!(format_gutter_text(&meta, 2), " E 10 ");
    }

    #[test]
    fn gutter_text_format_diff_sign_left_of_line_number() {
        let meta = GutterLineMeta {
            line_idx: 9,
            display_line: 9,
            fold_start: false,
            severity: None,
            diff_sign: Some(('+', 0x33aa33)),
            is_virtual: false,
        };
        // D.3.d.2: ' ' (fold) + ' ' (sev) + '+' (diff) + "10" + ' ' (trail) = "  +10 ".
        assert_eq!(format_gutter_text(&meta, 2), "  +10 ");
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
        assert_eq!(byte_to_combined_col(line, line.len(), &inlays), line.chars().count() + 5);
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
        assert_eq!(out, vec![(2, 6, 2)], "start row spans [start_byte, line_len)");
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
        push_range_quads(&mut out, &[range(0, 1, 0, 3)], 0, line_text, &inlay_offsets, 0x42);
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
