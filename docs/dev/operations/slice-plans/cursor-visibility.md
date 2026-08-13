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
| CV.2 | Phantom trailing line: a file ending in `\n` renders one row too many | ✅ |
| CV.3 | Name the line-count coordinate spaces so CV.2 cannot recur | 📝 |
| CV.4 | The other readers of the matrix's stamped `wrap_width` | 📝 |
| CV.5 | Oil / file-tree cursor highlight trails the caret by `scroll` | ✅ |
| CV.6 | Oil and the file tree paint through bespoke, kind-specific renderers | 📝 (own plan) |

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

## CV.2 — the phantom trailing line ✅

**Fixed 2026-08-13.** The diagnosis (rope space used where content
space is meant) was right. The *location* was not — worth recording,
because the wrong location is a plausible one and fixing it alone
changes nothing on screen.

### Where it actually came from

Measured by rendering three shapes through the real paint path and
reading the gutter numbers back:

| buffer | painted | correct |
|---|---|---|
| `"alpha\nbravo\ncharlie\n"` | 1,2,3,**4** | 1,2,3 |
| `"alpha\nbravo\ncharlie"` | 1,2,3 | 1,2,3 |
| `"alpha\nbravo\ncharlie\n\n"` | 1,2,3,4,**5** | 1,2,3,4 |

`cells_worker.rs:428` and `:591` are real rope-space leaks and were
fixed, but they clamp the *coverage* range for a cache-hit check —
correcting them alone left all three rows exactly as above. The row
that reaches the screen is produced by
**`lattice_host::folds::visible_source_lines`**, whose `total_lines`
bound both renderers were passing in rope space. That function is the
single row producer shared by the TUI and GPUI, which is why it is
also the right place to *name* the space: one doc comment now governs
both peers, and passing the wrong count in either one reintroduces the
row in that peer alone.

### The fix

- **`Buffer::content_line_count()`** (new, `lattice-core`) — the count
  every editor and user means: `"a\nb\n"` is two lines, and so is
  `"a\nb"`. Floors at 1, matching vim's one-empty-line empty buffer.
  This is the "add a content-space accessor under a NEW name" half of
  CV.3, landed early because CV.2 needs it; the rename-and-triage half
  is still CV.3.
- `visible_source_lines`' `total_lines` documented as content space,
  and both call sites (`render.rs`, `window.rs`) switched.
- All six `snapshot.buffer.line_count()` sites in `cells_worker`
  switched, so the matrix's rows, its `source_line_count`, its
  coverage range and the incremental guard all sit in one space.

**Gutter width deliberately left in rope space.** `gutter_width` /
`cells_worker::gutter_cols` are fed the raw count in the host *and*
both renderers, so they agree today; a file of exactly 999 content
lines gets a gutter one column wider than it needs. Cosmetic,
consistent, and moving it means moving the host and both peers in
lockstep — CV.3's job, not a drive-by (the wrap width is derived from
this, so a half-move would desync CV.1's clamp).

### Tests

- `buffer::tests::content_line_count_excludes_the_trailing_empty_line`
  — the accessor, including the shapes that must NOT be trimmed (a
  genuinely blank final line is content).
- `render::tests::a_file_ending_in_a_newline_paints_no_extra_row` —
  the painted frame numbers exactly the lines the file has, across
  terminated / unterminated / blank-last-line / single-line / empty.

`dr3_active_pane_compose_characterization`'s expected fingerprint had
the phantom baked in as a fifth numbered row on a four-line file; it is
now the second `~`.

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

**Half of this landed with CV.2.** `Buffer::content_line_count()` now
exists — the new-name accessor. What remains is the rename of
`line_count()` → `rope_line_count()` and the 123-site triage it forces.
Do not skip the rename on the grounds that "the accessor exists now":
the accessor without the rename leaves the wrong answer as the default,
which is the exact condition that let CV.2 recur.

Two findings from CV.2 that belong in the triage:

- **The gutter is still rope space** — `gutter_width` (TUI),
  `cells_worker::gutter_cols` (host), and GPUI's
  `total_lines_for_gutter` all take the raw count. They agree with each
  other, so the only symptom today is one wasted column on a file of
  exactly 10ⁿ content lines. They must move **together**: CV.1's wrap
  width is `viewport_width - gutter_cols(...)`, so a half-move desyncs
  the vertical clamp from what gets painted, which is the CV.1 bug
  again by another route.
