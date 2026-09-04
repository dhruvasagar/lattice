---
summary: "scan-view-clockreport-mode: cr puts a clock report at the top of a scan view — clocked time totalled up the outline, with own time beside each total."
related: [scan-view-mode, multibuffer-mode, org-mode]
---

# scan-view-clockreport-mode

`cr` in a [scan view](help:scan-view-mode) puts a **clock report** at the top of
the view: how much time was clocked, totalled up the outline. Press it again to
take it away.

It answers *where did the day go*, which is a different question from *what
should I do next* — so it is a report over the clocked time the scan collected,
not a view of the rows.

```
  Clock total  3:15
  Lattice  2:45  (0:30)
    OA.16  1:20
    Review  0:55
  Notes  0:30
```

---

## Quick reference

| Keystroke | Meaning |
|---|---|
| `cr` | Show / hide the clock report |
| `:scan-view-clockreport-mode` | The same toggle, by name |
| `gD r` | The same toggle from the org agenda's view menu |

`c` still means *change* everywhere else — the chord only fires in a scan view.

---

## Reading it

**Totals roll up.** Time clocked on a child counts for its parent, and its
parent's parent, all the way to the top. A report that listed only the headlines
you clocked directly would tell you which task you touched and never how long
the project took.

**Own time sits beside the total, in parentheses**, whenever the two differ:
`Lattice 2:45 (0:30)` means two and three-quarter hours went into Lattice, half
an hour of it on the project headline itself and the rest in its children. A
leaf's own time *is* its total, so nothing is shown there.

**A headline you never clocked still appears** if something under it was
clocked — otherwise there would be no name to hang the total on.

**The order is the outline's**, not the clock's. The longest entry does not
float to the top; a child always follows its parent. The indentation is the
only thing saying which rows belong together, and sorting by duration would
destroy it.

Times read `H:MM`, the same spelling the `=> 1:30` summaries in your files use.

---

## The range

The report covers **today** by default.

Range is a display choice, not another scan: the view holds every clocked span
it found, so switching between a day, a week or a year re-reads data already in
hand rather than re-walking your files.

With nothing clocked in range the report says so in a sentence —

```
  No clocked time in range
```

— rather than drawing an empty table under a `0:00` total, which reads like
something broke.

---

## Where the numbers come from

From whichever plugin produced the view's rows. The editor contributes the
report; a scan source contributes the clocked time, and one that reports none
gets an empty report rather than an error. The org plugin is the reference
source: its `CLOCK:` lines are what fill the tree above.

---

## What you cannot do

The report is **display only**. Its lines sit above the view rather than in it,
so the cursor cannot rest on one and there is nothing to jump to. Move down into
the rows and act on those instead.

---

## See also

- [`scan-view-mode`](help:scan-view-mode) — the views this report appears on.
- [`org-mode`](help:org-mode) — clocking in and out of a headline.
