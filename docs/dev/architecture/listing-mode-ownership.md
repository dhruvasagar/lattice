# Listing mode-ownership — the majors own entry navigation

> **Status: implemented (LM.0–LM.4, 2026-09-28).** Sequencing + commit
> hashes live in the archived slice plan
> (`docs/dev/operations/slice-plans/archive/listing-mode-ownership.md`).
>
> `directory-listing-mode.md` §2 states *"entry navigation is major-owned"*
> but never designed how; this fragment is that design, now realized —
> oil-mode and file-tree-mode own `<CR>` / `-` / `<C-s>` / `<C-v>` / `<C-t>`
> through their keymaps + `action_handlers`, and the host keeps no
> `BufferKind::{Oil,FileTree}` navigation branch.

## 1. What this fixes

After the DL series, oil and the file tree are `Document`-backed and
share a presentation minor (`directory-listing-mode`). But the chords
that *navigate and open* — `<CR>`, `-` — never became mode-owned. They
are hard-wired in the host's buffer-local input gate
(`lattice-host/src/input.rs`), which branches on `BufferKind::Oil` /
`BufferKind::FileTree`, dispatched through a `match active_buffer` on
`Action::FollowLink`, with the handler bodies (`Editor::do_oil_follow`,
`do_file_tree_follow`, `do_oil_navigate_up`, `do_open_oil`) living on
`Editor` in `lattice-host`.

That is the exact shape `feedback_buffers_no_special_case` and
`feedback_mode_owns_its_surface` forbid: a kind-branch in the host plus
host-owned handler bodies for a mode's chords. It is logged as Oil
cleanup-debt (mode-architecture.md §13). This fragment migrates the
navigation/open surface into `oil-mode` and `file-tree-mode` so that
**the chord choice AND the handler body both live with the major that
owns the buffer**, and the host keeps no `BufferKind::{Oil,FileTree}`
branch for it.

It also adds the user-facing feature that motivated the work: open the
entry under the cursor in a split (`<C-s>`), vertical split (`<C-v>`) or
new tab (`<C-t>`), the picker's own vocabulary
(`lattice_picker::OpenTarget`), applied to listings.

## 2. Why the majors, not the minor

`directory-listing-mode` (the minor) owns *presentation* — icons,
per-row colour, display options — and no keymap, deliberately: `<CR>`
means "open" in the tree and "nothing" (in the open sense) in oil, and
`-` re-lists the parent. Behaviour differs per major, so the chords and
their handlers belong to each **major**, not the shared minor. This is
the same split the minor's own design draws (§2: presentation shared,
behaviour major-owned).

## 3. The blocker: mode handlers cannot reach listing state

A mode-owned action handler is a closure
`Fn(&ActionContext) -> Option<Effect>` (the diff/multibuffer precedent).
`ActionContext` exposes `buffer_id`, `cursor`, `selection`,
`services: &ServiceRegistry`, `events` — and **nothing else**. Oil and
file-tree per-buffer state (`OilDir`, `OilSnapshotLocal`,
`FileTreeEntries`, `FileTreeNerdFonts`) lives in the host-owned
`Editor::buffer_locals`, which no service exposes. So a handler cannot
even read the entry under the cursor.

Contrast diff (`DiffSubsystemHandle`) and multibuffer
(`MultibufferRegistryHandle`): their per-buffer state lives in an
Arc-shared service handle a handler pulls from `ctx.services`. Oil and
the file tree keep theirs in `buffer_locals` instead — so rather than
build a service, LM.1 widens the handler boundary to read it (§3.1).

### 3.1 Chosen (LM.1): `ActionContext` exposes the buffer's locals

The state stays in `Editor::buffer_locals` exactly where the DL series
put it. What changes is that the mode-handler boundary gains a read path
to it: `ActionContext` grows

```rust
pub buffer_locals: Option<&'a BufferLocals>,   // the active buffer's locals
pub fn buffer_local<T: BufferLocal>(&self) -> Option<&T>;
```

The host's chord-dispatch site passes the active buffer's locals; the
auxiliary firing paths (prompt submit, transient item, `Confirm`
yes-action) pass `None`, and the accessor degrades to "no such local"
rather than branching. A handler resolves the entry under the cursor by
reading `ctx.buffer_local::<OilSnapshotLocal>()` etc.

**Why this, not a `ListingRegistry` service (rejected).** The registry
was the first design here and is recorded as rejected: relocating the
state out of `buffer_locals` into a service would have dropped it from
`:describe-buffer`'s `iter_descriptors` enumeration — trading the
self-documenting-help pillar (§5.11) for handler-reachability. Reading a
buffer's own locals from its handler is also the more general mechanism
(any mode handler wants it), not a listing special-case, and it is
smaller: no state move, no `Editor` field, no dual-boot-path wiring, no
`DocumentClosed` cleanup subscriber.

