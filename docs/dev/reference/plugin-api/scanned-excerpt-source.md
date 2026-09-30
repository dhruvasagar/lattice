<!-- @generated from wit/ by crates/lattice-plugin-api (render.rs).
     Do not edit: run `UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api`. -->

# `scanned-excerpt-source`

**Direction:** shared types only (not called directly) · **Capability:** none (pure data / dispatch) · **Worlds:** `scanned-excerpt-source-plugin` (imports)

OM.A1: plugin-contributed agenda rows.

A plugin teaches lattice to recognise "things with a date on them" in a
filetype the editor has never heard of. Org's agenda is the first and the
motivating one, but nothing here is org: a source names the file
extensions it wants offered, is handed one file's text at a time, and
returns the rows it found.

### It is a multibuffer, so the row shape is an excerpt

The host turns each [`entry`] into an `Excerpt { source, start_line,
end_line, header }` in a multibuffer view — which buys jump-to-source,
edit-propagates-to-source, headerline status and refresh from machinery
that already ships (`org-mode.md` §6.1). That is why an entry carries a
*line* rather than a rendered string: an agenda you can only read is a
lesser feature wearing the name.

### Text AND a tree — structure from one, characters from the other

The text was always here. OT.3 adds the tree beside it, because a scan
that recognises structure by matching line prefixes cannot see CONTEXT.
`* TODO ` at the start of a line inside a `#+BEGIN_SRC` block is example
text, not a headline, and no line matcher can tell — the fact is not on
the line. org's text scan invented a phantom agenda row there.

**Both, not either.** An earlier draft of this slice replaced the text
with the tree, on the theory that the per-file copy was the cost worth
removing. Two measurements killed that: the copy is **217 ns** per file
(`benches/agenda_scan_input.rs`), and the parse that buys the tree is
**1–2 ms** — so the copy was never the expense. Worse, a tree alone
cannot answer what a scanner asks: this seam exposes node kinds and
ranges but no node TEXT, so a guest would need one boundary crossing per
headline to read a TODO keyword — about 50 µs per file, 200× the copy it
was avoiding. Structure from the tree, characters from the text.

`tree` is `none` when the extension resolves to no registered language or
the parse yields nothing. A source is independent of the `language` seam
(see `extensions` below), so a filetype with no grammar must still scan —
it simply scans text, as it always did.

**The guest still touches no filesystem** — no preopens, no `walk`, and
not `tree-sitter.parse-file` either. The host must read the file anyway
to build the source `Document`, so it reads once and parses once, and
the guest is handed both results. That keeps this the one seam that
needs no capability at all.

### Where it runs

Off the UI and actor threads, on a spawned scan task. Not the keystroke
path — but it IS the critical path of a producer, so a guest that blocks
in `scan` backs up the agenda the way a slow `error-parser` backs up a
build. Budgeted per call like every other seam.

### What the host does with a bad entry

Validates and drops, never traps. A malformed file must not fail the
agenda — `error-parser`'s rule, because it is the same failure class.

## Uses

