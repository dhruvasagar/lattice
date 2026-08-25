# Inline media — Path 4

> **Design fragment.** Contracts, data model, rationale, rejected
> alternatives, paramount-goal alignment. Sequencing lives in
> [`../operations/slice-plans/inline-media.md`](../operations/slice-plans/inline-media.md).

**Status: design only.** Nothing here is built. This fragment exists
because Path 4 was pulled back from post-1.0 (2026-08-24) and the
reasons it was deferred deserve an answer rather than a reversal.

## 1. What this is

An image, rendered where it appears in the buffer, at its natural size:

```org
[[file:docs/diagram.png]]
```

Org is the forcing case — inline images are how org files are read, not
a decoration — but nothing in the design is org-specific. Markdown's
`![](…)`, a LaTeX fragment, a plotted chart and a Mermaid diagram are the
same mechanism with a different producer.

## 2. Why this was deferred, and what actually changed

`design.md` §Phase 9 retired rich-buffer rendering from v1 on 2026-08-18
and moved Path 4 to post-1.0 alongside it, on the grounds that they share
a **variable row-height substrate** that did not exist: a Fenwick
cumulative-height index and "every scroll / cursor / viewport calculation
that currently assumes uniform rows."

That is now only half true, and the half that changed is the expensive
half.

**The paint substrate landed.** Thread F built per-display-row variable
height into the GPUI editor element: `row_scale[i]` gives a row's height
as a multiple of `line_height`, `row_tops` is its cumulative top, and
every paint site reads geometry by index rather than assuming
`origin.y + line_height * i`. It was built for scaled markdown headings,
and a row whose height is 8.3 line-heights because it holds an image is
the same mechanism with a larger number. With all-1.0 scales it reduces
to the old arithmetic exactly, so the cost is already paid and already
exercised.

**The scroll substrate did not.** `Editor::scroll: u32` is a display-row
index, shared by both peers, and `ensure_cursor_visible` does row
arithmetic against `viewport_height` — also a row count. That is the real
remaining work, and §4 is about not letting it destabilise the TUI.

So the honest position is not "Path 4 is a rewrite". It is: the drawing
is done, the scrolling is not, and the media pipeline does not exist.

## 3. Whole rows or true variable height

Two readings of "inline image", and the difference is the whole cost:

**(a) Whole-row blocks.** The image is scaled to fit exactly N display
rows. `scroll` stays a row index and means the same thing on both peers.
This is what `VirtualRowKind::BrandingBlock` already does for the
dashboard mark — GPUI paints a 2-D composition the cell grid cannot
express, the TUI paints the same rows as terminal art, and both agree on
row count.

**(b) True variable height.** The block's height is whatever the image
needs; rows are no longer an integral unit of vertical space.

**This design takes (b).** (a) is cheaper and would have shipped inside
the existing substrate, but it snaps every image to a multiple of the
line height, which is visible as letterboxing on anything that is not
coincidentally the right aspect ratio, and it makes the rendered size
depend on the font size. An editor that shows a diagram slightly wrong is
worse than one that does not show it, and the whole point of the feature
is fidelity.

The trade-off accepted: (b) is where the scroll rework lives.

## 4. The core stays row-anchored; peers own the height map

The load-bearing decision, and the one that keeps the TUI intact.

A naive Path 4 makes scroll a pixel offset. That is wrong here: pixels
are not a concept the TUI has, and `Editor::scroll` is shared host state
that both peers read. Pushing pixels into the core exports a GPUI
concern into a renderer-neutral type and breaks the peer that cannot
have images.

Instead:

- **Core**: `scroll` stays `u32`, a **display-row index** — well-defined
  however tall the rows are. `viewport_height` keeps its type but has to
  change *meaning*, from a row count to a budget in line-heights; §4.0
  records why and what it costs.
- **Peer**: each renderer owns the mapping from rows to its own vertical
  unit. GPUI already has it (`row_scale` → `row_tops`, in pixels). The
  TUI's mapping is the identity — one row, one cell.
- **Sub-row offset**: GPUI gains a peer-local `sub_row_px` for smooth
  scrolling *within* a tall row. It never enters core state, never
  round-trips through `RenderState`, and is discarded on any core-driven
  scroll change.

