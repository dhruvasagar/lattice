# Org structure editing — headlines, lists and checkboxes — slice plan

> **Status: Active.** Opened 2026-09-06. Implements
> [`org-mode.md`](../../architecture/org-mode.md) §5.6.

Design owns *what* and *why*; this file owns *when* and *in what order*.

Spans two repos. Slices marked **(plugin)** land in
`~/src/dhruvasagar/lattice-org-plugin`; **(host)** ones in `lattice`.

## Status

| Slice | Title | Status |
|---|---|---|
| OS.0 | An Insert-mode plugin chord reaches a grammar action — pin it **(host)** | 📝 |
| OS.1 | The keyboard protocol, so Shift+Enter exists at all **(host)** | 📝 |
| OS.2 | A Visual-mode plugin action can see its region **(host)** | 📝 |
| OS.3 | `Lists` — the model, and `Checkboxes` rebuilt on it **(plugin)** | 📝 |
| OS.4 | `<M-CR>` — meta-return dispatches on what is at point **(plugin)** | 📝 |
| OS.5 | `<M-S-CR>` — the variant, and the headline insert family **(plugin)** | 📝 |
| OS.6 | The Meta-arrows: promote/demote *is* indent/outdent **(plugin)** | 📝 |
| OS.7 | `<M-Up>` / `<M-Down>` — move an item or a subtree **(plugin)** | 📝 |
| OS.8 | `<C-t>` / `<C-d>` in Insert, declining off a list **(plugin)** | 📝 |
| OS.9 | Bullet cycling, and line ↔ item ↔ headline **(plugin)** | 📝 |
| OS.10 | The Visual peers **(plugin)** | 📝 |
| OS.11 | `:help org` — the Lists section, and the site **(plugin + host)** | 📝 |

## Dependencies

**OS.3 is the gate for everything in the plugin.** Nothing below it can be
written against a list the plugin cannot see.

- **OS.0 blocks OS.4 and OS.8** — both bind in Insert, and if an Insert-mode
  plugin chord does not reach a grammar action, both are dead bindings that
  test green through direct dispatch. It is a test slice that may turn into a
  fix; find out before writing eight of them.
- **OS.2 blocks OS.10** and nothing else.
- **OS.1 blocks nothing.** `<M-S-CR>` is unreachable in the TUI without it, but
  reachable in GPUI and through the `<leader>o…` peer either way, so OS.5 lands
  green with or without it. Sequenced early because it is small and because
  testing OS.5 by keypress in a terminal wants it.
- **OS.4 → OS.5.** The variant is an arm on the table OS.4 builds.
- **OS.6, OS.7, OS.8, OS.9 are independent of each other** — four disjoint verb
  groups over one model. Order them by appetite.
- **OS.11 last**, because it documents what actually landed.

The three host slices are independent of one another and can land in any
order, or in parallel with OS.3.

---

## OS.0 — An Insert-mode plugin chord reaches a grammar action **(host)** 📝

Design: [`org-mode.md`](../../architecture/org-mode.md) §5.6.2.

**A test slice, deliberately, and it may become a fix.**

`keymap_insert.rs:617`'s `action_from_bound` builds `Action::Invoke(inv)`,
which resolves through the unified dispatcher against the grammar registry —
where a plugin's `register_action` entries live. So it *should* work. But
`plugin-actions-need-a-dispatch-fallback` records this exact class biting
twice already (`ActionHandlerRegistry` lookups missing plugin grammar actions;
prompt submits and transient rows silently doing nothing), and the symptom is
a chord that does nothing — indistinguishable from one that is unbound.

Two facts to pin, both against a real plugin fixture, both by **pressing the
chord** rather than dispatching by name:

1. A plugin `ModeKeymapBinding` with `binding_mode: Insert` fires its guest
   action when the chord is typed in Insert mode, and only in buffers where
   the mode is active (`lookup_with_context` scoping).
2. `Effect::Declined` from that action falls through **one layer** to the
   binding underneath — the `<C-t>` / `<C-d>` story depends on it, and
   decline-in-Insert has no existing coverage. `decline-only-shared-chords`
   is the rule being verified, not assumed.

If either fails, this slice is the fix and OS.4 / OS.8 wait on it. If both
pass, the tests stay: they are the regression guard for two behaviours the
plugin surface is about to depend on heavily.

## OS.1 — The keyboard protocol, so Shift+Enter exists at all **(host)** 📝

Design: [`org-mode.md`](../../architecture/org-mode.md) §5.6.1.

`runtime.rs:141-152` sets up raw mode, the alternate screen, bracketed paste
and mouse capture, and never pushes `KeyboardEnhancementFlags`. Without them a
terminal cannot express Shift+Enter or Ctrl+Enter — it sends a bare `\r`, so
`<S-CR>`, `<C-CR>` and `<M-S-CR>` are unreachable for **every** consumer, not
only org.

