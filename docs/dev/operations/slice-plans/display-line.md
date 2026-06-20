# Slice plan — display-line model (retire the per-character cell grid)

Design: `../../architecture/display-line.md`. Goal: one per-line, windowed,
incrementally-maintained, **always-text-current** display cache (`DisplayMatrix`
of `DisplayLine`), consumed identically by both renderers, replacing the
per-character `CellMatrix` AND the legacy `highlights_worker` /
`VisibleSpans` / `VisibleRows`. Kills the per-keystroke whole-viewport flicker.

**Acceptance bar (every slice respects it):** only the edited line may visibly
change per keystroke; all other visible lines stay pixel-identical; the typed
char appears immediately; syntax recolour may lag a frame (eventual). See
`feedback_ux_over_paramount_goals`.

**Placement:** `DisplayLine` + `DisplayMatrix` + `DisplayChunk` live in
`lattice-host` (alongside `RowRun`/`RowPrepaint`; `RowRun` needs
`lattice_syntax::Style`, and `lattice-cells` is dependency-free + slated for
deletion). They reuse the payload-agnostic `lattice_cells::{MatrixVersion,
EditDelta, CHUNK_SIZE_WHOLE_DOC, wrap_segments}` until B4.

## B0 — design fragment ✅ (2026-06-04)

`docs/dev/architecture/display-line.md` written + signed off.

## B1 — `DisplayLine` + `DisplayMatrix` + machinery  🚧

- `DisplayLine { source_line, text: Box<str>, runs: Vec<RowRun>, col_map:
  Arc<[(u32,u32)]>, col_count, fold: Option<FoldHead> }` (≈ `RowPrepaint` +
  line identity + wrap width + fold head).
- `DisplayChunk` / `DisplayMatrix`: port the `CellChunk`/`CellMatrix` machinery
  verbatim with the new payload — `empty`/`whole_doc`/`chunked`,
  `row_at_source_line`, `slice`, `covered_*`/`covers`, `segment_count`,
  `wrap_width`, version.
- Worker: a `build_display_*` path producing `DisplayLine`s (the
  `build_chunk_rows`/`build_row_cells` logic, but emitting `combined` text +
  `RowRun`s + `col_map` instead of `Vec<Cell>`); inlay splice / tab expansion /
  whitespace markers / fold elision identical.
- Single shared `rebuild_zone_rows` reuse (whole-doc + chunked) on `DisplayLine`
  — unchanged lines Arc-reused byte-identical.
- Tests: machinery parity (mirror `matrix.rs` tests) + a build parity test
  asserting the `DisplayLine` for a line yields the same visible text + styles
  as the current `CellRow`.
- NOT consumed by renderers yet. Both caches coexist this slice.

## B2 — always-current + TUI cutover  🚧

The flicker is the per-keystroke whole-viewport stale-guard fallback: the async
worker leaves the matrix one frame behind the snapshot, both renderers detect
`matrix.version.text != snapshot.text_version` and abandon the whole viewport.
B2 makes the matrix **always text-current** so the guard is unnecessary, then
cuts the TUI over and deletes its guard. Split into four green sub-slices.

### Threading guarantee (HARD constraint on every B2 sub-slice)

The UI thread **blocks on the actor per edit** (`mutate_blocking_with` /
`ApplyAndReply` → `dispatch` applies the edit + publishes in its tail, *then*
replies — editor_actor.rs:600-611). So the edit-critical (blocking) path must do
only **O(edit-size) work**:

- **ALLOWED in the sync edit path:** rebuild the edited line(s)' display *text* +
  structure (prefix-reuse + edited-line text rebuild + suffix `shifted_by` —
  Arc refcount bumps, O(window)). On a keystroke the syntax snapshot is stale,
  so `highlight_range` returns `None` and the rebuild does **zero highlighting**
  — it is microseconds, dominated by the rope edit + render-state build the
  actor already does per keystroke.
- **FORBIDDEN in the sync edit path:** any `highlight_lines` call; any
  synchronous tree-sitter reparse (reparse stays on its existing async path);
  any O(viewport)/O(file) work beyond the windowed Arc reuse.
