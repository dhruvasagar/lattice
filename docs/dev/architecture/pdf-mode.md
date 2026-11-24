# PDF mode — reading a PDF in the editor

> **Design fragment.** Contracts, data model, rationale, rejected
> alternatives, paramount-goal alignment. Sequencing lives in
> [`../operations/slice-plans/pdf-mode.md`](../operations/slice-plans/pdf-mode.md).

`:e paper.pdf` shows the paper, page by page, in the GUI. It is what
[`inline-media.md`](inline-media.md) §8.1 anticipated when it put
`presents_extensions` on the `Mode` trait rather than branching for
pictures: "A PDF or archive viewer is the same shape". The forcing case is
[`org-export.md`](org-export.md)'s `C-c C-e l o`, which compiles a PDF and
should be able to show it without leaving the editor, and which re-shows it
on every re-export.

## 1. What it is: `image-mode`, with one block per page

**`pdf-mode` is an ordinary major on an ordinary buffer.** Not a
`BufferKind`: `:ls`, `:bn`, `:bd` and the modeline treat it like any file. It
lives in `lattice-mode` beside `image-mode` and claims `.pdf` through
`Mode::presents_extensions`, so the bytes never become a rope.

**The buffer's text is one line per page:** `Page 1 of 12`, `Page 2 of 12`, …
Each line anchors one **media block**, the page, through the same
`AsyncMediaSource` registry, pump, sizing and off-thread decode that
`image-mode` and org's inline images use. Consequences, each of them free:

