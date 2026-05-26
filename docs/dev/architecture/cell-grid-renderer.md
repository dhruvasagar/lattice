# Cell-grid renderer architecture

Anchor: [design.md §5.6](design.md) (rendering layered architecture) and paramount goal #1 (sub-frame keystroke→glyph at 120 Hz, ≤ 8 ms).

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
| **S3** | 🔄 in progress | TUI cutover. Sub-sliced as S3.a (substrate modifier bits), S3.b (TUI consumes CellMatrix for document body), S3.c (retire RowPrepaint for code-class buffers). |
| **S3.a** | ✅ done | Cell modifier flag bits. `Cell::flags` gains `BOLD` / `ITALIC` / `UNDERLINE` / `DIM` / `REVERSE` constants (5 bits) plus `is_bold` / `is_italic` / `is_underline` / `is_dim` / `is_reverse` accessors. `cells_worker::resolve_style(theme, style) -> (u32, u16)` replaces `resolve_fg`; the worker now packs `theme.syntax_style(style).modifiers` into the cell's flags via the new `modifiers_to_flags` helper. `ChunkInputs` carries `default_flags` so cells outside any styled span pick up the theme's default modifiers. Inlay-spliced cells carry only `INLAY` — they don't inherit the surrounding syntax style's modifiers. 5 tests cover the bit table, packing helper, keyword=BOLD + comment=ITALIC via Catppuccin defaults, and inlay-cell isolation. |
| **S3.b** | ✅ done | TUI converter `lattice_ui_tui::cells_render`: `cell_row_to_combined_spans` (every cell, post-inlay column space) and `cell_row_to_source_spans` (INLAY-flagged cells skipped, drop-in compatible with the legacy source-byte body-spans shape). Walks cells once, groups consecutive cells with matching `(fg, bg, modifier_bits)` into one `ratatui::text::Span`. `cell_to_style` maps `cell.fg/bg != 0` to `Color::Rgb(...)`, leaves `fg/bg == 0` unset (host `Color::Default` semantics), and packs the five modifier flag bits to ratatui's `Modifier::BOLD/ITALIC/UNDERLINED/DIM/REVERSED`. Truecolor terminals render exactly; 16-color terminals get ratatui's automatic nearest-named-color downsampling (small visual delta vs. the legacy `host→tui` theme adapter path — acceptable for the cell-grid model's truecolor target). 13 tests cover the empty case, single + adjacent-merge + style-break grouping, full modifier-flag table, modifier composition, fg/bg unset semantics, INLAY filter in source-spans, all-INLAY empty path, non-ASCII codepoint round-trip, and a realistic keyword + identifier + paren row. Wiring into `draw_buffer` deferred to S3.c (touches the overlay pipeline; OverlayState design discussion). |
| **S3.c** | 🔄 in progress | Sliced one overlay at a time. S3.c.0 lands the body wiring + dual-path fallback. S3.c.1–4 validate each overlay layer against the cell-derived body. S3.c.final retires the RowPrepaint fallback for code-class buffers. |
| **S3.c.0** | ✅ done | Body wiring foundational slice. New `CellMatrix::row_at_source_line(target) -> Option<&CellRow>` walks chunks by `start_source_line` (sub-µs for a 100K-line/128-chunk-size buffer). `lattice-ui-tui::render::compose_visible_lines_inner`'s non-messages body branch now prefers `crate::cells_render::cell_row_to_source_spans(cell_row)` when the published matrix has a row for the line; falls back to `RowPrepaint.runs` when the matrix lags (boot frames, folded rows, out-of-coverage). The fallback shape is bit-identical to the pre-cutover path so any matrix-lag window doesn't flicker. Workspace 3157 / 3157 (was 3154, +3 substrate lookup tests). Downstream overlays (whitespace, semantic tokens, hlsearch, visual, diagnostics, …) consume the body opaquely and continue to work — validated by the existing 1420+ TUI tests still passing. |
| **S3.c.1** | ✅ done | Whitespace decoration validation against cell-derived bodies. 7 tests in `cells_render::tests` feed cell-derived source spans through `render::apply_whitespace_decoration` and assert glyph substitution lands at the correct source-byte positions: mid-line space → `·`; leading-whitespace → `›`; trailing-whitespace → `▷`; tab → `→`; EOL marker append; all-off noop preserves the body; substitution across a multi-fg span boundary (where two cells with different fg form separate ratatui spans, the classifier still walks byte positions correctly). Confirms `apply_whitespace_decoration` consumes cell-derived spans opaquely. |
| **S3.c.2** | ✅ done | Semantic-tokens overlay validation against cell-derived bodies. `apply_semantic_token_overlay` visibility bumped to `pub(crate)` (matches `apply_whitespace_decoration` precedent). 6 tests in `cells_render::tests` validate the overlay walks cell-derived spans correctly: mid-span partial coverage → split into pre/mid/post; full coverage → fg replaced across the row; out-of-range → no-op; existing BOLD modifier (from syntax style) preserved while overlay adds ITALIC; fg replaced, bg from earlier pass preserved; multi-span body with overlay straddling the fg boundary → each side's overlap region gets the overlay fg. |
| **S3.c.3** | ✅ done | Bg-layer overlays validation against cell-derived bodies. `apply_match_overlay` (visual / hlsearch / current_match / substitute / doc-highlight) and `apply_underline_overlay` (diagnostics) visibility bumped to `pub(crate)`. 8 tests cover the match overlay's REPLACE-style semantics (splits at partial coverage, full row coverage, out-of-range noop, multi-span cross-boundary walk, sequential composition where a later overlay overrides an earlier bg, REPLACE drops cell's BOLD when overlay omits it) and the underline overlay's ADDITIVE semantics (UNDERLINED OR-ed in while existing fg/bg/BOLD all preserved). |
| **S3.c.4** | ⛔ planned | Fold suffix + post-overlay inlay splice. |
| **S3.c.final** | ⛔ planned | Retire the RowPrepaint fallback for code-class buffers once every overlay is validated. RowPrepaint stays for markdown/help/popups (rich rendering). |
| **S4** | ⛔ planned | GPU glyph atlas + `paint_cells` in `lattice-ui-gpui`. Replaces `EditorElement`'s `shape_line` path for code-class buffers. Status line / popup / picker / help stay on the Shaped path. |
| **S5** | ⛔ planned | Criterion bench harness for held-key scroll + Ctrl-D + paste. Tunes `chunk_size`, atlas size/count. Recorded in `benchmarks.md`. Probe stripped. |
| **S6** | ⛔ planned | Cleanup: strip probes, retire any shape_line code on the code path, audit decoration assignment. |

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
