# Mouse Architecture (developer reference)

Design fragment for `ui.mouse` and the editor-body gestures built on
it. Slice plan: `docs/dev/operations/slice-plans/mouse.md`.

## 1. Where this started

`ui.mouse` shipped in MO.1 with exactly one consumer: modeline
elements that declare an `on_click`. The option's own doc said so —
*"Editor-body click/drag and terminal passthrough are not built yet;
both will read this same option."* The TUI's event handler returned
early on anything that was not a left-press on a modeline zone, and
the GPUI peer had hit-test primitives (`hit_test.rs`) with a comment
recording that nothing consumed them.

MO.2 is the editor body: **scroll, click-to-position, drag-to-select**.

## 2. The spine: every inverse is derived from the forward map

A mouse gesture is the inverse of rendering. The editor already owns
three forward maps — source position → column
(`lattice_cells::source_byte_to_display_col`), source line → display
row (`buffer_line_to_visible_row_with`), and pane → rect (the layout
pass) — and each is the map the **caret** is drawn with.

So every inverse here is derived from its forward map rather than
written as a second arithmetic:

| Question | Forward map | Inverse |
|---|---|---|
| which pane? | the layout pass | zones recorded during paint |
| which source line? | `buffer_line_to_visible_row_with` | recorded during compose |
| which source position? | `source_byte_to_display_col` | binary search over it |

This is not a stylistic preference. `lattice-cells/src/coords.rs`
exists because three copies of the *forward* column arithmetic drifted
once conceal was added, and its module doc records the symptom: "an
elision the cursor agrees with and the search highlight does not is a
caret sitting off its own match." A hand-written inverse is a fresh
copy with the same failure mode and a worse symptom — a click that
lands one column off reads as a shaping bug, not as a missing rule.

The property this buys, stated once: **a click lands exactly where the
caret would be drawn.** Not approximately, and not "except under wrap /
conceal / inlays".

### 2.1 The column inverse

`lattice_cells::display_col_to_source_byte(col, max, inlays, conceals)`
binary-searches the forward map for the largest source position whose
column is still `<= col`. `O(log line_len)` forward evaluations, on a
path driven by a human's hand.

Two placements fall out of the arithmetic rather than needing cases:

- **On a concealed span → the first visible position at that column.**
  Every position in the hidden range shares the range's start column,
  so the largest of them is the one just past the hidden text — the
  character actually drawn there.
- **On inlay text → the source position before the splice.** Inlay
  columns have no source position; the one before them is where the
  caret already is.

Units are the char-resolved space the cell substrate uses (see the
byte-vs-char note on `CellRow::byte_to_combined_col`), so the caller
resolves char ↔ byte.

### 2.2 The pane map

`lattice_host::mouse::PaneHitMap` — zones pushed by the renderer
**during paint**, cleared at the top of each frame. The
`ModelineHitMap` beside it draws the line in the same place and for the
same reason: a map rebuilt from layout inputs after the fact is a
second implementation of the layout, free to disagree with the one on
screen, and the symptom is a click landing a pane away. A pane that
stops painting stops being clickable, because nothing pushed a zone.

Zones cover the pane's **content** rect, so a status footer is not the
buffer. `text_left` (gutter + sign columns) is recorded rather than
recomputed — four inputs feed it, and `gutter_cols` is now the one
expression the compose loop, the caret walk and the hit map all read.

Last match wins, so a pane painted over another takes the click without
the map carrying a z-index.

## 3. The two gestures

Renderers resolve geometry and dispatch semantic actions; the host
knows nothing about cells or pixels. Same shape as keys, which reach
`Action` through `input.rs` — the host is a router.

`Action::MouseScroll { pane, down }` → `Editor::do_mouse_scroll`
: The pane under the **pointer** scrolls, and does **not** take focus.
  Vim (`mousescroll`), Zed and Helix agree, and it is what makes a
  wheel over a reference split usable mid-edit. One notch is three
  lines — vim's `ver:3` default, as `mouse::MOUSE_SCROLL_LINES`.

  The active pane delegates to `do_scroll_line`, inheriting `<C-e>` /
  `<C-y>`'s fold-aware step and cursor clamp rather than restating
  either. An inactive pane has no live cursor, so it moves its own
  `scroll` through the same fold walk against **its** buffer's folds
  (`DocumentFolds`), then clamps its stashed cursor — without that,
  focusing the pane later snaps the view back and silently undoes the
  scroll.

`Action::MouseGoto { pane, line, byte, extend }` → `Editor::do_mouse_goto`
: Carries a buffer coordinate and nothing about the screen. Unlike
  scroll it **does** focus the pane — clicking into a split is how you
  move to it.

  `extend` is the whole difference between press and drag, and
  **selection is Visual mode, not a parallel concept**: a drag ends
  with the region live in `ModalState::Visual(Charwise)`, so `d`, `y`
  and every operator work on it with no new machinery. That is the
  design's "Visual mode IS the active region" taken literally. The
  anchor is whatever the press established, which is what lets
  press-then-drag work without the press predicting a drag. A plain
  press leaves Visual, as a click does everywhere else.

  No `snap_cursor_past_closed_folds`: the renderer resolved this line
  by inverting the map it painted with, so it is a row the user can
  see. Snapping exists for paths that move the cursor blind to fold
  state; applied here it would move the caret off the clicked line.

Positions are clamped host-side rather than trusted. A click past the
last row is an ordinary gesture — terminals report a cell for every
row, including blank ones — not a bug to propagate into a cursor.

## 4. Terminal buffers

Editor semantics, same as any other buffer: scroll the scrollback,
click to position, drag to select. **No `BufferKind` branch**, which
"buffers must not have kind-specific logic" forbids outright.

Passthrough to the child pty is a separate mechanism, not a flag on
this one: it needs SGR re-encoding and a per-app mode handshake, and
the property-based way to reach it is a per-buffer "consumes raw input"
flag rather than a kind test. Deferred with that named.

## 5. Known gaps

- **GPUI's editor body has no listeners.** `hit_test.rs` has the
  primitives; `editor_element.rs` has no mouse or scroll reference at
  all. Both peers were equally unbuilt at MO.2's start, so this is
  divergence created by MO.2 and it is the first thing that closes it.
- **GPUI's `combined_col_to_byte` is conceal-blind.** It walks real
  UTF-8 bytes while the shared inverse works in char-columns —
  reconciling the two is the question `subtract_conceals`' doc already
  defers ("its own non-ASCII risk"), and a mouse slice is the wrong
  place to settle it. Until then a GPUI click in a buffer with conceal
  rules (org links, markdown) lands off.
- **`ui.mouse` still defaults off.** The flip waits on the body
  gestures being complete; the option doc carries the reasoning
  (capture takes click-drag selection and middle-click paste away from
  the terminal emulator).
- **No `mousescroll` option.** Three lines is a constant with one
  named home, so the option has an obvious thing to replace.
