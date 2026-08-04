# Pane buffer history

**Status:** design. Slice plan:
[`slice-plans/pane-buffer-history.md`](../operations/slice-plans/pane-buffer-history.md).

Per-pane back/forward navigation over the buffers a pane has shown.
`<C-6>` steps back, `<C-7>` steps forward, `:history pane-buffers`
opens the pane's history as a picker.

---

## 1. Goal

A pane accumulates a trail of the buffers it has displayed. The user
walks that trail without leaving the pane, landing back on the exact
cursor position they left. Splitting a pane starts a **new** trail —
the new pane is a new place to work, not a copy of where you have
been.

Distinct from the three neighbours already in the tree:

| Surface | Scope | Granularity |
|---|---|---|
| Position history (`<C-o>` / `<C-i>`) | Global | Every `AutoJump` — motions, searches, marks |
| `:ls` / `:bn` / `:bp` | Global | Registry order, not visit order |
| MRU picker (`:buffers`) | Global | Recency, not per-pane, not walkable |
| **Pane buffer history** (`<C-6>` / `<C-7>`) | **One pane** | **Buffer visits only** |

---

## 2. Why not a filtered view of position history — REJECTED

§5.1.1 of the design doc unifies the jump list and mark ring into one
ring that different keybindings walk through different filters, and
the obvious move is to make this a fourth filter. It does not work,
on two independent facts:

- **`PositionEntry` has no `pane_id`** (`lattice-host/src/state.rs`).
  The ring is global. Adding a pane id to every entry would be a wide
  change to a hot structure for one consumer.
- **The ring records every `AutoJump`** — `G`, `gg`, a search, a mark
  jump. It is bounded, so scrolling around inside a single file
  *evicts* the record of which buffers the pane visited. A pane
  history built on it would be destroyed by ordinary editing.

The second point is decisive: it is a functional loss, not a
stylistic one. Unification is right when the underlying data is the
same; here it is not — one records *positions*, the other records
*buffer visits per pane*.

---

## 3. Data model

	struct PaneHistoryEntry {
		buffer: BufferId,
		cursor: Position,
		scroll: u32,
	}

	struct PaneBufferHistory {
		entries: Vec<PaneHistoryEntry>,   // oldest → newest
		cursor: usize,                    // index of the CURRENT entry
	}

Cursor and scroll are stored **per entry**, not looked up from the
buffer. The same buffer can appear at several points in a trail at
different locations, and "take me back where I was" is the whole
point of a back key — a buffer-global last-position lookup would land
all of them in the same place.

---

## 4. Storage — a side table, and why that is load-bearing

	// on Editor
	pane_buffer_history: HashMap<PaneId, PaneBufferHistory>,

**Not a field on `PaneState`.** `PaneState` is `Copy`, and
`PaneTree::split_active` builds the new leaf with

	let new_state = self.leaves[active_idx];
	let new_state = PaneState { id: PaneId::next(), ..new_state };

— a field-wise copy. A `history` field there would be copied into the
new pane, which is exactly the behaviour this feature must not have;
avoiding it would mean *remembering* to reset that one field, and a
future field added the same way would silently inherit the bug.

Keyed by `PaneId` in a side table, the requirement holds **by
construction**: `PaneId::next()` is process-monotonic and never
reuses ids, so a freshly split pane has no map entry and therefore no
history. Nothing has to remember to clear anything.

