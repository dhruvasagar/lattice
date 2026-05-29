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
| **D.4.b**   | `HunkRowMapper`                      | `RowMapper` impl in `lattice-host/src/diff_pane_group.rs`. Constructed with an `Arc<DiffSession>` so it can read the latest `HunkIndex` via `current_hunks()`. Maps row N on the "current" side to a hunk-correspondent row on the "baseline" side (and vice versa): within a hunk → proportional within the hunk's range pair; between hunks → identity-offset adjusted by cumulative side-length deltas of intervening hunks. Pure function of `HunkIndex` + row + (from_idx, to_idx). Exhaustively tested in isolation: empty index ⇒ identity; one Add hunk ⇒ rows past the hunk shifted by hunk size; one Remove ⇒ symmetric; mixed Add/Remove/Change sequence ⇒ cumulative shifts add up; row inside a hunk ⇒ proportional mapping. No `Editor` integration yet — pure crate-level provider. |
| **D.4.c**   | Filler-row provider for hunk alignment | New `VirtualRowProvider` impl in `lattice-host/src/diff_filler.rs` that consumes the active `DiffSession`'s `HunkIndex` and emits filler virtual rows on each side so that hunks line up between the two panes. For each hunk, the shorter side gets filler rows equal to the length delta. Registered per-side when a side-by-side `DiffSession` opens; deregistered on `:diffoff`. The rows are anchored at the hunk-start line and emit blank `Cell`s styled with the deletion-block backdrop. Tests: Add hunk (current longer) ⇒ baseline side gets N filler rows; Remove hunk ⇒ current side gets N filler rows; multiple hunks ⇒ filler rows accumulate independently. |
| **D.4.d**   | `:diffsplit` / `:diffthis` ex-commands | Wires D.4.a + D.4.b + D.4.c into a user-visible flow. New ex-commands: `:diffsplit <file>` opens `<file>` in a vsplit, registers a `DiffSession` against the buffer pair (current + new), constructs a `PaneGroup` with `HunkRowMapper`, registers filler providers on both sides. `:diffthis` is the explicit two-pane setup: marking the current pane stages it for binding; the second `:diffthis` in another pane completes the session. `:diffoff` extended to symmetrically drop the pane group + filler providers when a side-by-side session is dropped. `:diff` (bare, no args) stays inline-only per the slice plan confirmation. End-to-end test: open two files in two panes, `:diffthis` in each, scroll one pane, observe the other follows the hunk mapping; `:diffoff` cleanly drops state on both panes. |
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