Push `DISAMBIGUATE_ESCAPE_CODES` — and only that flag.
`REPORT_ALL_KEYS_AS_ESCAPE_CODES` would route ordinary text input through the
escape path and is not wanted.

Three guards, because the failure mode is a terminal the user cannot type out
of:

- **Probe first.** `crossterm::terminal::supports_keyboard_enhancement()`
  gates the push. Crossterm 0.28 has it; no version bump.
- **An opt-out that does not need a rebuild.** `ui.keyboard-enhancement`
  (`auto` / `off`), because a terminal that answers the probe wrongly must be
  recoverable by editing config, not source.
- **Pop on teardown, including on panic.** The alternate-screen restore is
  already where it is for this reason; the flags follow it, on the same path.

Tests: the flag is pushed when the probe says yes and not when it says no; the
teardown pops what it pushed; `chord::from_event` still produces the same
`KeyChord` for the existing bindings under disambiguation (the regression
risk — a protocol change alters how Esc and the C0 controls arrive).

**Renderer parity: none needed.** This is a TUI-side terminal-setup change;
GPUI has always delivered these chords. That is the asymmetry being closed.

## OS.2 — A Visual-mode plugin action can see its region **(host)** 📝

Design: [`org-mode.md`](../../architecture/org-mode.md) §5.6.5.

`lattice-mode`'s `ActionContext` has carried `selection: Option<Range>` since
MG.18e; `lattice-grammar`'s — the one a *plugin* action arrives through —
never gained it, so the WIT mirror has nothing to copy and a Visual-mode plugin
action sees strictly less than the same action reached natively. That is
verbatim the reason OC.10 gave for adding `cursor` / `buffer-id` to
`ex-command-context`.

Four edits:

1. `lattice-grammar/src/registry.rs` — `selection: Option<ProtoRange>` on
   `ActionContext`.
2. `lattice-grammar/src/dispatcher.rs` — populate it on the action path from
   the existing `Range::Selection` resolver (`dispatcher.rs:756`), which
   already handles linewise / charwise / blockwise. `None` in Normal mode and
   on every non-chord firing path, matching the `lattice-mode` field's
   documented contract.
3. `wit/types.wit` — `selection: option<range>` on `action-context`.
4. `crates/lattice-plugin-host/src/boundary_grammar.rs:166` —
   `project_action_context` mirrors it.

Tests: a plugin action fired from Visual receives the resolved region;
linewise and charwise both arrive with the extents the native resolver
produces (charwise inclusive of the head, per vim); Normal-mode firing gets
`None`; a prompt-submit firing gets `None`.

**Explicitly not `apply-operator`.** Giving the operator seam a `document` is
the larger fix and the better long-term shape — it is what would make
text-transforming plugin operators possible at all — and it is recorded in
§5.6.5 as a known gap. No verb in this plan needs it, and putting a WIT change
plus its host wiring in front of every slice below is not a trade worth
making.

## OS.3 — `Lists` — the model, and `Checkboxes` rebuilt on it **(plugin)** 📝

Design: [`org-mode.md`](../../architecture/org-mode.md) §5.6.3.

The gate for the whole plugin half. A new `list.rs`, shaped like
`headline.rs`'s `Headlines` and `checkbox.rs`'s `Checkboxes`: tree-first over
the grammar's `list` / `listitem` / `bullet` nodes, indent-based fallback when
the buffer has no parse.

```
Bullet   ::= Dash | Plus | Star | Ordered { n, Dot | Paren }
Item     { line, indent, bullet, checkbox: Option<Check>, content_byte }
Lists    { item_at, enclosing_item, item_end, siblings, children, list_span }
renumber(list_span) -> Vec<(line, String)>
```

`item_end` spans the item's continuation lines **and** its nested children —
the unit a move or an indent has to carry.

**`Checkboxes` is rebuilt on top rather than left beside it.** Its `item_at`
becomes `Lists::item_at()` filtered to items carrying a box; `strip_bullet`
and the bullet-shape knowledge move into `list.rs` and stop being private to
checkboxes. The tally, cookie and `Parent` logic above it does not move.

Two independent list walkers would agree the day they were written and
diverge on the first grammar bump, surfacing as a cookie that quietly stops
updating rather than as a failing test — the silent duplication
`prefer-minor-modes-over-duplication` names.

**No new chords in this slice.** It lands green when `list.rs`'s own tests
pass *and* every existing checkbox test in `tests/org_structure.rs` still
does, unchanged. If any existing test needs editing to accommodate the
rewrite, that is a behaviour change and wants saying out loud, not absorbing.

