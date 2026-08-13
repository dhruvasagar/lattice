# Cursor visibility + line-count spaces — slice plan

> **Status: Active.** Opened 2026-08-13 from a live report. Nothing
> implemented yet; this records a diagnosis so the work can start
> without re-deriving it.

Two **separate** defects were found together and must not be conflated —
conflating them is how the second one keeps getting "fixed".

## Status

| Slice | Title | Status |
|---|---|---|
| CV.1 | `G` leaves the cursor one row below the drawn area | ✅ |
| CV.2 | Phantom trailing line: a file ending in `\n` renders one row too many | 📝 |
| CV.3 | Name the line-count coordinate spaces so CV.2 cannot recur | 📝 |
| CV.4 | The other readers of the matrix's stamped `wrap_width` | 📝 |

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

## CV.1 — the cursor lands outside the drawn area ✅

**Fixed 2026-08-13.** Hypothesis 1 (soft wrap) was right about *which*
axis, and wrong about the mechanism: the clamp does not ignore wrap, it
asks a cache that cannot answer yet and reads the non-answer as "one
row".

### What the measurement showed

Driven through the real paint path (`app_with` + `draw_frame` over a
`TestBackend`), reading the painted gutter numbers back off the frame:

| wrap | result |
|---|---|
| off | cursor painted in all 5 geometries |
| on | cursor off the painted area in **all 5** |

and with wrap on the scroll landed on *exactly the same line* as with
wrap off — the clamp had budgeted one row per line for content the
renderer was wrapping into two or three. Probing the matrix at the
moment of the motion showed `cells.wrap_width == 0` while the pane was
120 columns wide and wrapping.

That is the root cause. `bottom_anchored_scroll` took both halves of
its wrap geometry from `CellMatrix`:

- **the wrap width**, from the worker's stamp — still `0` on a freshly
  opened file, immediately after a resize, and on the keystroke that
  runs `:set wrap`, because the worker publishes asynchronously;
- **the per-line width**, from `row_at_source_line` — `None` for any
  line outside the windowed matrix, which for a *scroll* computation is
  precisely the set of lines it needs (everything between here and the
  jump target).

Both misses funnel into `segment_count`'s silent `None => 1`. The clamp
believes the viewport holds more content lines than it does, sets
`scroll` too high, and the cursor falls below the last painted row.

### The fix

Two host-side helpers in `crates/lattice-host/src/dispatch.rs`, and
`bottom_anchored_scroll`'s `line_cost` composed from them:

- `scroll_wrap_width()` — live pane geometry (`body_text_width`, or the
  popup's inner width when a popup has focus, mirroring
  `ensure_cursor_visible`'s own height selection). Never the stamp.
- `line_display_width(line)` — the built cell row's `col_count` when
  there is one, a tab-expanded scan of the rope when there is not.

Both mirror what the peers already do right. `body_text_width` is the
horizontal clamp's own derivation and shares `cells_worker::gutter_cols`
with the worker's `effective_wrap`, so the two axes cannot drift on the
gutter reservation; `cursor_display_col` was already cache-then-measure
on the horizontal axis; the TUI's caret walk
(`buffer_line_to_visible_row_with`) already falls back to the rope when
a row is missing. The vertical clamp was the one reader still trusting
the cache outright.

`wrap` itself is read from `option_cache`, not resolved per buffer:
that matches `build_one_pane_cells_input`'s established W.2 choice, and
a per-buffer resolve panics on the minimal configs some tests boot (the
same hazard `body_text_width`'s comment records).

### Why the existing wrap tests did not catch it

They call `seed_wrap_matrix`, which hands the clamp exactly the answer
it is supposed to derive — and two of them never set `:set wrap` at
all, so the seeded stamp *was* the wrap switch. Both were reworked to
turn wrap on through `do_set` and give the pane a real width, so they
now exercise the production path; `goto_last_line_keeps_last_line_visible_with_wrap_and_fold`
also got a budget big enough to make the walk actually cross the closed
fold it was written to test.

### Tests

- `dispatch::tests::goto_last_line_keeps_last_line_visible_with_wrap_and_an_unbuilt_matrix`
  — the unit pin, with the matrix in the state a real editor is in.
- `render::tests::cursor_lands_on_a_painted_row_after_goto_last_line`
  — the invariant, not the instance: after `G`, the cursor's source
  line is among the gutter numbers the frame painted. Swept over
  wrap × split × 5 terminal sizes (20 geometries), and asserted
  against the painted frame rather than against `editor.scroll`.
  Fails in 10/20 on the pre-fix clamp.

  The app is deliberately **not** settled before the motion. A test
  that lets the worker publish first passes on the broken build — the
  same hole `test_helpers::settle` exists to close, here in reverse.

### Cross-renderer

The clamp is host-side, so both peers get the fix with no renderer
change. GPUI needs no same-patch edit — but see CV.4.

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

## CV.4 — the other readers of the matrix's stamped `wrap_width` 📝

CV.1 fixed the vertical scroll clamp. Found while fixing it: it was not
the only reader treating the worker's stamp as live geometry. Recorded
rather than folded into CV.1 — each is a distinct user-visible defect
with its own blast radius, and one of them needs a test-harness change
wide enough to deserve its own commit.

1. **`Editor::active_wrap_width`** (`dispatch.rs`) returns
   `cells_matrix_for(bid).wrap_width` directly, and `gj` / `gk` /
   `g0` / `g$` branch on it. With the stamp still `0` they silently
   degrade to `j` / `k` / `0` / `$` — display-line motion stops being
   display-line motion until the worker publishes. The same
   `segment_count(line)` calls inside `do_display_line_{down,up}` also
   want `line_display_width`'s fallback.

   The cost is in the tests, not the fix: `seed_wrap_matrix` is how
   several `display_line_*` tests turn wrapping on at all, so pointing
   `active_wrap_width` at live geometry makes them all need
   `do_set("wrap")` plus a pane width.

2. **GPUI's renderer** (`editor_element.rs`, the `prepaint` wrap_width
   read) takes the stamp from `DisplayMatrix` then `CellMatrix`, with
   no live-geometry fallback — so on an unstamped matrix GPUI paints
   *unwrapped* where the TUI paints wrapped (the TUI computes its own
   wrap width from the pane rect at paint time). This is a genuine
   peer divergence, and it is the reason CV.1 needed no GPUI edit:
   after CV.1 the host budgets 2 rows where GPUI paints 1, which
   over-scrolls for a frame — visible, but it cannot hide the cursor
   the way under-scrolling did.

## Cross-renderer

Per the standing parity rule, CV.2 lands in the same patch for both
renderers. GPUI has only **2** `line_count()` call sites against the
TUI's 11, so verifying it is cheap once the helpers exist. (CV.1 needed
no GPUI change — the clamp it fixed is host-side.)