**Paramount goals + UX:** protects #2/#3 (the majors reach per-buffer
state and own their grammar surface) *and* the introspection pillar
(state stays enumerable). Perf-neutral: one `HashMap` lookup on the
dispatch path, off the render hot path.

### 3.2 Many listings, each independent — the everything-is-a-buffer contract

There is no "the oil buffer" and no "the file tree". A user can hold
several oil buffers and several file-tree buffers open at once — in
different panes, different tabs — each rooted at a different directory,
each with its own cursor, scroll, snapshot and expansion state. They are
ordinary buffers and must behave independently: navigating one re-lists
*only* that buffer; toggling a directory in one tree leaves every other
tree pixel-stable.

This is a hard contract, and the design keeps it structurally:

- `buffer_locals` keys `BufferLocals` by `BufferId`, and a handler reads
  only `ctx.buffer_id`'s locals. There is no process-global listing
  state and no "active listing" singleton.
- Every navigation/open effect carries an explicit
  `view: BufferId` — `OilNavigate { view, dir }`,
  `FileTreeToggle { view, entry_index }` — and the applier mutates only
  that buffer's rope and only that buffer's registry entry.
- A handler reads the entry under the cursor from `ctx.buffer_id`'s
  state, never from a shared "current" pointer.
- An async result that moves the cursor lands with
  `Effect::CursorMoveIn { target, position }` (not `CursorMove`): by the
  time a re-list completes the focused buffer may be a *different*
  listing, and the cursor must land in the buffer the result was
  computed for. Per `feedback_decorations_update_in_place`, the other
  listings' rows never change.

The multibuffer registry is precedent that per-`BufferId` service state
supports arbitrarily many independent instances; this inherits that.

## 4. Opening in a target pane — new Effect vocabulary

A mode handler speaks `Effect`s; it never calls `&mut Editor`. Opening
in the active pane is `Effect::OpenBufferAt` (the multibuffer `<CR>`
precedent). Opening in a split/vsplit/tab is **not** in the vocabulary:
the picker reaches it through an `Action` path
(`PickerAcceptInVSplit` stashes `editor.picker_open_target`), which a
handler cannot use.

New peer-applied variant:

```rust
Effect::OpenInTarget {
    path: Option<PathBuf>,
    position: Position,
    target: OpenTarget,
}
```

Its peer arm (TUI + GPUI, in lockstep) is exactly the picker-accept
sequence: `editor.prepare_open_target_pane(target)` then
`editor.open_buffer_at(path, position, false, None, None)`. `<CR>`
(current pane) keeps returning plain `OpenBufferAt`; the split/vsplit/tab
chords return `OpenInTarget`.

`OpenTarget` (`Default | Split | VSplit | Tab`) moves from
`lattice-picker` to `lattice-core` (beside `SplitOrientation`), so
`lattice-grammar`'s `Effect` can name it without depending on
`lattice-picker` (wrong layering direction). `lattice-picker`
re-exports it, so every existing `lattice_picker::OpenTarget` call site
is untouched.