- **Heavy work stays async** on the worker: full highlight (`highlight_lines`),
  reparse-completion recolour, theme/fold rebuilds. Syntax colour is eventual
  (edited line keeps prior/default colour for a frame or two — within the
  keystroke UX contract).
- **Enforced, not asserted:** B2.3 adds an edit-path bench asserting a hard bound
  (target < ~200µs for a typing keystroke on a 100k-line file). Exceeding it is
  a failing bench, not a shipped regression.

### B2.1 — per-pane `DisplayMatrix` output cell  ✅ (2026-06-04)

Mirror `cells_matrix_cell`: `Editor` per-buffer registry + `display_matrix_for`
+ boot seed (Arc identity), a `PaneCellsInputs.display_matrix` field, a
`CellsRenderState` field + `pane_matrices`-style map. No-op plumb (nothing builds
/ reads it yet). NOTE: the new `PaneCellsInputs` field hits the ~7 construction
sites the `scroll` field did in H.3a (render_state default, dispatch publisher,
virtual_rows_worker, three cells_worker test helpers, the bench) — update all.

### B2.2 — worker produces `DisplayMatrix`  ✅ (2026-06-04)

`recompute_pane` now produces the `DisplayMatrix` canonically via
`build_display_matrix` + `try_incremental_display_build` (mirrors of
`build_matrix` / `try_incremental_build`, reusing `pick_chunk_size` /
`window_bounds` / the prefix-reuse + suffix-shift partition; rows from
`build_display_rows`). **Single source of truth:** the cell grid is now a
projection (`display_matrix_to_cell_matrix` / `display_line_to_cell_row`) of the
canonical `DisplayMatrix`, written into the existing `cells_matrix` cell so the
not-yet-cut-over renderers (TUI until B2.4, GPU until B3) keep painting off the
derived cells. The four cell builders (`build_matrix`, `try_incremental_build`,
`build_chunk_rows`, `build_row_cells`) are `#[allow(dead_code)]` parity oracles
until B4 deletes them with the cell path.

Landed with:
- **`WS_TRAILING` cell flag** (`lattice-cells`): the `DisplayLine` model carries
  no byte positions, so a `DisplayRun` self-describes trailing-whitespace via
  this provenance bit; the projection (and the future renderers) resolve it →
  `theme.whitespace_trailing_style` fg, reproducing the cell path's trailing-red.
  The bit is stripped from the projected cell's flags (the cell path bakes
  trailing-fg into `fg` instead) so the projection stays byte-identical.
- **Per-keystroke reuse moved to `DisplayLine`**: unchanged `DisplayLine`s
  Arc-reuse their `text`/`runs`; the projected cell grid is rebuilt each tick
  (O(window), off-thread) — fine until B4 deletes it. The two `*_reuses_*` tests
  now assert `DisplayLine` Arc reuse on the canonical matrix.
- **Tests**: `projection_parity_ws_on_trailing_tab_inlay` (whole-matrix
  projection == `build_matrix` across ws-on / leading / interior / trailing /
  tab-expansion / inlay-splice). All 53 cells_worker + 686 host-lib tests green;
  TUI + GPUI libs compile.

### B2.3 — synchronous always-current rebuild + edit-path bench  ✅ (2026-06-04)

`dispatch::publish_render_state` now runs `cells_worker::sync_rebuild_pane_on_edit`
for each pane **before** `render_state.store` — so the published canonical
`DisplayMatrix` is text-current the instant the renderer paints; `version.text`
never lags the snapshot. The sync path attempts ONLY
`try_incremental_display_build` with a new `allow_highlight: false` flag (no
`highlight_lines` call ever lands on the edit-critical actor thread — enforced,
not incidental); ineligible publishes (no single edit, doc switch, chunk-shape
change, same-tick window miss) no-op and defer to the async worker. It
deliberately does NOT project to the cell grid — that O(window) projection stays
on the async worker, which `recompute_pane` now runs on a display-cache-hit when
the projected cells lag (so cells-consumers stay one worker-tick behind until the
TUI/GPU cutovers, B2.4/B3). The async worker keeps `allow_highlight: true` (full
colour); the edited line shows default fg for a frame or two until the reparse
lands and a later publish (syntax axis bumped, no `last_edit`) drives the worker's
highlighted rebuild.

