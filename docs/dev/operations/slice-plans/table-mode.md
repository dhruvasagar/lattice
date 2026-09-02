# Slice plan — `table-mode`

Design: [`table-mode.md`](../../architecture/table-mode.md).

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred.

| Slice | Description | Status |
|---|---|---|
| TB.0 | `<leader>` expands on the native mode-keymap path | ✅ |
| TB.1 | `table-mode`: the shared minor, at org's parity | ✅ |
| TB.2 | The org plugin sheds the generic surface **(plugin)** | ✅ |
| TB.3 | Richness beyond parity — emacs' org-table surface | 📝 |

TB.0 is a prerequisite, not a courtesy: `table-mode` inherits org's
`<leader>t…` set and would have shipped it dead.

TB.1 and TB.2 are deliberately two commits in two repositories, in that order.
Landing the plugin's removal first would take the feature away from org users
until the native mode arrived; landing it after means both bind `<Tab>` for
the length of one commit, which is a transient the plugin commit resolves.

---

### TB.0 — `<leader>` on the native mode-keymap path ✅

OM.2b made `<leader>` expand at bind time, at the single choke point every
*string-bound* binding funnels through — `try_bind_chord_string`, which is the
plugin-mode, plugin `register-binding` and `init.rs` route.

The **native** mode-keymap route does not go through it. It parsed
`entry.chord` raw, so `<leader>` reached `parse_chord_sequence` as an unknown
special key, the parse failed, and the binding was dropped with a `warn`.

No native mode had used `<leader>` yet, which is the only reason this had
never fired. Found before writing `table-mode` rather than after, which is why
it is a one-line fix instead of a debugging session. The test asserts the trie
holds `<Space>z` — the substituted default — rather than merely that something
is bound: a literal `<leader>` sitting in the trie is a binding nobody can
type, and would pass a weaker assertion.

### TB.1 — `table-mode` ✅

The shared minor, at parity with what `org-table-mode` carried: `<Tab>` /
`<S-Tab>` cell walk, align, row up/down, column left/right, insert and delete
row and column. Design §1–§7 carries the reasoning; what follows is what the
slice actually did.

**Three modules, one engine.** `layout.rs` (HP.1, already here) keeps the
unattended whole-document pass `lattice-help` uses. `model.rs` adds the
interactive view — table-at-caret, dialect-preserving render — and `edit.rs`
the structural operations, both built on `layout`'s own `cells` /
`visible_width` / `render_row` rather than a second copy. `mode.rs` is the
mode, its keymap and its bodies.

**One behaviour change to the existing engine.** An explicit `:---` left
marker used to be dropped on render, since left is the default and the output
is identical. Fine for generated help pages, wrong the moment the same engine
realigns the user's own file: a `:` they typed vanishing from a git diff is a
change they did not make. `Align::LeftMarked` round-trips it; the alignment it
means is unchanged, and the help-page pass gets the fidelity for free.

**The layering is the tested half.** `crates/lattice-host/tests/table_mode_layering.rs`
pins that `<Tab>` declines outside a table (so org's cycle and the builtin
jump still get it) and that every `<leader>t…` chord returns `Effect::None`
instead — because a decline re-runs a multi-key chord's trailing key alone,
and `<leader>tdc` declining would fire `c` and sit waiting for a motion. The
same file asserts each chord's *target* rather than merely that something is
bound: `translate_mode_keymaps` drops an entry whose command name does not
resolve, so a typo between keymap and registration is a silent keymap.

### TB.2 — the org plugin sheds the generic surface ✅

`src/table.rs` and the eleven generic chords + actions are gone from the org
plugin. `org-table-mode` stays registered with an **empty keymap** as org's
table home (design §5) — until `#+TBLFM:` lands it is a declared placeholder,
which is worth saying plainly rather than papering over. Deleting it and
re-adding it then would rename an entry in the plugin's mode list to buy
nothing; keeping it is what makes the intent of the split legible.

The callback ids 21..=31 are left as a **gap** rather than reused: a callback
id is a wire value between the guest's registration and the host's dispatch,
and shifting the ones above it to close a hole would silently re-point every
action after it.

**The plugin's suite found two real bugs in TB.1**, which is the argument for
doing the two slices in this order rather than merging them:

1. `rewrite` computed the replaced range's end line from the RENDERED result.
   Align and the two swaps do not change the row count, so old span and new
   agreed and it looked correct — then insert addressed a line past the
   buffer's end and did nothing, and delete left its last row behind. Every
   unit test asserted the rendered *text* and none the range, which is exactly
   why they all passed. `the_replaced_range_is_the_old_table_not_the_new_one`
   now asserts it in both directions.
2. A `| a | b |` line inside `#+BEGIN_SRC` was taken for a table. OT.7 found
   this in the plugin and answered it with the tree; that answer does not
   survive the move, because `lattice-mode` has no tree-sitter dependency and
   should not grow one to ask about `#+BEGIN_SRC`. Counting delimiters from
   the top of the file is the same answer in `O(lines)` of `starts_with` —
   once per chord, not per frame.

`tab_off_a_table_falls_through_to_the_headline_cycle` passes unmodified, which
is the assertion that the decline chain survived having one of its links move
to another crate.

### TB.3 — beyond parity 📝

What emacs' org-table has that this does not yet:

- Cell-level operations: blank, copy down, transpose.
- `<C-c>-` insert a rule; `<C-c>^` sort rows by the column at the caret.
- `<Tab>` at the last cell of the last row **creates** a row (emacs does; the
  model deliberately returns `None` there today and leaves the decision to the
  caller, see `Table::next_cell`).
- Realign as you type, rather than only on a chord. This one needs its own
  performance argument before it is designed — it is an edit-time hook on
  every keystroke inside a table, and paramount goal #1 is the constraint, not
  a footnote.
- Formulas (`#+TBLFM:`) — org's, so `org-table-mode`'s, not this mode's.