Tests: each bullet shape parsed, including `*` at column 0 being a headline
and not a bullet (the existing rule, carried over); nesting by indent with and
without a tree; `item_end` over continuation lines and nested children;
`renumber` over an ordered list with gaps and with mixed delimiters.

## OS.4 — `<M-CR>` — meta-return dispatches on what is at point **(plugin)** 📝

Design: [`org-mode.md`](../../architecture/org-mode.md) §5.6.4.
Needs OS.0, OS.3.

`org-meta-return` stops being headline-only and becomes an arm table, the
`ctrl_c_ctrl_c` (OE.3) shape:

| At point | Inserts |
|---|---|
| checkbox item | a checkbox item at the same indent |
| plain list item | a plain item at the same indent |
| headline | a sibling, after the subtree (existing behaviour, unchanged) |
| table row | nothing — `Effect::Declined`, `table-mode` owns it |
| preamble / prose | `Effect::None` (existing, deliberate) |

**The headline arm calls the body it already calls.** No copy of
`meta_return`'s current logic; the arm dispatches to it.

Bound in **Insert and Normal**: `<M-CR>` in both, `<leader><CR>` kept in
Normal as the existing spelling.

Ordered lists renumber in the same `Effect::ApplyEdit` as the insert, so one
`u` restores the list whole.

Tests: each arm, by **pressing the chord** in both binding modes — not by
dispatching the action by name, which passes on the broken version too. The
table arm's decline reaching `table-mode`. The preamble refusal.

## OS.5 — `<M-S-CR>` — the variant, and the headline insert family **(plugin)** 📝

Design: [`org-mode.md`](../../architecture/org-mode.md) §5.6.4.
Needs OS.4. Reads better after OS.1 but does not need it.

The shift arm of the same table — "the other kind":

| At point | `<M-S-CR>` inserts |
|---|---|
| checkbox item | a **plain** item |
| plain list item | a **checkbox** item |
| headline | a sibling carrying the first TODO keyword |

Plus `org-insert-subheading` — a child rather than a sibling — which has no
modifier chord (emacs reaches it through `C-c C-x C-w`'s neighbourhood, not a
gesture) and gets `<leader>oi` alone. That letter was left deliberately free
at OA.27 and this is the insert group it was left free for.

Tests: each arm by keypress; the TODO arm picks the *first* keyword of the
configured sequence and not a hardcoded `TODO`; subheading nests under a
headline with existing children without adopting them.

## OS.6 — The Meta-arrows: promote/demote *is* indent/outdent **(plugin)** 📝

Design: [`org-mode.md`](../../architecture/org-mode.md) §5.6.4, §5.6.6.
Needs OS.3.

`org-meta-left` / `org-meta-right` and `org-shift-meta-left` /
`org-shift-meta-right`, bound to `<M-Left>` / `<M-Right>` / `<M-S-Left>` /
`<M-S-Right>` in **Normal only**.

Gesture-named because they dispatch: on a headline they promote / demote, on a
list item they outdent / indent. The shift pair carries the subtree or the
sub-items.

**Each arm calls the body the dedicated chord calls** — the headline arms
invoke exactly what `<leader>oh` / `ol` / `oH` / `oL` invoke, which stay bound
and unchanged.

Refusals per §5.6.6: a level-1 subtree does not promote; an outdent at column
zero is refused rather than silently becoming a headline.

Tests: both arms of all four chords; the two refusals, each asserting the
echo rather than only the absence of an edit; ordered-list renumbering after an
indent that changes a sub-list's membership.

## OS.7 — `<M-Up>` / `<M-Down>` — move an item or a subtree **(plugin)** 📝

Design: [`org-mode.md`](../../architecture/org-mode.md) §5.6.4, §5.6.6.
Needs OS.3.

`org-meta-up` / `org-meta-down` on `<M-Up>` / `<M-Down>`, Normal only, with
`<M-S-Up>` / `<M-S-Down>` as emacs' subtree-explicit peers on the same
ActionIds.

The headline arm is the existing `move_subtree` body; the list arm is its
analogue over `Lists::siblings`, carrying the item's continuation lines and
nested children as one unit.

**A move stops at its parent** — swaps with the previous or next *sibling*,
does nothing at either end of the chain rather than splicing the item into a
neighbouring list.

`<leader>oK` / `oJ` stay bound and unchanged.

Tests: item move up and down among siblings, with nested children carried; the
two end-of-chain refusals; a move across a nested sub-list that must be
skipped rather than entered; renumbering after a move in an ordered list.

## OS.8 — `<C-t>` / `<C-d>` in Insert, declining off a list **(plugin)** 📝