- **The line number is the page number.** `42G` goes to page 42, `gg` / `G`
  to the first and last, `j` / `k` move a page, and `` ` `` marks and the jump
  list work, because they are lines.
- **The label line is the page's caption**, and the cursor's home.
- **The TUI shows the labels**, and each block's alt text. This matches
  images there, whose whole TUI rendering is alt text (inline-media §7.1).

**Page count arrives after open.** `presents_extensions` builds the buffer
with one line before anything is read, as it must: the open path does no I/O
on the actor. The mode's source probes the file off-thread (§3); when the
count lands, the mode writes the remaining label lines through the owner's
`apply_edit_batch`, like any synthetic buffer's owner. Page 1's line and
block are there from the first frame, so lines appearing *below* it move
nothing on screen.

**Read-only, by the same three gates as `image-mode`**: `ReadOnly = true`,
`implies` → `read-only-mode`, and `do_write`'s refusal, because writing the
label text back would replace the PDF with a page list.

## 2. Fitting, zooming, scrolling

| key | does | convention |
|---|---|---|
| `j` / `k`, `J` / `K` | next / previous page | zathura: `J`/`K`; lines already move by page |
| `<C-e>` / `<C-y>`, wheel | scroll within a page | GPUI's `sub_row_px` |
| `s` | fit width | zathura |
| `a` | fit page (whole page in the window) | zathura |
| `+` / `-` / `=` | zoom in / out / reset | zathura, pdf-tools |
| `r` | rotate 90° | zathura |
| `o` | open in the system viewer (`open-external-uri`) | |

The keys follow **zathura**, the vim world's reference viewer: users carry
that muscle memory, and pdf-tools' emacs keys collide with vim motions.
They live on `pdf-mode`'s major layer.

**Fit page is the default.** A page sized to the window is the `image-mode`
rule (`MediaFit::Window`, never upscaling), and one page per screen makes
`j` / `k` behave like page turns. **Fit width** makes a portrait page taller
than the window; the block is then taller than the viewport and scrolls
smoothly *within* itself through GPUI's peer-local `sub_row_px`
(inline-media §4). No core change: the core's unit is still the row, and a
page is one row however tall it is.

Zoom is a buffer-local option, `pdf.zoom` (a fit or a percentage), so it
survives a reload and `:setlocal` reaches it. It is in the media refresh key
with the pane size, so a change re-renders exactly as a resize does.

## 3. Rendering never touches the UI thread

The pipeline is inline-media §5's, unchanged in shape:

1. **Probe** (`spawn_blocking`): parse the file and read the page count and
   each page's media box, in points. Cheap, because no page is drawn. **Every
   page's size is known before any page's pixels**, so the document's height
   is right from the first frame and nothing reflows as pages render.
2. **Render on demand, in two passes.** Only pages in or next to the
   viewport are rasterised, on `spawn_blocking`. Each is drawn first at the
   fitted size at **1×**, then at the fitted size times the display scale,
   which replaces it. A page is vector: it is *rendered at the target*, like
   SVG (inline-media §8.2), so zooming in is crisp rather than resampled.
   The first pass exists because PF.0 measured figure-heavy pages at
   108–330 ms at 2× and at most 75 ms at 1× (text pages: 22–34 ms and
   1–9 ms). So a heavy page appears soft at once and sharpens, rather than
   appearing late. On a 1× display the second pass is skipped.
3. **Land through the inbound primitive**, so a page appears without a
   keypress.
4. **Placeholder at the right size until it lands.** On a zoom or resize the
   previous raster, scaled, is the placeholder, never a blank. A page that
   blinks to empty and back on every `+` is the flicker the UX contract
   vetoes.

**Memory is bounded.** A US-letter page at 1600 px wide and 2× scale is
~52 MB of BGRA. The page cache is an LRU **by bytes**, `pdf.cache-mb`
(default 256), and holds rasters only, never parsed documents beyond the one
open per buffer. Pages scrolled far away are evicted and re-rendered on
return.

**A page that fails to render is an answer** (inline-media §7.1): its block
shows the alt text and the error, and the other pages are unaffected.

## 4. The renderer: hayro

Pages are rasterised by [**hayro**](https://github.com/LaurenzV/hayro)
(Apache-2.0 OR MIT), a pure-Rust PDF interpreter and rasteriser, inside
`lattice-media` behind a **`pdf` cargo feature**. `lattice-cli`'s `gui`
feature enables it.

- **Pure Rust**, so CI and the release matrix build it on Linux, macOS and
  Windows with no native library to ship, pin or load.
- **The same family as the SVG path.** hayro comes from resvg's circle and
  rasterises through `vello_cpu`. Its pixmap's alpha convention is pinned the
  way `lattice-media` already pins SVG's (§8.2): with a known-pixel test in
  both directions, because the mistake reads as "slightly wrong colours".
- **Feature-gated** because it is the largest new thing in the tree, and a
  TUI-only build would compile it for nothing. Without the feature,
  `pdf-mode` still claims `.pdf` (a PDF must never be read into a rope),
  and the buffer is one line: `paper.pdf — PDF pages are drawn by the GUI;
  o opens it in the system viewer`.

**Its known weakness is youth.** Uncommon fonts, shadings and blend modes may
render imperfectly where pdfium would not. PF.0 measures this against a
corpus before anything is built on it (§7); a page hayro cannot draw is the
§3 error case, never a crash.

## 5. Reloading

A PDF that changes on disk, because export recompiled it, **re-renders in
place, at the same page and scroll**. Autoread already watches the file; for
a presented buffer the reload invalidates the probe and the page cache for
that path (the cache key carries `mtime`), keeps the cursor's line (its
page), and leaves the old rasters as placeholders until the new ones land.
If the page count drops below the cursor's page, the cursor clamps to the
last page. This is the `l o`-then-edit loop, and it must not jump.

## 6. Not built, and why

- **Text: search, selection, copy.** It needs a text layer extracted per
  page and a hit-to-page mapping. ⛔ until reading works; then search would
  be a multibuffer of page hits, the existing search surface.
- **Links and the outline.** Internal links (jump to page) and the document
  outline (`:pdf-outline` as a buffer). ⛔, natural follow-ups once text
  exists.
- **Annotations, forms, editing.** ❌: a viewer, not an editor, and not v1.
- **Inline PDFs in org** (`[[file:paper.pdf]]` showing page 1). One line in
  org's media producer once `lattice-media` renders pages. ⛔, small.
- **Dark-mode inversion** (zathura's `recolor`). ⛔, a render option.
- **TUI rasters** (kitty or sixel graphics). Images do not have them either.
  A terminal-graphics peer is its own design and would serve both.

## 7. Rejected alternatives

- **pdfium** (Chrome's engine, through `pdfium-render`). The best fidelity,
  at the cost of shipping and loading a native pdfium library on every
  platform in every release artefact, permanently. If PF.0's corpus shows
  hayro below the bar, this is the fallback, behind the same `pdf` feature
  and the same probe/render interface, so the rest of the design does not
  change.
- **mupdf.** AGPL; lattice is MIT.
- **Poppler's `pdftoppm` as a process.** One process per page (tens of ms
  each), and nothing at all where poppler is not installed, which is most
  macOS and Windows machines.
- **One media block for the whole document.** A single tall block has no
  per-page anchor, so `42G`, marks and the page label would all need a
  mapping that one line per page gives for free.
- **A `BufferKind::Pdf`.** Kind-specific logic is the failure mode
  everything-is-a-buffer exists to prevent, and `image-mode` already proved
  a presented file is an ordinary buffer with a producer.

## 8. TUI and GPUI

Both peers run the same mode, the same buffer and the same keys; they
differ only in what a media block paints, as for images. **GPUI** paints
the page. **The TUI** paints the label line and the block's alt text, and
`o` hands the file to the system viewer. Scroll-within-a-page is GPUI's
peer-local `sub_row_px`; in the TUI a page is one row, and `j` / `k` still
move by page.

## 9. Paramount-goal alignment

- **#1 performance:** probe and rasterise are off-thread; only visible pages
  render; the first frame is correctly sized; memory is capped.
- **#2 extensibility:** the producer is the existing media seam, so a plugin
  wanting PDF thumbnails gets them from the same `lattice-media` path.
- **#3 vim modal editing:** pages are lines, so counts, `G`, marks and the
  jump list are vim's own, unmodified.
- **#4 asynchronicity:** every page lands through the inbound primitive;
  nothing waits on a render.
- **UX:** fit-to-page first frame, no blank flashes on zoom or reload, and a
  re-export that keeps your page.
