---
summary: "table-mode: pipe-table editing in markdown and org — <Tab> walks the cells and realigns, <leader>t moves and inserts rows and columns, and your table's own style is kept."
related: [markdown-mode, multibuffer-mode, help]
---

# table-mode

A table is a run of lines starting with `|`:

```
| Name  | Qty |
|-------|-----|
| bread |   1 |
```

`table-mode` turns that into something you can edit rather than something you
have to hand-space. It is on in **markdown and org buffers** and nowhere else.

---

## Quick reference

| Keystroke | Meaning |
|---|---|
| `<Tab>` | Next cell — realigning the table on the way. At the last cell, adds a row |
| `<S-Tab>` | Previous cell |
| `<leader>t\|` | Align this table |
| `<leader>tK` / `<leader>tJ` | Move this row up / down |
| `<leader>tH` / `<leader>tL` | Move this column left / right |
| `<leader>tr` | Insert a row below |
| `<leader>tc` | Insert a column to the right |
| `<leader>tdr` | Delete this row |
| `<leader>tdc` | Delete this column |
| `<leader>t-` | Insert a horizontal rule below |
| `<leader>ts` / `<leader>tS` | Sort this section by the column at the cursor, ascending / descending |
| `<leader>tb` | Blank this cell |
| `<leader>ty` | Copy this cell down, incrementing a trailing number |
| `<leader>tT` | Transpose the table |

`<leader>` is `<Space>` unless you have changed it.

In **insert mode**, the same navigation keys work and keep you typing:

| Keystroke | Meaning |
|---|---|
| `<Tab>` / `<S-Tab>` | Next / previous cell, realigning — you stay in insert |
| `<CR>` | The row below, same column. At the last row, adds one |
| `<Esc>` | Realign the table, then leave insert as usual |

---

## Building a table

Type the first row, press `<Tab>` at the end of it, and you get a new row to
type into. That is the whole loop — `<Tab>` walks cells and adds a row when it
runs out of them. It works the same in insert mode, so you never have to leave
it while filling a table in.

`<CR>` in insert mode goes to the row below in the **same column**, which is
how you fill a column downwards; at the last row it adds one. To leave the
table, press `<Esc>`.

**The table realigns when you leave a field** — on `<Tab>`, `<S-Tab>`, `<CR>`
or `<Esc>` — not on every character. While you are typing inside a cell the
row stays ragged, and then snaps. That is what emacs does too, and it is
deliberate: realigning on every keypress would redraw every row of the table
while you type in one of them.

`<leader>ty` fills a column downwards. It copies the cell you are on into the
row below and **increments a trailing number**, so `Q3` becomes `Q4` and `07`
becomes `08` with its width kept. A cell that does not end in digits copies
unchanged. If there is no row below, it makes one.

`<leader>t-` puts a horizontal rule below the current row — the line that
separates a header from the body.

---

## Sorting

`<leader>ts` sorts by the column your cursor is in; `<leader>tS` sorts it the
other way.

Three things it does that are worth knowing:

- **It sorts only the section you are in** — the run of rows between rules. A
  header above a rule stays where it is.
- **It picks numeric or alphabetic from the data.** If every value in the
  column is a number they sort as numbers, so `10` comes after `9` rather than
  before it. Otherwise they sort as text, ignoring case.
- **Empty cells go last either way.** An empty cell is a missing value, not a
  small one, so sorting descending does not float the blanks to the top.

Your cursor follows its row, so you can see where the row you were looking at
ended up.

---

## Transposing

`<leader>tT` swaps rows and columns.

Horizontal rules do not survive it, and cannot: a rule separates groups of
rows, and after the swap those groups are columns — there is no horizontal
line left that means what it meant. Emacs does the same.

---

## Your table keeps its own style

Org writes its rules `|---+---|`; markdown writes `|---|---|` and can mark
column alignment with colons. Both are tables, and aligning one **never**
rewrites it into the other — the table says which it is, and that is preserved.

Markdown's alignment markers are honoured and kept:

```
| Left | Centre | Right |
|:-----|:------:|------:|
| a    |   b    |     c |
```

A table with no rule at all — which most org tables are — is still a table.
Nothing is inserted that you did not write.

When you add a rule with `<leader>t-`, it copies the style of a rule the table
already has. If the table has none, the file decides: `.org` files get
`|---+---|`, everything else gets `|---|---|`.

Indentation is kept too, so a table nested under a headline stays where you
put it.

---

## Columns line up by what you see

Width is measured in the columns a terminal actually advances, not in
characters. `世` is two columns wide and `é` is one, so a table of CJK or
accented text lines up on screen rather than lining up only in the byte count.

---

## Mid-edit is when you need it most

A row with fewer cells than the rest is padded out, not refused — you are most
likely to press align in the middle of typing a row, which is exactly when it
is ragged.

Two things are deliberately not allowed, because what they leave behind is not
a table:

- The **last remaining row** cannot be deleted. Use `dd`.
- The **last remaining column** cannot be deleted.

---

## Outside a table, these keys are not yours

This is the part worth knowing, because it is what makes the mode safe to have
on all the time.

`<Tab>` in a table moves to the next cell. Anywhere else in the same buffer it
does whatever it did before — folds a headline in org, jumps forward in the
jump list otherwise. The mode declines the key rather than swallowing it, and
the next layer down gets it.

The `<leader>t…` chords simply do nothing outside a table. They do not fall
through, and that is on purpose: falling through would re-run the chord's last
key on its own, so `<leader>tdc` outside a table would fire `c` and leave you
in a half-typed change operation.

---

## See also

- [`markdown-mode`](help:markdown-mode) — one of the two majors this mode
  rides. The other is org's, which the org plugin contributes; org keeps its
  own table extras there, formulas above all.
