# Pane groups

Authoritative design for Lattice's pane-group substrate: the
primitive that makes side-by-side diff, three-pane merge,
`:set scrollbind` / `:set cursorbind`, `:windo`, and zen-mode
satellites all hang off one shape.

This document is a *companion* to `design.md` (§5.9 buffer
model), to `diff-system.md` (§5.2 + D.4 — first non-trivial
consumer), and to `virtual-rows.md` (orthogonal primitive,
composes with overlay rows but doesn't depend on this).

## 1. The design goal

A **pane group** is a set of `(pane, buffer)` pairs that
scroll together under a pluggable row-mapping function.
Membership is keyed on the pair, not just the pane,
because **a pane's buffer is mutable** — `:bn` / `:bp` /
`:b N` / picker accept can all swap the buffer inside a
pane. The grammar surface (`<C-y>` / `<C-e>` / `C-f` /
`C-b` / `j` / `k` / `H` / `L` / `zz` and the rest) stays
unchanged — it operates on the active pane's `scroll`
field. The substrate observes the change and propagates
it to bound panes whose currently-displayed buffer still
matches their registered pairing.

The buffer-pair keying matters because:

- **Diff side-by-side** (D.4) binds two specific buffers:
  the current document and the baseline document. If the
  user `:bn`s away from the baseline buffer in the right
  pane, the binding *must* suspend — propagating scroll to
  a now-unrelated buffer would scroll the user to the
  wrong place.
- **Resuming** when the user switches back is free — the
  pairing record still exists, propagation just resumes
  on the next tick.
- **Subsystem lifecycle** can decide whether a buffer
  switch *drops* the membership entry (diff's hard
  contract: switching away ends the session for that pane)
  or just *suspends* it (vim's `:set scrollbind` semantics
  would keep the pane in the group across switches).

Five consumers fall out for free:

1. **Side-by-side two-way diff** (D.4). Two panes, two
   members of one group, mapper consults `HunkIndex` for
   hunk-correspondent row mapping.
2. **Three-pane merge** (D.6). Three members, same shape.
3. **`:set scrollbind` / `:set cursorbind`** (post-1.0 vim
   parity). Members = the panes that have the option set,
   mapper = identity.
4. **`:windo`** (vim parity). Members = every pane, mapper
   = identity for scroll, the command-execution itself runs
   per-member.
5. **Zen-mode satellites** (post-1.0). Members = the focus
   pane + each satellite, mapper = identity (or a custom
   "follow the focus" rule).

Two saved invariants frame the choices:

> Buffers must not have kind-specific logic — never branch
> render / motion / scroll on `BufferKind`. -- saved
> feedback `feedback_buffers_no_special_case.md`

> Best long-term fit beats easy implementation. -- CLAUDE.md
> decision heuristic #1

The first rules out a diff-specific scroll-binding shortcut.
The second rules out the rejected alternative of folding
`PaneGroup` into the `PaneNode` tree (sketched in §8).

## 2. The shape

```
        ┌────────────────────────────────────────────┐
        │   Editor                                   │
        │     pane_groups: Vec<PaneGroup>            │
        │     pane_tree:   PaneTree                  │
        │     buffers:     BufferRegistry            │
        └─────────────┬──────────────────────────────┘
                      │
                      ▼
        ┌────────────────────────────────────────────┐
        │   PaneGroup                                │
        │     members: Vec<PaneGroupMember>          │
        │       └─ each is (PaneId, BufferId)        │
        │     mapper:  Arc<dyn RowMapper>            │
        │     id:      PaneGroupId                   │
        └─────────────┬──────────────────────────────┘
                      │
        ┌─────────────┴────────────┐
        │                          │
        ▼                          ▼
   ┌──────────────────┐       ┌──────────────────┐
   │ IdentityRowMapper│       │  HunkRowMapper   │
   │   row -> row     │       │  (D.4.b)         │
   └──────────────────┘       └──────────────────┘
                                      │
                                      ▼
                              consults active
                              `HunkIndex` for
                              hunk-correspondent
                              row alignment
```

Pane groups live on `Editor`. They are *not* a property of
`PaneState` or a `PaneNode` variant — those represent
geometry; groups represent scroll-binding policy. The two
axes are orthogonal: a diff group can bind two panes that
sit on opposite sides of a horizontal split, and `:windo`
binds every pane regardless of tree shape.

Membership is keyed on `(PaneId, BufferId)` — the pair the
group was *registered with*. A pane's currently-displayed
buffer is *observed* at propagation time; when it differs
from the registered buffer, that member is treated as
suspended (skipped) for the tick. The owning subsystem
decides whether the divergence is permanent (drop the
membership / drop the whole group) or transient (leave it
in place so a re-`:b`-back resumes binding).

## 3. The data model

### 3.1 `PaneGroupId`

```rust
pub struct PaneGroupId(pub u32);
```

Stable identifier minted on group creation. Used by
subsystems that own the group (diff for D.4 sessions, zen
for satellites) to drop their group on teardown.

### 3.2 `PaneGroupMember`

```rust
pub struct PaneGroupMember {
    pub pane: PaneId,
    pub buffer: BufferId,
}
```

The membership pair: the pane the user is viewing through
and the buffer that pane was registered with. Propagation
compares this against the pane's *current* buffer at
dispatch tail; mismatch ⇒ skip.

### 3.3 `RowMapper` trait

Lives in `lattice-host` (not `lattice-core`) because
non-identity mappers typically need host-side state
(`HunkIndex` from `DiffSession`, future LSP-driven row
mapping, etc.). `lattice-core` stays free of host
dependencies; the trait sits a layer up.

```rust
pub trait RowMapper: Send + Sync {
    fn map_row(
        &self,
        from_member_idx: usize,
        to_member_idx: usize,
        row: u32,
    ) -> u32;
}
```

Indices into `PaneGroup::members` rather than `PaneId`
keep the API stable across pane re-ordering and let mappers
build position-aware lookups (the hunk mapper at D.4.b reads
the `HunkIndex` via the member's role — "current" vs
"baseline" — encoded at registration time).

Identity mapper is one line — the default for
`:set scrollbind` parity:

```rust
pub struct IdentityRowMapper;
impl RowMapper for IdentityRowMapper {
    fn map_row(&self, _: usize, _: usize, row: u32) -> u32 { row }
}
```

### 3.4 `PaneGroup`

```rust
pub struct PaneGroup {
    pub id: PaneGroupId,
    pub members: Vec<PaneGroupMember>,
    pub mapper: Arc<dyn RowMapper>,
}
```

Owned by `Editor::pane_groups: Vec<PaneGroup>`. Subsystems
add and remove groups through `Editor::add_pane_group` /
`Editor::drop_pane_group`. Group membership is a small
`Vec` (typically 2 for diff, 3 for merge, possibly more for
`:windo`); a `HashMap` would be heavier than the linear
scan.

A `(pane, buffer)` pair belongs to at most one group at a
time. A single pane may appear in multiple groups *only*
when each membership is tagged with a distinct buffer —
e.g. a user opens a diff, then `:bn`s and opens another
diff in the same pane; the first group's membership goes
dormant (buffer mismatch suspends propagation) while the
second group's membership is live. This isn't true
multi-binding in the runaway sense rejected below; it's the
natural consequence of buffer-keyed membership and
preserves the "active pane drives" invariant.

Multi-*active* group membership (one pane bound to two
groups whose buffers both equal the pane's current buffer)
is rejected: propagation semantics become ambiguous when
two mappers disagree on where to scroll. If a future
consumer genuinely needs multi-binding, it composes mappers
(`ChainedRowMapper`) rather than registering twice.

## 4. The propagation algorithm

Scroll changes propagate at the **dispatch tail** via
`Editor::propagate_pane_group_scroll`, invoked from
`publish_render_state` before the worker wakes fire.

Steps:

1. Identify the active pane `A`, its currently-displayed
   buffer `bA`, and its current `self.scroll`.
2. Find the group `G` (if any) where some member
   `m_A = (A, bA)` matches — pane *and* buffer both. If
   no group matches the pair, return.
3. For each other member `m_B = (B, bB)` in `G`:
   - Resolve B's currently-displayed buffer
     (`pane_tree.leaves[idx_of(B)].buffer_id`).
   - **Buffer check:** if B's current buffer ≠ `bB`, skip
     this member — the user has switched buffers in B, the
     binding is suspended until they switch back.
   - Otherwise resolve `from_idx` = index of `m_A` in
     `members`, `to_idx` = index of `m_B`.
   - `mapped = mapper.map_row(from_idx, to_idx, self.scroll)`.
   - Write `mapped` into `B`'s `PaneState.scroll` (stashed —
     `B` is not the active pane).
4. The next `:wincmd w` to `B` swaps `PaneState.scroll`
   into `self.scroll` as today.

This means the *whole world* of existing scroll sites
(`do_scroll_line`, `do_scroll_cursor_to`, the `H/M/L`
clamps, the half-page jumps, `gg` / `G`, etc.) stays
unchanged — they all write `self.scroll`, and propagation
falls out of the tail.

The buffer check is also what makes membership keying on
`(pane, buffer)` sound: a pane that's no longer showing
its registered buffer is invisibly inert in the group, no
explicit "is this membership still live?" flag needed.

**Idempotency** matters because the tail runs every tick.
The mapper output is deterministic for a given input, and
overwriting `PaneState.scroll` with the same value is a
no-op. Optionally cache `last_propagated_scroll` per group
to skip when unchanged; benchmark first, optimise second.

**Cursor binding** (`:set cursorbind` parity) follows the
same shape — separate `propagate_pane_group_cursor` with
its own mapper trait, deferred to a follow-on slice (not
load-bearing for D.4). The two propagations don't have to
share mapper trait identity; cursor mapping is column-aware
where scroll mapping is row-only.

## 5. Composition rules

- **At most one *active* membership per pane.**
  `add_pane_group` rejects a new membership whose
  `(pane, buffer)` matches an existing active member in
  some other group. Inactive memberships (where the pane
  isn't currently showing the registered buffer) are
  ignored by the check — they can coexist freely.
- **Active pane drives.** Only the active pane writes
  `self.scroll`; others have stashed `PaneState.scroll`.
  Propagation is unidirectional per tick — when the user
  swaps active panes, the new active pane's stashed scroll
  becomes authoritative and propagates back.
- **Empty groups are pruned.** Closing the last member of a
  group drops the group on the next tick. The owning
  subsystem (diff, zen, ...) should drop the group
  explicitly when its session ends, but the prune is a
  safety net.
- **Buffer changes don't auto-drop memberships.** A
  `:bn` in a bound pane suspends that member's
  participation (buffer check fails); the membership
  record stays in the group, ready to resume if the user
  switches back. **Subsystems that want hard-drop
  semantics** (diff: leaving the baseline buffer ends the
  session) subscribe to buffer-change events and call
  `Editor::drop_pane_group` (or `remove_member`) from
  their handler. The substrate doesn't impose the policy;
  it lets the policy hold.
- **Tree edits stay independent.** `<C-w>s` / `<C-w>v` /
  `:close` don't touch `pane_groups`. The group survives a
  resize, a swap, or a rotation; a `:close` that removes
  the last member triggers the prune.

## 6. Grammar surface impact

**None.** This is the whole point. Every `z*` / `H` / `M` /
`L` / `gg` / `G` / `C-d` / `C-u` / `C-f` / `C-b` / etc.
arm keeps writing `self.scroll`; the propagation runs at
the tail. `:set scrollbind` (when it lands) parses to a
boolean per pane; the option-change handler walks the
panes and (re)constructs a single identity-mapper group
containing every pane with `scrollbind=on`.

## 7. Plugin path (forward-looking)

A WIT-registered `RowMapper` falls naturally out of this
shape. The plugin's mapper impl is a thin shim that calls
into wasmtime; the propagation path treats it the same as
a built-in. Fuel limits and crash isolation apply
uniformly because `map_row()` is a single typed call.
Plugin-owned groups register/deregister around their
lifecycle events the same way diff sessions do today.

## 8. Rejected alternatives

- **PaneGroup as a `PaneNode` variant.** Folds the
  scroll-bind into the geometric tree. Couples group
  membership to adjacency, which breaks `:windo` (binds
  every pane regardless of tree shape) and zen-mode
  satellites (binds non-adjacent panes). Rejected:
  paramount goal #1 alignment with vim grammar requires
  the two axes orthogonal.
- **Per-pane group_id field on `PaneState`.** O(1)
  membership lookup but pays a `Option<PaneGroupId>` on
  every pane forever, and — more importantly — breaks the
  buffer-pair keying. A `group_id` field on `PaneState`
  would need a parallel buffer match check anyway, so the
  apparent O(1) win evaporates. At expected scales (1-3
  groups, ≤ 8 panes), the linear scan over `pane_groups`
  is cheaper than the lifetime cost.
- **One global mapper switched by mode.** Tempting for
  diff-only use but blocks composition. Multiple
  concurrent groups need independent mappers.
- **Diff subsystem owns its secondary cell / virtual-row
  / highlights pipeline (call this *D1*; rejected
  2026-05-29).** Considered when carving D.4.d. Idea:
  `DiffSession` holds per-side `Arc<ArcSwap<CellMatrix>>`
  cells, spawns its own builder task, exposes matrices
  through the existing diff-subsystem snapshot in
  `RenderState`. Pro: bounds the side-by-side pipeline
  to diff crates; `CellsRenderState` keeps its single-
  document shape. Con: forces the cell builder to be
  callable from two places (`cells_worker` + diff
  subsystem builder), duplicating the wake / cache-hit
  / chunking discipline that already lives inside
  `cells_worker`. Three-pane merge (D.6) would
  re-duplicate the same plumbing again. Future
  consumers (LSP preview pane, picker live-preview)
  would each grow their own builder, fragmenting the
  matrix-build path.

  **Chosen: D2 — workers iterate the per-document
  registry.** `Editor::cells_matrices` (and the parallel
  registry for virtual rows / highlights, landing later)
  is the storage primitive. The existing workers iterate
  over visible buffers and rebuild per buffer.
  `RenderState` grows a per-pane matrix-snapshot lookup
  so the renderer paints each pane against the matrix
  matching its buffer. Cost: `CellsRenderState` shape
  changes from single-snapshot to per-pane inputs;
  dispatch's `publish_render_state` populates one entry
  per visible pane. Trade accepted: paramount goal #1
  (performance) is protected because the worker keeps
  its single wake / single cache-hit discipline — the
  iteration is over typically 1–2 visible buffers, and
  the per-buffer cache-hit shortcut means scrolling one
  pane only rebuilds that pane's matrix. The registry
  also generalises cleanly to D.6 (three panes), `:windo`
  if it ever needs per-pane rich rendering, and future
  preview panes — none of those have to grow their own
  builder. Heuristic #2 reading: while diff is the only
  *near-term* consumer that needs more than one
  document's matrix simultaneously, the design choice
  is between "diff grows a parallel pipeline" (D1) and
  "the existing pipeline learns to handle multiple
  documents" (D2). D2 is the lower long-term entropy
  even though both shapes work for diff alone.

## 9. Slice plan

Sequencing lives in
[`docs/dev/operations/slice-plans/pane-groups.md`](../operations/slice-plans/pane-groups.md);
authoritative status per slice lives in
[`docs/dev/operations/implementation.md`](../operations/implementation.md).
This fragment owns *what* and *why*; the slice plan owns
*when* and *in what order*.

## 10. Testing strategy

- **Unit tests on `PaneGroup` + registry**: add / remove
  round-trip; same-`(pane,buffer)` rejected; closing a
  member prunes; member-index lookup.
- **Unit tests on identity propagation**: 2-pane group at
  identity mapper, scrolling member A propagates A's
  scroll to member B verbatim — when B's current buffer
  matches its registration.
- **Unit tests on buffer-mismatch suspension**: 2-pane
  group at identity mapper, switch B to a different
  buffer mid-test, observe that propagation skips B for
  that tick; switch back, observe propagation resumes.
- **End-to-end test with a stub mapper**: a 2-pane group
  with a `+1` offset mapper, scroll on A puts A.scroll+1
  on B.
- **Bench** (D.4.e): `pane_group_scroll_dispatch_p99_us`
  at 2-pane / 3-pane groups; `pane_group_no_group_p99_us`
  as the noop baseline (every tick, no group exists).
  CI gate enforces the keystroke budget.

## 11. Risks

- **Tail-time propagation lag.** Scroll appears in the
  bound pane on the next paint cycle, not synchronously
  with the input event. Practically invisible (a single
  paint tick) but worth flagging if a user reports the
  bound pane "feels off." Mitigation: synchronous
  propagation from `set_scroll` once the chokepoint
  refactor is justified.
- **Mapper-induced infinite loops.** A mapper that maps
  `A.row → B.row` and on the next tick `B.row → A.row+1`
  would produce a runaway. The "active pane drives,
  others follow, never reverse" rule prevents this by
  construction — propagation reads the active pane's
  scroll, writes others' stashes, never the reverse.
- **Group survival across tree mutations.** A
  `:close` that removes a member must leave the group
  consistent. Safety-net prune in §5 handles it.

## 12. Cross-references

- `lattice-core/src/ui/pane.rs` — `PaneTree`, `PaneState`,
  `PaneId` (geometry layer; unchanged by this fragment
  except for the new `PaneGroupId` type).
- `lattice-host/src/pane_group.rs` (new) — `PaneGroup`,
  `RowMapper`, `IdentityRowMapper`, `Editor` extension
  methods.
- `lattice-host/src/dispatch.rs` — `publish_render_state`
  calls `propagate_pane_group_scroll` at the tail.
- `docs/dev/architecture/diff-system.md` §5.2 + D.4 — first
  non-trivial consumer (HunkRowMapper).
