# Soft line-wrapping — slice plan

Sequencing companion to
[`../../architecture/soft-wrap.md`](../../architecture/soft-wrap.md).
The design fragment owns *what* and *why*; this file owns *when* and
*in what order*. Each slice lands green with the four artefacts.

Confirmed decisions (2026-06-03): wrap geometry is computed in the
off-thread cells worker and published in the `CellMatrix` (Option A);
pane-width changes trigger a debounced full rebuild through the
worker's existing recompute path; `display.whitespace.*` is respected
for free because wrapping slices already-decorated cells.

| Slice    | Title                                                                                   | Status |
|----------|-----------------------------------------------------------------------------------------|--------|
| **W.1**  | ✅ `viewport_width` plumbed into `PaneCellsInputs` from `PaneState.viewport_width` (production site `dispatch.rs`; all test/bench fixtures updated). No behaviour change — width accepted, unused until W.2. 666 host tests green. (build_matrix/build_chunk_rows threading moves to W.2 where it's consumed.) | ✅ |
| **W.2**  | ✅ A2 data model. `lattice_cells::wrap_segments(col, width)` + `CellMatrix.wrap_width` field + `CellMatrix::segment_count(line)`. Worker (`recompute_pane`) stamps `wrap_width = pane.wrap ? viewport_width : 0` onto every published matrix; wrap/width change invalidates via a direct `existing.wrap_width` compare in the cache-hit guard (no `MatrixVersion` axis churn). `PaneCellsInputs.wrap` plumbed from `option_cache.wrap_lines`. 1 `CellRow`/line preserved. Tests: cells `wrap_segments_arithmetic` + `segment_count_reads_wrap_width`; host `recompute_pane_stamps_wrap_width_and_invalidates_on_toggle`. 667 host + 62 cells green. **Wrap-on build bench dropped as moot** — under A2 the build does identical work wrap on/off (only stamps a field); the new cost is render-side slicing, benched in W.4/W.5. | ✅ |
| **W.3**  | ✅ Host scroll seam filled. `bottom_anchored_scroll` rewritten as an upward accumulation summing `segment_count(line) + virtual_rows_at(line)` per source line (reads the active buffer's published `CellMatrix`). Replaced the `+overflow` jump (which overshot when a line spans >1 display row) with a single minimal-scroll pass; reproduces every M.V/scrolloff result exactly and handles wrap + lines-taller-than-viewport. Test `ensure_cursor_visible_accounts_for_wrapped_lines` (12 lines ×2 segments fit 5 in a 10-row viewport ⇒ scroll 7). 668 host tests green. | ✅ |
| **W.4**  | 🚧 TUI render **landed + verified**. `CellRow::segment(seg,width)`; `split_body_into_segments` (style-preserving, geometry == host `wrap_segments`); compose push-site splits each source line into segments — gutter number/fold/diag on segment 0, dim `↪` (U+21AA, no nerd-font dep) on continuations, height-capped. **Truncation fix (2026-06-03):** the body was clipped to `buffer_w` *before* the split, so `:set wrap` showed nothing; now truncation is skipped when wrap is on (`body_trunc_w = u32::MAX`) so the splitter sees the full line. Tests: `split_body_*`, `compose_wraps_long_line_when_wrap_on` (wraps into `↪` segments, tail renders), `compose_does_not_wrap_when_wrap_off`; wrap-off byte-identical (1471 TUI + 63 cells green). Resize re-wraps live (compose splits at current area width per frame). **Remaining: cursor screen-position mapping** under wrap in `cursor_screen_position_at` — `row += Σ(segment_count-1 of visible lines above cursor, fold-aware) + cursor_seg`; `body_col = display_col % wrap_width`. Cursor screen-position mapping landed (see W.4.t "Cursor display-position") and overlays align in cell-column space (W.4.t.1, 2026-06-05); TUI document wrap is feature-complete. GPUI parity landed in W.5; W.6/W.7 remain. | ✅ |
| **W.5**  | ✅ GPUI render parity (2026-06-05). `editor_element::prepaint` now expands each source line into `seg_count` display rows (the same `lattice_cells::wrap_segments` geometry the host scroll model + TUI use), reading `wrap_width` from the active `DisplayMatrix`/`CellMatrix`. Gutter number/fold/diag on segment 0; dim `↪` (U+21AA, `WRAP_CONT_GUTTER_COLOR`) continuation gutter, same `gutter_width+4` cell geometry. Active-pane body: `paint_cells_row` now takes a `&[Cell]` slice so `paint` paints `CellRow::segment(seg, wrap_width)` per row; fallback ShapedLine path slices `(combined, runs)` per segment (`slice_runs_to_char_range`, char-boundary safe). Cursor: `row += display_col / wrap_width`, `body_col = display_col % wrap_width` (mirrors TUI W.4), off-budget guard. **Mechanism deviation (on merit, vs. TUI's span-baking):** TUI applies overlays to the body spans *before* splitting, so segments inherit styles for free; GPUI paints overlays as column-positioned `paint_quad`s, so it **re-buckets** overlay + diagnostic quads per segment (`quads_for_segment`) into each segment's local column window. Wrap-off / single-segment lines push the full row verbatim → byte-identical to pre-W.5. Tests: `w5_segment_char_range_*`, `w5_quads_for_segment_*`, `w5_slice_runs_to_char_range_*` (incl. multibyte); 103 gpui tests green (`--features window`); full `--features gui` binary builds. Clears the W.4-was-TUI-only parity debt per `feedback_tui_gpui_parity`. **Manual GPUI visual check pending** (`cargo run --features gui -- --gui`). | ✅ |
| **W.6**  | `gj` / `gk` display-line motions + wrap-aware `g0` / `g$` in `lattice-grammar` | 🗒 |
| **W.7**  | Width-change debounced rebuild wiring + `:set wrap` flips it on end-to-end; `↪` icon-palette fallback; four-artefact close | 🗒 |

## W.4.t — Tab display width in the cell grid (Option A)

**Status:** ✅ core landed + tested 2026-06-03; overlay-alignment (W.4.t.1) landed 2026-06-05.

Surfaced from W.4: a literal `\t` in the body rendered at terminal
tab-stop width while the cell model counted it as 1 column, so
wrapped tab-indented lines (README line 2) mis-rendered. Chosen fix
**A** (user-confirmed): the cells builder models a tab at its true
display width — one width model the host scroll model + both
renderers share. Rejected B (renderer-local, desyncs host scroll) and
C (1-col tabs, poor indentation) per the UX-convention rule + heuristic #1.

**Landed (green: 669 host + 1472 TUI + 118 config):**
- `Tabstop` option default **8 → 4** (house style); all dependent
  tests/defaults updated.
- `cells_worker::build_chunk_rows` expands each `\t` to the next
  multiple of `tabstop` cells, tracking the running display column
  (`cells.len()`). Respects `display.whitespace.tab`: marker glyph
  leads + space fill when whitespace is shown and a glyph is set;
  plain spaces otherwise. `WhitespaceConfig.tabstop` plumbed +
  folded into both whitespace version-hashes (so `:set tabstop=N`
  rebuilds). Records the `fill-1` byte↔column expansion in
  `inlay_offsets` (reuses the existing mapping → GPUI
  `byte_to_combined_col` cursor/overlays get it for free).
- `col_count`/`segment_count` now reflect true tab width → **host
  scroll correct on tab lines**; cell body has **no literal `\t`** →
  **wrap renders tab lines correctly** (the reported bug).
- TUI compose skips the redundant second `apply_whitespace_decoration`
  on the cell-derived path (it was re-classifying the expanded
  fill-spaces + desyncing).
- TUI cursor `display_col_for_byte` made **tab-aware** (advances to
  the tab-stop) — previously `UnicodeWidthStr` treated `\t` as ~0.
- Tests: `recompute_pane_expands_tabs_to_tabstop_width`,
  `display_col_for_byte_expands_tabs_to_tabstop`, + the 4 default-8
  assertions updated to 4.

**Cursor display-position (landed 2026-06-03).** The cursor row is
now derived from the **same layout walk that paints** — the buffer↔
display coordinate transform — rather than a flat 1-row-per-line
count. `buffer_line_to_visible_row_with` sums, for every visible
source line, `above_vrows + segment_count + below_vrows` (folds
already collapse to the heading row); `segment_count = ⌈col_count /
wrap_width⌉` matches `split_body_into_segments` exactly. The cursor's
own position splits its display column: `row += display_col /
wrap_width` (which segment) and `body_col = display_col % wrap_width`
(column within it). This is the principled approach the major editors
use — a single coordinate transform that accounts for **all**
height/width-affecting decorations (folds, wrap, virtual/block rows,
inline inlays) — closest to Zed's `DisplayMap`/`DisplaySnapshot`,
and equivalent to Neovim's logical↔display line mapping
(`screenpos()`/`w_wrow`). It is NOT a 2-segment special case: it is
correct for a line wrapping into any number of visual rows (test
`cursor_row_accounts_for_multi_segment_wrap_above_and_within` covers a
3-segment wrap, cursor both below and within). Fixes the
`dd`-deletes-the-wrong-line bug. Inline inlays already flow through
`display_col_for_byte` (inlay shift) + `byte_to_combined_col` (GPUI);
tabs flow through the same display-column path (W.4.t).

**W.4.t.1 — overlay alignment ✅ (2026-06-05; manual visual check pending).**
The TUI overlay appliers (`apply_match_overlay`, `apply_semantic_token_overlay`,
`apply_underline_overlay`) mapped **source-byte ranges directly onto body byte
positions**, assuming `body bytes == source bytes`. Tab expansion (and multi-byte
`→`/whitespace markers) broke that, so the visual / hlsearch / current-match /
diagnostics / semantic / document-highlight overlays (and the inlay splice) sat a
few cells off on tab-indented lines.

**Fix:** in `compose_visible_lines_inner`, map each overlay endpoint **source byte →
body column → body byte** (`map_ob` closure) before applying, gated on `body_from_cells`
(identity on the plain-text fallback, so plain ASCII is unchanged).

**Mechanism note (deviates from the original "use `byte_to_combined_col`" plan, on
merit):** the body's column model is **char-count** — what `split_body_into_segments`
slices by, matching the cells builder's `cells.len()` tab expansion — so the mapping uses
a dedicated char-count `source_byte_to_body_col` + `nth_char_byte`, **not** the
width-based `display_col_for_byte` and **not** the cell-row `byte_to_combined_col`. Two
reasons: (1) the TUI compose path reads the **display** matrix, which doesn't carry the
cell row's `inlay_offsets` table that `byte_to_combined_col` relies on for tab expansion;
(2) char-count (not display-width) is what the body is actually segmented by, so it also
fixes wide-char lines and stays identity for ASCII. Unit test
`w4t1_source_byte_maps_to_expanded_body_position`; wrap-off / non-tab lines byte-identical
(full TUI suite 1470 green). Visual confirmation (highlights land on the glyph on
tab-indented lines) is a manual TUI check.

## Sequencing

- **W.1** is the isolated plumbing prerequisite — width must reach the
  builder before anything wraps. Lands green with wrap still off (the
  width is accepted but unused).
- **W.2** depends on W.1. Introduces the segment data model + the
  off-thread segmentation. Gated by a bench so wrap-on build cost
  stays off the hot path.
- **W.3** depends on W.2 (needs published segment counts). The seam in
  `bottom_anchored_scroll` is already marked; this slice fills the
  `wrap_segments(line)` term. Reuses the `M.V` scroll-model tests.
- **W.4 / W.5** depend on W.2 (need segmented rows to render) and move
  in lockstep per `feedback_tui_gpui_parity`.
- **W.6** depends on W.2 (display-line motions resolve targets from
  segment geometry). `j` / `k` stay logical-line.
- **W.7** wires the debounced width-change rebuild and flips `:set
  wrap` on for real, with the continuation-marker icon fallback.

## Cross-references

- Display-row scroll model + the seam this rides on:
  [`multibuffer-views.md` §M.V](./multibuffer-views.md).
- `display.whitespace.*` decoration applied pre-publish in
  `cells_worker::build_chunk_rows` — wrapping respects it for free.
- Existing popup wrap to reuse: `manually_wrap_lines` /
  `wrap_aware_cursor_offset` in `lattice-ui-tui/src/render.rs`.
