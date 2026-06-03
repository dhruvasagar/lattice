# Slice plan — incremental highlight & viewport-scoped cells

Design: `docs/dev/architecture/incremental-highlight.md`. Goal: highlight +
cell-matrix recompute O(edited/visible lines), never O(file) — large-file
competitiveness vs Helix/Zed/Neovim while keeping our baked-cell cache.

**Bench tracking is a release gate.** Every slice ships its bench in
`crates/lattice-host/benches/cells_worker.rs` and records the number in
`docs/dev/operations/benchmarks.md` so regressions are visible across
sessions (per the CI perf-budget gate).

## H.1 — range-scoped rebuild highlight  🚧

- `ChunkInputs.spans_base: u32`; `build_chunk_rows` indexes
  `per_line_spans[line_idx - spans_base]`.
- Incremental whole-doc highlights `[edit_lo, affected_hi)`; chunked rebuild
  zone highlights `[rebuild_lo, rebuild_hi)`; `build_matrix` (full) stays
  `(0, line_count)` (H.3 scopes it).
- Tests: incremental parity stays green; new test asserts the narrow-range
  highlight call.
- Bench: per-keystroke incremental rebuild on a medium file vs the
  whole-file-highlight baseline.
- Impact: `cells_worker.rs` only.

## H.2 — changed-ranges invalidation

### H.2a — publish changed line ranges  ✅ (commit `4ae04ac`)

- `lattice-syntax`: `reparse_with_cached_tree(from)` computes
  `old.changed_ranges(new)` → `SyntaxSnapshot.changed_lines` (inclusive line
  ranges) + `reparsed_from_version`. Full reparse → `changed_lines = None`.
  Accessors `changed_lines()` / `reparsed_from_version()`.
- Test: `changed_lines_covers_the_edited_line`. No behaviour change (data only).

### H.2b — cells worker consumes it  🗒 (RE-EVALUATED — likely fold into / defer behind H.3)

**Re-evaluation (2026-06-04):** the original motive "kills the whole-file
recolour *flip* on reparse" is **moot** — `markdown_inline_spans_stable_across_unrelated_edit`
proved the highlight is deterministic across reparses (no flip; the
reparse-completion full rebuild produces identical colours). So H.2b's value is
**pure perf** (avoid the whole-file highlight on reparse-completion), which
**overlaps H.3**: once the matrix is viewport-windowed, the full rebuild is
already viewport-bounded and cheap on large files. H.2b's only distinct win is
chunked/large-file reparse cost — exactly H.3's domain.

Decision pending: either (a) skip H.2b, go straight to H.3, and let H.3's
window-refresh consume `changed_lines` (H.2a) to know which visible rows to
re-highlight; or (b) a minimal whole-doc H.2b only if a concrete need appears.
`changed_lines` (H.2a) is published and ready for whichever path.

- Sketch if pursued: on reparse-completion (no edit delta) with
  `existing.version.syntax == snapshot.reparsed_from_version` (coherence gate)
  and `changed_lines = Some`, rebuild the bounding line range of the dirty
  ranges, reuse prior rows elsewhere (H.1 whole-doc row-reuse, net=0); else
  full rebuild. Chunked: reuse non-dirty chunks.

## H.3 — viewport-scoped highlighting  🗒

- Active matrix covers visible + overscan, extended incrementally on scroll.
- `row_at_source_line` outside the window → `None`; compose graceful
  plain-text fallback for the transient off-window case.
- Both renderers: confirm TUI + GPUI handle a windowed matrix (off-window
  rows fall back identically).
- Tests: window covers viewport±overscan; scroll extends; off-window
  fallback.
- Bench: **headline** — synthetic 100k-line file, highlight + rebuild latency
  independent of file size (O(viewport)). Record in `benchmarks.md`.
- Impact: cells inputs (viewport range — already plumbed via
  `viewport_height/width`), `cells_worker.rs` windowing, compose fallback
  (`lattice-ui-tui` + `lattice-ui-gpui`).

## Sequencing

H.1 → H.2 → H.3. Each green + committed + benched before the next. H.1 also
resolves the user-reported markdown inline-content flicker (the whole-file
highlight lengthening the compose `cells_stale` plain-text window).
