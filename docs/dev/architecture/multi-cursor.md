# Multi-cursor

Sequencing and status: [`slice-plans/multi-cursor.md`](../operations/slice-plans/multi-cursor.md).

This fragment replaces [`design.md`](design.md) §5.2's one-paragraph
"Multi-cursor (post-1.0)" placeholder, and takes multi-cursor off §5.5's
bundled-plugin candidate list. It is a **native** feature: the selection set,
the grammar dispatcher and the renderers carry it, and no plugin does.

The user model is Neovim 0.13's (2026-09): cursors are *placed* by hand, edits
*broadcast* to every cursor, and motions move only the primary cursor unless
**follow mode** is on. Zed was read as the substrate reference (Rust, a
multibuffer that also carries cursors), and the comparison is recorded in §10.

## 1. Why native

Heuristic #6 asks what dependency surface a separate owner would carve out.
Multi-cursor has none of its own. What it needs is:

- **the selection set**, which already lives in `lattice-protocol` and on every
  `Document`;
- **the grammar dispatcher**, which has to run one invocation at N positions
  inside one actor message and one undo group;
- **the renderers**, which have to draw N carets.

A plugin could own none of those. It would sit *outside* the dispatch loop, so
every keystroke would cross the WASM boundary once per cursor. That costs
paramount #1 and buys no isolation, because the plugin would have to be trusted
with every edit anyway. The one piece that *is* mode-shaped, the placement
chords, is carried by a native minor mode (§6), which is the ordinary shape for
a native feature with a keymap.

## 2. What already exists

The data layer was built for this from the start (§5.2, §2.2). Most of the
substrate is in place:

| Piece | Where | State |
|---|---|---|
| `SelectionSet`: non-empty, one primary | `lattice-protocol/src/selection.rs` | ✅ can hold N; nothing produces N |
| Every selection shifted across every edit | `lattice-core/src/document.rs` `transform_selections` | ✅ runs over `all()` |
| One undo step for many edits | `Document::apply_edit_batch`, `begin_undo_group` / `end_undo_group` | ✅ |
| The set in snapshots and render state | `DocumentSnapshot.selections`, `ActiveDocumentRenderState.selections` | ✅ published; no renderer reads it |
| The set across the plugin boundary | `wit/types.wit` `selection-set`, effect `selection-change` | ✅ write, and read via event only |
| Per-row operator execution | `lattice-grammar/src/dispatcher.rs` `execute_operator_blockwise` / `merge_blockwise_effects` | ✅ the precedent for broadcast (§4.3) |
| Chord fall-through | `Effect::Declined` | ✅ how placement chords share keys (§6) |

What does not exist yet:

- **The host collapses the set on every keystroke.** `write_through_caret`
  (`lattice-host/src/dispatch.rs`) writes `SelectionSet::single(...)` from
  `Editor.cursor` at the end of every dispatch. So a second selection could not
  survive one keypress today.
- **Dispatch takes one position.** `execute(…, cursor: Position, …)` and
  `DocumentHandle::dispatch*` are single-position. No command declares whether
  it applies per cursor.
- **Insert mode bypasses the grammar.** `do_insert_text` inserts at
  `self.cursor` directly.
- **Registers hold one string.** A blockwise yank is rows joined with `\n`.

## 3. The selection set is the state

### 3.1 Source of truth

The document's `SelectionSet` becomes authoritative. `Editor.cursor` stays, as
the head of the **primary** selection. That keeps its roughly 670 read sites
correct without touching them, and the change is confined to the write side:

- `write_through_caret` stops writing `single(...)`. It writes the primary
  through `replace_primary` and leaves the secondaries alone.
- Every `set_selections_blocking(SelectionSet::single(..))` site in the host is
  audited. Each one either means "move the primary", and becomes
  `replace_primary`, or means "collapse", and says so by calling
  `collapse_to_primary`.

With one selection in the set, both paths do exactly what they do today. That
is what lets the refactor land with no behaviour change (MC.2).

### 3.2 Invariants

`SelectionSet` gains the invariant Zed keeps in `SelectionsCollection`, and
which `lattice-protocol` does not have yet: **sorted by position, disjoint,
merged on overlap**.

`normalize()` runs after every change of the set's shape: after a broadcast, a
placement, or a text-object selection. It sorts the selections and merges any
that touch or overlap, including cursors that land on the same position (three
cursors, then `$` on one short line). The primary index follows the merged
selection that contains the old primary.

