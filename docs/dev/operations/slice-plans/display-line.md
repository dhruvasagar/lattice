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

### B2.3 — synchronous always-current rebuild + edit-path bench  🗒

Actor runs the windowed incremental `DisplayMatrix` rebuild of the edited region
**before publishing** (in `dispatch`'s tail / publish), honouring the threading
guarantee above (text/structure only; stale-syntax → no highlight). Result:
`version.text` never lags the snapshot. Async worker retained for the
reparse-completion recolour. Add `display_edit_path` bench + record the bound in
`benchmarks.md`.

### B2.4 — TUI cutover  🗒

TUI body consumes `DisplayMatrix` (`text` + `runs` → ratatui cells, resolve
tag→colour); move `byte_to_combined_col` / `segment_count` onto `DisplayLine`;
**delete the TUI `cells_stale` guard** + the cell→span path. The flicker dies on
TUI here. (GPU still on the projected cells until B3.)

## B3 — GPU cutover  🗒

- GPUI consumes `DisplayMatrix`: build `TextRun`s over `combined`, one
  `shape_line` (LineLayoutCache-cached). **Delete the GPU `version.text` guard**
  and `shape_row_from_cells`.
- The flicker dies on GPU here.

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
