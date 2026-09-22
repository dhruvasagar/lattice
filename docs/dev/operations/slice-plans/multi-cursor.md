# Slice plan — multi-cursor

Design: [`multi-cursor.md`](../../architecture/multi-cursor.md).

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred (not yet) · ❌ dropped (not at all).

| Slice | Description | Status |
|---|---|---|
| MC.1 | `SelectionSet` invariants: normalize, toggle, rotate, collapse | 📝 |
| MC.2 | The set is the source of truth; `Editor.cursor` is the primary | 📝 |
| MC.3 | Secondary cursors drawn, TUI + GPUI | 📝 |
| MC.4 | `multi-cursor-mode`: placement chords | 📝 |
| MC.5 | `Broadcast` on `CommandSpec`; per-cursor execution in the actor | 📝 |
| MC.6 | Insert-mode fan-out | 📝 |
| MC.7 | Host actions that edit at the cursor opt in | 📝 |
| MC.8 | Registers carry per-cursor chunks | 📝 |
| MC.9 | Dot-repeat and macros under cursors | 📝 |
| MC.10 | Visual mode per cursor | 📝 |
| MC.11a | Multibuffer: split cross-excerpt edits per source (pre-existing bug) | 📝 |
| MC.11b | Multibuffer: outside edits leave `composed_doc` stale? (reproduce first) | 📝 |
| MC.11c | Multi-cursor in multibuffer views | 📝 |
| MC.12 | Plugins declare `broadcast` | 📝 |
| MC.13 | Completion accept at matching cursors | 📝 |
| MC.14 | User docs + site | 📝 |
| MC.15 | LSP `linkedEditingRange` trigger | ⛔ |

**Order.** MC.1–MC.3 land with no user-visible change: the set can hold N and
be drawn, but nothing produces N. MC.4 makes cursors placeable. MC.5 makes them
useful. Everything after MC.5 widens coverage. MC.11a/b fix pre-existing
multibuffer defects that multi-cursor makes likelier to be hit; they need not
wait for MC.5 and can be picked up whenever.

**Single-cursor is the regression surface.** Every slice through MC.10 must
leave single-cursor behaviour byte-identical, and the existing
`keystroke_glyph_ratchet` and `dispatch_publish` numbers must not move. A slice
that shifts either is not done.

---

### MC.1 — `SelectionSet` invariants 📝

Design §3.2. In `lattice-protocol/src/selection.rs`:

- `normalize(&mut self)` sorts by position and merges selections that overlap
  or touch, including identical cursors. The primary follows the merge that
  swallowed it.
- `toggle_cursor(Position)` adds a cursor, or removes it if one is already
  there. Removing the last one is refused, so the set stays non-empty.
- `rotate_primary(forward: bool)`, for `]C` / `[C`.
- `collapse_to_primary()`.
- `from_parts` normalizes.

**Tests:**
- merge of touching, overlapping and identical selections;
- the primary survives a merge, and so does a merge that swallows the primary;
- toggling off the last cursor is refused;
- rotation wraps.

**Bench:** extend `lattice-core/benches/document_hotpath.rs` with
`normalize_{10,1000}`.

### MC.2 — the set is the source of truth 📝

Design §3.1, §3.3. `lattice-host`:

- `write_through_caret` writes through `replace_primary`, not `single(...)`.
- Every `set_selections_blocking(SelectionSet::single(..))` site (17 today) is
  audited, and each becomes either `replace_primary` or an explicit
  `collapse_to_primary`, with a comment saying which and why.
- `PaneState` stashes secondaries; `load_active_pane` restores them, clamped.
  `activate_document` clears them on a buffer change.
- **`SelectionChange` arm:** `Effect::SelectionChange` with more than one
  selection is adopted as-is (normalized), where today it is collapsed.

**Tests (new file `crates/lattice-host/tests/multi_cursor_state.rs`):**
- a seeded 3-selection set survives `j`, `x` and `:w` untouched, apart from
  the primary moving;
- the set survives a pane switch and back;
- a buffer switch clears it;
- `u` after a single-cursor edit behaves exactly as before.

**Risk:** this is the widest slice. The audit is the work, not the code.

### MC.3 — secondary cursors drawn 📝

Design §7. **TUI and GPUI in one patch.**

- TUI: `compose_pane_lines` paints each visible secondary with the new
  `cursor.secondary` theme key. The primary stays the hardware cursor.
- GPUI: `EditorElement` gains `secondary_cursors`, painted on the existing quad
  path. `window.rs` fills it from `render_state.selections`.