Landed with:
- **Bench `display_edit_path`** + recorded in `benchmarks.md`: the sync path is
  **~4 µs flat across 5k→100k lines** (O(window)), ~45× under the 200 µs target.
- **O(file) bug the bench caught + fixed**: the chunked incremental rebuild's
  `rebuild_hi` defaulted to `new_line_count` when no suffix chunk remained (edit
  in the last covered chunk of a *windowed* matrix), rebuilding every row to EOF
  — 57 ms/keystroke on a 100k-line file. Now bounded to the published window's
  `covered_end_line()` shifted by `net`, in both
  `try_incremental_display_build` and its cell-path oracle.
- **Bench harness fix**: `rs_for` now threads a display-matrix cell so the
  incremental benches seed the *display* baseline (post-B2.2 the canonical build
  is the display matrix); previously they silently measured a full build.
- **Tests**: `sync_rebuild_on_edit_is_text_current_and_unhighlighted` (text-current
  + all-default runs despite a current syntax handle + prefix Arc-reuse),
  `sync_rebuild_skips_non_edit_publish`, `worker_projects_lagging_cells_after_sync_rebuild`.
  56 cells_worker + 689 host-lib tests green; TUI + GPUI libs compile.

### B2.4 — TUI cutover  🚧

Carved into B2.4a (cutover — the flicker fix) + B2.4b (delete the now-dead
cell→span path). B2.4a lands the user-visible win green; B2.4b is pure dead-code
removal with zero behaviour change.

#### B2.4a — TUI consumes `DisplayMatrix`  ✅ (2026-06-04)

The TUI document body reads `rs.cells.display_matrix` directly and resolves each
`DisplayLine`'s style-tagged `runs` → ratatui via the host theme at paint
(`cells_render::display_line_to_source_spans`, the `DisplayLine` analogue of
`cell_row_to_source_spans`). Resolution is byte-identical to the worker's
`display_line_to_cell_row` projection the TUI consumed pre-B2.4 (style→fg via
`theme.syntax_style`, `WS_TRAILING`→`whitespace_trailing_style` fg, fg `0`→pane
default, runs grouped by resolved style), so the cutover is visually invisible.

**The flicker dies here.** The old per-keystroke whole-viewport stale-guard fired
every keystroke because the async-projected cell grid lagged the snapshot by a
frame. The guard now reads the canonical `DisplayMatrix`, which B2.3 makes
text-current synchronously in the publish tail — so on a single-keystroke edit
`version.text == snap.text_version` and the guard does NOT fire. It remains (now
rarely firing) only for publishes the sync path skips (multi-edit batch, doc
switch), where plain current text beats stale styled text for a frame.

Also: the cursor-row wrap walk reads `DisplayMatrix::segment_count` /
`DisplayLine.col_count` (was `CellMatrix`/`CellRow`); added
`DisplayLine::byte_to_combined_col` (the `CellRow` analogue, forward-prep for the
B3 GPU cutover). Tests: 5 `display_*` resolver tests in `cells_render`; 46
cells_render + 1475 TUI + 7 display_matrix host tests green; GPU lib compiles.

#### B2.4b — delete the TUI cell→span path  ✅ (2026-06-04)

Deleted `cell_row_to_source_spans` / `cell_row_to_combined_spans` / `cells_to_spans`
/ `cell_to_style` (unused after the B2.4a cutover; the GPU has its own cell reader
until B3) and dropped the `lattice_cells::{Cell, CellRow}` import. The 13 Group-A
cell-conversion unit tests are deleted (their coverage moved to the `display_*`
resolver tests); the ~20 S3.c overlay-pipeline tests are re-housed onto a cell-free
`body(&[(text, fg)])` builder (the overlay functions —
`apply_whitespace_decoration` / `apply_*_overlay` / `splice_virtual_text_into_spans`
— are renderer-generic over `Vec<Span>`, so the bodies need no cell/display
provenance). `rgb_u32_to_color` stays (used by the new resolver). 33 cells_render +
1467 TUI tests green. The cells_render module is now display-only; only `CellMatrix`
(worker projection + GPU reader) survives, deleted in B3/B4.

## B3 — GPU cutover  ✅ (2026-06-04)

