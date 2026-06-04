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

## B2 — always-current + TUI cutover  🗒

- Actor runs the synchronous incremental `DisplayMatrix` rebuild of the edited
  region before publishing render state → `version.text` never lags the
  snapshot. Async worker retained for reparse-recolour / theme / fold.
- TUI body consumes `DisplayMatrix`; move `byte_to_combined_col` /
  `segment_count` onto `DisplayLine`; **delete the TUI `cells_stale` guard** and
  the cell→span path.
- The flicker dies on TUI here.

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