Secondary win: `PaneState` stays `Copy`, so split / close / layout
stay allocation-free (paramount #1).

### Garbage collection — reconcile, do not hook

Panes disappear through more than one path — `close_active` and
`collapse_to_active` today, and any future one. Hooking each removal
site is the kind of enumeration that goes stale silently.

Instead the map is **reconciled against the tree**: retain only keys
still present in `PaneTree::leaves`. Same shape as
`refresh_autoread_watcher`'s desired-set diff — one function that
cannot miss a caller, rather than N call sites that can.

The reconcile runs at the top of `active_pane_history_mut`, the one
path that *reads* history. Calling it from each pane-mutation site
would reintroduce the enumeration this avoids (there are ~10 in
`dispatch.rs`), and calling it per tick would spend O(panes) on every
keystroke for staleness nobody can observe.

A closed pane's entry therefore lingers until the next history
operation. Bounded and harmless: the map is keyed by `PaneId`, so a
dead pane's entry can never be *read*, and any navigation in a
surviving pane clears it. An entry only exists if its pane navigated,
so the worst case is "panes navigated-then-closed since the last
navigation" — single digits in practice.

---

## 5. Recording — one chokepoint, preview-safe by construction

Recording happens in `Editor::activate_buffer_only`
(`lattice-host/src/dispatch.rs`). It is already the single funnel for
"this pane now shows a different buffer", already guarded by

	if id != self.active_pane_buffer_id()

so a re-activation of the current buffer is not recorded, and it is
already where position history is pushed. The new push sits alongside
that one.

**Previews cannot pollute history.** A picker preview goes through
`Editor::set_preview_override`, which leaves the pane's committed
`PaneState.buffer_id` untouched and is projected into the render
state only (`preview-isolation.md` §5). Since it never reaches
`activate_buffer_only`, no preview is ever recorded. This is a
property of the existing architecture, not a filter this feature
adds — worth stating because "scrolling a picker fills my history
with junk" is the obvious way this feature goes wrong.

The **cursor/scroll captured** is the pane's *outgoing* position: on
leaving buffer A for B, the current entry (A) is updated with where
the cursor was, then B is pushed. The newly pushed entry's own
position is a placeholder until something leaves it — one rule,
"record where you were before moving", shared by the recording path
and the walk, rather than trying to guess where activation will land
the cursor before it has happened.

### What does and does not enter a trail

- **In-pane synthetic buffers do** — `:help`, `:lsp-log`, a file tree,
  an oil buffer. They route through `activate_buffer_only` like any
  other buffer, and under "everything is a buffer" there is no reason
  to special-case them out. Walking back to the help page you were
  reading is a feature, not a leak.
- **Floating popups do not.** The focus-steal path sets
  `active_buffer` / `popup_focused` and stashes `prev_pane_for_popup`,
  but never reassigns the pane's `buffer_id` — so an overlay cannot
  reach the chokepoint. `dismiss_popup`'s direct
  `pane.buffer_id = prev.buffer_id` restore is a no-op in that case;
  the sibling restore path that *does* change buffers calls
  `activate_buffer` first and is therefore recorded, correctly, as a
  real navigation.

Both fall out of existing structure rather than a filter this feature
adds — worth stating because the two direct `pane.buffer_id = …`
assignments in `dispatch.rs` look like chokepoint bypasses until you
check what they do.

---

## 6. Walk semantics — browser back/forward

Truncating, like a browser and like vim's jump list:

	visit A → B → C          entries=[A,B,C]  cursor=2
	<C-6>                    entries=[A,B,C]  cursor=1   (showing B)
	<C-6>                    entries=[A,B,C]  cursor=0   (showing A)
	<C-7>                    entries=[A,B,C]  cursor=1   (showing B)
	open D                   entries=[A,B,D]  cursor=2   (C's tail dropped)

Two invariants:

- **A walk never records.** Stepping back must not push a new entry,
  or the forward direction becomes unreachable. The walk moves
  `cursor` and switches the buffer through a path that bypasses the
  record step.
- **Visiting while walked-back truncates the forward tail.** Anything
  after `cursor` is dropped before the push.

**Deliberate divergence from vim.** Vim's `<C-^>` is alternate-file
*toggle* — press twice and you are back where you started. Here
`<C-6>` twice goes back two entries. A toggle cannot be half of a
back/forward pair, and the pair is what was asked for. Note this
costs nothing that works today: no `:b#` / alternate-buffer command
exists in the tree, so there is no existing behaviour to lose. If the
toggle is missed later it can land as its own named command rather
than by overloading a directional key with hidden state.

### Edge cases

- **At either end** — echo (`already at the oldest/newest entry`),
  no wrap. Wrapping turns a directional key into a cycle.
- **Entry's buffer was deleted** (`:bd`) — entries pointing at buffers
  no longer in the registry are skipped and dropped as the walk passes
  them, rather than eagerly purged on delete. Lazy keeps `:bd` off the
  hook for a data structure it should not know about.
- **Empty / single-entry history** — `<C-6>` echoes and does nothing.
- **Pane never navigated** — the map entry is created lazily on first
  buffer change, seeded with the pane's current buffer as entry 0, so
  the first `<C-6>` has somewhere to go back *from*.

---

## 7. Chords — `<C-6>` / `<C-7>`, and why not `<C-^>`

Terminals send **0x1E** for Ctrl+6 and **0x1F** for Ctrl+7. crossterm
0.28.1 (`event/sys/unix/parse.rs`) maps that range as:

	c @ b'\x1C'..=b'\x1F' => KeyEvent::new(
		KeyCode::Char((c - 0x1C + b'4') as char),   // 0x1E→'6', 0x1F→'7'
		KeyModifiers::CONTROL,
	)

so the chords arrive as `Char('6')+CTRL` / `Char('7')+CTRL`.
`<C-6>` / `<C-7>` are therefore the **only** spellings that match in
the TUI — a `<C-^>` binding would never fire, because crossterm never
produces `Char('^')+CTRL` for that byte. Both are plain ASCII control
codes, so neither needs the kitty keyboard protocol.

`KeymapLayer::Builtin`, `BindingMode::Normal`. This is universal pane
machinery with no owning mode, so the builtin layer is correct and
there is no mode-ownership surface to split.

**GPUI peer** must map Ctrl+6 / Ctrl+7 to the same `KeyChord`s. GPUI
delivers a keystroke with key `"6"` and a ctrl modifier rather than a
control byte, so this is a peer-specific mapping check, not something
the TUI path proves.

---

## 8. `:history pane-buffers`

`:history` already dispatches to a picker source by name
(`commands` → `history`, `searches` → `search-history`). This adds a
third arg, `pane-buffers` → `pane-buffer-history`, plus:

- a `PaneBufferHistorySource` in `lattice-picker`, newest-first,
  each row routing to "switch this pane to that entry",
- a `pane_buffer_history` field on `PickerContext` (owned vec, built
  per picker-open like `position_history` / `buffers`),
- `pane-buffers` added to the `gen:history-kinds` completion source
  so `:history <Tab>` offers it.

Rows show the buffer name and its stored line, with the current entry
marked. Accepting an entry moves `cursor` to it rather than pushing —
the picker is a random-access walk, not a new visit.

---

## 9. Options

`pane.buffer-history-size` — typed `usize`, default **100**,
customizable. Oldest entries evicted when the cap is hit. Consistent
with how the other rings here are bounded and tunable through `:set`
/ `:customize`.

Eviction shifts `cursor` down with the entries so the current
position does not drift onto a different buffer.

---

## 10. Error handling

Recoverable and expected states — walking past an end, an entry whose
buffer was deleted, a missing map entry for a pane — echo at
`EchoLevel::Info` or are silently skipped, never panic. Per the
diagnostic-logging rule, per-walk tracing is `debug!`; nothing here is
`info!`-worthy fan-out.

---

## 11. Cross-references

- `preview-isolation.md` §5 — why previews cannot reach the recording
  chokepoint.
- `keymap-architecture.md` §12 — the builtin-layer binding convention.
- `design.md` §5.1.1 — position history, and §2 above for why this is
  not a filter over it.
