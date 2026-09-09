# Pane zoom

**Status:** ✅ implemented (ZP.1–ZP.5). Slice plan:
[`slice-plans/archive/pane-zoom.md`](../operations/slice-plans/archive/pane-zoom.md).

Temporarily give the active pane the whole tab, then give the split
layout back exactly as it was. tmux's `prefix z`, bound to `<C-w>z`.

---

## 1. Goal

A split layout is how you keep two things in view. It is also, for
short stretches, in the way — you want the one file full-screen to
read a long function, and then you want your layout back. Vim's
answer is `<C-w>o` / `:only`, which *destroys* the other panes; the
layout is not coming back. Zoom is the non-destructive form: the
other panes stop being drawn, the tree that describes them is
untouched, and the second press restores it.

The distinction from its neighbours in the tree:

| Surface | Effect on the tree | Reversible |
|---|---|---|
| `<C-w>o` / `:only` (`collapse_to_active`) | Drops every other leaf | No |
| `<C-w>+` / `<C-w>>` (`resize_active_split`) | Nudges one ratio | By re-nudging |
| `<C-w>=` (`equalize_ratios`) | Resets every ratio | No |
| **`<C-w>z` (zoom)** | **None — layout preserved verbatim** | **Yes, by toggling** |

---

## 2. Where the state lives

Zoom is **layout state**, and `PaneTree` is the layout owner:

	pub struct PaneTree {
		leaves: Vec<PaneState>,
		root: PaneNode,
		active: usize,
		zoomed: Option<PaneId>,
	}

Three consequences fall out of that placement, and they are the
reason for it:

- **Per-tab for free.** `TabSlot.panes` stashes a whole `PaneTree`
  and tab switching is a `mem::swap` (see `ui/tab.rs`). A zoom flag
  on the tree travels with the tab that owns it. Storing it on
  `Editor` would mean a second field to stash and restore beside the
  swap, which is a thing a future tab operation can forget.
- **Both renderers, one write.** Renderers read `rs.panes.tree`, an
  `Arc<PaneTree>` published per frame. Nothing new has to be
  published for zoom to reach the TUI and GPUI peers.
- **Geometry, hit-testing and viewport sizing agree by
  construction** — see §3.

`PaneId`, not a leaf index: `close_active` shifts every index above
the removed one (`rewrite_indices_after_remove`), so an index-valued
zoom would silently re-target a different pane.

### Rejected: zoom in the renderer

Each renderer decides for itself to paint only the active pane. It
duplicates in two peers, and — decisively — it skips the consumers
that are not painting: mouse hit-testing, per-pane viewport sizing
(which resizes terminal PTYs), and cardinal navigation. Those would
keep operating on the unzoomed geometry while the screen showed
something else.

---

## 3. Geometry

`PaneTree::compute_rects(area)` is the single canonical layout
function. The TUI draw path, per-pane viewport sizing in
`runtime.rs`, pane-height motions, and cardinal navigation all route
through it. Zoom is one branch at its head:

	pub fn compute_rects(&self, area: PaneRect) -> Vec<(usize, PaneRect)> {
		if let Some(idx) = self.zoomed_index() {
			return vec![(idx, area)];
		}
		// ... existing recursive walk
	}

