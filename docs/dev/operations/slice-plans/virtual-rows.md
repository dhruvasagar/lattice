# Virtual rows — slice plan

Sequencing companion to
[`docs/dev/architecture/virtual-rows.md`](../../architecture/virtual-rows.md).
The design fragment is the source of truth for *what* and *why*;
this file owns *when* and *in what order*. Authoritative status
per slice lives in [`../implementation.md`](../implementation.md);
the bullets below summarise carve points.

- ✅ **D.0a** (2026-05-28) — Pure data layer in
  `lattice-cells`. `VirtualRow`, `AnchorPosition`,
  `VirtualRowMatrix`, `VirtualRowVersion`, `ProviderId`,
  `VirtualRowProvider` trait, `DisplayRowEntry`,
  `DisplaySlice`, `DisplaySliceIter`, `CellMatrix::display_
  slice` method. 15 new tests (5 on the matrix module + 10
  on the interleaver) cover sort/lookup, ordering contract,
  fold semantics, past-EOF anchors, scroll/height bounds,
  empty/chunked corners. Bench harness compiles
  (`virtual_row_layout` + `display_slice_iter`); CI gate
  enforcement deferred to first production consumer.
- ✅ **D.0a.1** (2026-05-29) — `virtual_rows_worker` on
  `lattice-host`. Sibling tokio task to `cells_worker`.
  `VirtualRowProviderRegistry` for consumer registration;
  worker awaits `VirtualRowsWake.notified()`, fingerprints
  the input axes (sorted hash of `[(provider_id,
  provider_version)]` + `source_line_count`), and either
  cache-hits or polls `collect()` on every registered
  provider, merges, and publishes `VirtualRowMatrix` via
  `ArcSwap`. Trait extension: added cheap `version()` method
  so the worker can short-circuit on the cache-hit path
  without polling expensive `collect()`s. New `Editor` fields
  `virtual_rows_matrix_cell`, `virtual_rows_wake`,
  `virtual_row_providers` mirror the `cells_*` shape;
  `editor_boot` spawns the worker alongside `cells_worker`;
  `publish_render_state` fires `virtual_rows_wake.notify_one()`
  after the cells wake. Reuses `RenderState.cells.snapshot`
  for the source line count rather than carving a new
  sub-state — the matrix is tied to the active document just
  like the cells matrix. 12 worker-module tests; 419
  `lattice-host` unit tests green overall.
- 🗒 **D.0b** — Scroll-binding pane groups. Independent of
  virtual rows; lands when D.4 (side-by-side diff) needs
  it. See `diff-system.md` §5.2.