Without this, two cursors that converge would edit the same spot twice on
every later keystroke. That is the bug class the invariant exists to rule out.

### 3.3 Ownership: the pane that is active

Neovim's cursors belong to a window. Ours live on the document, because that is
what `transform_selections` shifts. The two are reconciled the way the single
cursor already is:

- The document's set is the **active pane's** set.
- `snapshot_active_pane` stashes the secondaries into `PaneState` beside
  `cursor`, and `load_active_pane` restores them, clamped to the buffer.
- Switching the pane's buffer clears them, as it already resets `cursor`.

A stashed set does not track edits made from another pane while it is stashed.
The stashed primary cursor has the same limitation today and is clamped the
same way.

## 4. Broadcast

### 4.1 Which commands broadcast

`CommandSpec` gains `broadcast: Broadcast`:

```rust
pub enum Broadcast {
	/// Runs once per selection. Edits, operators, inserts.
	EachCursor,
	/// Runs at the primary only, unless follow mode is on. Motions.
	Follow,
	/// Runs once, at the primary. Ex-commands, window / buffer /
	/// picker / LSP-request actions.
	Primary,
}
```

The default comes from `CommandKind`, so no existing registration has to
change:

| `CommandKind` | Default | Why |
|---|---|---|
| `Operator` | `EachCursor` | the reason multi-cursor exists |
| `Motion` | `Follow` | `Q` + move + `Q` is how cursors get placed (§6) |
| `TextObject` | `Follow` | outside an operator it is a Visual-extending motion |
| `ExCommand` | `Primary` | `:w` runs once; ranged forms take ranges, not cursors |
| `Action` | `Primary` | most actions are about the editor, not a position |

Host actions that edit at the cursor opt in to `EachCursor` one by one
(MC.7): paste, `J`, `o`/`O`, `r`, `~`, `<C-a>`/`<C-x>`. An operator-pending
motion always resolves per cursor, because it runs inside the operator's
per-cursor execution. Follow mode only governs a bare motion.

Plugins declare the same field in their command registration, with the same
per-kind default (MC.12). A plugin operator therefore broadcasts with no change
to the plugin.

### 4.2 Follow mode

Follow mode is **off in Normal and on in Visual**, as in Neovim.

That default is not taste. It is what makes `Q` usable. If bare motions moved
every cursor, `Q` (add a cursor here) followed by any motion would drag the new
cursor along, and the only ways left to place cursors would be `1Q` and `VGQ`.
Helix, Zed and VSCode broadcast every motion because they place cursors through
selection commands rather than by walking a caret. Lattice follows the
vim-native reference here, and the keys are Neovim's (§6).

`q=` toggles follow mode, and `1q=` / `2q=` set it on / off explicitly. The
flag lives on the multi-cursor minor mode's buffer-local state.

### 4.3 Execution: sequential, inside the actor

A broadcast invocation runs the ordinary `execute_with_env` **once per
selection, bottom-up, inside the document actor, inside one undo group**:

```
begin_undo_group
for sel in selections, last to first:
	effect_i = execute_with_env(registry, doc, id, sel.head, invocation, env)
	doc.selections[i] = head / extents that effect_i reports
end_undo_group
normalize(doc.selections)
merge(effect_0 .. effect_n)   // §4.4
```

This generalises `execute_operator_blockwise` from "the rows of a rectangle" to
"the selections in the set", with two differences:

- **The line count may change.** Blockwise assumes it does not (its comment
  says so). Broadcast does not need to assume it, because each edit already
  runs `transform_selections` over the whole set. After cursor *k*'s edit, the
  selections not yet processed have been shifted, so bottom-up order is an
  optimisation (edits below do not move positions above) rather than a
  correctness requirement.
- **No undo rewind.** Blockwise undoes its per-row edits and replays them as
  one collapsed replace. Broadcast wraps the loop in an undo group instead,
  because a collapsed replace across arbitrary cursors would span most of the
  file.

Everything runs in **one actor message**. The host sends the invocation once;
the actor reads the set it already owns. `DocumentHandle::dispatch*` keeps its
signature. When the set has one selection, broadcast is exactly today's call.

**The multibuffer** runs the same loop over the scratch `Document` it already
builds in `dispatch_composed`. It seeds the scratch with the view's set, runs
the broadcast, lands the scratch's edits once through `land_composed_edits`,
and adopts the scratch's final set (§5).