One entry, the full area. Every downstream consumer inherits zoom
without knowing it exists — including the ones that stop the
renderer doing work for panes that are not on screen, which is why
zoom *reduces* per-frame cost rather than adding to it (paramount
goal #1).

`compute_rects_layout(area)` is the peer that always walks the full
split tree, ignoring zoom. It has exactly one caller — `navigate` —
and §4 explains why.

### GPUI

`collect_pane_geometries` and the paint walk in
`lattice-ui-gpui/src/window.rs` recurse over `PaneNode` themselves
rather than calling `compute_rects`. They each take the same
top-of-walk branch, in the same patch. The audit for the slice is
`rg -n "zoomed" crates/lattice-ui-gpui/` returning non-empty.

---

## 4. The invariant: zoomed ⟹ active

**The zoomed pane is always the active pane.** Every consumer of
`compute_rects` that looks up the active pane's rect
(`rects.iter().find(|(idx, _)| *idx == active_idx)`) then finds it —
under zoom the list has one entry and it is the right one. Without
the invariant those lookups fall to their `unwrap_or` defaults and
the active pane's height silently becomes the whole-screen height.

The invariant is enforced **inside `PaneTree`**, not at the call
sites, so a future caller cannot forget it:

| Method | Behaviour while zoomed |
|---|---|
| `set_active` | Clears zoom, then moves |
| `split_active` | Clears zoom, then splits |
| `close_active` | Clears zoom, then closes |
| `collapse_to_active` | Clears zoom, then collapses |
| `equalize_ratios` | No-op, returns `false` |
| `resize_active_split` | No-op, returns `false` |
| `navigate` | Unaffected — reads `compute_rects_layout` |

`navigate` is `&self` and cannot clear anything, so instead it reads
the **unzoomed** layout. `<C-w>j` while zoomed therefore finds the
pane that is spatially below in the real layout, and the `set_active`
that follows clears the zoom. The user sees: zoom drops, focus moves
one pane down. That is tmux's default `select-pane` behaviour and
Zed's toggle-zoom behaviour, and it means the navigation keys double
as the escape hatch out of zoom.

Had `navigate` used the zoom-aware `compute_rects`, it would see a
one-entry list, find no neighbour in any direction, and return
`None` — `<C-w>j` would silently do nothing.

Zoom on a single-leaf tree is a no-op returning `false`: there is
nothing to hide, and marking the tree zoomed would light the
indicator for a state the user cannot see.

---

## 5. Grammar surface

| Surface | Binding |
|---|---|
| Chord | `<C-w>z`, and `<C-w><C-z>` for parity with the ctrl-modified twins of `s` / `v` / `c` / `h` / `j` / `k` / `l` |
| Ex-command | `:zoom-pane` |
| Action | `Action::ToggleZoomPane` ← `AppEffect::ToggleZoomPane` |

`z` is unbound in the `<C-w>` layer and carries the tmux mnemonic.
It is registered in the `keymap_entry!` catalog so `:describe-key`,
`:keymap` and which-key document it like every other pane chord.

**Not `<C-w>o`.** It is vim's destructive `:only`, and a
non-destructive command wearing that chord looks identical on the
first press and diverges on the second. **Not `<C-w>_` / `<C-w>|`**:
those are vim's maximize-height / maximize-width ratio nudges, which
have no restore — rebinding them to a toggle changes what they mean
for anyone carrying the muscle memory.

**Not `:zoom`.** Per the ex-command naming rule, a bare generic name
is a premature grab: font/UI scaling in the GPUI peer is the
plausible future claimant of `:zoom`, and `:zoom-pane` says which
thing is being zoomed.

---

## 6. Indicators

One visible pane is not self-evidently *zoomed* — it is
indistinguishable from a tab that genuinely has one pane, which is
how you forget your splits exist. Two surfaces carry the marker:

- **Modeline** — a `core.zoom` built-in element, registered in
  `register_builtin_elements` and resolved host-side in
  `resolve_builtin_content`. Host-side resolution is what makes the
  TUI and GPUI peers paint identical content by construction
  (`modeline.md` §4). It composes with the existing
  `modeline.{left,center,right}` zone lists for placement.
- **Tabline** — `TabRenderItem` gains `zoomed: bool`, computed by
  the publisher. Zoom is per-tab state, so this is the only surface
  that can tell you a *background* tab is zoomed before you switch
  to it. Mirrors tmux's `Z` window-status flag.

Both palettes must occupy the same cell width per the icon
degradation rule; the marker is a plain `Z` in both, so this is
satisfied trivially.

### One option, not two booleans

	// pane.zoom-indicator
	Both (default) | Modeline | Tabline | None

Zoom-indication is one user concept. Two per-surface booleans would
scatter one decision across the `modeline` and `tabline` config
groups and read as independent knobs when they are not. The option
lives in the `pane` group, which owns the feature, and uses the
`labeled_enum!` shape already established by `tabline.show`.

Relying instead on the existing `modeline.{left,right}` zone lists to
drop `core.zoom` was rejected: it gates only the modeline half, and
turning one element off would require writing out a full explicit
element list.

---

## 7. Foldability

None. Zoom does not interact with folds, virtual rows, or the cell
grid — it changes which rect a pane is painted into, and every
per-pane content path is downstream of that rect.

---

## 8. Paramount-goal alignment

- **#1 Performance.** Net negative per-frame cost: hidden panes get
  no rect, so no element fan-out, no per-pane content resolution, no
  viewport-sizing command. The zoom check itself is one `Option`
  test at the head of an existing walk. Benched in
  `lattice-core/benches` as zoomed vs. unzoomed on a 4-pane tree.
- **#2 Extensibility.** `AppEffect::ToggleZoomPane` crosses the WIT
  boundary like every other pane effect, so a plugin can zoom.
- **#3 Modal editing.** A new `<C-w>` chord in the window-command
  submap, documented in the keymap catalog — the same shape as every
  other pane command.
- **#4 Asynchronicity.** No async surface. Zoom is synchronous tree
  state applied on the actor thread.

**UX:** no flicker risk. Zoom changes the layout only on an explicit
keypress; unedited content in the surviving pane is repainted at a
new size, which is the same path a split or a terminal resize
already takes.