- Default `cursor.secondary` in every bundled theme, with a sensible fallback
  for user themes that lack it.

**Tests:**
- TUI compose, seeded 3-cursor set: the secondary cells carry the style and
  the rest of the line does not;
- GPUI element test with the same set;
- an off-screen secondary costs nothing: assert the per-frame conversion only
  visits viewport rows.

**Audit:** `grep -rn "secondary" crates/lattice-ui-gpui/` is non-empty.

### MC.4 — `multi-cursor-mode`: placement 📝

Design §6. New `lattice-mode/src/modes/multi_cursor.rs`, in `default_modes`,
keymap at `KeymapLayer::MinorMode`. Every handler returns
`Effect::SelectionChange` or `Effect::Declined`.

- `Q` toggles a cursor at the primary. `1Q` puts one at every match of the last
  search.
  - **Verify first** that the action context exposes the last search pattern.
    If it does not, add it to the context rather than reaching into `Editor`.
- `Q` in Visual puts a cursor on each line.
- `gQ` restores the last cleared set. Buffer-local mode state holds it.
- `<C-l>` clears the cursors, and declines when there is one cursor.
- `]C` / `[C` rotate the primary, and decline when there is one cursor.
- `q=` / `1q=` / `2q=` control follow mode, stored as buffer-local state (read
  by MC.5).
- A status-line item shows `n/N cursors` when there is more than one.

**Tests:**
- each chord's effect on the set;
- `<C-l>` with one cursor still redraws, and `]C` with one cursor still moves
  to the class end (assert the buffer and cursor, per
  `decline-only-shared-chords`);
- `q=` resolves to the mode, not the macro-record wildcard;
- `:describe-key Q` names the mode.

### MC.5 — broadcast 📝

Design §4.1, §4.3, §4.4. **The core slice.**

- `lattice-grammar`: `Broadcast` on `CommandSpec`, defaulted from
  `CommandKind`. A new `execute_broadcast` beside `execute_with_env` runs the
  bottom-up per-selection loop inside one undo group, writes each result back
  into the document's set, normalizes, and merges effects per the §4.4 table.
- `lattice-runtime`: the actor's dispatch arm calls `execute_broadcast` when the
  set has more than one selection and the command is `EachCursor` (or `Follow`
  with follow mode on). One message, same `DocumentHandle` signature.
- **Verify** that nested undo groups compose when a broadcast `c` opens the
  Insert session's group inside the broadcast's. If they do not, the broadcast
  group yields to the session group.
- One cursor erroring or declining leaves that cursor unchanged and logs at
  `debug!`; the others proceed.

**Tests (`crates/lattice-host/tests/multi_cursor_broadcast.rs`):**
- `dw`, `dd`, `cw`, `x`, `>>` at 3 cursors: assert the **text and each
  selection's position**, including a line-count-changing `dd` at adjacent
  lines (the case blockwise never had to handle);
- cursors that converge merge;
- `j` moves only the primary; with follow mode on it moves all;
- one `u` undoes the whole broadcast.

**Benches:**
- `dispatch_publish/multi_cursor_{1,10,100,1000}` for a broadcast `dw`,
  recorded in `benchmarks.md`;
- `multi_cursor_1` must match today's `keystroke_publish`.
- If `_1000` exceeds a frame, the batched transform (design §8) becomes a
  sub-slice MC.5b before moving on.

### MC.6 — Insert-mode fan-out 📝

Design §4.5. `lattice-host`:

- `do_insert_text` builds one `apply_edit_batch` across every cursor.
- `<BS>`, `<CR>`, electric reindent and auto-wrap are evaluated per cursor
  within that batch.
- Completion, signature help and hover stay on the primary.

**Tests:**
- `ifoo<Esc>` at 3 cursors: text and positions;
- `<BS>` at column 0 on one cursor but not the others;
- `<CR>` with auto-indent;
- an auto-pair `(` at 3 cursors, which reaches broadcast through `Invoke`;
- one `u` undoes the session.

**Ratchet:** add a 10-cursor case to `keystroke_glyph_ratchet.rs`.

### MC.7 — host actions opt in 📝

Design §4.1. Classify the host `Action` enum: paste, `J`, `o`/`O`, `r`, `~`,
`<C-a>`/`<C-x>` go `EachCursor`, and everything else stays `Primary`. Opted-in
host actions run their handler once per cursor with `self.cursor` set to that
cursor's head, inside one undo group.