`EditorElement` now carries `display_matrix: Option<Arc<DisplayMatrix>>` (the
primary shaping source) + `host_theme` (a `Copy` struct, for per-run style→colour
resolution). The two prepaint shaping sites read `display_matrix.row_at_source_line`
and shape via the new `cells_paint::display_line_to_text_runs` (style-tagged runs →
`TextRun`s, resolving exactly as the worker's `display_line_to_cell_row` projection
— `reverse`/`dim`/bg/inlay-fg all reproduced by building a synthetic `Cell` per run
and reusing `cell_to_text_run`). `shape_row_from_cells` is deleted; `cell_row_to_text_runs`
is now used only by its own tests (deleted in B4).

**The flicker dies on GPU here**: the stale guard now reads the canonical
`DisplayMatrix`, which B2.3 keeps text-current synchronously, so it no longer fires
per keystroke.

Deviation from the original plan ("delete the version.text guard"): the guard is
**kept**, pointed at the display matrix. Deleting it would paint stale (pre-edit)
display rows on the rare publishes the sync path skips (multi-edit batch, doc
switch); keeping it falls those frames back to the legacy `shape_row` (current rope
text, syntax catches up next frame) — identical to the TUI's B2.4a decision and the
correct UX. The guard firing per-*keystroke* was the flicker; B2.3 stopped that.

`cell_matrix` is retained for the experimental env-gated `paint_cells` per-glyph
path only (still reads the worker's cell projection); it + the projection die in B4.
99 GPU + 19 cells_paint tests green; `cargo build --features gui -p lattice-cli`
links.

### B2.5 — chunked-zone row-reuse (the *second* flicker cause)  ✅ (2026-06-04)

B2.3/B2.4a/B3 each claimed "the flicker dies here" — but they only killed the
**version-stale guard** firing per keystroke. A *second*, independent flicker
survived and only showed on **chunked-mode** files (line_count > 4×viewport, e.g.
README at 500 lines): the syntax colour of the **entire viewport** toggled off→on
every keystroke even though `version.text` was current (`stale=false`) the whole
time, so the guard-based diagnosis missed it.

Root cause, in `try_incremental_display_build`: the **whole-doc** branch reused
prior `DisplayLine`s row-by-row (rebuild only `[edit_lo, affected_hi)`), but the
**chunked** branch rebuilt the entire `chunk_size`-aligned **rebuild zone**
wholesale via `build_display_rows`. On the sync edit path (`allow_highlight:
false`, B2.3) that meant ~`chunk_size` (≈64) lines rendered **colourless every
keystroke**; since the viewport sits inside one chunk, the whole screen lost
colour, recoloured a frame later by the async worker. That asymmetry — whole-doc
reused, chunked didn't — *was* the bug. (Violated `feedback_decorations_update_in_place`:
unchanged lines must never lose their cues.)

Fix: the chunked rebuild zone now does the **same row-level reuse** as whole-doc —
gather prior rows from the straddling chunks, reuse `< edit_lo` (unshifted) and
`>= pre_hi` (shifted by `net`), rebuild only `[edit_lo, affected_hi)`, then
re-bucket into `chunk_size`-aligned chunks (identical chunk starts to the old
wholesale loop, so next-edit prefix/suffix detection is unchanged — only the ROWS
differ, reused vs rebuilt). Now a keystroke decolours only the edited line
(text-synchronous, colour-eventual — the keystroke UX contract), never the chunk.
The `display_edit_path` bench profile is unchanged (still O(window); reuse trades
`build_display_row` calls for Arc clones).

Test: `chunked_incremental_reuses_rows_in_rebuild_zone` — an in-place edit mid-chunk
asserts unchanged lines (prefix, and a line after the edit in the same chunk) reuse
their prior `text` AND `runs` Arcs (the colour-carrying payload), while only the
edited line is rebuilt. 57 cells_worker + 691 host + 1467 TUI tests green.

## B4 — retire the legacy HIGHLIGHT CACHE (cell grid kept as GPUI projection)  🗒