A media block therefore declares its size **twice**, and honestly: an
intrinsic pixel size for peers that can render it, and a **row count**
for peers that cannot. The row count is authoritative for the core, so
the document has the same number of display rows on both peers and every
row-based calculation stays peer-agnostic.

### 4.0 What IM.0 found — `viewport_height` is the real problem

The first draft of this section claimed every existing scroll, motion,
`zz`/`zt`/`zb`, page and `ensure_cursor_visible` calculation "keeps
working unchanged in row units". The IM.0 audit shows that is **half
right, and the wrong half is load-bearing**.

- **`Editor::scroll` survives.** It is a row *index*, and a row index
  stays well-defined however tall the rows are. 340 sites, no change.
- **`Editor::viewport_height` does not.** It is a row *count* —
  GPUI computes it as `available_px / row_px` with a uniform
  `row_px = font_size * 1.3`. Once rows differ in height, "how many rows
  fit" is not a constant: it depends on *which* rows, and therefore on
  the scroll position. The division is simply wrong.

The affected set is small, shared, and named. In `lattice-host` these
consume `viewport_height` as "the rows that fit":

`ensure_cursor_visible`, `bottom_anchored_scroll`, `do_page`,
`do_scroll_line`, `do_jump_viewport` (`height / 2` centring — `zz`, `H`,
`M`, `L`), `do_scroll_cursor_to` (`zt` / `zz` / `zb`), and
`preview_viewport_for`. `cells_worker`'s prefetch window also reads it,
but as a heuristic (`viewport_height * 2`), so it degrades rather than
breaks.

That is ~7 shared functions out of 113 host code sites — the audit's
"dangerous set", and much smaller than the raw grep counts suggest.

**The fix keeps pixels out of the core.** `viewport_height` stops meaning
"a count of rows" and starts meaning **the pane's height in line-height
units** — the same number it is today, reinterpreted. Alongside it each
peer publishes a per-row **weight** in line-heights (`f32`): the TUI
publishes all `1.0`, GPUI publishes its existing `row_scale`. The seven
functions then spend a *budget* of line-heights rather than counting
rows.

With all-1.0 weights the arithmetic reduces to today's exactly, which is
the property that keeps the TUI unchanged and makes the change testable
before any image exists. Pixels never enter shared state; the core deals
in line-heights, a unit both peers have.

This is a revision to the plan, not a contradiction of it: the core is
still row-anchored, and a block still declares its size twice. What
changed is that one shared field needed its meaning sharpened, and IM.1
grew a peer-published weight vector.

The consequence, stated plainly: **an org buffer with images scrolls to
the same places on both peers, and only one of them shows pictures.**
That is the divergence the Phase-9 retirement warned about, accepted
deliberately. It is bounded to *what is inside the block* rather than
*where the block is*, which is what keeps it from metastasising into the
scroll model.

### 4.2 What must be shared, and what must not

Worth stating plainly, because "inline images are a GPUI feature" is the
natural first assumption and it is *almost* right.

**Shared, and unavoidably so: the row reservation.** `Editor::scroll` is a
display-row INDEX owned by the host, and every scroll calculation runs
host-side in that row space — `ensure_cursor_visible`, paging, `H`/`M`/`L`,
`zz`/`zt`/`zb`. The virtual-row matrix that defines display rows is
host-produced (`virtual_rows_worker.rs`), not peer-produced.

So a renderer cannot simply invent image rows the host does not know
about. If it did, the host would believe line N sits at row N while the
peer drew it eight rows lower: the cursor lands in the wrong place, `zz`
centres on nothing, paging drifts. The reservation has to be in the shared
model.

That is the *whole* of what must be shared — "a block of R rows exists at
line L, H line-heights tall". The host never needs to know it is an image.

**Not shared: pixels.** Decode, cache, upload and paint are the drawing
peer's business, and the host is a conduit for the descriptor rather than
a consumer of it.