- **`last_addressable_line` exists three times** — `lattice-grammar`
  (`builtins.rs`), `lattice-ui-tui` (`app.rs`), `lattice-host`
  (`dispatch.rs`) — all computing `content_line_count() - 1` by hand.
  The triage should collapse them onto the accessor.

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

## CV.5 — the oil / file-tree cursor highlight trails the caret ✅

**Fixed 2026-08-13.** Reported against oil in a horizontal split:
scrolling the cursor past the bottom of the pane left the caret on the
right line while the highlight fell behind.

### Root cause

```rust
.enumerate()          // index is ALREADY absolute
.skip(scroll)         // first surviving item is (scroll, …)
.take(viewport)
.map(|(i, name_str)| {
    let line_idx = scroll + i;      // scroll counted twice
```

`enumerate` runs before `skip`, so its index is the absolute source
line; adding `scroll` again makes it `2·scroll + k`. The highlight
therefore fires `scroll` rows above the caret, which reads its row from
`cursor.line - scroll` and is correct. Measured: `scroll = 7`,
`cursor.line = 17` ⇒ caret row 10, highlight row 3 = 17 − 2·7.

The split is not incidental. A full-height pane shows a short listing
without ever scrolling, and at `scroll == 0` the two expressions agree
— which is why this survived.

**Four sites, one copied mistake:** `draw_oil_pane` and
`draw_file_tree_pane` in the TUI, and both of their GPUI counterparts.
Oil is worse than the highlight in both peers: it also indexes the
entry list with the doubled value (`snapshot.get(line_idx)` /
`entries.get(line_idx)`), so scrolled rows drew the wrong icon too.
The file-tree paths pair their entries through a `zip` that precedes
the `skip`, so only their highlight was wrong.

The picker candidate loops use the same `enumerate → skip` shape and
are **correct** — they compare against an absolute `selected`, never
re-adding scroll. The GPUI completion rows slice `[scroll..end]`
*before* enumerating, so their `scroll + i` is right too. Both were
checked; neither was touched.

### Tests

`render::tests::pane_listing_cursor_highlight_and_caret_share_a_row_when_scrolled`
asserts the invariant on the painted frame — the reverse-video row is
exactly the caret's row — for **oil and the file tree together**, in a
horizontal split, scrolled. Verified to fail on each defect
independently by restoring one fix at a time.

Testing them together is the point: the report named only oil, and the
file tree had the identical bug with nothing to announce it. Same
failure mode as `magit-diff-mode`'s missing `x` — a gap in a copied set
does not report itself (see `prefer-minor-modes-over-duplication`).

## CV.6 — oil and the file tree paint through bespoke renderers 📝

CV.5 was a one-line arithmetic error, but it was only *possible*
because `draw_oil_pane` / `draw_file_tree_pane` (and their GPUI twins)
are hand-written paint paths that duplicate what `compose_pane_lines`
already does for every other buffer — scroll windowing, cursor-row
resolution, per-row styling. Four copies of the same walk meant one
mistake in four places, and the standing rule is explicit that the
right move for a kind-specific helper is to *remove* the special path,
not to keep its parity in step.

Not folded into CV.5 deliberately. The reported defect is an off-by-
`scroll` in the row index; routing oil and the file tree through the
shared compose path is a rewrite that has to carry per-entry icons and
styling across, and it risks new *visual* regressions in the panes it
touches. UX is the higher court: taking that risk as the vehicle for
fixing a highlight offset is the wrong trade. Fix the arithmetic, land
it, then do the convergence on its own with its own tests.

Note it composes with CV.2: these paths window by source line with no
fold or wrap accounting at all, so they are the one place where the
rope/content distinction genuinely does not arise today — and would,
the moment they join the shared path.

**Design + sequencing now live on their own**, because the fix turned
out to be a storage migration plus a shared minor mode rather than a
renderer change:

- design — `docs/dev/architecture/directory-listing-mode.md`
- slices — `docs/dev/operations/slice-plans/directory-listing-mode.md`
  (DL.1–DL.6)

## Cross-renderer

Per the standing parity rule, CV.2 lands in the same patch for both
renderers. GPUI has only **2** `line_count()` call sites against the
TUI's 11, so verifying it is cheap once the helpers exist. (CV.1 needed
no GPUI change — the clamp it fixed is host-side.)

CV.5 landed in both peers in one patch: GPUI carried the identical
defect in both its oil and file-tree paint paths. It has no test —
`lattice-ui-gpui` has no frame-level harness — so the TUI test is the
guard and the GPUI fix is verified by inspection plus
`cargo build -p lattice-ui-gpui --features window`.