**Re-sliced 2026-06-20 after a pre-deletion audit — approach (A).** The original
B4 ("delete the cell grid + projection + `paint_cells` + `shape_row`") is
**superseded**. The audit found the cell-grid projection is now GPUI's
**production** per-glyph source — `paint_cells_row` is the default active-pane
glyph path (`editor_element.rs` `use_paint_cells = cell_matrix.is_some()`,
S4.final.f; the `LATTICE_PAINT_CELLS` env-gate is dead, `paint_cells.rs:310`),
and the Thread-F heading scale / variable-row-height work is built on it. So the
cell grid is **retained** as an always-current *projection* of the canonical
`DisplayMatrix` (single source of truth — B2.3's flicker fix stands; a derived
projection reintroduces no two-cache skew). B4 now deletes only the redundant
legacy *highlight cache* and migrates the last two legacy consumers.

**Audit findings (2026-06-20).** The legacy symbols are NOT dead —
`pane_highlights` 33, `VisibleSpans` 38, `VisibleRows` 37, `highlights_worker`
22, `RowPrepaint` 28 refs across both renderers + host. Live consumers:
- **TUI** `draw_inactive_document` (render.rs:2929) — inactive panes via
  `pane_highlights`/`visible_rows`/`visible_spans`.
- **GPUI** the fallback shaping path (`build_line_with_inlays` + `visible_spans`,
  editor_element.rs:660) for folded/boot/inactive rows.

`lattice_cells::Cell` + `cell_flags` are **shared payload** (virtual rows,
multibuffer headers, file-tree, diff overlay, headerline) — KEPT.
`CellMatrix`/`CellChunk`/`CellRow` + the projection
(`display_matrix_to_cell_matrix`/`display_line_to_cell_row`) are **KEPT** as
GPUI's glyph projection. `shape_row_from_cells` was already deleted (B3).

### B4.1 — migrate the last legacy consumers → `DisplayMatrix`  🗒

Both renderers' remaining `visible_spans`/`pane_highlights` consumers move onto
the canonical `DisplayMatrix` (TUI + GPUI in lockstep):
- TUI `draw_inactive_document`: render inactive panes from the pane's
  `DisplayMatrix` (resolve `DisplayLine` runs → ratatui via the host theme — the
  same `display_line_to_source_spans` the active body uses). Closes the interim
  "inactive panes have no syntax colour" caveat.
- GPUI fallback: source folded/boot/inactive rows from `DisplayMatrix`; the
  `EditorElement.visible_spans` field + `build_line_with_inlays` retire.
- After this, NO production path reads `visible_spans`/`pane_highlights`/
  `visible_rows`/`highlights_worker` output.
- Tests: inactive-pane syntax-colour parity (both renderers);
  `multibuffer_is_a_regular_buffer` + active-body tests stay green.

### B4.2 — delete the dead legacy highlight cache  🗒

Once B4.1 lands (re-run the audit grep to confirm zero production refs), delete:
- `highlights_worker` (module + spawn) + the `highlight_wake` plumbing.
- `VisibleSpans` / `VisibleRows` / `RowPrepaint`.
- `pane_highlights` (Editor field/registry) + `refresh_pane_highlights`
  (+ `runtime.rs` caller) + `Action::RefreshPaneHighlights`.
- the GPUI `EditorElement.visible_spans` field + `build_line_with_inlays`.
Each deletion gated on a green build + a fresh grep proving no remaining
non-test reference. The two pre-disabled triggers (`refresh_pane_highlights`,
`highlight_wake`) go with their machinery. **Do NOT** touch
`CellMatrix`/`CellChunk`/`CellRow`/the projection/`Cell`/`cell_flags`.

### Out of scope (was the original B4) — full cell-grid retirement

Deleting `CellMatrix`/`CellChunk`/`CellRow` + the projection + `paint_cells`
would require re-homing GPUI's per-glyph painting (incl. the Thread-F heading
scale + bg quads) onto a `DisplayLine` painter. A separate future initiative
if/when the `ShapedLine` path can subsume per-glyph control — not pursued now
(heuristic #1: no concrete merit win over the always-current projection, and a
real regression risk on the paint hot path). The design fragment records the
cell grid's retained role as GPUI's per-glyph projection.

## Sequencing

B0 → B1 → B2 → B3 → B4. Each green + committed; TUI + GPUI move in lockstep at
their cutover slices. Bench: extend `cells_worker_windowed_build` (or a
`display_build` peer) so the per-line build keeps the O(viewport) profile.
