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

MO.2 is the editor body: **scroll, click-to-position, drag-to-select**
— and, once those work, `ui.mouse` defaulting on.

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

### 3.1 Clicking a link (MO.3)

A press **on a link** follows it; a press anywhere else positions.
Emacs's `mouse-1-click-follows-link`, and what every help viewer and
browser does.

The gate is `Editor::help_link_at(pos).is_some()` — a *property*, not a
`BufferKind` test, and that is what makes one rule correct in four
places at once:

- **help / dashboard** seed `HelpLinks` ranges at creation → a click on
  a label follows, a click on body text does not;
- **oil / file tree** seed none. Their `<CR>` follow is a different
  gesture over a different table, and a click must stay a plain cursor
  move rather than opening whatever row it landed on;
- **a document** has none either.

A kind test would have had to name all four and would have got oil
wrong. It is also why the gate exists at all rather than calling the
follow unconditionally: `do_help_follow_link` echoes "no link under
cursor" on a miss, so an ungated call would put that in the echo area on
every click on ordinary text.

**Not on a drag.** Extending a selection across a link is how you copy
its text; following on the way past would make that impossible, and
would fire once per move event.

Both peers get this without either one knowing about it — they dispatch
`MouseGoto` and the follow lives in the host's arm.

**Help-as-popup is the gap.** The rule reaches any link buffer living in
a *pane* (the dashboard, `:help` opened into one). The transient help
popup paints outside `draw_panes`, so no `PaneHitZone` covers it and a
click there is inert. Fixing it properly is the queued "help moves into
the buffer registry" work, not a popup special-case in the mouse path —
that case would be deleted by the move.

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

## 5. Renderer status

**TUI: all three gestures.** The compose loop records each painted
row's origin (`PaneHitMap::set_rows`) in lockstep with the rows it
emits, so the inversion is a lookup rather than a re-derivation — which
is what makes it correct under soft wrap (the row carries its wrap
*segment*, so a click on the tail of a wrapped paragraph lands on the
tail), closed folds, and virtual rows. Virtual rows and the `~` filler
carry no origin and fall back to the last row above that has one, so
clicking below a short buffer lands on its last line.

**GPUI: all three.** The wheel needs no hit-testing — the element *is*
the pane, so the listener closes over its id, the same `cx.listener`
routing the modeline and tabline clicks already use.

Click and drag register through `window.on_mouse_event` inside
`EditorElement::paint`, which is the only place the element's **bounds**
exist and every term is relative to them. Everything the handler needs
is captured at paint time rather than read back later: a handler
outlives the frame that made it, and by the time a click arrives the
prepaint state has been rebuilt against a layout the user never clicked
on.

Three inverses compose there, and each is again the forward map run
backwards:

- **y → row** searches the cumulative `row_tops` rather than dividing
  by a line height, because rows are *not* uniform: a scaled heading is
  taller, and a division would drift further down the pane.
- **x → column** inverts `column_origin_x`, which is now the single
  expression `paint` and the click handler share. It carries the two
  things a uniform `advance * col` misses — per-token scaling on a
  heading row, where the advance is not uniform *across* the row, and
  `leftcol` panning. `ScaledLine::x_offset` delegates to the same walk
  via `ColumnScale`, a glyph-free view of the piece layout, so the caret
  and a click cannot be placed by two different walks (and the handler
  need not clone `ShapedLine`s to outlive the frame).
- **column → byte** is `hit_test::combined_col_to_byte`, rewritten in
  MO.2 as a binary search over this peer's own
  `byte_to_combined_col` — see below.

## 6. Known gaps

- **The conceal reconciliation did not need settling after all.** The
  worry was that `hit_test::combined_col_to_byte` walked real UTF-8
  bytes while `lattice-cells`' inverse works in char-columns, and that
  making GPUI conceal-aware meant unifying them — the question
  `subtract_conceals`' doc defers for its non-ASCII risk. It does not:
  each peer inverts **its own** forward map, so each is consistent with
  where *it* draws the caret, and neither has to adopt the other's
  column space. The old hand-written walk knew about inlays and not
  conceal, so it disagreed with the forward map it was supposedly the
  inverse of; the binary search picks conceal up for free and will pick
  up whatever the forward map learns next.

  Its tests changed behaviour as a result, deliberately: a click on
  inlay text now lands on the byte **before** the splice rather than
  snapping forward to the anchor. The old answer put the caret three
  columns right of the click, because the forward map draws the anchor
  byte on the far side of the hint.
- **Six `editor_element` gutter tests fail under `--features window`,
  and pre-date MO.2** (verified by stashing). They are invisible to
  `scripts/precommit.sh`, which never builds that feature — so the GPUI
  peer's window-gated tests are effectively ungated. Worth closing
  separately; it is a gate gap, not a mouse one.
- **No `mousescroll` option.** Three lines is a constant with one named
  home, so the option has an obvious thing to replace. GPUI converts a
  pixel delta through `row_px` so a trackpad and a wheel travel the
  same distance; the TUI has only notches.
- **Clicking a fold, a sticky-context row or a gutter does nothing
  special.** All three resolve (the pane is right, so the wheel works
  over them) but carry no text column or no origin, so a click is
  inert. Fold-toggle-on-gutter-click and jump-on-sticky-click are the
  obvious next gestures and neither needs new geometry.
