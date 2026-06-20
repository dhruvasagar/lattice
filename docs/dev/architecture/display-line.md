# Display-line model (retiring the per-character cell grid)

Status: design fragment (2026-06-04). Slice plan: `../operations/slice-plans/display-line.md`.
Supersedes the per-character `CellMatrix` substrate (`cell-grid-renderer.md`)
and the legacy scroll-windowed highlight cache (`highlights_worker` /
`VisibleSpans` / `VisibleRows`).

> **Amendment (2026-06-20) — the cell grid is RETAINED as GPUI's per-glyph
> projection (approach A).** B0–B3 + B2.5 landed: `DisplayMatrix` is the
> canonical, always-current cache and the flicker is gone. A pre-B4 deletion
> audit then found the GPU peer re-adopted the cell grid as its **production**
> per-glyph painter: `paint_cells_row` is the default active-pane glyph path
> (`editor_element.rs` `use_paint_cells = cell_matrix.is_some()`, S4.final.f;
> the `LATTICE_PAINT_CELLS` env-gate is now a no-op), and the Thread-F heading
> scale / variable-row-height work is built on it. So the original goal of
> *deleting* the cell grid is **descoped**: the `CellMatrix` lives on, but only
> as a **derived projection** of the canonical `DisplayMatrix`
> (`display_matrix_to_cell_matrix`) — not a second source of truth, so the
> two-cache fragmentation this design fixed does **not** return. What B4 still
> deletes is the redundant legacy *highlight cache* (`highlights_worker` /
> `VisibleSpans` / `VisibleRows` / `RowPrepaint` / `pane_highlights`), after
> migrating the last two consumers (TUI `draw_inactive_document`, GPUI
> `build_line_with_inlays` fallback) onto `DisplayMatrix`. Full cell-grid
> retirement (re-homing GPUI per-glyph painting onto a `DisplayLine` painter)
> is a separate future initiative, deferred on merit. See the slice plan's B4
> re-slice. `lattice_cells::Cell` + `cell_flags` are shared payload
> (virtual rows, multibuffer headers, file-tree) and were never in scope to
> delete.

> **Amendment (2026-06-20) — `highlights_worker` survives as `overlay_worker`
> (B4.2, approach B).** The worker had two jobs: the dead span/row highlight
> cache (`VisibleSpans`/`VisibleRows`/`RowPrepaint`) AND the **live**
> `static_overlay_quads` producer (`bucket_static_overlays` — search-match /
> substitute / doc-highlight backgrounds, consumed every frame by both
> renderers). B4.2 deleted only the dead cache and **kept** the overlay
> producer, renaming the worker → `overlay_worker` (`HighlightWake` →
> `OverlayWake`) to match its remaining job. Deleting the module wholesale would
> have silently dropped those highlight backgrounds — a UX regression caught
> before execution. So the legacy *highlight* cache is gone; the live *overlay*
> decoration path stays. Consolidating overlay bucketing into `cells_worker`
> (one worker) is a possible future slice, not pursued now. **B0–B4 COMPLETE.**

## Why this exists

The editor flickers on every keystroke: editing one line restyles the whole
viewport (backtick / link content "goes white", then recovers). Root cause is
**two parallel highlight caches plus per-renderer fallback guards**:

1. `CellMatrix` — a per-character grid (`Cell { codepoint, fg, bg, flags }`,
   16 bytes/char), chunked + windowed + incrementally rebuilt; the active-pane
   primary in both renderers.
2. `VisibleSpans` / `VisibleRows` (`RowPrepaint`) — the legacy scroll-windowed
   highlight cache from `highlights_worker`; the renderers' fallback and the
   inactive-pane / `*messages*` / help path.

Because the cell matrix is rebuilt **asynchronously**, for one frame after each
keystroke `matrix.version.text != snapshot.text_version`. Both renderers detect
that skew and **abandon the whole-viewport matrix**, falling back — TUI to plain
rope text, GPUI to `visible_spans`/`shape_row`. The fallback renders differently
from the matrix, so the entire viewport restyles for that frame. Editing the
bottom flickers the top because it is a *whole-viewport* fallback.

This is a fragmentation bug, not a per-char-vs-per-line bug — but it exposed
that the per-character grid is the wrong primitive for a GPU-accelerated editor
(see "Why per-line", below). We fix the fragmentation **and** move to the right
primitive in one migration, because the consolidation work is the same either
way and doing it twice is wasteful.

## The UX contract (the acceptance bar)

Per `feedback_ux_over_paramount_goals`: UX outranks the paramount goals when they
conflict. The bar for every keystroke:

- **Only the edited line may visibly change.** Every other visible line is
  pixel-identical frame-to-frame.
- **The typed character appears immediately** (display text is synchronous).
- The edited line's **syntax colour may lag** a frame or two (eventual
  consistency is acceptable) — it shows its prior colour until the reparse
  lands, never blank-then-recolour across the viewport.

