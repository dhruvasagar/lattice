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

## H.2 — changed-ranges invalidation  🗒

- `lattice-syntax`: after a reparse, compute `old_tree.changed_ranges(&new)`;
  publish affected line ranges on `SyntaxSnapshot` (new field).
- `cells_worker.rs`: on the reparse-completion wake (no edit delta), rebuild
  only rows intersecting the dirty ranges instead of full-rebuilding.
- Tests: local edit → one row rebuilt; unclosed-fence edit → all re-spanned
  rows rebuilt; no whole-file recolour.
- Bench: reparse-completion rebuild cost vs full-rebuild baseline.
- Impact: `lattice-syntax` (snapshot + worker), `cells_worker.rs`.

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