### 4.4 Merging the per-cursor effects

| Effect from each cursor | Merged result |
|---|---|
| `Edits` | concatenated, in application order |
| `CursorMove(p)` / `SelectionChange` | becomes that selection's new head / extents |
| `Yank` | one register write holding N chunks, in document order (§4.6) |
| `EnterMode` | must agree. A mismatch is a bug: logged at `debug!`, primary's wins |
| `Echo`, notices, anything else | the primary's only |
| `Declined` / an error at one cursor | that cursor is left as it was, and the others proceed. One `debug!` line per skipped cursor, never a panic. |

### 4.5 Insert mode

Typing does not go through the grammar (`do_insert_text`). It fans out in the
host: a typed character becomes **one `apply_edit_batch` inserting at every
cursor**, with the positions taken from one snapshot and applied in descending
order. This is the one place where computing every edit against one snapshot
(Zed's shape) is trivially right: the text is identical and the positions are
known.

- `<BS>`, `<CR>`, electric reindent and auto-wrap fan out the same way, each
  evaluated per cursor.
- A chord bound in Insert (auto-pair's `(`, a snippet key) is an `Invoke`, so it
  reaches §4.3 and broadcasts like any other command.
- The insert session's undo group already spans the whole session, so
  `ciwfoo<Esc>` at five cursors is one `u`.
- The completion popup, signature help and hover follow the **primary**.
  Accepting a completion applies the same replacement at every cursor whose
  word-before-cursor matches the primary's (MC.13). This is the convention in
  Zed and VSCode, and the one that makes "type a prefix, accept" work across
  cursors.

This is live fan-out, not block-insert's replay-on-`<Esc>`. The UX contract
says the typed character appears immediately. A replay would show it at one
cursor and the rest on `<Esc>`, which is a visible delay on content the user
edited.

### 4.6 Registers

A yank at N cursors writes **one register entry holding N chunks**, not N
separate registers:

```rust
pub struct UnnamedRegister {
	content: String,          // the chunks joined with '\n' -- what a
	                          // single-cursor paste and the clipboard see
	kind: YankKind,
	chunks: Option<Vec<String>>, // Some(n) when yanked at n cursors
}
```

On paste:

- **N cursors and N chunks:** chunk *i* goes to cursor *i* in document order.
  This is Neovim's "each cursor pastes its own yank".
- **Any other combination:** `content` is pasted at every cursor. This covers
  one cursor, a count mismatch after a merge, or a register written without
  cursors.

This gives Neovim's behaviour in the common case, with no per-cursor register
file to keep aligned as cursors merge and move, and the register stays one
ordinary value to the register store, the yank ring and the clipboard. It is Zed's
storage shape (`Vec<ClipboardSelection>`) with vim's paste semantics.

### 4.7 Dot-repeat and macros

Both are mechanical, because both already re-enter the dispatch path (§5.2):

- `.` re-dispatches `last_change` from each cursor, since `last_change`
  broadcasts on replay like any invocation. Its recorded insert text reaches
  §4.5's fan-out.
- A macro records `Action`s. Replayed under N cursors, each action broadcasts
  as it would if typed. The macro runs once and its actions broadcast; it does
  not run N times independently. The two differ only for a macro that moves
  with bare motions, and follow mode is the user's control for that.

No cursor state is recorded in either. Replay applies to whatever cursors exist
at replay time, which is what makes "record at one cursor, place five, `@q`"
work.

### 4.8 Visual mode

Each `Selection` already carries its own `anchor` and `visual`. Visual mode
under N cursors means N anchored selections:

- Entering Visual anchors every selection at its head.
- Follow mode is on, so motions extend every selection.
- A Visual operator broadcasts with `Range::Selection` bound to each
  selection's own extents.

`Editor.visual_anchor` stays as the primary's anchor, for the same reason
`Editor.cursor` does (§3.1). `visual_selection_range` / `visual_block_extents`,
which read `primary()`, get `all()` peers for the renderers.

Blockwise Visual under multiple cursors is **out of scope**. A rectangle is
already a per-row multi-selection, so N rectangles is a composition with no
user demand behind it. `<C-v>` collapses to the primary first.

## 5. Multibuffer

A multibuffer view is a regular buffer (`multibuffer_is_a_regular_buffer.rs`),
so multi-cursor must work there with no kind-branch. The substrate already
provides most of it:

- **Selections:** the view owns them (`MultibufferState.selections`) in
  composed coordinates.
- **Dispatch:** grammar dispatch runs against a scratch `Document` built from
  the composed snapshot.
- **Edits reach the sources:** operator edits land once, through
  `land_composed_edits` → `apply_edit_batch_sync`, which forwards each edit to
  its source.
- **Undo across files:** the composed document's own undo is the transaction
  record, replayed onto the sources (MU.1). This does the job of Zed's
  per-buffer transaction map (`multi_buffer/src/transaction.rs`).

So §4.3's loop runs unchanged on the scratch, and the view adopts its final set.

Two defects came up while designing this. Both predate multi-cursor, which
makes them likelier to be hit, so the slice plan fixes them first,
**reproduce-first** (MC.11a/b):

1. **A cross-excerpt edit only partly reaches its sources.**
   `build_source_edit` resolves the excerpt from the edit's *start* row and
   clips the end to that excerpt. The composed text takes the whole edit, and
   the part in the second excerpt never reaches its file. This contradicts
   [`multibuffer-views.md`](multibuffer-views.md) §"Multi-excerpt selections",
   which describes per-excerpt splitting. The fix is that splitting: each
   excerpt's portion goes to its own source. Zed's
   `convert_edits_to_buffer_edits` is the reference: the start excerpt takes
   the replacement text and the others take deletions.
2. **An outside edit may leave `composed_doc` stale.** This is from reading the
   code and is unverified at runtime. `recompose_inner` rebuilds the snapshot
   and row translation but not `inner.composed_doc`, and it carries
   `state.selections` over untransformed. If it reproduces, the recompose
   writes the new text into `composed_doc` and maps the selections through the
   row translation.

## 6. Surface

The chords are Neovim 0.13's, except where our builtin grammar already uses the
key.

| Chord | Action | Notes |
|---|---|---|
| `Q` | toggle a cursor at the primary's position | Normal. Unbound today. |
| `1Q` | a cursor at every match of the last search | reads the last search, as `n` does |
| `Q` in Visual | a cursor on each line of the selection | Neovim's `VGQ` |
| `gQ` | restore the set last cleared | unbound today |
| `<C-l>` | clear secondary cursors | **declines** with one cursor → builtin `redraw_screen` |
| `]C` / `[C` | make the next / previous cursor primary | **declines** with one cursor → builtin class-end motions |
| `q=` / `1q=` / `2q=` | toggle / on / off follow mode | an exact `q=` beats the `q<char>` macro wildcard, like `q:` |

**The chords that clash are shared through `Effect::Declined`, not taken
over.** With one cursor, `<C-l>` still redraws and `]C` still jumps to a
class's end, because the multi-cursor handler declines and dispatch
re-resolves one layer down. With several cursors they act on the cursors. That
is the trade-off to name: while cursors are live, `]C` cannot be broadcast as
the class-end motion. The chord was Neovim's first, and vim users who reach for
it are asking about cursors.

**Ownership.** The chords and their handler bodies belong to
**`multi-cursor-mode`**, a minor mode in `lattice-mode/src/modes/`, beside
`surround`. It is in `default_modes`, active in every buffer, and bound at
`KeymapLayer::MinorMode`. Every handler is a pure function from the context to
`Effect::SelectionChange(set)` (or `Declined`). Adding, toggling, restoring,
rotating the primary and clearing are all set arithmetic the mode does itself,
and none of it is an `Editor::` method.

The host owns only the substrate: honouring a multi-selection `SelectionChange`,
running broadcast (§4), and fanning out Insert (§4.5). Those are the generic
dispatch loop, which every buffer kind uses, so this is not a half-migration
under the mode-ownership rule.

Every action carries docs, so `:describe-key Q` and which-key work without
further effort. No ex-commands are added in the first cut; if they are, they
take the dashed namespaced form (`multi-cursor-clear`).

## 7. Rendering

The renderers read `render_state.selections` (already published) instead of
the primary alone. The TUI and GPUI change in the same slice.

- **TUI:** the primary stays the hardware cursor. Each secondary is a
  reverse-video cell styled by a new theme key `cursor.secondary`, applied in
  `compose_pane_lines` the way the search-match overlay is. Visual extents are
  painted per selection.
- **GPUI:** `EditorElement.cursor: Option<CursorState>` becomes the primary plus
  `secondary_cursors: Vec<CursorState>`, painted with the same quad path and the
  secondary colour. Visual quads are built per selection.

Only the viewport's selections are converted, so the cost is O(visible cursors),
not O(cursors). Cursors off-screen are counted in the modeline
(`3/12 cursors`), which the multi-cursor mode contributes as a status-line item.

**UX contract.** "Only the edited line may visibly change" is about lines the
user did not edit. With N cursors the user edits N lines on purpose, and those N
lines change. Nothing else may.

## 8. Performance

- **Placement is free.** Set arithmetic in a mode handler: no text is touched.
- **A broadcast costs one actor message, N grammar executions, and O(N) selection
  transforms per edit.** That is O(N²) transforms per broadcast. At `1Q` over a
  large file (N ≈ 1000) it is about 10⁶ position comparisons, a few
  milliseconds. The benches (MC.5) either show that fits a frame, or
  `transform_selections` gets a batched form that shifts the whole set once per
  broadcast. Which one is decided by measurement, not ahead of it.
- **Insert fan-out is one batch.** One actor message, one rope transaction, one
  snapshot publish, however many cursors there are.
- **The UI thread does nothing new beyond drawing visible carets.**

New benches:

- `dispatch_publish/multi_cursor_{1,10,100,1000}`, for a broadcast `dw`.
- A 10-cursor case in `keystroke_glyph_ratchet.rs`, so CI fails if typing
  under cursors regresses.

The single-cursor numbers must not move. With one selection, the broadcast loop
is exactly today's call.

## 9. Paramount-goal alignment

- **UX:** live fan-out keeps "typed character appears immediately" at every
  cursor. Declining clashing chords keeps `<C-l>` and `]C` working for anyone
  not using cursors.
- **#1 Performance:** one actor message per keystroke, whatever N is. No UI-thread
  work beyond visible carets. Benches (§8) and the single-cursor ratchet guard
  it.
- **#2 Extensibility:** plugin operators, motions and Insert chords broadcast
  with no plugin change (§4.1). Plugins can write the set (`selection-change`).
  A synchronous full-set read is not added (MC.12 notes it).
- **#3 Vim grammar:** the grammar is untouched. Broadcast calls the same
  `execute_with_env` N times, and every existing motion, operator and text
  object gains multi-cursor behaviour by construction.
- **#4 Async:** the loop runs in the document actor, which already owns the text
  and the set. Nothing new crosses a thread boundary per cursor.

## 10. Rejected alternatives

- **A plugin** (§5.5's former listing). Rejected in §1: the plugin sits outside
  the dispatch loop, and one boundary crossing per cursor per keystroke costs
  #1 for no isolation gain.
- **Snapshot transaction** (Zed, Helix): compute every cursor's edit against one
  snapshot and apply them as one sorted batch. Zed can do this because its vim
  layer is written as pure selection transforms
  (`motion.expand_selection(map, selection)`), with no third-party motions.
  Lattice operators are `Fn(&mut OperatorContext) -> Effect` that edit the
  document directly, and plugins register them. Moving to "report ranges, don't
  apply" would change the contract for every built-in and plugin operator,
  which costs #2 and #3, to win a constant factor the sequential loop may never
  need. It is kept for Insert fan-out only (§4.5), where it costs nothing.
- **Zed-style CRDT anchors** instead of positions. Anchors survive any edit
  with no transform pass, which is the O(N²) of §8 removed. But it is a
  rewrite of `lattice-core`'s position model, justified only if §8's benches
  show the transform cost. That is heuristic #1's "genuinely better on merit"
  test, applied to a measurement we do not have yet.
- **A register per cursor** (Neovim's literal model). N registers per register
  name must stay aligned with a set that merges and reorders, which is state
  whose lifetime matches the set and does not belong in the register store.
  §4.6's N-chunks-in-one-entry gives the same result whenever the counts match.
- **Follow mode on by default** (Helix, Zed, VSCode). This makes `Q` useless
  (§4.2).
- **`<Esc>` clears cursors** (VSCode, vim-visual-multi). In vim, `<Esc>` leaves
  Insert, and clearing on it would drop the cursors the user was about to
  `.`-repeat at. Neovim uses `<C-l>`.
- **A selection id** (Zed's `Selection.id`). Nothing needs it. Register chunks
  map by document order, and merges re-derive the primary. It would widen the
  WIT `selection` record for no consumer.
