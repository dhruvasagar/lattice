+++
title = "Soft line-wrapping (`:set wrap`)"
+++


Design fragment. Sequencing lives in the slice plan
[`../operations/slice-plans/soft-wrap.md`](../operations/slice-plans/soft-wrap);
this file owns the *what* and *why*.

## Status

Not implemented. `Wrap` is a registered option
(`lattice-config::core_options::Wrap`, default `false`) that flows
into `OptionCache::wrap_lines`, but nothing consumes it: the editor
body renders one display row per source line and clips at the right
edge (there is no horizontal-scroll / `leftcol` either). Popups
(help / hover) wrap via `manually_wrap_lines` +
`wrap_aware_cursor_offset` in `lattice-ui-tui`, but that path is
popup-only.

## Problem

With `wrap` on, one source line must occupy `N ≥ 1` *display* rows.
Today the entire stack assumes `1 source line == 1 display row`:
`CellRow` carries a single `source_line`, `CellMatrix.visible_line_count`
equals the post-fold source-line count, the host scroll model counts
doc lines, and both renderers walk rows 1:1. Wrapping breaks that
invariant everywhere a display row is produced or counted.

This is the **same class of divergence** as excerpt-header virtual
rows (see [`multibuffer-views.md`](../multibuffer-views/) §M.V): a
display row that is not a document line. The scroll model already
isolated the seam — `Editor::bottom_anchored_scroll` sums
`Σ_line (1 + virtual_rows_at(line))`, and the `1`-per-line term is
explicitly the place wrap-segment count joins.

## Decision — wrap geometry is computed off-thread and published

**The cells worker (`lattice-host::cells_worker`) computes wrapping
and publishes the resulting display geometry in the `CellMatrix`.**
Renderers and the host scroll model *read* that geometry; none of
them re-derive it.

Rejected alternative: wrapping at paint time in each renderer. It
traps wrap geometry on the render thread where the host scroll model
can't see it (re-opening the display-row-vs-doc-line scroll bug under
wrap), forks the algorithm across TUI and GPUI
(violating cross-renderer parity), and would have to re-derive
`display.whitespace.*` decoration to find correct break columns.

### Paramount-goal alignment

- **#1 (sub-frame latency).** Wrap segmentation runs on the existing
  off-thread cell builder, once per edit / width-change, *not* per
  frame. The render thread iterates pre-computed rows. Segmentation
  of monospace cells is a column-chunk (break every `width` columns),
  cheap even off-thread; a bench gates the wrap-on build cost.
- **#2 (extensibility) / everything-is-a-buffer.** Wrap geometry is a
  single published fact every consumer reads uniformly — no
  `BufferKind` branch, no per-renderer wrap logic. A new renderer or
  the scroll model reads `visible_row_count` / `rows_for_source_line`
  the same way.
- **#4 (asynchronicity).** The width-change rebuild is debounced
  through the worker's existing recompute path; nothing blocks the
  UI.

### `display.whitespace.*` is respected for free

The cell builder applies whitespace decoration (tab → `→`,
trailing → `·`, …) per cell *before* the matrix is published
(`cells_worker::build_chunk_rows`). Because wrapping slices the
already-decorated cells, wrap break points fall on displayed columns
automatically — a tab rendered as a multi-column glyph wraps at its
displayed width, not its source byte width. Paint-time wrapping would
have had to re-apply whitespace rules to find break columns; Option A
makes the requirement a non-issue.

## Data model — A2 (publish `wrap_width`, slice at paint)

Locked 2026-06-03 after confirming each `Cell` is exactly one display
column (`CellRow::col_count == cells.len()`, whitespace already
decorated into cells). Wrap breaks therefore fall at trivial column
boundaries `k·width` — there is no per-renderer *wrapping decision*,
only arithmetic slicing of an already-built, already-highlighted cell
array.

- **`CellMatrix` keeps one `CellRow` per source line.** The
  source-line ↔ matrix-row 1:1 invariant — which `row_at_source_line`,
  the incremental-rebuild line-delta path (S2.4.b), `pick_chunk_size`,
  fold elision, and virtual-row anchoring all rest on — is preserved
  untouched. The matrix gains a single published field `wrap_width:
  u32` (`0` ⇒ wrap off / not laid out) recording the column width the
  pane was built at.
