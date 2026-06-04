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

## B4 — delete legacy  🗒

- Consumer audit first (`*messages*`/help bodies, inactive panes, virtual rows,
  wrap, cursor, hit-test).
- Delete: `highlights_worker`, `VisibleSpans`/`VisibleRows`/`RowPrepaint`
  (legacy cells), `pane_highlights`, `CellMatrix`/`Cell`/`CellRow`/`CellChunk`,
  `shape_row`. Fold `MatrixVersion`/`EditDelta` into their final home.

## Sequencing

B0 → B1 → B2 → B3 → B4. Each green + committed; TUI + GPUI move in lockstep at
their cutover slices. Bench: extend `cells_worker_windowed_build` (or a
`display_build` peer) so the per-line build keeps the O(viewport) profile.
