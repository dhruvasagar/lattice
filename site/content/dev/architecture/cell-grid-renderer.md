+++
title = "Cell-grid renderer architecture"
+++

# Cell-grid renderer architecture

Anchor: [design.md §5.6](design) (rendering layered architecture) and paramount goal #1 (sub-frame keystroke→glyph at 120 Hz, within the one-frame ceiling ≤ 8.3 ms).

This document specifies the renderer substrate that replaces the per-frame `WindowTextSystem::shape_line` path for code/terminal/file-tree/synthetic buffers. The Shaped path (markdown previews, help with rich text, future prose mode) is preserved as a sibling and is out of scope for this work.

The design satisfies four constraints simultaneously:

1. Cursor motion and overlay updates paint at memcpy + atlas-lookup speed regardless of buffer size, scroll velocity, or background async work.
2. Edits, syntax recomputes, LSP arrivals, fold changes never block the UI/edit thread. All matrix-affecting work runs on a background tokio worker.
3. TUI and GPU surfaces consume the same substrate. The cell matrix is the contract; only the output target differs.
4. Decoration freshness is bounded to "next paint" for overlay-class state and to "next paint after worker publishes" for cell-baked state. No state is ever rendered before it is consistent with the cursor's published row.

---

## Why the current shape-line path can't meet goal #1

`WindowTextSystem::shape_line` runs HarfBuzz-class shaping (font matching → glyph mapping → kerning → positioning) per call. Held-j probe (commit `734486d`, 2026-05-25) measured:

| Workload | shape_count | shape_us per paint |
|---|---|---|
| Cursor stationary in viewport | 216 (full viewport, body + gutter) | ~500 |
| Cursor scrolling 1 line/event | 216 | 20 000 – 30 000 |
| Ctrl-D scrolling 10 lines/event | 216 | 160 000 – 250 000 |

The per-call cost is real layout work, not redundant calls. A renderer-side content cache (Layer A, retired) reduced call count to 2 per paint but each remaining call cost the same 10 ms, conserving total wall time. The cost is structural to `shape_line`; the only path to ≤ 1 ms paint is to bypass `shape_line` entirely for code-class content.

Cell-grid + glyph atlas is the path. Every high-performance terminal (alacritty, wezterm, kitty) and most GPU editors (VSCode, Sublime via DirectWrite cache, browsers) use the same decomposition. For monospace, single-font, ASCII-heavy source code, full text shaping is wasted work; an atlas + per-cell quad emission hits sub-microsecond per cell.

---

## Architecture

### Three concerns, decomposed cleanly

| Concern | Owner | Update cadence | Cost on hot path |
|---|---|---|---|
| **Cell matrix** | cell-builder worker (tokio) | async, on (text, syntax, inlay, fold, theme) version bump | zero — UI reads via ArcSwap |
| **Overlay state** | host substrate (per-keystroke RenderState publish) | per-event | zero — UI reads via ArcSwap |
| **Painting** | renderer (UI thread) | per frame | slice + atlas lookup + emit quad |

The renderer never builds cells, never shapes lines, never walks ropes. It loads three Arcs, iterates the visible slice, and emits quads.

### Data model

The substrate types live in a new `lattice-cells` crate. Pure data; no I/O, no rendering, no rope dependencies. Both renderer crates and `lattice-host` depend on it.

```rust
/// 16 bytes. Cache-line friendly; ~4 cells per cache line.
#[repr(C)]
pub struct Cell {
    pub codepoint: u32,   // unicode scalar; 0 = blank
    pub fg: u32,          // 0xRRGGBB; theme-resolved
    pub bg: u32,          // 0xRRGGBB or 0 = transparent
    pub flags: u16,       // bit 0: inlay; bit 1: ws marker; rest reserved
    _padding: u16,
}

pub struct CellRow {
    pub cells: Arc<[Cell]>,             // body cells, inlay-spliced
    pub source_line: u32,               // logical line (pre-fold) in the buffer
    pub inlay_offsets: Arc<[(u32, u32)]>, // (orig_byte, char_width) for byte↔col remap
}

pub struct CellChunk {
    pub start_source_line: u32,         // logical line where this chunk starts
    pub rows: Arc<[CellRow]>,           // ordered by source_line; folded rows elided
    pub version: MatrixVersion,         // captured at build time
}

pub struct CellMatrix {
    pub chunks: Arc<[Arc<CellChunk>]>,  // ordered by start_source_line
    pub chunk_size: u32,                // logical lines per chunk; 0 = whole-doc mode
    pub source_line_count: u32,         // total logical lines (pre-fold)
    pub visible_line_count: u32,        // total matrix rows (post-fold)
    pub version: MatrixVersion,         // max version across chunks
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct MatrixVersion {
    pub text: u32,
    pub syntax: u32,
    pub inlay_hints: u32,
    pub folds: u32,
    pub theme: u32,
}
```

Diagnostics, doc-highlights, search-matches, hlsearch, cursor, selection, visual range — none affect cell content. They are overlay state, not in `MatrixVersion`.

### Overlay state

Per `RenderState`, alongside the existing per-subsystem render states:

```rust
pub struct OverlayState {
    pub gutter: Arc<[GutterRow]>,           // per visible row: line_num, fold_marker, severity
    pub cursor: CursorState,                // line, column, shape (block/bar/underline)
    pub selection: Option<RangeOverlay>,    // selection / visual range
    pub current_match: Option<RangeOverlay>,
    pub all_matches: Arc<[RangeOverlay]>,   // hlsearch
    pub doc_highlights: Arc<[RangeOverlay]>,
    pub diagnostics: Arc<[DiagnosticRange]>, // underline ranges
    pub substitute_preview: Arc<[RangeOverlay]>,
}
```

Each field is published via the existing `RenderState` machinery (ArcSwap snapshot). The paint loop loads the whole `OverlayState` once per frame.

### Decoration assignment

| Decoration | Bucket | Reason |
|---|---|---|
| Text codepoint | Cell | content; cursor-invariant |
| Syntax fg | Cell | bound to text; cursor-invariant |
| Semantic-token fg override | Cell | bound to text; cursor-invariant |
| Inlay hint text | Cell | shifts glyph layout — must be in matrix |
| Fold elision | Cell | changes which rows exist |
| Line number / fold marker / severity icon | Overlay | per-row, cheap to format per paint |
| Cursor (block/bar/underline) | Overlay | cursor-coupled |
| Selection / visual range bg | Overlay | cursor-coupled |
| Hlsearch / current match / substitute bg | Overlay | cursor-coupled |
| Doc highlight bg | Overlay | async LSP, no layout change |
| Diagnostic underline | Overlay | async LSP, no layout change |
| Whitespace marker | Overlay | toggle without rebuild |

The principle: any decoration that doesn't change glyph layout is an overlay. Async-arriving decorations that pass that test appear on the next paint with zero matrix work.

### Cell-builder worker

A tokio task in `lattice-host`, sibling to `highlights_worker`. Subscribes to `MatrixVersion` field changes via the existing `RenderState` cascade. On a version bump:

1. Compute the smallest rebuild set: chunks whose covered `start_source_line .. start_source_line + chunk_size` intersects the changed range.
2. For edits, downstream chunks (lines past the edit) have their `start_source_line` shifted by `Δ`; their `rows` are unchanged. No rebuild for those chunks.
3. Build affected chunks: read rope, walk syntax spans, weave inlay hints, apply fold map, emit `CellRow` array.
4. Coalesce: if more version bumps arrive during the build, drop intermediate results and rebuild against the latest state.
5. Atomically swap the published `Arc<CellMatrix>`.

Cost target: a chunk rebuild (≈ 128 lines × 10 µs/row) ≈ 1.3 ms. Worker stays ≤ 10% busy at sustained 30 Hz typing.

### Chunking policy

Self-tuning to viewport:

- **Whole-doc mode** when `source_line_count ≤ 4 × viewport_height`. Single chunk == whole doc. No chunking overhead.
- **Chunked mode** otherwise. `chunk_size = 2 × viewport_height`, rounded to a power of two for cache-friendly layout.
- A typical 70-line viewport produces 128-line chunks. A 100 K-line buffer = ~780 chunks at ~2 KB metadata each = ~1.5 MB chunk-table overhead.
- LRU cap at `8 × viewport_height / chunk_size` chunks resident if memory pressure shows up (deferred; mostly N/A at expected scale).

### Paint loop (UI thread)

```text
1. Load matrix     = rs.cells.matrix.load_full()       // ArcSwap, wait-free
2. Load overlays   = rs.cells.overlays.load_full()     // ArcSwap, wait-free
3. slice = matrix.slice(scroll, viewport_height)       // O(log chunks) binary search
4. for row in slice {
       for cell in row.cells:
           glyph = atlas.lookup_or_raster(cell.codepoint, font_id, size)
           emit_quad(x, y, glyph.uv, cell.fg, cell.bg)
           x += glyph.advance
   }
5. for row in slice { emit gutter quads via atlas }
6. emit cursor quad, selection quads, hlsearch quads, doc_highlight quads, diagnostic underline quads
7. GPU submit
```

No conditional branches on buffer state, no version checks, no shape calls. Every paint is the same shape. Bounded by viewport × max-cells-per-row × constant.

---

## Invalidation, by source

| Source | Triggers | Worker action |
|---|---|---|
| Edit | text version bump + Δ range | rebuild intersecting chunks; shift `start_source_line` on downstream chunks (no rebuild) |
| Syntax recompute | syntax version bump + affected range | rebuild chunks intersecting range |
| Diagnostic update | overlays.diagnostics swap | none — overlay layer only |
| Doc highlight | overlays.doc_highlights swap | none — overlay layer only |
| Inlay hint arrival | inlay_hints version bump + range | rebuild chunks intersecting range (changes layout) |
| Fold change | folds version bump | rebuild chunks intersecting fold range |
| Theme change | theme version bump | rebuild all chunks (rare event) |
| Cursor / selection / visual range | overlays.cursor swap | none — overlay layer only |

The cell-builder worker computes "smallest rebuild set" by intersecting the changed range against chunk ranges. For pure typing (single-line edits), this is one chunk worth of rebuild per keystroke.

---

## Trade-offs

Accepted:

- **One-frame cursor-cell lag after a typed character.** The cursor renders at its new column immediately (overlay); the cell *under* the cursor reflects pre-edit content for at most one frame, then updates. Invisible during normal typing; only observable in constructed stress tests.
- **No `shape_line` for code-class content.** Loses ligatures (most code fonts ship with them off by default; we mirror that), complex-script support in source code (rare; available via Shaped buffer mode), and subpixel positioning (we choose pixel-aligned for monospace).
- **Glyph atlas as renderer-side infrastructure.** Texture upload, eviction, and per-font-size buckets. Manageable scope; alacritty/wezterm have proven the model.
- **Selection / hlsearch / visual range as overlay quads, not cell-bg mutations.** Keeps the cell matrix cursor-invariant. Costs one extra quad emission per cell in the selection range; trivial.

Rejected:

- ~~Lazy decoration rendering (paint cells now, decorations later).~~ Coherence problem: user could act on stale visual state. Replaced by overlay composition, which has the same async benefit without the coherence cost.
- ~~Synchronous chunk rebuild on edit.~~ Would block the edit handler by 1–10 ms per keystroke. Replaced by fully async path; accept the one-frame cell lag.
- ~~Sub-chunk row Arc structure (`Arc<[Arc<CellRow>]>`) for row-granular sync patching.~~ Adds metadata overhead without a load-bearing reason given the async path is fast enough.
- ~~"Render when cursor idle" debouncing.~~ Cursor-coupled overlays must paint every frame anyway; the half-and-half rule would create two render paths.
- ~~Drop GPUI for direct wgpu.~~ Re-implements windowing/input/focus/animations; net cost ≫ net gain.

---

## Promise

Cursor motion in any buffer is bounded by `O(viewport_size)` atlas lookups plus a constant overlay pass. Edit-induced downstream effects (re-syntax of distant lines, re-numbering line 9999) catch up within a few frames asynchronously; they never block input. Decoration updates (LSP highlights, diagnostics, doc highlights, search matches) appear on the next paint at zero matrix cost.

Sub-millisecond paint, regardless of viewport size or scroll velocity. Held-key bursts at any OS-produced input rate. TUI and GUI behave identically from the buffer's perspective.

---

## Slicing