- **Display geometry is derived, not stored.** Per source line,
  `segment_count(line) = max(1, ⌈col_count / wrap_width⌉)` when
  `wrap_width > 0`, else `1`. The host scroll model reads this
  (`row_at_source_line(line).col_count` + `wrap_width`); both
  renderers slice `cells[k·width .. (k+1)·width]` into display rows at
  paint. Slicing is a sub-borrow of the same `Arc<[Cell]>` — O(viewport
  columns), pure arithmetic, no shaping/wrapping computation on the
  render thread.
- **Syntax highlighting + whitespace are preserved for free.** Colour
  (`Cell.fg`) and style flags are resolved per cell at build time;
  slicing partitions the coloured cells without touching them, so a
  wrapped segment carries exactly the colours/styles of the unwrapped
  row at the same columns. Wrapping cannot desync highlighting from
  text — there is no re-highlight-per-display-row step.
- **Width** enters via `PaneCellsInputs.viewport_width` (W.1, landed).
  The host does **not** need to know width — it reads `wrap_width` off
  the published matrix.

### Rejected: A1 (builder emits N `CellRow`s per source line)

Earlier draft of this doc. It bloats `visible_line_count` into
post-wrap space and breaks the 1:1 source-line ↔ matrix-row invariant
the whole stack depends on, forcing re-derivation across
`row_at_source_line`, the incremental rebuild, chunk sizing, fold
elision, and virtual-row anchoring. Large, fragile blast radius for
the easier-to-picture model — heuristic #1 (long-term fit beats easy
implementation) forbids it. A2 keeps the source structure canonical
and derives display geometry from one published fact (`wrap_width`).

## Scroll model

`bottom_anchored_scroll` replaces its constant `1`-per-line term with
the published per-line segment count
(`Σ_line (wrap_segments(line) + virtual_rows_at(line))`). With
`wrap` off, `wrap_segments(line) == 1` and behaviour is identical to
today. `scrolloff` margins compose unchanged (they already route
through `bottom_anchored_scroll` for the bottom margin; the top margin
stays doc-line, since scrolloff is document context).

## Grammar surface

`j` / `k` stay **logical-line** motions (vim default with `wrap`).
Add `gj` / `gk` as **display-line** motions (next/prev wrap segment),
and wrap-aware `g0` / `g$` (start/end of display row). These are new
`lattice-grammar` motions; logical-line motions are unchanged.
Display-line motions need the published segment geometry to resolve a
target, consumed via the active buffer's matrix.

## Virtual-row interaction

A `VirtualRow` anchors to a `source_line`. Under wrap, `Above`
anchors emit before the source line's **first** segment and `Below`
anchors after its **last** segment — the interleaver keys off
`source_line`, which is stable across segments, so no change to the
`VirtualRowMatrix` anchoring model. `virtual_rows_in_line_range`
(used by the scroll seam) is unaffected: it counts virtual rows in a
*source-line* range, orthogonal to wrap segments, and the scroll seam
adds the two terms.

## Renderers (TUI + GPUI, lockstep)

- The gutter renders once per **source line**: line number / fold
  marker / diagnostic / diff sign on `segment_idx == 0`; continuation
  rows show a continuation marker (`↪`, with a BMP fallback per the
  icon-palette degradation rule) and a blank number column.
- The cursor maps to `(source_line, segment_idx)`; both peers already
  have a doc-row→display-row remap (`doc_to_shaped_row_local` in GPUI,
  the compose walk in TUI) that extends to segments.
- TUI can reuse the popup `wrap_aware_cursor_offset` logic for the
  intra-line cursor column.

## Foldability

Folds operate on source lines and compose above wrapping: a closed
fold hides all segments of its lines. No interaction beyond "fold
filters source lines before segmentation".

## Performance posture

Wrap-on matrix build is benched (`cells_worker` already has bench
infra) to confirm segmentation stays off the hot path; a width-change
debounced rebuild reuses the edit-rebuild path. Render-thread cost
stays O(viewport display rows), unchanged in order from today.
