# Indentation guides

Status: design fragment (2026-08-16). Slice plan:
`../operations/slice-plans/indent-guides.md`.

Anchors: `cell-grid-renderer.md` (decoration bucketing, per-pane matrices),
`display-line.md` (the canonical `DisplayLine` substrate), `auto-indent.md` §3
(`IndentUnit` — the resolved indent level), paramount goal #1.

A vertical rule at each level of indentation, drawn in the whitespace to the
left of the code, with the block enclosing the cursor drawn brighter. The TUI
paints a glyph into a cell; the GPU peer paints a one-pixel quad. Both consume
the same worker-resolved layer, so neither renderer implements the rule that
decides where a guide goes.

## Why this exists

Nesting depth in a large function is read from the left margin, not counted.
Every editor a Lattice user arrives from has this (VSCode, Zed, Helix, Neovim
via `indent-blankline`), and the muscle memory is visual rather than
grammatical — which puts it under the *UX-follows-convention* rule rather than
the *architecture-follows-paramount-goals* one. The convention is stable across
all four references, so there is nothing to arbitrate: guides at every level,
continuous through blank lines inside a block, active block highlighted.

What is *not* settled by convention is where the layer is computed and who owns
the rule for whether a given column may be painted. That is the whole of this
design.

## The geometry, precisely

A guide is never painted into a cell that holds text. Concretely:

```
fn f() {
│                    blank inside the block — guide kept
│   if c {
│   │   work();
│   │                blank before the closer — guide kept
│   }              ← no guide at column 4: that cell holds `}`
}                  ← no guide at column 0: that cell holds `}`
                   ← between blocks: nothing
fn g() {
```

Two facts fall out of the picture and are worth naming, because they are what
most of the design has to arrange for:

1. **A guide's extent is a property of the block, not of the line.** The blank
   line after `fn f() {` has no indentation of its own, yet it carries a guide;
   the `}` line has indentation to its left, yet its own column carries none.
2. **Which guide is *active* is a property of the cursor**, and the cursor moves
   at keystroke rate. Anything that recomputes per cursor move is on the hot
   path by definition.

### Block extent

Guides share their notion of "block" with `foldmethod=indent`
(`lattice-host/src/folds.rs`): open a block at line `i` when the next non-blank
line has strictly greater indent; extend while lines are deeper; blank lines are
transparent (they neither break nor extend a block); then apply the
closer-inclusion heuristic that swallows a trailing pure-bracket line (`}`,
`};`, `})?;`) sitting at the opener's indent.

Sharing is not tidiness. `zc` and the active guide are two views of the same
question — *what block am I in?* — and a user who folds a block and sees a
different extent than the one that was just highlighted has been told two
different things by the same editor. The shared walk is extracted to
`lattice-core::indent_blocks` and both callers use it (IG.1, IG.5).

The extracted walk is measured in **display columns** (tabs expanded to
`tabstop`), not the raw leading-whitespace character count `compute_indent_folds`
used. That closes the TODO in `folds.rs` — `foldmethod=indent` currently treats
a tab as one column, so a tab-indented file folds at the wrong boundaries — and
it is what makes the shared-extent guarantee true rather than approximately
true.

An indent jump of more than one level (opener at column 0, body at column 8,
`shiftwidth=4`) emits a guide at each intervening grid column over the same line
range. There is no structure between those levels to give them different
extents.

### The paint predicate

`IndentBlock` records the **opener** as `start_line` and the **closer** as
`end_line` — both inclusive. Then one predicate decides everything:

> Block `b` paints on row `r` iff `b.start_line <= r <= b.end_line`
> **and** (`r` is blank **or** `b.col < indent_columns(r)`).

Walk the picture above against it. On `fn f() {` (column 0, depth 0) the block
at column 0 fails `0 < 0` — no guide, so nothing is drawn over `f`. On the
matching `}` the same test fails for the same reason. On `work();` (depth 8) the
blocks at columns 0 and 4 both pass — two guides. On an interior blank line the
blank arm passes for every spanning block, which is exactly the
continue-through-blanks behaviour. On a blank line *between* two blocks no block
spans it, so nothing is drawn.

Including the opener and closer in the range is what makes the *active* pick
correct on those two lines — with the cursor on `if c {` the innermost enclosing
block is the one that line opens — while painting on them stays suppressed by
the predicate, for free. One range serves both jobs.

A consequence worth stating as an invariant, because the renderers depend on it:
**a published guide mark always lands on a blank cell.** Neither peer needs a
"don't overwrite text" guard, and a bug in the predicate surfaces as a test
failure in the producer rather than as corrupted text in one renderer.

## Where the layer lives

Guides are published **per pane, beside that pane's `DisplayMatrix`, built by
`cells_worker` in the same pass** (`CellsRenderState::display_pane_matrices` →
a sibling `indent_guides` map of `Arc<ArcSwap<IndentGuides>>`).