Design: [`org-mode.md`](../../architecture/org-mode.md) §5.6.2.
Needs OS.0, OS.3. Reuses OS.6's bodies.

The Insert-mode spelling of indent / outdent, on the ActionIds OS.6 built.

**These decline, and it is not optional.** `<C-t>` / `<C-d>` are Builtin
Insert bindings (`keymap_entry.rs:473-474`) — genuinely shared chords with a
real meaning underneath — so off a list item they must return
`Effect::Declined` and let vim's shiftwidth indent run. That is the `<C-a>` /
`<C-x>` argument (§OM.9) in Insert mode, and OS.0 is what proves the
fall-through actually happens there.

Tests: indent and outdent a list item mid-typing, by keypress, asserting the
caret stays where the typist left it; **`<C-t>` on a prose line inside an org
buffer still performs vim's shiftwidth indent**, asserted through a keypress
rather than by observing that org's action returned `Declined`.

## OS.9 — Bullet cycling, and line ↔ item ↔ headline **(plugin)** 📝

Design: [`org-mode.md`](../../architecture/org-mode.md) §5.6.
Needs OS.3.

Three verbs, Normal mode:

| Chord | Action | Does |
|---|---|---|
| `<leader>o-` `<C-c>-` | `org-cycle-list-bullet` | `-` → `+` → `1.` → `1)` → `-` across the whole list at point |
| `<leader>o_` | `org-toggle-item` | the line at point becomes a list item, or stops being one |
| `<leader>o*` `<C-c>*` | `org-toggle-heading` | **extended**: a list item becomes a headline at the enclosing level |

Cycling acts on **the whole list**, not one item — a list with mixed bullets
is not a thing org produces, and cycling one item would create one. That is
also emacs' no-region behaviour for `C-c -`.

`<C-c>-` and `<C-c>*` are safe because `<C-c>` is only ever a prefix here;
`<C-c><C-c>` must remain the sole terminal binding beneath it, or every longer
chord under `<C-c>` dies (`a-bound-prefix-kills-its-longer-chords`).

Tests: cycle through all four bullet shapes and wrap; ordered forms renumber
from 1 on entry; `org-toggle-item` round-trips a prose line and preserves
indentation; a checkbox item toggled to prose loses its box and its parent's
cookie updates in the same edit; `<leader>o*` on a list item produces a
headline at the enclosing level, not level 1.

## OS.10 — The Visual peers **(plugin)** 📝

Design: [`org-mode.md`](../../architecture/org-mode.md) §5.6.5.
Needs OS.2 and the verb slices whose actions it binds.

The same ActionIds bound in `BindingMode::Visual`, each reading
`ctx.selection` and applying its verb to every item the region touches:
`<M-Left>` / `<M-Right>` / `<M-S-*>` / `<M-Up>` / `<M-Down>`, `<leader>o-`,
`<leader>o_`, `<leader>o*`, `<C-Space>`.

One edit per invocation, not one per item — a partially-indented region is a
worse state to be left in than either end, and `u` has to take the whole thing
back.

**A mixed-level region shifts every item by one, preserving relative
structure** — it does not flatten the region to a common level. Indenting a
selection that contains a parent and its child must leave the child a child.
And the edit is **all-or-nothing**: if *any* item in the region would hit a
refusal (a level-1 promote, an outdent at column zero), the whole invocation
refuses and says which item stopped it. A region verb that silently applied to
the two-thirds of a selection that could move is the class of surprise §5.6.6
exists to prevent.

Tests: a region spanning several items at one level; a mixed-level region
preserving relative structure; a mixed-level region containing one item that
must refuse, refusing whole and naming it; a region spanning a headline and a
list; a region with a `None` selection (Normal-mode firing) falling back to
the point-scoped behaviour rather than erroring.

## OS.11 — `:help org` — the Lists section, and the site **(plugin + host)** 📝

Three artefacts, one slice, because they describe one surface:

- **(plugin)** `doc/org.md` — a new **Lists** section covering the bullet
  shapes, the four verb groups, the refusals and the ordered-list renumbering
  rule; the existing **Structure** table extended with the new chords; the
  **Checkboxes** section extended with the insert and kind-toggle verbs. This
  is what `:h org` renders.
- **(host)** `docs/user/org.md` — the "What you get" table's **Outline** row
  extended to name list and checkbox structure editing.
- **(host)** site: `docs/user/org.md` is already in `site/data/nav.toml`
  (section `config`, doc `org`), so no nav change — but the sync must run and
  search must pick the new text up (`docs-land-on-the-zola-site-too`).

Write it against what landed, not against this plan. Any slice that shipped
differently gets documented as it shipped, and this plan's status table gets
corrected rather than the doc bent to match it.
