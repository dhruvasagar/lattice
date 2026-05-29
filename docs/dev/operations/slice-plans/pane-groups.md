# Pane groups — slice plan

Sequencing companion to
[`docs/dev/architecture/pane-groups.md`](../../architecture/pane-groups.md).
The design fragment is the source of truth for *what* and *why*;
this file owns *when* and *in what order*. Authoritative status
per slice lives in [`../implementation.md`](../implementation.md).

Pane groups underpin diff side-by-side (D.4), three-pane merge
(D.6), `:set scrollbind` / `:set cursorbind`, `:windo`, and
zen-mode satellites. The primitive lands as the foundation slice
of D.4 (called D.0(b) in the diff slice plan since virtual rows
got D.0(a)).

Each slice ships green-on-merge with the four artefacts
CLAUDE.md mandates: architecture documentation (the design
fragment, updated as needed), benchmark coverage where
load-bearing, test coverage of the new scenarios + failure
modes, graceful error handling.

| Slice       | Title                                | What lands |
|-------------|--------------------------------------|------------|
| **D.4.a** ✅ | `PaneGroup` substrate (2026-05-29) | `PaneGroupId` hoisted into `lattice-core::ui::pane`. New `lattice-host/src/pane_group.rs`: `PaneGroupMember { pane, buffer }`, `RowMapper` trait, `IdentityRowMapper`, `OffsetRowMapper` (smoke-test stub kept in production code), `PaneGroup { id, members, mapper }`. New `Editor::pane_groups: Vec<PaneGroup>` field + `add_pane_group` / `drop_pane_group` / `remove_pane_group_member`. Registration conflict check honours buffer pairing — duplicate **active** members rejected, suspended memberships coexist. `propagate_pane_group_scroll` invoked at `publish_render_state` tail: finds the group whose member matches `(active_pane.id, active_pane.buffer_id)`, walks other members, writes `mapper.map_row(...)` into stashed `PaneState.scroll` *only when* the target pane's current buffer still matches the registered one (mismatch ⇒ skip; closed pane ⇒ skip). Empty groups auto-prune on `remove_pane_group_member`. **9 tests** (4 in `pane_group`, 5 dispatch integration); 475 host + 1461 workspace tests green. |
| **D.4.b** ✅ | `HunkRowMapper` (2026-05-29) | New `lattice-host/src/diff_pane_group.rs`: `RowMapper` impl constructed with `Arc<DiffSession>` + baseline / current `PaneGroup::members` indices. `map_row` dispatches on the (from, to) pair: baseline→current direction, current→baseline direction, or identity fallback for unfamiliar pairs (future three-pane configurations). Per-direction algorithm is a single cumulative-shift walk over the published `HunkIndex`: row before this hunk ⇒ apply accumulated shift; row inside ⇒ proportional (`offset * to_len / from_len`, capped at `to_len - 1`) or collapse to `to.start` when either side is empty (Add / Remove); row past ⇒ accumulate `current_len - baseline_len` and continue. Pure helpers `map_baseline_to_current` / `map_current_to_baseline` exposed for direct unit testing. Defensive: malformed hunks (< 2 ranges) skipped (no panic). **12 tests** (empty index, single Add, single Remove, Change proportional both expand and compress, cumulative shift across multiple Adds, mixed Add/Remove cancels, malformed skipped, unfamiliar pair identity, round-trip through published session). 487 host + 1461 workspace tests green. |
| **D.4.c** ✅ | Filler-row provider (2026-05-29) | New `lattice-host/src/diff_filler.rs`. `Side::{Baseline, Current}` enum + `FillerRowProvider { session, side }` implementing `VirtualRowProvider`. Pure helper `compute_filler_rows(index, side)` walks each hunk: `|baseline_len - current_len|` filler rows on the shorter side; anchor at `range.start` with `Above` when the shorter range is empty (Add ⇒ baseline, Remove ⇒ current), or `range.end - 1` with `Below` for Change with one shorter side. Pure-sync `collect()` — no off-thread refresh task needed (O(hunks)). Provider ids side-distinct (`0xD1FF_0001_*` baseline, `0xD1FF_0002_*` current) so both coexist with inline overlay's `0xD1FF_0000_*`. Conflict hunks skipped (D.6); malformed defensively skipped. **14 tests** (empty, Add baseline-only, Remove current-only, Change-baseline-longer, Change-current-longer, Change-equal, multiple hunks accumulate per side, Conflict skipped, malformed skipped, ids distinct per side + no overlay collision, collect reads published hunks, version changes with session revision, version differs across sides at rev 0). 501 host + 1461 workspace tests green. **Wiring caveat:** worker is single-document today; per-document matrices land in D.4.d alongside the `:diffsplit` wiring so fillers can show on both panes simultaneously. |
| **D.4.d.0** ✅ | Per-document cells-matrix registry surface (2026-05-29) | New `Editor::cells_matrices: Arc<Mutex<HashMap<BufferId, Arc<ArcSwap<CellMatrix>>>>>` field. Boot seeds the active document's entry **sharing Arc identity** with `Editor::cells_matrix_cell` so the existing single-writer hot path (cells_worker → cells_matrix_cell → RenderState.cells.matrix) stays bit-identical. New `Editor::cells_matrix_for(buffer_id)` helper is the single port into the registry: lazy-inserts an empty cell on first ask, idempotent on repeat asks. No worker iteration yet — D.4.d.1 makes the worker rebuild per visible buffer. **2 tests** (active-doc Arc-identity invariant, lazy-insert + idempotent + distinct-per-buffer). 503 host tests green. |
| **D.4.d.1** | Cells worker iterates registry | Switch `cells_worker` from single-cell write (`cells_matrix_cell.store(...)`) to per-visible-buffer iteration over `Editor::cells_matrices`. Each visible pane's buffer gets a rebuild routed through `cells_matrix_for(pane.buffer)`; the active-doc Arc-identity invariant means today's renderer read path keeps landing on the seeded entry. RenderState carries a per-pane matrix snapshot map so renderers paint each pane against the matrix for that pane's buffer. |
| **D.4.d.2** | Same surface for virtual rows | Mirror of D.4.d.0 + D.4.d.1 on `virtual_rows_matrix_cell` — per-document registry + worker iteration. Required so filler providers (D.4.c) attached to baseline + current panes both publish independently. Carved into four sub-slices that mirror the cells side one-for-one: D.4.d.2.0 (per-doc registry surface), D.4.d.2.1.a (`VirtualRowProviderRegistry` keyed per `BufferId`), D.4.d.2.1.b (`PaneCellsInputs.virtual_rows_matrix` field populated at publish via `virtual_rows_matrix_for(buffer_id)` — mirror of D.4.d.1.a), D.4.d.2.1.c (worker iterates `rs.cells.panes`, writes via `pane.virtual_rows_matrix` — mirror of D.4.d.1.b), D.4.d.2.1.d (`VirtualRowsRenderState::pane_matrices` + `matrix_for_pane(pane_id)` — mirror of D.4.d.1.c). See `../implementation.md` for slice-by-slice status. |
| **D.4.d.3** | `:diffthis` / `:diffsplit` / `:diffoff[!]` ex-commands | Wires D.4.a + D.4.b + D.4.c + D.4.d.{0,1,2} into a user-visible flow. **DiffSession is the source of truth for participants** — `:diffoff` reads from the session's `descriptor.watch` list, not from the tab tree. The tab is **not** a grouping unit; the session is. Carved into two sub-slices: **D.4.d.3.a** lands `:diffthis` (state-machine on `Editor::pending_diffthis` — first call stages, second in different pane completes, same pane unstages, v1 errors on a third), the `register_two_pane_diff` helper that builds `DiffSession` against two live buffers via `BufferBaseline` + `BufferCurrentSource` (`watch = [baseline, current]`) plus `PaneGroup` with `HunkRowMapper` plus per-side `FillerRowProvider` registered through the per-`BufferId` provider registry (D.4.d.2.1.a), the `BufferId → primary` indirection inside `DiffSubsystem` (`secondary_index` + `lookup_session_for`) so either side's `:diffoff` finds the same session, and the unified `do_diff_off(force: bool)` that walks `descriptor.watch` to tear down filler providers / pane group / session atomically. v1 semantics: `:diffoff` and `:diffoff!` are operationally identical because removing one side of a two-way diff collapses the whole session; the bang is forward-compat for D.6 three-way merge. **D.4.d.3.b** lands `:diffsplit <file>` — composes vsplit + open + the `register_two_pane_diff` helper for the demo-able end-to-end flow. `:diff` (bare, no args) stays inline-only per the slice plan confirmation. |
| **D.4.e**   | Bench `pane_group_scroll_p99_us`     | New `crates/lattice-host/benches/pane_group.rs` + `[[bench]] pane_group` in Cargo.toml. Workloads: `pane_group_no_group` (baseline — every tick checks `pane_groups`, none exist), `pane_group_identity_propagation` (2-pane identity-mapper group, scroll cost per tick), `hunk_row_map_p99_us` (`HunkRowMapper::map_row` at 100 hunks). CI gate enforces the keystroke budget. |

Slice sequencing:

- **D.4.a is the load-bearing slice.** Lands green
  standalone; everything else depends on it. No diff
  consumer in this slice; tests use stub mappers.
- **D.4.b** depends on the existing `DiffSession` /
  `HunkIndex` (already in tree via D.2). Independent of
  D.4.a — pure crate-level.
- **D.4.c** depends on D.0(a) virtual rows (already
  shipped) + D.2 (`DiffSession`). Independent of D.4.a /
  D.4.b — fillers are observable through the existing
  virtual-rows worker.
- **D.4.d** depends on D.4.a + D.4.b + D.4.c. First slice
  with end-to-end user visibility.
- **D.4.e** depends on D.4.d so a real consumer is in
  place to bench against.
