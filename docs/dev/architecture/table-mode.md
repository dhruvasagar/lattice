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

## 8. Realigning: on field exit, not on keystroke (TB.4)

### What it costs

Measured on the TB.4 bench (`crates/lattice-mode/benches/table.rs`, release,
5 columns):

| rows | parse | parse + render (one realign) |
|---|---|---|
| 5 | 3.2 µs | **7.5 µs** |
| 50 | 32 µs | 78 µs |
| 500 | 340 µs | 829 µs |

Plus two recognition numbers: a line that is **not** a table costs **22 ns**
(one `starts_with` and out — the path every declining `<Tab>` in a paragraph
takes, and by far the most frequent), and a table **2000 lines into a file**
costs **83 µs**, which is the `#+BEGIN_`/fence scan TB.1's fix introduced.

A table someone actually typed realigns in 7.5 µs — under 0.1% of a 120 Hz
frame. **Cost is not what decides this.**

### What decides it

The keystroke UX contract: *only the edited line may visibly change per
keystroke; everything else stays pixel-stable.* A realign rewrites every row
of the table. Running it per keystroke is, by construction, a pixel change to
content the user did not edit — on every character. That is a veto, and it
holds however fast the function gets.

Recording the numbers anyway is the point of the bench. It stops the next
person re-litigating this as a performance question, and it is the baseline
for the realign that *does* run interactively.

### Which is what emacs does anyway

The TB.4 note originally said emacs realigns on every edit inside a table.
**That is wrong**, and worth correcting rather than quietly dropping: org-mode
calls `org-table-align` from `org-table-next-field`, `org-table-previous-field`
and `org-return` — that is, **when you leave a field**. Type into a cell in
emacs and the table stays ragged until you `TAB` out of it.

So the emacs-parity answer and the contract-safe answer are the same answer,
and the debounce this section was reserved to argue about is not needed.

### The modal split

`table-mode` is the first mode here where the same chord deliberately means
different things in different modal states — which is not a special case but
the model working: a chord resolves per `BindingMode`, and a table is a thing
you both navigate (Normal) and fill in (Insert).

| Chord | Normal | Insert |
|---|---|---|
| `<Tab>` | Next cell; realign; caret on the cell's text | Same, **staying in Insert** — you are filling a row |
| `<S-Tab>` | Previous cell; realign | Same, staying in Insert |
| `<CR>` | *unbound* — `<CR>` is "first non-blank of the next line", and a table row is a line | Next **row**, same column; realign; creates the row at the bottom |
| `<Esc>` | *unbound* | Realign, then fall through to the native exit-insert |

`<Esc>` is the one that makes this feel like "as you type": fill a cell, press
`<Esc>`, the table snaps. It is `fall_through: true` (SN.3c.2b) — the mode
realigns and the dispatcher continues to whatever `<Esc>` natively means, so
the mode never hardcodes exit-insert. `active-snippet-mode`'s `<Esc>` is the
precedent.

`<CR>` is bound only in Insert, and that asymmetry is deliberate. In Normal it
already means something a table row wants (move down a line); in Insert it
means "split this line", which inside a table row is never what you meant.

### No edit when nothing changed

Every body compares the rendered table to what is in the buffer and emits
`Effect::CursorMove` instead of `Effect::ApplyEdit` when they match.

Without this, `<Tab>` through an already-aligned table would push an undo
entry per cell, and `<Esc>` would push one every time you left Insert inside a
table — undo steps for edits that changed nothing, which is worse than the
papercut it looks like: it makes `u` stop meaning "undo my last change".

### Insert `<Tab>` and the completion popup

Binding `<Tab>` in Insert puts this mode in the same chord as completion
accept and snippet-placeholder jump. It is safe because minor layers overlay
**in activation order** (`ActiveModes::keymap_gated_ids`), and both of those
modes activate *later* than `table-mode` — the popup when it opens, the
snippet session when it starts — so they shadow it for as long as they are
up. `table-mode` attaches when the buffer opens and is therefore underneath
both. A host test pins the precedence rather than trusting the reasoning.