**WIT boundary:** `OpenInTarget` is host/peer-only initially — it joins
the host-only group in `boundary_effect.rs` (a typed "no WIT mirror"
error, like `OpenPrompt`'s peers), not a silent drop. A plugin opening
in a split is a deliberate future WIT addition, not a side effect of
this slice. Keeps the WIT wire honest (`feedback_wit_canonical_sdk_ergonomics`).

### 4.1 Directory rows

`<C-s>`/`<C-v>`/`<C-t>` on a **directory** row open an oil browser rooted
at that directory in the new split/vsplit/tab — consistent with `<CR>`
opening a directory. This reuses the target-pane split then the existing
"open oil at dir" path (`Effect::OpenOil { dir }` already exists; its
applier is `do_open_oil`), composed as `Effect::Many([…split…, OpenOil])`
or a small `OpenOilInTarget` peer, decided at LM.3.

## 5. The in-place re-list / toggle branches

The directory branches of `<CR>` and `-` are not opens — they mutate the
listing in place: oil reloads the snapshot for the new directory (fs
I/O) and rewrites the buffer's rope; the file tree toggles a directory's
expansion and rewrites its rope. Neither is expressible as
`OpenBufferAt`.

The **decision** (which directory, from cursor + current state) is read
by the mode handler through `ListingRegistry` and stays mode-owned. The
**apply** (fs read + render + rope rewrite + icon publish + cursor
reset) is mechanical and host-applied, via data-carrying effects the
handler emits, each naming the `view` it acts on (§3.2):

- `Effect::OilNavigate { view, dir }` — re-list an oil buffer to `dir`
  (covers both `<CR>` into a child and `-` up to a parent; the handler
  resolves `dir`).
- `Effect::FileTreeToggle { view, entry_index }` — toggle expansion.

Their appliers reuse the existing render/rewrite machinery
(`write_oil_listing` / `set_file_tree_entries`, unchanged — still
reading/writing `buffer_locals`) and the owner-write path
(`replace_owned_buffer`, which
bypasses the read-only gate for a subsystem-owned synthetic buffer).
This is the diff pattern — the service/mode decides, the host applies a
typed data effect — not a half-migration: no `Editor::do_<x>` remains
bound to the mode's chords.

**fs I/O** stays synchronous in the applier, exactly as
`OilSnapshot::open` / `reload` are on the dispatch path today. Moving
listing I/O to `SubsystemBoot::inbound` is a separate, pre-existing
concern (the I/O is already synchronous here); this migration introduces
no new UI-thread I/O.

## 6. Keymaps

`oil-mode` and `file-tree-mode` grow `MajorMode(mode_id)` keymap entries
via `Keymap::from_entries` (the diff/multibuffer shape); the host's
K.2.4 boot pass (`translate_mode_keymaps`) builds the trie and pushes the
layer. No host keymap code, no new `Action` enum variants.

| chord | oil-mode | file-tree-mode |
|---|---|---|
| `<CR>` | open file / re-list into dir | open file / toggle dir |
| `-` | re-list to parent dir | open oil at parent of entry |
| `<C-s>` | open file in split (dir → oil-in-split) | same |
| `<C-v>` | open file in vsplit (dir → oil-in-vsplit) | same |
| `<C-t>` | open file in tab (dir → oil-in-tab) | same |

The chords resolve only in mode-active buffers (K.1.c's per-keystroke
filter), so `<C-v>` (blockwise visual) and `<C-t>` (tag-pop) keep their
Builtin meaning everywhere else. `<C-s>` is unbound at Builtin.

The host input gate loses its `BufferKind::Oil` block entirely, and
`BufferKind::FileTree` is split out of the shared `Help | FileTree`
block — **Help and Dashboard keep their gate wiring (Esc-dismiss,
`<CR>`-follow) unchanged**; only file-tree's keys migrate.

## 7. Acid test

A new listing-shaped provider crate lands with **zero** `Editor::`
method additions in `lattice-host` and **zero** new host `Action`
variants — it registers a mode (keymap + handler closures) and reads
state through `ctx.buffer_local::<T>()`. The removal of `do_oil_follow` /
`do_file_tree_follow` / `do_oil_navigate_up` from `lattice-host`, and the
disappearance of the `BufferKind::{Oil,FileTree}` gate branches, is what
proves the migration is not a half-migration.

## 8. Test contract

- A sibling of `multibuffer_is_a_regular_buffer.rs` for oil + file-tree:
  the vim grammar (`gg`, `<C-d>`, motions) still resolves in these
  buffers, and the new chords resolve *only* in them.
- **Independence (§3.2):** two oil buffers open at once, rooted at
  different directories — navigating one re-lists only that buffer and
  the other's rope, cursor and scroll are byte-for-byte unchanged. Same
  for two file trees: toggling a directory in one leaves the other's rope
  identical. This is the everything-is-a-buffer guard and it must assert
  on both buffers, not just the acted-on one.
- `<C-s>`/`<C-v>`/`<C-t>` on a file row open it in a new
  split/vsplit/tab respectively (asserted on the pane tree, not just the
  effect) — the LM.0 primitive proven end to end.
- `<C-s>` on a directory row opens oil rooted at it in the split.
- Re-list / toggle still work after migration: `<CR>` into a child dir
  re-lists; `-` re-lists the parent with the came-from cursor landing;
  `<CR>` on a tree directory toggles expansion. Assert the async result
  is visible **without** a follow-up keypress (the inbound-wake contract)
  where an async path is involved.
- Oil `:w` round-trip still derives the right renames after the state
  moved to the registry (the snapshot the diff reads is the registry's).
- Help/Dashboard: `<CR>`-follow and Esc-dismiss unchanged after
  `FileTree` leaves the shared gate block.
- Both renderers: the LM.0 grep audit
  (`grep -rn "Effect::OpenInTarget" crates/lattice-ui-gpui/`) is
  non-empty — GPUI was not missed.

## 9. Benchmarks

The navigation path was already `O(entries)` for the re-list render; the
migration changes where the code lives, not its complexity. The
per-keystroke keystroke→glyph ratchet (CI) must not regress: moving
`<CR>`/`-` from a host gate to a mode keymap layer adds one trie layer
lookup scoped to the buffer, which the existing keymap-resolution bench
covers. No new bench is warranted unless the ratchet moves; if it does,
that is the signal to add one.
