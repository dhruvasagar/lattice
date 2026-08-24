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

- **Core**: `scroll` stays `u32`, a **display-row index**. Every existing
  scroll, motion, `zz`/`zt`/`zb`, page and `ensure_cursor_visible`
  calculation keeps working unchanged in row units.
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

The consequence, stated plainly: **an org buffer with images scrolls to
the same places on both peers, and only one of them shows pictures.**
That is the divergence the Phase-9 retirement warned about, accepted
deliberately. It is bounded to *what is inside the block* rather than
*where the block is*, which is what keeps it from metastasising into the
scroll model.

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
It composes with the existing decoration seam rather than adding a new
one — a media block is a decoration with a size.

The guest sends a path and a row count; it never sends pixels. Beyond
avoiding a large copy across the boundary, this keeps `fs:read` gating
host-side: the guest names a file, the **host** decides whether the
plugin may read it, and a plugin cannot smuggle arbitrary bytes onto the
screen.

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

**Terminal graphics protocols (kitty / sixel / iTerm2) in the same
slice.** These would make the TUI a genuine peer for this feature and
retire the divergence in §4. Rejected *for now* as scope, not as
direction — it is a second hard substrate, and the block descriptor
above is deliberately protocol-agnostic so a TUI implementation can be
added later without touching the core or the seam. Noted here so the
option is not lost.

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