- [`display-span`](types.md#record-display-span) from [`types`](types.md)

## Functions (0)

_(none — a shared type interface)_

## Types (4)

### record `annotation`

```wit
record annotation {
    text: string,
    spans: list<display-span>,
}
```

HB.5: one line hung below a row, and how it is coloured.

One line rather than a list: the consistency graph is one row, and
heights and scroll interactions are not worth inventing for a consumer
that does not exist. A `list<annotation>` is the obvious widening.

**Fields**

- `text`: `string` — The line's text, rendered as-is. Not an excerpt of anything — this
  is the one place a scan source draws content of its own.
- `spans`: `list<display-span>` — Byte spans into `text` (NOT into the row's source line, which this
  is not part of), so a guest's own registered elements
  (`org.habit.overdue`) reach the row with the active colourscheme
  applied and no colour crosses the boundary.

  **A slot here names a THEME ELEMENT, and only that.** `entry.spans`
  also accepts tree-sitter capture names (`keyword`, `string`),
  because those stay a semantic style the cells worker colours at
  paint time — but a virtual row's cells carry a baked colour, so the
  annotation resolves its slots when the row is built and a capture
  name has nothing to resolve against. An unknown slot renders in the
  renderer's default foreground rather than failing the row.

  Validated per span like `entry.spans`: a bad one costs itself, and
  an annotation whose spans are all bad still renders its text. A row
  must never lose its annotation because a decoration was malformed.

### record `entry`

```wit
record entry {
    line: u32,
    end-line: u32,
    group: string,
    label: string,
    sort-key: s64,
    spans: list<display-span>,
    annotation: option<annotation>,
    emphasis: bool,
}
```

One agenda row the guest recognised in a file.

**Fields**

- `line`: `u32` — **0-based** line of the row's anchor, `error-parser`'s
  convention. Becomes the excerpt's `start_line`.
- `end-line`: `u32` — Last 0-based line of the excerpt, inclusive. Equal to `line` for
  the one-row-per-headline case. A guest wanting the headline plus
  its `SCHEDULED:` line returns `line + 1` here.
- `group`: `string` — Grouping **key**. Rows that sort next to each other and share a
  key render under ONE header — which is how a date group shows
  one header for N rows drawn from N different files.

  It is a key, not a label, because the guest cannot know which
  of its rows will land first once every other file's rows are
  interleaved by the sort. The host compares keys AFTER sorting
  and titles the first row of each run; the rest render no header.
- `label`: `string` — The header title for this row, used when it turns out to start
  a group — `"Today"`, `"2026-08-27 Thu"`. Rows sharing a `group`
  should carry the same `label`; the first one after the sort is
  the one rendered.

  The row's own text is the source line itself. This is a header,
  not a rendered agenda line: an excerpt shows the file, which is
  what makes the agenda editable rather than a list of strings.
- `sort-key`: `s64` — Host stable-sorts across files on this, ascending. The guest
  owns what it means (an epoch day, a priority rank, a composite).
- `spans`: `list<display-span>` — OA.5: how this row is COLOURED, as byte spans into the row's own
  first line.

  Without this a row is painted by the source file's tree-sitter
  grammar, because that is all the host has — so an agenda looks
  like org text that happens to be out of order, rather than like an
  agenda. The keyword, the priority, the tags and the date are
  semantics only the guest knows.

  Offsets are relative to the start of `line`, not to the composed
  view: the guest cannot know where its row lands once every other
  file's rows are interleaved by the sort. The host translates after
  sorting, the same way it titles group runs.

  `display-span.slot` names a style rather than carrying one, so a
  guest's own registered theme elements (`org.todo.WAITING`) resolve
  through exactly the path a `highlights.scm` capture takes. Empty is
  the ordinary case for a source with nothing to say about colour —
  the grammar's own highlighting is then what shows, unchanged.
- `annotation`: `option<annotation>` — HB.5: a row to hang BELOW this one, or `none`.

  A row's own text is a verbatim excerpt of a source line, so there is
  nowhere in it to put something the guest computed — org writes a
  habit's consistency graph at column 50 because its agenda line is
  generated text, and ours is the file. The annotation becomes a
  `virtual-row` anchored below the row instead.

  It rides the entry rather than a producer seam of its own for
  [`spans`]'s reason: a general producer would be handed the COMPOSED
  buffer, and a guest cannot know where its row lands until the sort
  has interleaved every other file's rows. See `org-agenda.md` §5b.

  `none` is the ordinary case — an agenda of plain TODOs grows no
  second rows.
- `emphasis`: `bool` — MH.A6: render this row's header EMPHASISED, if it turns out to
  start a group.

  Read only from the row that starts the group — the same rule
  [`label`] already lives by, and for the same reason: the guest
  cannot know which of its rows lands first once the sort has
  interleaved every other file's, so it sets this on EVERY row of
  the group and the host reads whichever one wins. Setting it on
  some rows of a group and not others is a guest bug whose symptom
  is "the header is sometimes emphasised".

  What emphasis LOOKS like is the colourscheme's business, not the
  guest's: the host renders these headers from
  `multibuffer.excerpt_header.emphasis[.title]` rather than from
  anything named here. A theme that does not define those elements
  renders an emphasised header exactly like an ordinary one —
  undistinguished, never invisible.

  **One per view is a convention this cannot enforce.** A guest that
  emphasises every group has emphasised nothing.

  `false` is the ordinary case and is byte-identical to the
  behaviour before this field existed.

### record `clock-span`

```wit
record clock-span {
    line: u32,
    outline: list<string>,
    day: s64,
    minutes: u32,
}
```

OA.14b: time clocked on one headline on one day.

Independent of [`entry`] on purpose — see `scan`'s doc. A span is
reported for every clocked headline the guest saw, whether or not that
headline became an agenda row.

**Aggregated per (headline, day) by the guest**, not one span per
`CLOCK:` line. A headline clocked four times in a morning is one span,
which is the granularity every report actually renders and keeps a file
with years of history from crossing thousands of records it would only
sum again.

**Every span the file has, not just the ones in view.** The report's
range is the agenda's span — day, week, month or year — and the host
filters on `day` when it builds the table. Carrying them all is what
lets `gD` switch that range and redraw from data already in hand
instead of re-walking the corpus for each answer.

**Fields**

- `line`: `u32` — 0-based line of the HEADLINE the time was logged under (not of the
  `CLOCK:` line), so a report row can locate its entry.
- `outline`: `list<string>` — The headline's outline path: outermost ancestor first, the headline
  itself last. Its length is the outline level, which is what emacs's
  `:maxlevel` bounds.

  A PATH rather than a name plus a level, because the report is a
  hierarchy and totals roll up it. An ancestor that logged no time of
  its own emits no span, so the host cannot name it from the span
  list — carrying the chain is what lets the tree be rebuilt without
  inventing zero-minute rows for every parent.
- `day`: `s64` — Days since the Unix epoch that the clocked time is filed under.

  A span crossing midnight is counted whole on the day it began
  rather than split. Emacs splits it; matching that is a refinement
  this record can carry later without changing shape.
- `minutes`: `u32` — Minutes clocked. A running (unclosed) clock contributes nothing —
  its duration is not yet a fact, and guessing one would make the
  report disagree with the file.

### record `scan-result`

```wit
record scan-result {
    entries: list<entry>,
    clock: list<clock-span>,
}
```

What one file's scan produced.

**Fields**

- `entries`: `list<entry>` — The agenda rows, filtered by whatever the guest's sections admit.
- `clock`: `list<clock-span>` — Every clocked span in the file, unfiltered. Empty for the
  overwhelming majority of files, which costs nothing.