## Why per-line (merit, not novelty)

`RowPrepaint { combined: Box<str>, runs: Vec<RowRun>, inlay_offsets }` already
exists and the GPU peer already consumes it. It is the renderer-agnostic per-line
display unit. Concretely better than the per-char grid:

- **GPU-native.** GPUI shapes `combined` directly with per-run colours in one
  `WindowTextSystem::shape_line` (cached by `LineLayoutCache`). The cell grid
  forces `shape_row_from_cells` to *un-bake* per-char cells back into a string +
  runs every rebuild — a redundant intermediate. Per-line removes it; shaping
  (ligatures, complex scripts) is cleaner.
- **One cache.** A per-line model subsumes BOTH the cell grid and the legacy
  `visible_spans`/`visible_rows` — they collapse into one windowed/incremental/
  always-current matrix. Deletes the second cache, both stale guards, and the
  fallback chains (the fragmentation source).
- **Lighter.** A few style runs per line vs 16 bytes × N chars.
- **Style as tags, resolved at paint.** `RowRun` carries a style tag; the
  renderer resolves tag→colour via the per-frame theme. Theme changes become
  instant (no content rebuild).

Paramount-goal alignment: protects #1 (UI thread reads one coherent pre-resolved
cache; no per-frame re-derivation or two-cache skew; display resolution —
inlay splice, tab expansion, whitespace markers, fold elision — happens once
off-thread). The per-char grid did not *violate* a paramount goal, so this is
chosen on merit (better GPU primitive + one cache), per
`feedback_long_term_design_over_quick_fix`.

## The model

`DisplayLine` — one per source line that survives fold elision:

	struct DisplayLine {
		source_line: u32,        // logical source line (for gutter, marks, motions)
		text: Box<str>,          // final display string: inlays spliced, tabs
		                         // expanded to display width, whitespace markers
		                         // substituted (fold elision is at line granularity:
		                         // a folded interior produces no DisplayLine)
		runs: Vec<RowRun>,       // style-tagged byte runs over `text`
		col_map: Arc<[(u32, u32)]>, // source-byte -> extra display columns inserted
		                         // (inlay/tab expansion); drives byte<->display-col
		col_count: u32,          // display width, for wrap segment_count
		fold: Option<FoldHead>,  // Some when this line heads a closed fold
	}

`DisplayMatrix` — reuses the existing `CellMatrix` machinery verbatim, only the
row payload changes from `Vec<Cell>` to `DisplayLine`:

- chunked + viewport-windowed (H.3 `WINDOW_CAP_LINES`, overscan, `window_bounds`),
- `MatrixVersion` axes (text / syntax / inlay / folds / theme / whitespace),
- incremental rebuild with the **single shared `rebuild_zone_rows`** reuse
  (unchanged `DisplayLine`s are `Arc`-reused byte-identical → pixel-stable;
  only the edited line(s) recolour),
- `row_at_source_line` / `slice` / `covers` / `segment_count` unchanged in shape.

Coordinate helpers (`byte_to_combined_col`, `segment_count`) move from `CellRow`
onto `DisplayLine` with the same logic (they already operate on `inlay_offsets` +
`col_count`).

## Always-current (kills the stale guard)

The fallback guards exist only because the cache lagged the snapshot for a frame.
Fix: the actor (core thread) runs the **synchronous incremental rebuild of the
edited region before publishing** the render state, so the published
`DisplayMatrix` is always text-current (`version.text == snapshot.text_version`).
It is O(viewport) (post-H.3) and off the UI thread, so paramount #1 holds. The
async worker remains for non-edit triggers (reparse-completion recolour, theme,
fold). With the matrix always text-current, **both stale guards and both
fallback paths are deleted** — the renderers always render the matrix. Unchanged
lines are reused (stable); the edited line shows new text immediately and
recolours when the reparse lands (eventual, per the contract).

## Renderers become thin mappers

- **TUI**: `DisplayLine` → ratatui cells (`text` + `runs`, resolve tag→colour).
- **GPU**: `DisplayLine` → `TextRun`s over `combined`, one `shape_line`
  (LineLayoutCache-cached). No un-bake.

Both differ only in tag→primitive mapping; caching/optimization/windowing live
once in the substrate.

## Rejected alternatives

- **Keep `CellMatrix`, fix fragmentation in place (the surgical patch).** Fixes
  the flicker at lower risk but keeps the per-char grid — the redundant GPU
  intermediate. Rejected on merit: when the consolidation must happen anyway,
  doing it on the better primitive is the right destination
  (`feedback_long_term_design_over_quick_fix`).
- **Baked RGB on runs (vs style tags).** Forces a full content rebuild on every
  theme change. Rejected: tags + per-frame resolution is cheaper and instant.

## Slice plan

See `../operations/slice-plans/display-line.md` (B0 design → B1 type + machinery
→ B2 always-current + TUI cutover → B3 GPU cutover → B4 delete legacy).