> **UX (higher court):** every visible pane gets guides, because the cells
> worker already builds per-pane matrices. Nothing is cursor-coupled inside the
> matrix, so no line is republished on cursor motion — no flicker, and no pixel
> changes to content the user did not edit.
> **Paramount goals:** protects #1 — block computation is off-thread in a pass
> that already runs, cursor motion costs the worker nothing, and the renderer's
> per-frame work is a walk of precomputed per-row marks, `O(viewport)`.
> Sacrifices nothing material; the cost is one more per-pane `ArcSwap` cell.
> **Heuristic #1 (long-term fit, on merit):** one producer of the paint
> predicate. The rejected shapes below each end with *both* renderers
> re-deriving "may this column be painted" — two implementations of a subtle
> rule, which is the parity-drift generator the cross-renderer lockstep rule
> exists to prevent.
> **Heuristic #2 (paramount, not other editors):** anchored on goal #1 and on
> the existing per-pane coverage guarantee, not on how VSCode structures it.
> **Standing-rule check (mode ownership):** no `Editor::do_*`, no `Action`
> variant, no `BufferKind` branch. A buffer that should not have guides sets
> `display.indent-guides = false` through `Mode::options()` — the IN.11 seam.

### Rejected

- **Bake guide glyphs into `DisplayLine.text` with a run flag.** The whitespace
  markers work this way, so it is the obvious move. It fails three ways. It
  forces the terminal glyph onto the GPU peer — a `│` character instead of a
  hairline — or makes GPUI un-bake what the worker just baked. The active guide
  is cursor-coupled, so highlighting it means rebuilding display lines on cursor
  motion: whole-line repaint while moving, the flicker class the UX contract
  vetoes. And a row's guides depend on its *neighbours* (the blank-line case),
  which breaks `DisplayLine::with_source_line` — the refcount-bump reuse that
  makes an edit shift downstream rows without rebuilding them.
- **Bucket per row in `overlay_worker`, like `StaticOverlayQuads`.** The key set
  is right (text, scroll, viewport, folds) but the worker is **active-pane
  only**; a split would show guides on one side and not the other, which is a
  visible defect, not a deferred feature. It also re-buckets on scroll for data
  whose coverage window the cells worker already tracks.
- **Derive the active block from tree-sitter rather than lexical indent.** More
  accurate on continuation lines and wrapped argument lists, but it makes an
  always-on visual depend on grammar availability and on async parse state — the
  guide would appear, vanish and reappear as parses land. Paramount #2 is about
  making the editor extensible, not about routing core visuals through the
  extension substrate.

## Data model

`lattice-core::indent_blocks` — pure, no I/O, no config reads:

```rust
pub struct IndentBlock {
    pub col: u16,        // display column the guide occupies
    pub start_line: u32, // opener line, inclusive
    pub end_line: u32,   // closer line, inclusive
}

pub fn indent_blocks(depths: &[Option<u16>], step: u16) -> Vec<IndentBlock>;
```

`depths[i]` is `None` for a blank line and otherwise the leading whitespace of
line `i` in display columns (`IndentUnit::columns_of`). Keeping the input a
depth slice rather than the lines themselves is what lets `compute_indent_folds`
and the guide builder share the walk while resolving depth differently during
the migration, and it makes the walk trivially testable without a `Buffer`.

`lattice-host` (`src/indent_guides.rs`) — the published layer, beside the
`DisplayMatrix` it rides with:

```rust
pub struct GuideMark { pub col: u16, pub block: u16 }

pub struct IndentGuides {
    pub blocks: Arc<[IndentBlock]>,        // for the per-frame active pick
    pub rows: Arc<[Arc<[GuideMark]>]>,     // indexed by source_line - covered_start
    pub covered_start: u32,
    pub version: MatrixVersion,            // the stamp of the matrix built alongside
}
```

The worker resolves the paint predicate and publishes the *result* per row, so
the renderers hold no logic. `blocks` is retained alongside because the active
pick needs extents, and a block index on each mark is what lets a renderer style
one column differently without a second lookup.

Both representations come from one build, in one function, in one pass — they
are two projections of a single computation rather than two caches that can
disagree.

> **Amendment (IG.2, 2026-08-16) — the layer lives in `lattice-host`, not
> `lattice-cells`.** The original sketch put it in `lattice-cells` on the
> grounds that both renderers depend on that crate. They also both depend on
> `lattice-host` — `DisplayMatrix` itself lives there — and `lattice-cells` is
> deliberately kept near-dep-free (its only edge is `lattice-theme`, and its
> Cargo.toml says why). `IndentBlock` comes from `lattice-core`, so homing the
> layer in `lattice-cells` would have added a `lattice-cells → lattice-core`
> edge to buy nothing: the consumers reach `lattice-host` either way. The
> exception is `MatrixVersion::indent`, which is an axis of a `lattice-cells`
> type and belongs there.

