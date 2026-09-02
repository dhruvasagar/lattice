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
| `<Tab>` | Next cell — realigning the table on the way |
| `<S-Tab>` | Previous cell |
| `<leader>t\|` | Align this table |
| `<leader>tK` / `<leader>tJ` | Move this row up / down |
| `<leader>tH` / `<leader>tL` | Move this column left / right |
| `<leader>tr` | Insert a row below |
| `<leader>tc` | Insert a column to the right |
| `<leader>tdr` | Delete this row |
| `<leader>tdc` | Delete this column |

`<leader>` is `<Space>` unless you have changed it.

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
