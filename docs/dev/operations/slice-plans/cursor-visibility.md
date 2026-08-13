# Cursor visibility + line-count spaces — slice plan

> **Status: Active.** Opened 2026-08-13 from a live report. Nothing
> implemented yet; this records a diagnosis so the work can start
> without re-deriving it.

Two **separate** defects were found together and must not be conflated —
conflating them is how the second one keeps getting "fixed".

## Status

| Slice | Title | Status |
|---|---|---|
| CV.1 | `G` leaves the cursor one row below the drawn area | 📝 |
| CV.2 | Phantom trailing line: a file ending in `\n` renders one row too many | 📝 |
| CV.3 | Name the line-count coordinate spaces so CV.2 cannot recur | 📝 |

---

## The report

`docs/dev/notes/todo.org` has 219 lines (`cat -n` agrees; other editors
agree).

1. **CV.1** — `G` puts the cursor on line 219, but the last line drawn
   is 218. The cursor sits one row below the visible area.
2. **CV.2** — lattice also shows an empty line 220 that no other editor
   shows.

These are independent. CV.1 is a scroll / cursor-visibility bug; CV.2 is
a line-counting bug. Fixing CV.2 might mask CV.1 by freeing a row, which
is exactly why they are tracked apart.

## CV.1 — the cursor lands outside the drawn area 📝

**Where it lives:** `Editor::ensure_cursor_visible`
(`crates/lattice-host/src/dispatch.rs:6241`).

The function is already careful: it subtracts **sticky** virtual rows
(tutor HUD, diff-mode header) from the height budget and clamps
`scrolloff` to half the effective viewport. So the off-by-one is
something it does *not* yet account for.

**Hypotheses, ranked — these are hypotheses, NOT findings.** The one
lesson that held all through the session that produced this note is that
measuring beat reasoning every single time; three confident diagnoses
(the linker, an always-dirty build cache, the `settle` helper) were all
wrong, and both real finds came from instrumentation.

1. **Soft wrap.** The scroll budget is computed in *document lines*
   while the renderer draws *display rows*. A wrapped long line consumes
   2+ rows, so `scroll + height - 1` over-estimates how many document
   lines fit and pushes the last one off. `todo.org` is prose with long
   lines — exactly the shape that triggers this.
2. **Non-sticky virtual rows** between `scroll` and the cursor (diff
   deletion blocks, excerpt headers) consuming rows the budget does not
   subtract. Only sticky ones are subtracted today.
3. **CV.2 bleeding in** — the phantom row consuming the last slot. The
   reporter believes this is separate and that is probably right, but it
   is a real row in the display matrix, so eliminate it as a contributor
   rather than assuming.

**The decisive measurement, first step of the slice.** With `todo.org`
open, compare `DisplayMatrix::row_count()` for the visible range against
`viewport_height`:

- rows > height ⇒ hypothesis 1 (wrap).
- rows == height but the cursor is still outside ⇒ hypothesis 2 (budget
  arithmetic).

One measurement discriminates. Do it before writing a fix.

**Tests.** Pin the *invariant*, not the instance: after ANY cursor
motion, the cursor's display row lies within the drawn range. Asserted
across the matrix the reporter named — wrapped and unwrapped, with and
without folds, in a split and full-width. `chrome_rows_composes_with_arbitrary_splits_and_terminal_sizes`
is the existing model for that shape.

## CV.2 — the phantom trailing line 📝

`Buffer::line_count()` (`crates/lattice-core/src/buffer.rs:59`) returns
**ropey's raw count**: `"a\nb\n"` reports 3. Its doc comment says
"callers compose semantics they need", and a test pins that as
deliberate.

The leak that produces the visible row is
`crates/lattice-host/src/cells_worker.rs:428`:

```rust
let coverage_line_count = snapshot.buffer.line_count();   // ROPE space
let visible_lo = pane.scroll.min(coverage_line_count);
```

Rope space used where **content** space is meant, bounding the range fed
to the display matrix — so the phantom logical line becomes a phantom
display row. `:591` has the same shape.

**Do NOT fix this by clamping in display space.** That would break folds
and wrap, which legitimately make display rows differ from content
lines.

## CV.3 — name the spaces 📝

The reason CV.2 has been fixed more than once is that the primitive
makes the wrong answer the default and asks **123 call sites** to
remember the correction. There is no authoritative "last line" helper
anywhere in the tree.

There are four distinct quantities, and conflating any two regenerates
this bug class:

| space | counts | affected by |
|---|---|---|
| rope | ropey `len_lines()` | trailing `\n` ⇒ phantom |
| logical / content | document lines | trailing `\n` only |
| display rows | rows laid out | folds (range → 1 row), soft wrap (1 → N), virtual rows |
| viewport rows | rows visible in THIS pane | split geometry, scroll, chrome |

`G` and the motions are **content** space — vim's `G` goes to the last
line of the buffer, not the last display row. The renderer's last row is
**display / viewport** space, and that authority already exists and is
correct: `DisplayMatrix::row_count()`
(`crates/lattice-host/src/display_matrix.rs:166`) is computed after
wrap, folds and virtual rows, per pane. The architecture already has the
right separation; CV.2 is one space leaking into another at a specific
boundary, not a missing abstraction everywhere.

**The rename is the anti-regression mechanism.** Rename the raw
accessor to `rope_line_count()` and add a content-space accessor under a
NEW name. Do **not** reuse `line_count()` for the corrected semantics:
reusing it silently flips the meaning of all 123 sites, whereas removing
it makes the compiler stop at each one exactly once. That triage is the
work, and it needs judgement — some sites genuinely want rope space (LSP
position mapping, byte-offset math).

## Cross-renderer

Per the standing parity rule, CV.1 and CV.2 land in the same patch for
both renderers. GPUI has only **2** `line_count()` call sites against
the TUI's 11, so verifying it is cheap once the helpers exist.