### Why `indent` is its own version axis

`MatrixVersion` gained an `indent` axis rather than folding `shiftwidth` into
the existing `whitespace` one. The two are not interchangeable: renderers gate
*painting* on the whitespace axis — a mismatch drops the viewport to raw text
for a frame, which is the correct trade for whitespace markers because they are
baked into `DisplayLine.text`. Guides are not baked into anything, so paying a
whole-viewport fallback frame to toggle them would be a regression bought for
nothing. The `indent` axis gates the rebuild only, like `syntax`, `folds` and
`theme`.

The axis carries `shiftwidth` and whether guides are enabled — the two inputs
that change the layer's geometry. `display.indent-guides.char` and `.active`
are deliberately absent: each renderer resolves them at paint, so changing them
needs a repaint, not a rebuild. `tabstop` is absent because it already bumps
the `whitespace` axis and invalidates the same matrix; one input on two axes is
one input that can drift.

## Per-frame renderer work

Identical in both peers:

```text
active = blocks.iter().enumerate()
    .filter(|(_, b)| b.start_line <= cursor_line && cursor_line <= b.end_line)
    .max_by_key(|(_, b)| b.col)              // innermost enclosing block
for each visible row r:
    for mark in rows[r - covered_start]:
        style = if mark.block == active { indent.guide.active } else { indent.guide }
```

The active guide has **zero lag** — it is picked from the cursor row the frame
already holds, so no worker rerun and no wake are involved in cursor motion.
This is the whole reason extents are published rather than a precomputed
"is active" bit.

- **TUI** — a span pre-pass in the position `apply_whitespace_decoration`
  occupies, before `clip_spans_horizontally`. It therefore works in display-column
  space and `leftcol` is handled by the existing clip rather than by a second
  offset calculation. It substitutes the guide glyph, padding past end-of-line
  for blank rows, and preserves the cell's background so a guide inside a
  selection keeps the selection tint. Unlike the whitespace pre-pass it runs on
  cell-derived bodies, because guide marks are published in the same display-column
  space the cell-derived body is laid out in.
- **GPUI** — a quad pass in `EditorElement::paint` after the body glyphs:
  `fill(x = text_origin_x + advance * (col - leftcol), y = line_y, w = 1px, h = line_height)`,
  2px for the active guide. The `col → x` formula already exists for the cursor.
  Nothing is substituted; consecutive rows join into a continuous rule, which is
  the look the cell-based peer approximates with a box-drawing glyph.

The two mechanisms differ because the substrates differ — a terminal cell holds
one glyph and cannot hold a hairline. What does *not* differ is which columns
are painted and which one is active; that is settled upstream of both.

## Surface

| Option | Type | Default | Notes |
|---|---|---|---|
| `display.indent-guides` | bool | `true` | buffer-local capable |
| `display.indent-guides.char` | string | `│` | TUI only; empty string disables the glyph |
| `display.indent-guides.active` | bool | `true` | highlight the enclosing block |

Theme elements: `indent.guide`, `indent.guide.active`.

Guide spacing is the buffer's effective indent level — `IndentUnit::step()`,
i.e. `shiftwidth` — not `tabstop`. They are separate options for the reason
`auto-indent.md` §3 gives, and guides follow the one that means "one level of
indent".

## Invalidation

Guides ride the `DisplayMatrix` build, so they inherit its coherence: same
snapshot, same `MatrixVersion` stamp, published in the same pass. There is no
separate staleness axis and no separate wake.

| Source | Effect |
|---|---|
| Edit | matrix rebuild → guides rebuilt over the covered range |
| Fold change | matrix rebuild; guides index by `source_line`, so a closed fold simply has no visible row to paint |
| `shiftwidth` change | must bump the matrix version axis that carries `tabstop` — see the slice plan; this is the one axis guides add |
| `display.indent-guides*` | option-cache bump → rebuild |
| Cursor motion | nothing — the active pick is per-frame in the renderer |

When the matrix is stale or absent (boot frame, buffer switch) the renderers
already fall back to plain rope text; guides are absent for that frame and
return on the next publish. Missing hairlines for one frame is the correct
degradation — it is invisible next to the fallback that is already happening.

## Deferred

- **Soft wrap.** `wrap_width` is 0 today (`display-line.md`). When wrapping
  lands, guides paint on a wrapped line's first segment only: leading-whitespace
  columns exist only there, and repeating them on continuation segments is a
  separate design question (VSCode and Zed answer it differently).
- **Per-level colouring ("rainbow" guides).** The block index is already on each
  mark, so a palette lookup is a small follow-up if it is ever wanted.
- **Configurable GPUI line widths.** 1px / 2px until someone has a display that
  makes it wrong.
