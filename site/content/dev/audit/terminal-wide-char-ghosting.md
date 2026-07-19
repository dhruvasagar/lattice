+++
title = "Terminal wide-char ghosting — one grid cell ≠ one emitted char"
+++


**Audited 2026-07-07**, prompted by "ghosted characters left over on the screen"
in terminal panes (`:terminal`, `:claude`) — stray glyphs that survive tab
switches, do not scroll with the buffer content, and clear only on a force
redraw (`<C-l>`). The trace found the terminal render path violates the width
invariant every cell-grid renderer relies on. This document records the
invariant so the next terminal-render change doesn't re-introduce the same class
of bug.

## The invariant (load-bearing)

> **The characters a renderer emits for one terminal row must have the same
> total display width as the grid's column count — every grid column maps to
> exactly one display column.** A wide glyph (East-Asian Wide / Fullwidth /
> emoji-presentation, width 2) owns its two columns *through the renderer's own
> width shaping*; alacritty's companion `WIDE_CHAR_SPACER` cell must **not** be
> emitted as a separate character.

Violate it — emit both the wide glyph *and* its spacer cell as characters — and
one grid column pair (glyph + spacer, 2 columns) becomes three emitted display
columns (glyph=2 + spacer-space=1). Every wide glyph in a row shifts the rest of
the row right by one and desynchronises the renderer's column model from the
grid.

## How alacritty represents a wide glyph

`alacritty_terminal` lays a width-2 glyph across **two** grid cells:

- the `WIDE_CHAR` cell holds the glyph itself, and
- the following cell carries the `WIDE_CHAR_SPACER` flag (a placeholder so the
  grid's column count matches the display; alacritty's own renderer skips it).
- the edge case — a wide glyph that would land in the last column — uses
  `LEADING_WIDE_CHAR_SPACER` in the last column and reflows the glyph to the
  next line.

Alacritty decides width with `unicode-width` (0.2.0 in this tree), the same
table ratatui and the GPUI shaper use — so all three agree on *which* glyphs are
wide. The bug is not a width-table disagreement; it is that our renderers
emitted the spacer cell as content.

## Where it broke

`crates/lattice-terminal/src/reader.rs::map_cell` copied `ch: a.c` and **dropped
the cell flags**, so the substrate `Cell` couldn't tell a spacer from a real
space. Both terminal renderers then walked `0..cols` and pushed one `char` per
grid column with no width handling:

- TUI: `lattice-ui-tui/src/render.rs::draw_terminal_pane` — builds a ratatui
  `Line` of `cols_to_paint` chars.
- GPUI: `lattice-ui-gpui/src/window.rs::build_terminal_inner` — builds a
  per-row run string of `snap.cols` chars.

So for a grid row `[…, W(wide), spacer, x, …]` at grid cols `[c, c+1, c+2]`, both
emitted `"…W x…"` — glyph, an explicit space, then `x` — placing `x` at display
column `c+3` instead of `c+2`.

## Why it ghosted (the TUI path)

The document path never hits this — document text flows through width-correct
rendering — which is why **only terminal panes ghost**. Two compounding effects
in the TUI:

1. **Column shift.** Content after each wide glyph lands one column too far
   right; the row's tail is pushed off the pane and truncated.
2. **ratatui diff desync (the ghost).** ratatui's `Buffer::diff`
   (`ratatui-0.29.0/src/buffer/buffer.rs`) decides which cells to emit from
   per-cell `symbol().width()`:

   ```rust
   if !current.skip && (current != previous || invalidated > 0) && to_skip == 0 { emit }
   to_skip     = current.symbol().width().saturating_sub(1);
   invalidated = max(current.width, previous.width).saturating_sub(1);
   ```

   The `to_skip` / `invalidated` counters assume a well-formed buffer where a
   width-2 symbol is followed by its own continuation cell. The extra
   spacer-space we injected makes that bookkeeping land one cell off. Under
   **auto-scroll / scrollback** — where the wide glyph's position shifts every
   frame and ratatui emits a near-full-screen diff many times per second — the
   mis-counted `to_skip` causes ratatui to **skip cells that genuinely
   changed**. The stale glyph is never re-emitted, so it sticks on screen until
   a full `terminal.clear()`. Because ratatui's previous-buffer now diverges
   from the real screen, the stale cell survives tab switches and bleeds into
   whatever pane later occupies those columns.

This is why the trigger is *automatic* scrolling (hundreds of diffs) rather than
manual scrolling (one repaint), and why the symptom is terminal-only and
redraw-clearable.

The GPUI peer rebuilds its element tree each frame (no diff), so it does not
*ghost*, but the same emission bug misaligns its columns — hence the fix applies
to both for parity.

## The fix

Preserve alacritty's width decision in the substrate and honour it in both
renderers:

1. `Cell` gains `wide_spacer: bool` (`lattice-terminal/src/cell.rs`).
2. `map_cell` sets it from `WIDE_CHAR_SPACER | LEADING_WIDE_CHAR_SPACER`
   (`reader.rs`).
3. Both renderers **skip** `wide_spacer` cells when building a row — the
   preceding wide glyph already occupies two display columns via the renderer's
   own shaping, so grid column and display column return to 1:1 (which also
   fixes cursor / overlay column alignment on rows with wide glyphs).

Degrades safely: if a renderer's width table ever disagreed with alacritty's and
treated the glyph as width 1, skipping the spacer would leave a trailing blank —
never a ghost.

## Rules of thumb

- **One grid column in → one display column out.** When emitting a terminal row,
  the emitted string's display width must equal the grid's column count. Skipping
  `wide_spacer` cells is what keeps that true; the wide glyph supplies its own
  second column.
- **Never emit a spacer cell as a character.** `WIDE_CHAR_SPACER` /
  `LEADING_WIDE_CHAR_SPACER` are structural placeholders, not content.
- **A new terminal renderer (or a libghostty substrate swap) must honour
  `wide_spacer`.** The `Cell` flag is the contract; a renderer that ignores it
  re-introduces this bug.
- **"It looks aligned in a document buffer" proves nothing about terminals.**
  Terminal grids carry arbitrary child-program output (wide glyphs, spacers);
  documents don't. Terminal-render correctness needs its own width coverage.
