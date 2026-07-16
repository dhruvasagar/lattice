+++
title = "Incremental highlight & viewport-scoped cells"
+++

# Incremental highlight & viewport-scoped cells

Status: design locked 2026-06-04. Slice plan: `docs/dev/operations/slice-plans/archive/incremental-highlight.md`.

## Problem

The cell-builder worker (`lattice-host/src/cells_worker.rs`) bakes resolved
colours into a per-pane `CellMatrix` (whole-doc for small files, chunked for
large). That cache is good — better than Helix/Zed, which re-run the
highlight query per render and don't cache resolved cells. But it is fed by
an **eager whole-file highlight**: every rebuild calls
`SyntaxHandle::highlight_lines(0, line_count)`, which runs the highlights
query — and for markdown the block + `markdown_inline` + fenced **injection
re-parse** — over the entire file, even for a one-character edit. And the
reparse-completion path (the editor actor's `async_landed` wake, slice B.1)
does a **full** matrix rebuild.

Two consequences:

1. **Latency:** per-keystroke highlight is O(file), not O(edit). On large
   files this blows the keystroke→glyph budget (paramount goal #1).
2. **Flicker:** the compose `cells_stale` path renders the whole viewport as
   plain `Span::raw` (no fg, no underline) for the frames between a keystroke
   and the worker republishing. Because the rebuild runs the slow whole-file
   markdown highlight, that window is long enough to see — styled inline
   content (code spans, links) visibly drops to plain then snaps back on
   every keystroke. (User-reported 2026-06-04.)

## How the field scopes highlighting

Weighted per `feedback_editor_design_references` (Helix/Zed = Rust +
tree-sitter substrate; Vim/Emacs = convention). Treated as **data**, not
justification (heuristic #2) — the justification is the latency budget.

- **Helix / Zed (Rust, tree-sitter):** incremental parse of layered trees;
  highlight iterator runs over the **viewport byte range** each render. No
  baked-cell cache — they re-query the small visible range (cheap because the
  tree is cached).
- **Neovim (tree-sitter):** highlights query run **lazily over the redraw
  region**; query cursor scoped to the line range; injected trees walked
  within that range.
- **Emacs `jit-lock`:** *just-in-time* — fontifies only the visible portion
  in chunks, defers off-screen to idle timers; caches fontification until an
  edit invalidates a region.
- **Vim (regex syntax):** per-line state machine with backward `sync`; caches
  per-line state; highlights on redraw.

Universal pattern: **highlight is viewport-scoped / on-demand, never
whole-file-eager.** The tree-sitter trio additionally use
`Tree::changed_ranges(old, new)` to know exactly which bytes changed after a
reparse and re-highlight only dirty regions.

## Design

Keep the baked-cell `CellMatrix` cache. Feed it **range-scoped** and
**changed-ranges-invalidated** highlights, and make the matrix a **viewport
window** on large files. Result: cached cells AND O(edit)/O(viewport)
recompute — the best of both models.

### H.1 — range-scoped rebuild highlight

`ChunkInputs` gains `spans_base: u32`; `build_chunk_rows` indexes
`per_line_spans[line_idx - spans_base]` (relative). Each rebuild path
highlights only the range it actually rebuilds:

- incremental whole-doc → `highlight_lines(edit_lo, affected_hi)`,
- incremental chunked rebuild zone → `highlight_lines(rebuild_lo, rebuild_hi)`,
- full rebuild → `(0, line_count)` until H.3 makes it viewport-scoped.

`highlight_lines(start, end)` already scopes the query cursor to the byte
window and returns a Vec of length `end - start` (relative) — the infra
exists; we were only ever calling it with `(0, count)`.

### H.2 — `changed_ranges`-driven invalidation

The syntax worker computes `old_tree.changed_ranges(&new_tree)` after each
reparse and publishes the affected line ranges on `SyntaxSnapshot`. On the
reparse-completion wake (no edit delta), the cells worker rebuilds **only**
the rows intersecting those ranges instead of full-rebuilding — so a 1-line
edit whose syntax effect is local recolours one row; an unclosed-fence edit
that re-spans the file recolours exactly the affected rows. Kills the
whole-file recolour flip on reparse.

### H.3 — viewport-scoped highlighting

The active matrix covers the **visible range + overscan**, extended
incrementally on scroll, rather than whole-doc / chunked-whole-file. Highlight
+ cell build become O(viewport) regardless of file size. `row_at_source_line`
outside the window returns `None`; compose falls back to a cheap plain-text
row for the (rare, transient) off-window case until the window extends.

## Paramount-goal alignment

- **#1 (latency):** the entire point — per-frame/keystroke work → O(viewport),
  independent of file size at H.3. All highlight work stays on the cells +
  syntax workers, never the UI thread.
- **#2 (extensibility), #3 (vim grammar), #4 (async):** unaffected; the
  syntax/cells workers already run off-thread per goal #4.

## Rejected alternatives

- **Drop the baked-cell cache, re-query per render (Helix/Zed model).**
  Rejected: our cache is an advantage; re-querying per render trades a cheap
  ArcSwap read for a query run every frame. We keep the cache and fix its
  feeder instead (heuristic #1: the cache isn't the problem).
- **Per-line regex/state cache (Vim model).** Rejected: tree-sitter +
  changed-ranges is strictly better on our substrate.

## Tests + benches (four-artefact rule)

Bench coverage is a release gate for this initiative, tracked in
`docs/dev/operations/benchmarks.md` and CI (per the perf-budget CI gate).
Each slice lands its bench with its code:

- H.1: existing incremental parity tests stay green; assert highlight is
  invoked with the narrow range (not whole-file). Bench: per-keystroke
  rebuild on a medium file — must drop vs the whole-file baseline.
- H.2: edit→reparse changes only the local row's spans → only that row
  rebuilt; fence edit → all re-spanned rows rebuilt. Bench: reparse-completion
  rebuild cost vs full-rebuild baseline.
- H.3: **large-file (100k-line) bench** — highlight + rebuild latency must be
  O(viewport), independent of file size; graceful off-window fallback test.
  This is the headline large-file competitiveness number.

Benches live in `crates/lattice-host/benches/cells_worker.rs` (extend the
existing harness) and are recorded in `benchmarks.md` so regressions are
visible across sessions.
