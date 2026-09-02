# `table-mode` — pipe tables, once

Sequencing and status: [`slice-plans/table-mode.md`](../operations/slice-plans/table-mode.md).

Pipe tables belong to markdown and org alike. `| a | b |` means the same thing
in both, and so does every operation over it: line the columns up, walk to the
next cell, move a row, insert a column. By the standing rule that shared
behaviour is a minor mode and never a copied keymap, that surface belongs in
**one** mode spanning both majors.

This fragment records where that mode lives, why, and what stays with org.

## 1. Why the host owns it

Two things decide it, and the second is the load-bearing one.

**The host was already the owner in fact.** `lattice-mode/src/modes/table/`
has existed since HP.1, and its module doc named the consumer it was waiting
for:

> It lives here rather than in `lattice-help` because the next consumer is a
> `table-mode` in this directory: an org-table-style minor mode that realigns
> as you type, moves between cells, and inserts and deletes rows and columns.

`layout.rs` beside it parses pipe tables, honours `:` alignment markers, keeps
escaped `\|` inside its cell, skips fenced blocks, and measures cells by
**display width**. The org plugin grew a second engine over the same syntax
whose own doc conceded that `chars().count()` is "honestly wrong for CJK".
Two implementations existed; this is not a new home, it is the end of the
duplication.

**Only the host can serve both majors.** `markdown-mode` is native. A table
mode living in the org plugin would make markdown table editing require the
org plugin installed *and* enabled — and absent it, nothing would announce the
gap, because a chord nobody bound simply does nothing. UX is the higher court
and that loses on it.

The cost is stated rather than hidden: this is behaviour that *could* have
been a guest concern, made native, and paramount goal #2 gives up a little for
it. The precedent that settles it is `refreshable-view-mode` and
`foldable-view-mode` — shared minors in the host for exactly this reason, and
each replacing a set of copied keymaps that had already drifted.

## 2. The dialect is read off the table, not declared

Org writes `|---+---|`. Markdown writes `|---|---|` and may carry `:` markers.
Both are tables, and an align that rewrote one into the other would edit a
file's dialect because someone asked to line up columns.

So `Row::Separator` carries the join character and the alignment markers it
was **found with**, and re-rendering reproduces them.

There is deliberately **no `table.dialect` option** and no seam for a major to
declare one. An option is a second source for a fact already in the buffer,
and two sources can disagree: a `+`-joined table pasted into a markdown file
would be rewritten by a correct-looking option. Reading the file cannot be
wrong about the file.

The one case the file cannot answer is creating a rule where none exists — and
no chord does that today, so the question does not arise. When it does, that
default is org's to declare, and `org-table-mode` is where it goes.

## 3. Interactive recognition is looser than the unattended pass

`layout::format_tables` requires a separator row to call something a table,
and is right to: it walks whole documents unattended, and prose like ``use
`a | b` `` must not be mangled into a one-column table.

`model::Table::at` does not require one, and is also right, for the opposite
reason: **the user pointed at this table.** Org tables routinely have no rule
at all, and refusing to align them would refuse the common case. The risk
profile is what differs, not the syntax.

## 4. Layering: decline, or consume — and the difference is not stylistic

`<Tab>` in a table advances a cell. Everywhere else it must still mean what it
meant, so the body returns `Effect::Declined` and the dispatcher re-resolves
against the layers below: `org-mode`'s headline cycle, then the builtin
jump-forward. Two hops.

```
<Tab>  →  table-mode : in a table?    next cell + align
                        else          [declined]
       →  org-mode   : on a headline? cycle
                        else          [declined]
       →  Builtin    : jump-list forward
```

**`Declined` is correct only for a chord a lower layer also binds.** A decline
re-runs a multi-key chord's *trailing key alone*, so declining `<leader>tK`
outside a table would fire a bare `K`, and `<leader>tdc` would fire `c` — the
change operator, left waiting for a motion. The `<leader>t…` family returns
`Effect::None`: nothing below binds those chords, so consuming them is both
the honest no-op and the only safe one.

An operation that *cannot apply* — a row already at the top, the last
remaining column — also consumes rather than declining. The caret is in a
table; falling through to a headline cycle there would be a surprise.

## 5. What stays with `org-table-mode`

`org-table-mode` is not retired and is not redundant. It keeps what is
genuinely org's, none of which any other major wants:

- `#+TBLFM:` formula lines and recalculation — the large one.
- `org-table-sort-lines`, export/import.
- Column groups (`|/|<>|`), and the org default for a rule created from
  nothing (§2).

What it sheds is the generic surface every pipe table shares. The split is the
same one `agenda-view-mode` and `org-agenda-mode` make on the agenda view:
both modes are active, each owns its keymap *and* its handler bodies, and
neither is a half-migration.

## 6. Activation

`ActivationPolicy::Majors(["markdown-mode", "org-mode"])`.

Naming a plugin major from the host is deliberate rather than a layering slip:
the policy matches a mode id at activation time, so the host needs no
knowledge of the plugin beyond the string. The alternative — org declaring the
relationship from its side — would put one mode's activation surface in two
repositories.

Not `Always`. A table mode on a Rust buffer would take `<Tab>` from completion
the moment a line started with `|`, and a match arm does.

## 7. Edits

Every operation is a pure function from a table and a caret to a new table and
a new caret, applied as **one** `Effect::ApplyEdit` over the table's line
span. A column insert touches every line, and a half-applied column is a
corrupt table — worse than either end state. It also means `u` undoes the
operation rather than its last row.

The caret is tracked as a *cell*, not a byte offset: alignment rewrites every
row, so the offset does not survive, and what the user means by "where I was"
is the cell. The offset is re-derived from the rendered line.