The descriptor currently rides in `VirtualRow::media` because a row and its
content travelling together is one publish with no keying or lifetime
problem, and because it matches `BrandingBlock`. The cost is one
`Option<Arc>` per virtual row and a `PathBuf`-carrying type in
`lattice-cells` that nothing there reads. If that second cost starts to
bite, the split is to keep an opaque block id in the row and a
peer-side table from id to descriptor; the shared surface above does not
change either way.

### 4.1 The document-level height index

`row_tops` is viewport-local, rebuilt per paint, O(visible rows). That is
correct for painting and insufficient for two things:

- **the scrollbar**, whose thumb size and position are proportions of
  total document height;
- **absolute positioning** (`G`, a jump to line N, restoring a scroll
  position) when tall rows lie between here and there.

Those need a cumulative height over the whole document — the Fenwick
index the original Phase 9 named. It is **peer-local** (GPUI's is in
pixels), updated on edit and on block resize, and it is the one genuinely
new data structure in this design.

It is not on the keystroke path: a Fenwick update is O(log n) per changed
row and happens on edit, not per frame.

## 5. Decode never touches the UI thread

Paramount goal #1, non-negotiable, and the reason this is not simply
"call `gpui::img()`".

`gpui` accepts an `ImageSource` and will happily load from a path — which
means file I/O and PNG/JPEG decode inside the render pass. That is
exactly the forbidden pattern.

The pipeline:

1. A provider contributes a block descriptor naming a **path**, not
   bytes. Cheap, synchronous, no I/O.
2. A media service resolves it off-thread (`spawn_blocking`): read,
   decode, scale to the device's display scale, cache by
   `(path, mtime, target_size)`.
3. The result lands through the **inbound primitive**, so it reaches the
   screen without a keypress (`SubsystemBoot::inbound`, per
   `boot-composition.md` §3 — a bare `TickCallback` would sit until the
   user happened to press a key).
4. Until it lands, the block paints a placeholder at its declared size.
   **The size is known before the pixels are**, which is what stops the
   document reflowing when an image finishes loading.