**Tests:** one per opted-in action at 3 cursors.

**Paste:** `do_paste_blockwise` issues one edit per row, and may produce one
undo step per row (unverified). Check that while in here, and group it if so.

### MC.8 — per-cursor register chunks 📝

Design §4.6. `UnnamedRegister` gains `chunks`. A broadcast `Yank` writes one
entry with N chunks.

On paste:
- N cursors with N chunks → chunk *i* goes to cursor *i*;
- otherwise → the joined content goes to every cursor.

The clipboard and the yank ring see the joined content.

**Tests:**
- `yiw` then `p` at 3 cursors: each pastes its own word;
- 3 chunks pasted at 2 cursors pastes the joined text;
- a named register works the same;
- a single-cursor yank is unchanged.

### MC.9 — dot-repeat and macros 📝

Design §4.7. Expected to be mostly tests, since both paths re-enter dispatch.

**Tests:**
- `ciwfoo<Esc>` at one cursor, place 3, `.`;
- record `@q` at one cursor, replay at 3;
- a macro containing a bare motion, with follow mode on and off.

Any code change this needs is a finding, and the design doc gets updated.

### MC.10 — Visual per cursor 📝

Design §4.8.
- Entering Visual anchors every selection. Follow mode is on in Visual.
- Visual operators broadcast with each selection's own `Range::Selection`.
- `visual_selection_range` / `visual_block_extents` get `all()` peers. The
  renderers paint per selection, in both peers.
- `<C-v>` collapses to the primary.

**Tests:**
- `vwd` at 3 cursors;
- `Vy` then `p`, through MC.8's chunks;
- `<C-v>` collapses.

### MC.11a — split cross-excerpt edits 📝

Pre-existing bug, design §5 item 1. **Reproduce first:** a failing test where
an edit spans two excerpts from different files and the second file never
changes. Then split per excerpt in `build_source_edit` /
`apply_edit_batch_sync`: the start excerpt takes the replacement text and the
others take deletions. Update `multibuffer-views.md` if its description
diverges from what ships.

### MC.11b — stale `composed_doc` after an outside edit 📝

Design §5 item 2, unverified. **Reproduce first:**
1. edit a source in another pane;
2. assert the view's next operator edit lands on the new text.

If it reproduces, `recompose_inner` writes the new text into `composed_doc` and
maps `state.selections` through the new row translation. If it does not, mark
this ❌ with what the reproduction showed.

### MC.11c — multi-cursor in multibuffer views 📝

Design §5.
- `dispatch_composed` seeds the scratch `Document` with the view's set, runs
  `execute_broadcast`, lands through `land_composed_edits`, and adopts the
  scratch's final set.
- Insert fan-out goes through the view's `apply_edit_batch`.

**Tests:** extend `multibuffer_is_a_regular_buffer.rs` with a broadcast `x`
across two excerpts from different files, and assert both source documents.

### MC.12 — plugins declare `broadcast` 📝

Design §4.1, §9 #2.
- WIT: an optional `broadcast` on plugin command registration, defaulted from
  kind as for native commands.
- The trampoline passes it through.

**Test:** a fixture plugin operator broadcasts at 3 cursors with no change to
its code.

A synchronous full-set read for plugins (today they get only the primary, or
the full set via `selections-changed`) is **not** added here. It waits for a
consumer.

### MC.13 — completion accept at matching cursors 📝

Design §4.5. Accepting a completion replaces the word-before-cursor at every
cursor whose prefix matches the primary's, in one batch.

**Tests:**
- 3 matching cursors;
- 1 of 3 not matching, which is left untouched;
- one `u`.

### MC.14 — user docs 📝

1. Write `docs/user/multi-cursor.md` with a `summary:` line.
2. Add it to `site/data/nav.toml` (Guide tier, beside Visual mode) and to the
   `:help` index `docs/user/README.md`.
3. Run `sync-docs.sh` and `zola build`, and confirm the page is in
   `docs-search.json`.
4. In `design.md`, point §5.2's placeholder at the fragment and drop
   multi-cursor from §5.5's plugin candidates.

This design commit already does step 4.

### MC.15 — LSP `linkedEditingRange` trigger ⛔

Deferred to the LSP plan (4.5.f in `docs/dev/notes/lsp-features.md`), whose
stated blocker was "multi-cursor shadow-edit machinery". MC.6's fan-out is that
machinery: linked ranges become cursors for the duration of the edit. It comes
back when MC.6 is ✅, as an LSP slice rather than a multi-cursor one.
