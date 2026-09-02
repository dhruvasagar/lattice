# Slice plan — `table-mode`

Design: [`table-mode.md`](../../architecture/table-mode.md).

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred.

| Slice | Description | Status |
|---|---|---|
| TB.0 | `<leader>` expands on the native mode-keymap path | ✅ |
| TB.1 | `table-mode`: the shared minor, at org's parity | ✅ |
| TB.2 | The org plugin sheds the generic surface **(plugin)** | 📝 |
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

### TB.2 — the org plugin sheds the generic surface 📝

Delete `src/table.rs`'s engine and the eleven generic chords + actions from
`org-table-mode`. The mode stays registered as org's table home (design §5).

Its surface is empty the moment this lands, and that is worth saying plainly
rather than papering over: until TB.3 or a formula slice, `org-table-mode` is
a declared placeholder. The alternative — deleting it and re-adding it when
`#+TBLFM:` arrives — trades a documented empty mode for a rename in the
plugin's mode list, and keeping it is what makes the *intent* of the split
legible to whoever picks up formulas.

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