Point 4 is a UX requirement, not an optimisation. A buffer that jumps
around as images decode fails the keystroke contract ("no pixel change to
content the user did not edit") on every scroll.

Intrinsic size comes from the image header, which is a small bounded read
— still off-thread, but far cheaper than a full decode, so a block can
size itself before its pixels exist.

## 6. Data model

A new virtual-row kind, since blocks already flow through
`VirtualRowMatrix` and the interleaver:

```
VirtualRowKind::MediaBlock
```

carrying a descriptor:

| field | meaning |
|---|---|
| `source` | path or URI; never bytes |
| `intrinsic` | natural size in px, once known from the header |
| `rows` | authoritative display-row count (the core's unit) |
| `fit` | how intrinsic maps into the block (contain / width / fixed) |
| `alt` | text the TUI paints, and the accessibility label |

`alt` is not an afterthought: it is the TUI's entire rendering, and it is
what `:describe-buffer` and a screen reader read.

## 7. The plugin seam

Org contributes images from WASM, so the descriptor needs a WIT surface.

**Correction (2026-08-25).** This section previously said the seam
"composes with the existing decoration seam rather than adding a new one —
a media block is a decoration with a size". That is wrong, and writing
code against it would have been the way to find out. `decorations` is a
**gutter** producer: it mirrors `Mode::gutter_decorations` and returns
per-line cues for the gutter column. A media block is a virtual-row group,
which is a different thing entirely — and in fact **no WIT seam can
produce a virtual row today**; none of the twenty-five interfaces mentions
them.

So this is a new ABI surface, and it is a NARROW one by choice
(alternatives in §9):

```wit
interface media {
    record media-block {
        anchor-line: u32,
        path: string,
        alt: option<string>,
        fit: media-fit,
    }
    media-blocks: func(ctx: decoration-context)
        -> result<list<media-block>, string>;
}
```

The guest names a file and a line. The **host** resolves the intrinsic
size, computes rows and `height_lh`, and builds the virtual rows — so
sizing policy stays in one place and a plugin cannot reserve arbitrary
vertical space or paint outside its block.

The guest never sends pixels. Beyond avoiding a large copy across the
boundary per image, this keeps `fs:read` gating host-side: the guest names
a file, the **host** decides whether that plugin may read it, and a plugin
cannot smuggle arbitrary bytes onto the screen.

Shape follows `decorations`: an async producer the host calls on a trigger
and caches per buffer, never on the render path (§7 rule 7 — per-frame
WASM is a paramount-#1 violation).

## 8. Org's surface

- `[[file:diagram.png]]` on its own line becomes a block.
- `org.inline-images` (bool, default off) gates it. Default off because
  a buffer that silently loads and decodes every referenced file on open
  is a surprise, and because the TUI cannot show them.
- `<leader>oI` toggles for the buffer; `org-toggle-inline-images`.
- A link that is *not* alone on its line stays a link. Inline-with-text
  images need intra-line variable height, which the `row_scale` substrate
  does not provide.

## 9. Rejected alternatives

**Whole-row blocks (§3a).** Cheaper, ships inside the existing substrate,
keeps both peers pixel-identical in layout. Rejected because letterboxing
makes the rendered size a function of the font size — the feature exists
for fidelity.

**Pixel-based core scroll.** Simplest for GPUI, and it exports a renderer
concern into shared state, breaking the peer that has no pixels.
Rejected on the everything-is-a-buffer / first-class-TUI grounds that
retired Phase 9 in the first place.

**Bytes across the WIT boundary.** A large copy per image, per load, and
it moves the capability decision into the guest. Rejected.

**A general `virtual-rows` seam** — a guest contributing arbitrary rows
(cells, kind, height, scales) rather than media specifically. Strictly
more powerful: inline charts, LaTeX and anything else would arrive through
one ABI instead of a new seam per content type. Rejected for now because
it exposes the CELL GRID across the plugin boundary, which is a large and
effectively permanent commitment, and hands every plugin a way to paint
arbitrary content anywhere in a buffer. The narrow `media` seam can be
subsumed by it later; the reverse is not true.

**Host-side org support** — the host recognising `[[file:…]]` itself, no
ABI at all. Fastest to ship and rejected outright: it would be the first
time the host knows what an org link is, which is exactly what
org-as-a-plugin exists to prevent.

**Terminal graphics protocols (kitty / sixel / iTerm2) in the same
slice.** These would make the TUI a genuine peer for this feature and
retire the divergence in §4. Rejected *for now* as scope, **explicitly not
as direction** (decision 2026-08-25): it is a second hard substrate, and
tmux — where this editor is often run — needs passthrough for graphics
and is the usual place it gets painful.

Not foreclosing it has a concrete consequence rather than being a
sentiment. The descriptor stays protocol-agnostic; the decode path lives
in `lattice-media`, a crate that belongs to **neither peer**, precisely so
the TUI can grow an image path without moving it; and `MediaBlock::alt`
stays mandatory so a terminal that cannot draw still says what is there.
A TUI implementation should touch neither the core nor the plugin seam.

## 10. Paramount-goal alignment

- **#1 Performance.** Decode and I/O off-thread; size known before
  pixels, so no reflow; Fenwick updates on edit, not per frame; the paint
  path already reads per-row geometry by index. The risk is the
  document-level index on very large files, which the slice plan benches.
- **#2 Extensibility.** Contributed through the plugin seam by the mode
  that owns the content. The host learns nothing about org links.
- **#3 Vim grammar.** A block is display rows; motions, counts and
  operators keep working in row units because the core's unit does not
  change.
- **#4 Asynchronicity.** Results arrive through the inbound primitive and
  paint themselves without a keypress.

**UX, the higher court.** The placeholder-at-known-size rule (§5.4) is
what keeps this from violating the keystroke contract. If a slice cannot
hold that line, it does not ship.

## 11. Risks

- **Renderer divergence.** Accepted and bounded (§4), but it is real: a
  feature the TUI cannot show. Mitigated by `alt` and by keeping the
  descriptor protocol-agnostic.
- **The scroll rework is where bugs live.** `ensure_cursor_visible`
  already juggles sticky rows, context rows and popup viewports. Adding
  tall rows to that arithmetic is the highest-risk slice and wants its
  own tests before any media exists.
- **Cache growth.** Decoded images at display scale are large. Bounded
  cache, evicted by buffer close.