| Slice | Status | What lands |
|---|---|---|
| **S1** | ✅ landed | `lattice-cells` crate: `Cell`, `CellRow`, `CellChunk`, `CellMatrix`, `MatrixVersion`. Pure data, slicing API, 31 unit tests on whole-doc / multi-chunk / fold-elision / out-of-bounds. |
| **S2** | 🔄 in progress | Cell-builder worker in `lattice-host`. Subscribes to version cascade, coalesces, rebuilds chunks, publishes via ArcSwap on `RenderState`. Internally sliced (S2.1–S2.5 below). |
| **S2.1** | ✅ done | Plumbing only. `CellsRenderState` in `render_state.rs` (matrix `Arc<ArcSwap<CellMatrix>>`, wake `Notify`, `MatrixVersion` axes, snapshot/syntax/inlay/folds/theme inputs). `Editor.cells_matrix_cell` + `Editor.cells_wake`. `publish_render_state` populates the cells state. No worker yet — matrix stays empty. Workspace tests green. |
| **S2.2** | ✅ done | Minimal worker. `cells_worker.rs` sibling of `highlights_worker.rs`. Wake → read RS → build whole-doc `CellMatrix` from rope text alone (ASCII codepoints, no syntax fg, no inlays, no folds). Spawned from `editor_boot.rs` with stable-Arc identity over `cells_matrix_cell` + `cells_wake`. `WorkerDecision` covers `Clear` / `CacheHit` / `Recomputed`; published matrix carries the publisher's `MatrixVersion` so subsequent wakes short-circuit via `differs_from`. 5 tests cover clear / cache-hit / recompute / version-bump / empty-text. |
| **S2.3** | ✅ done | Full cell content. Syntax span → `cell.fg`, inlay-hint splicing, fold elision, theme palette wiring. All three sub-slices (S2.3.a / S2.3.b / S2.3.c) landed; matrix is feature-complete content-wise. S3 (TUI cutover) can now start in parallel with S2.4–S2.5 perf work. |
| **S2.3.a** | ✅ done | Theme palette + syntax fg. `Hash` derived on host `Theme` / `Style` / `Color` / `NamedColor` / `Modifiers`. `CellsRenderState` gains a `theme: Theme` field. `dispatch.rs` folds `hash(host_theme)` into `MatrixVersion::theme`. Worker calls `snapshot.highlight_lines(...)` when the syntax snapshot is current, walks per-line spans, resolves `Style → fg` via `theme.syntax_style`. Stale syntax (snapshot text_version < doc text_version) falls back to default fg. 4 tests cover keyword/comment fg, no-handle default-fg, stale-syntax fallback, theme-version-bump rebuild. |
| **S2.3.b** | ✅ done | Inlay-hint splicing. `CellsRenderState.inlay_hints` is now populated from the dispatch (clones the Arc the syntax substate uses — single cache scan, two readers). Worker buckets the payload by line, splices each `(orig_byte, text)` into the row's cell vector during the char walk: each inlay char emits one `Cell::new(codepoint, inlay_fg, 0, flags::INLAY)`; `(orig_byte, char_width)` is recorded on `CellRow::inlay_offsets` so `byte_to_combined_col` returns the post-inlay column. Trailing inlays (orig_byte == line_len) splice at EOL; multiple inlays sort ascending by byte. Inlay fg is the hard-coded `DarkGray` (0x7f7f7f) matching the TUI's choice — dedicated theme slot is follow-up alongside #19. 5 tests cover single mid-line splice + flags + offsets, multi-inlay byte ordering, byte-0 leading splice, trailing-inlay EOL splice, inlay-version-bump rebuild. |
| **S2.3.c** | ✅ done | Fold elision. `CellsRenderState` gains `foldenable: bool`; dispatch folds it into `MatrixVersion::folds` (the cells axis only — syntax substate's hash keeps its list-only shape). Worker builds `FoldIndex::from_folds(&cells.folds, cells.foldenable)` and skips source lines where `line_inside_closed_fold(line) == true` — the fold's `start_line` stays visible per vim semantics; only interior lines `(start_line, end_line]` are elided. `CellMatrix::source_line_count` preserves the pre-fold logical line count; `visible_line_count` reflects post-fold rows. 4 tests cover closed-fold interior elision, open-fold no-op, foldenable-off disables elision, two non-overlapping closed folds. |
| **S2.4** | ✅ done | Chunked mode + incremental rebuild. S2.4.a landed the structural switch; S2.4.b landed the incremental rebuild with prefix-reuse / rebuild zone / suffix-shift partitioning. |
| **S2.4.a** | ✅ done | Chunked-mode structural switch. `pick_chunk_size(viewport_height, line_count)` returns `WholeDoc` when `viewport_height == 0` or `line_count ≤ 4 × viewport_height`, otherwise `Chunked(next_pow2(2 × viewport_height))` (clamped to a 16-line floor). `build_matrix` orchestrates: a `ChunkInputs<'_>` borrow bundle is built once and a small `build_chunk_rows` helper produces each chunk's rows; orchestrator dispatches to `CellMatrix::whole_doc` or `CellMatrix::chunked`. 6 tests. |
| **S2.4.b** | ✅ done | Smallest-rebuild-set + downstream chunk shift. New `lattice_cells::EditDelta` carries `(start_line, lines_removed, lines_added)`; `CellRow::with_source_line` and `CellChunk::shifted_by` reuse the cell `Arc` payload (refcount bump only) when shifting downstream chunks. `Editor.last_edit_for_cells` tracks the per-publish-cycle single-edit delta (set in `publish_document_changed`, cleared on multi-edit / batch / undo / redo / non-single-edit mutation, `take()`n by `build_render_state`). `CellsRenderState.last_edit` carries it to the worker. `try_incremental_build` checks eligibility (single-text-edit step, other axes unchanged, mode + chunk_size compatible, line-count consistency for doc-switch guard) and either: in **whole-doc** mode rebuilds the only chunk; in **chunked** mode partitions published chunks into prefix-reuse (`Arc::clone`), rebuild zone (`build_chunk_rows` over `[rebuild_lo, rebuild_hi)`), and suffix-shift (`shifted_by(net_delta)`) regions. New `WorkerDecision::RecomputedIncremental` variant. 7 tests cover whole-doc-with-edit, chunked-prefix-reuse + suffix-shift with cell-Arc identity sharing, deletion shifts negative, eligibility failures (no last_edit / theme axis / line-count mismatch / mode change). |
| **S2.5** | ✅ done | Coalescing contract + `paint_request` integration verified. tokio `Notify` is permit-style — a burst of `notify_one()` calls during one build collapses to exactly one stored permit, and the next iteration runs against the latest `render_state.load_full()`. Net behaviour: a burst of N publishes during one build produces exactly 2 builds (original + tail catch-up); no queue, no intermediate states. No explicit debounce needed (and adding one would only add latency without reducing useful work). `paint_request` fires on `Recomputed` / `RecomputedIncremental` / `Clear`; `CacheHit` stays silent. Cells + highlights workers write to independent `ArcSwap` cells with no contention. 4 tests cover the decision state machine under interleaved wakes, no-cross-talk between cells and spans cells, full end-to-end through the actual `run` loop in a tokio runtime (publish → wake → matrix updates → `paint_request` fires; redundant wake → CacheHit, no extra paint signal), and burst-coalescing through `Notify` permits. |
| **S3** | ✅ done | TUI cutover complete. S3.a landed substrate modifier bits; S3.b landed the `CellRow → ratatui::Span` converter; S3.c landed the body wiring + per-overlay validation (S3.c.0–4) + RowPrepaint fallback retirement (S3.c.final). Document buffers now render exclusively through the cell-grid path. Markdown / help / messages keep their existing rich-render paths through the highlights worker. |
| **S3.a** | ✅ done | Cell modifier flag bits. `Cell::flags` gains `BOLD` / `ITALIC` / `UNDERLINE` / `DIM` / `REVERSE` constants (5 bits) plus `is_bold` / `is_italic` / `is_underline` / `is_dim` / `is_reverse` accessors. `cells_worker::resolve_style(theme, style) -> (u32, u16)` replaces `resolve_fg`; the worker now packs `theme.syntax_style(style).modifiers` into the cell's flags via the new `modifiers_to_flags` helper. `ChunkInputs` carries `default_flags` so cells outside any styled span pick up the theme's default modifiers. Inlay-spliced cells carry only `INLAY` — they don't inherit the surrounding syntax style's modifiers. 5 tests cover the bit table, packing helper, keyword=BOLD + comment=ITALIC via Catppuccin defaults, and inlay-cell isolation. |
| **S3.b** | ✅ done | TUI converter `lattice_ui_tui::cells_render`: `cell_row_to_combined_spans` (every cell, post-inlay column space) and `cell_row_to_source_spans` (INLAY-flagged cells skipped, drop-in compatible with the legacy source-byte body-spans shape). Walks cells once, groups consecutive cells with matching `(fg, bg, modifier_bits)` into one `ratatui::text::Span`. `cell_to_style` maps `cell.fg/bg != 0` to `Color::Rgb(...)`, leaves `fg/bg == 0` unset (host `Color::Default` semantics), and packs the five modifier flag bits to ratatui's `Modifier::BOLD/ITALIC/UNDERLINED/DIM/REVERSED`. Truecolor terminals render exactly; 16-color terminals get ratatui's automatic nearest-named-color downsampling (small visual delta vs. the legacy `host→tui` theme adapter path — acceptable for the cell-grid model's truecolor target). 13 tests cover the empty case, single + adjacent-merge + style-break grouping, full modifier-flag table, modifier composition, fg/bg unset semantics, INLAY filter in source-spans, all-INLAY empty path, non-ASCII codepoint round-trip, and a realistic keyword + identifier + paren row. Wiring into `draw_buffer` deferred to S3.c (touches the overlay pipeline; OverlayState design discussion). |
| **S3.c** | 🔄 in progress | Sliced one overlay at a time. S3.c.0 lands the body wiring + dual-path fallback. S3.c.1–4 validate each overlay layer against the cell-derived body. S3.c.final retires the RowPrepaint fallback for code-class buffers. |
| **S3.c.0** | ✅ done | Body wiring foundational slice. New `CellMatrix::row_at_source_line(target) -> Option<&CellRow>` walks chunks by `start_source_line` (sub-µs for a 100K-line/128-chunk-size buffer). `lattice-ui-tui::render::compose_visible_lines_inner`'s non-messages body branch now prefers `crate::cells_render::cell_row_to_source_spans(cell_row)` when the published matrix has a row for the line; falls back to `RowPrepaint.runs` when the matrix lags (boot frames, folded rows, out-of-coverage). The fallback shape is bit-identical to the pre-cutover path so any matrix-lag window doesn't flicker. Workspace 3157 / 3157 (was 3154, +3 substrate lookup tests). Downstream overlays (whitespace, semantic tokens, hlsearch, visual, diagnostics, …) consume the body opaquely and continue to work — validated by the existing 1420+ TUI tests still passing. |
| **S3.c.1** | ✅ done | Whitespace decoration validation against cell-derived bodies. 7 tests in `cells_render::tests` feed cell-derived source spans through `render::apply_whitespace_decoration` and assert glyph substitution lands at the correct source-byte positions: mid-line space → `·`; leading-whitespace → `›`; trailing-whitespace → `▷`; tab → `→`; EOL marker append; all-off noop preserves the body; substitution across a multi-fg span boundary (where two cells with different fg form separate ratatui spans, the classifier still walks byte positions correctly). Confirms `apply_whitespace_decoration` consumes cell-derived spans opaquely. |
| **S3.c.2** | ✅ done | Semantic-tokens overlay validation against cell-derived bodies. `apply_semantic_token_overlay` visibility bumped to `pub(crate)` (matches `apply_whitespace_decoration` precedent). 6 tests in `cells_render::tests` validate the overlay walks cell-derived spans correctly: mid-span partial coverage → split into pre/mid/post; full coverage → fg replaced across the row; out-of-range → no-op; existing BOLD modifier (from syntax style) preserved while overlay adds ITALIC; fg replaced, bg from earlier pass preserved; multi-span body with overlay straddling the fg boundary → each side's overlap region gets the overlay fg. |
| **S3.c.3** | ✅ done | Bg-layer overlays validation against cell-derived bodies. `apply_match_overlay` (visual / hlsearch / current_match / substitute / doc-highlight) and `apply_underline_overlay` (diagnostics) visibility bumped to `pub(crate)`. 8 tests cover the match overlay's REPLACE-style semantics (splits at partial coverage, full row coverage, out-of-range noop, multi-span cross-boundary walk, sequential composition where a later overlay overrides an earlier bg, REPLACE drops cell's BOLD when overlay omits it) and the underline overlay's ADDITIVE semantics (UNDERLINED OR-ed in while existing fg/bg/BOLD all preserved). |
| **S3.c.4** | ✅ done | Fold suffix + post-overlay inlay splice validation. `splice_virtual_text_into_spans` bumped to `pub(crate)`. 7 tests cover the inlay splice walking cell-derived bodies: byte-0 prepend, mid-span split, span-boundary clean insert (no split), past-end append, multi-inlay reverse-byte-order sequencing, fold suffix tail append, and fold-suffix-after-inlay-splice ordering. Confirms the splice function and the fold suffix push both compose with cell-derived bodies just as they did with RowPrepaint bodies. |
| **S3.c.final** | ✅ done | RowPrepaint fallback retired in `compose_visible_lines_inner`'s document-body branch. The cell-derived path is now the sole source for document bodies; empty-matrix windows (boot frame before the first cell-builder publish, or the brief buffer-switch gap) emit plain-text `Span::raw(line_text)` — semantically equivalent to the legacy fallback's degraded state. The highlights worker still runs and `view.visible_rows` stays populated for markdown / help / messages bodies (which live in other render functions). The 1420+ existing TUI tests + 41 cells_render tests all pass — no visual regression. S3 closes. |
| **S4** | 🔄 in progress | GPUI cutover. Sliced like S3.c: S4.0 introduces the cell→TextRun converter; S4.1 wires it into `EditorElement::prepaint` body branch with a fallback to the existing `shape_row_from_prepaint` / `build_line_with_inlays` path during validation; later sub-slices retire that fallback and replace `shape_line` with a glyph-atlas `paint_cells`. Status line / popup / picker / help stay on the Shaped path throughout. |
| **S4.0** | ✅ done | `lattice-ui-gpui::cells_paint` module. `cell_row_to_text_runs(row, font) -> (combined_text, Vec<TextRun>, inlay_offsets)` produces the exact shape `EditorElement::prepaint` feeds into `WindowTextSystem::shape_line`. Walks `row.cells` once, groups consecutive same-fg cells into one `TextRun` (mirrors `build_line_with_inlays`'s collapse), passes `row.inlay_offsets` through verbatim. `make_run_with_color` visibility bumped to `pub(crate)` for reuse. Modifier coverage is fg-only at this slice (matching legacy parity); BOLD / ITALIC / UNDERLINE / DIM / REVERSE rendering deferred to later sub-slices alongside the atlas. 8 tests cover empty / single / adjacent-merge / fg-break / inlay-offsets passthrough / inlay-cells-separate-run / realistic keyword+ident+paren row / non-ASCII utf-8 byte length. |
| **S4.1** | ✅ done | Body wiring foundational slice. `EditorElement` gained `cell_matrix: Option<Arc<CellMatrix>>` populated from `render_state.cells.matrix.load_full()` at the `window.rs` construction site (active pane only — inactive panes pass `None`, mirroring the `visible_rows` split). `prepaint`'s body added `shape_row_from_cells(row, window)` closure that runs `cell_row_to_text_runs` → `shape_line`. Both dispatch sites (no-gutter fallback walk + gutter-driven walk) now use a 3-way `cells → prepaint → legacy` if-let chain: cells looked up by absolute source line via `CellMatrix::row_at_source_line`, falling through to `shape_row_from_prepaint` and finally `shape_row` on `None` (folded rows / boot frames / buffer-switch gap). 8 cells_paint tests + 363 host + 1461 cells+TUI all green. Mirrors S3.c.0. |
| **S4.2** | ✅ done | Modifier propagation. `cell_row_to_text_runs` regrouping key changed from `fg` only to `(fg, bg, style_bits)` where `style_bits = flags & (BOLD\|ITALIC\|UNDERLINE\|DIM\|REVERSE)`. Per-cell `TextRun` construction now reads modifier bits: BOLD → `font.weight = FontWeight::BOLD`; ITALIC → `font.style = FontStyle::Italic`; UNDERLINE → `underline = Some(UnderlineStyle { thickness: 1px, color: None, wavy: false })`; DIM → fg + bg RGB channels multiplied by `0.6`; REVERSE → fg ↔ bg swap (with documented bg=0 fallback). `cell.bg != 0` also passes through as `TextRun.background_color`. `INLAY` and `WS_MARKER` deliberately excluded from `STYLE_FLAGS_MASK` — they're provenance flags, not visual style, so they don't break runs. 18 tests (10 grouping + 8 modifier) cover the table and the composition cases. |
| **S4.3** | ✅ done | `shape_row_from_prepaint` closure + its preceding plumbing retired in `EditorElement::prepaint`. Both dispatch sites collapse from a 3-way `cells → prepaint → legacy` chain to a 2-way `cells → legacy`. `EditorElement.visible_rows` field, the `RowRun` import, the `active_rows_guard` load in `window.rs`, and the `visible_rows:` initializer all removed. `make_run_with_color` stays `pub(crate)` because `build_line_with_inlays`'s legacy `LineRunBuilder` is the remaining consumer (retires in S4.final). Highlights worker continues to publish `rs.syntax.visible_rows` — TUI's markdown / help / messages bodies still consume it. 1461 cells+TUI + 363 host + 41 cells + 18 cells_paint tests all green. |
| **S4.final** | 🔄 in progress | Per-cell `paint_glyph` path. Replaces `shape_line` on the active-pane document body. GPUI 0.2.2 already exposes `Window::paint_glyph(origin, font_id, glyph_id, size, color)` with its own internal sprite atlas — we don't reimplement the atlas, only the per-codepoint `GlyphId` resolution + the per-cell paint loop. Sliced like S2/S3: a–f below. Status line / popup / picker / help / gutter / synthetic buffers stay on the Shaped path throughout. |
| **S4.final.a** | ✅ done | `GlyphResolver` infrastructure. `crates/lattice-ui-gpui/src/glyph_resolver.rs` defines `GlyphKey { font_id, ch }`, `ResolvedGlyph { font_id, glyph_id, is_emoji }`, and `GlyphResolver` (a `HashMap<GlyphKey, Option<ResolvedGlyph>>` wrapper). Key uses a resolved `FontId` (which already encodes family + weight + style + features + fallbacks via `TextSystem::resolve_font`) — so two cells with different BOLD/ITALIC bits hash to different keys naturally. Cache stores `Option<ResolvedGlyph>`: `Some(Some(_))` = resolved, `Some(None)` = sticky-unresolvable, `None` = never queried. `ResolvedGlyph.font_id` may differ from `key.font_id` for fallback-resolved glyphs (S4.final.e). 9 cache-only unit tests cover empty / insert-then-get / sticky-None / distinct-char-keys / distinct-FontId-keys / re-insert-overwrites / emoji-flag-round-trip / fallback-font-id-recorded / clear. Wiring into `GpuiApp` + the resolve path (which calls `WindowTextSystem::layout_line`) lands in S4.final.b. |
| **S4.final.b** | ✅ done | Per-cell `paint_glyph` body path wired in behind a runtime toggle. `GlyphResolver::resolve(ch, font, font_size, window)` added (cache hit → return; miss → `layout_line` → extract glyph_id from `LineLayout.runs[0].glyphs[0]` → cache result; sticky `None` for unresolvable codepoints). `EditorView` now owns `glyph_resolver: Arc<Mutex<GlyphResolver>>`, shared across panes. New `crate::paint_cells` module exports `paint_cells_row(row, line_origin, advance, line_height, ascent, font, font_size, default_fg, resolver, window) -> usize` (returns glyph count) and `paint_cells_enabled()` (reads `LATTICE_PAINT_CELLS=1`/`true` via `OnceLock`). `EditorElement` gains `glyph_resolver` field + prepaint state gains `font`, `font_size`, `text_ascent` (from `text_system.ascent(font_id, font_size)`). The body paint loop in `EditorElement::paint` branches: when toggle ON + active pane + `CellMatrix::row_at_source_line` hits, `paint_cells_row` runs and `ShapedLine::paint` is skipped for that row; otherwise the legacy path runs unchanged. Cursor + overlays still use prepaint ShapedLine metrics (`glyph_advance` matches `paint_cells` advance — monospace single-font cells line up). 71 lattice-ui-gpui tests + 41 cells + 363 host + 1461 cells+TUI all green. |
| **S4.final.c** | ✅ done | Hit-testing primitives. Survey found that `ShapedLine::closest_index_for_x` was never called in this codebase (cursor positioning already uses monospace `text_origin_x + glyph_advance * char_col` math, and the editor body has no `on_mouse_down` handler yet), so the slice landed the *infrastructure* the future mouse-select handler will consume rather than a ShapedLine retirement. New `crate::hit_test` module: `x_to_combined_col(advance, x) -> u32` (mouse x → column; floor, clamps negatives), `col_to_x(advance, col) -> Pixels` (inverse), `combined_col_to_byte(line, col, inlay_offsets) -> usize` (column → source byte, walks chars + handles inlay anchors with cursor-before-byte semantics: clicks inside an inlay snap to the inlay's source-byte anchor). 13 tests cover x↔col round-trips, defensive clamps, multi-byte chars, single-inlay snap, multi-inlay composition, EOL inlay clamp. No mouse handler wired in — that's a UX slice. |
| **S4.final.d** | ✅ done | Modifier paint on the cells path. Three new helpers in `paint_cells`: `dim_channel(packed) -> u32` (×0.6, mirrors `cells_paint::dim_channel`), `apply_color_modifiers(cell) -> (fg, bg)` (REVERSE swap, then DIM attenuation), `cell_font_variant(base, cell) -> Font` (BOLD → `weight = FontWeight::BOLD`, ITALIC → `style = FontStyle::Italic`). `paint_cells_row` now: (1) applies `apply_color_modifiers` first so bg quad + glyph + underline all see final colours; (2) uses `cell_font_variant` per cell so BOLD/ITALIC cells resolve different `(font_id, glyph_id)` pairs via the existing resolver cache; (3) emits a 1-px underline quad at `baseline + 2px` for `cell.is_underline()` cells (GPUI 0.2.2's `TextSystem` doesn't expose `underline_position` publicly, so paint_cells uses a constant offset — matches `cells_paint`'s `UnderlineStyle { thickness: 1px, wavy: false }`). 12 tests cover the helpers (dim table, REVERSE+DIM composition with bg=0 edge case, BOLD/ITALIC font weight+style, BOLD+ITALIC composition, non-font modifier bits don't change font). The `EditorElement::paint` body branch is unchanged — `paint_cells_row`'s richer per-cell handling is internal to the function. |
| **S4.final.e** | ✅ done | Emoji + font fallback. Survey of `WindowTextSystem::layout_line` confirmed it already performs platform-level fallback font matching — the returned `LineLayout.runs[0].font_id` carries whichever font in the chain actually rendered the codepoint, and `glyphs[0].is_emoji` flags colour glyphs. The `GlyphResolver` already cached the resolved `(font_id, glyph_id, is_emoji)` triple correctly, and `paint_cells_row` already dispatched between `paint_glyph` (monochrome) and `paint_emoji` (colour) on `is_emoji`. What S4.final.e added: (1) **`is_notdef(glyph_id)` helper** — detects OpenType's `.notdef` (`GlyphId(0)`); requires a one-`unsafe` transmute because `GlyphId`'s u32 field is `pub(crate)` in gpui 0.2.2 (`#[allow(unsafe_code)]` scoped to that helper). (2) **Sticky-`None` for `.notdef`** — `resolve`'s three failure modes (empty runs / empty glyphs / notdef glyph) now uniformly cache as `None`, so the paint loop never wastes a `paint_glyph` call on a tofu and the cache reflects a clean "no glyph anywhere" state. (3) **Observability** — three `tracing::debug!` lines (target `lattice_gpui::glyph_resolver`) fire on fallback selection, notdef, and empty layout, so `RUST_LOG=lattice_gpui::glyph_resolver=debug` shows which codepoints triggered fallback during validation. 1 new test (`is_notdef_detects_zero_glyph_id`) pins the helper's contract; 10 total glyph_resolver tests. Tofu placeholder rendering (a box outline for sticky-`None` cells) is deferred — the bg quad + underline still emit, so the cell visually exists; future UX slice can add the box. |
| **S4.final.f** | ✅ done (scoped) | Retired the `LATTICE_PAINT_CELLS` env-var toggle. `EditorElement::paint`'s active-pane body now uses `paint_cells_row` unconditionally when `cell_matrix.is_some()` and the row's source line is covered. The legacy `ShapedLine::paint` body path remains for: (a) inactive panes (`cell_matrix == None`; the cells worker only publishes for the active document), and (b) active-pane transient fallback (boot frame before first cells publish, buffer-switch gap, folded rows that the gutter walk didn't pre-filter). Consequently `shape_row` / `build_line_with_inlays` / `LineRunBuilder` / `make_run_with_color` / `cells_paint::cell_row_to_text_runs` all **stay** for those callers — the spec's "delete legacy" goal is deferred until a follow-up that migrates inactive panes to cells (which needs the cells worker to publish for non-active buffers). 97 lattice-ui-gpui tests still green; the env-var-toggle test from S4.final.b retires alongside the function. |
| **S5** | ✅ done (cells_worker side) | Criterion bench `crates/lattice-host/benches/cells_worker.rs` times `recompute` across three workloads (`full_build` / `incremental_build` / `cache_hit`) at three line counts (`100` / `1_000` / `5_000`) — exercising both whole-doc and chunked modes around the `4 × viewport_height = 240` threshold. First-run numbers in `../operations/benchmarks.md`: cache_hit ~33 ns (flat), incremental_build ~103 µs at 5k lines, full_build ~1.9 ms at 5k lines. **Paint-side benches (`paint_cells_row`, `GlyphResolver::resolve` miss path) defer** — both need a live GPUI window so they're outside the Criterion surface. Held-key scroll / Ctrl-D / paste continue to be measured via the `[held-j-*]` probes until S6 strips them. |
| **S6** | ✅ done | Probes stripped. `[held-j-shape]` (prepaint shape-cost log + `shape_count` / `shape_us` / `_shape_t` scaffolding inside `EditorElement::prepaint`), `[held-j-render]` (render-entry + post-ensure logs in `window.rs::Render::render`), `[held-j-timing]` (per-stage `Instant::now()` timing in `on_key_down` + `dispatch_action`), and the short-lived `[held-j-paint]` from the S6-investigation aside — all removed. Held-j scrolling on macOS is smooth; the WSLg-specific stall the user observed was confirmed external (compositor backpressure invisible to in-process probes). The Criterion bench from S5 + the perf benches in `lattice-ui-gpui/benches/editor_element_frame.rs` are the going-forward measurement vehicles. Goal-#1 measurements continue against `benchmarks.md`. |

Estimated calendar: 6–9 weeks across all slices.

**S2.3 is the natural sync point**: after it lands, the matrix is feature-complete content-wise and S3 (TUI cutover) can proceed in parallel with S2.4–S2.5 (perf optimisations).

---

## Open questions

1. **`chunk_size` exact value.** `2 × viewport_height` is the design; bench (S5) picks the rounded power-of-two anchor.
2. **Whole-doc-mode threshold.** `4 × viewport_height` is a guess; bench validates.
3. **Atlas page size and count.** Probably 2048 × 2048 pages × 4–8 resident, but depends on per-platform texture limits. S4 work.
4. **Atlas eviction policy.** LRU per-glyph or per-(font, size) bucket. S4 work.
5. **Selection bg as one-quad-per-selected-cell vs one-quad-per-contiguous-run.** Run optimisation deferred until profile shows it matters.
6. **Word wrap interaction.** Each logical line could produce multiple matrix rows; the `source_line` field generalises to `(source_line, wrap_segment)`. Add when word wrap lands in the editor, not before.
7. **Inlay hint rebuild granularity.** Today: any inlay change rebuilds the affected chunk. Could be tightened to per-row if profiling shows the chunk rebuild is the bottleneck on heavy LSP workloads.

---

## Conventions for updating this doc

- Update the slice table as each lands; move completed slices' "What lands" detail into prose.
- When the bench (S5) lands, fold numbers from `benchmarks.md` into the "Promise" section.
- Append rejected alternatives to "Trade-offs" as they come up; future readers benefit from the audit trail.
- Cross-reference paramount goals explicitly when motivating changes.
