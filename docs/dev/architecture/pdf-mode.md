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
`image-mode` and org's inline images use. A block names its page as
`MediaSource::PdfPage { path, page }`, a second variant beside
`MediaSource::Path`: the enum is already "what this block shows", and putting
the page in the path (`paper.pdf#3`) would make every reader of a path strip
it, and break on a file really named that. Consequences, each of them free:

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

## 2. Navigating: the keymap

Reading is the whole job of this buffer, so its keys are chosen for reading,
not inherited by accident from text editing. Two sources, weighted the way
the convention rule says: **vim** for anything that is already a line motion
(pages *are* lines), and **zathura**, the vim world's reference viewer, for
what a viewer adds. pdf-tools' emacs keys collide with vim motions and are not
followed.

| key | does | from |
|---|---|---|
| `j` / `k`, `J` / `K` | next / previous page; a count moves that many | vim lines; zathura `J`/`K` |
| `gg` / `G` / `{n}G`, `:{n}` | first / last / page *n* | vim, zathura |
| `<Space>` / `<S-Space>`, `<C-f>` / `<C-b>`, `<PageDown>` / `<PageUp>` | a screen down / up, continuing onto the next page | zathura, vim |
| `<C-d>` / `<C-u>` | half a screen down / up | vim, zathura |
| `<C-e>` / `<C-y>`, wheel | a step down / up | vim |
| `h` / `l` | scroll left / right, when the page is wider than the window | zathura (deferred, PF.4b) |
| `s` / `a` | fit width / fit page | zathura (deferred, PF.4b) |
| `+` / `-` / `=` | zoom in / out / back to fit | zathura, pdf-tools (deferred, PF.4b) |
| `r` | rotate 90° clockwise | zathura (deferred, PF.4b) |
| `R` | reload from disk (it also reloads by itself, §5) | zathura |
| `gx` | open in the system viewer | vim's "open with the system handler" |
| `q` | close the PDF's buffer, as `:bd` | zathura, magit |
| `<C-o>` / `<C-i>`, `m{a-z}` / `` `{a-z} `` | jump list and marks | vim, unchanged |

The page keys live on `pdf-mode`'s major layer, so none of them reaches a
text buffer. Most need no body: `J` / `K` name vim's `j` / `k`, and `<Space>`,
`<S-Space>`, `<PageDown>` and `<PageUp>` name `<C-f>` / `<C-b>`. The keys a
picture would want too (`q` and `R` today, `gx` when it lands) are on
**`viewer-mode`**, a minor both `image-mode` and `pdf-mode` imply, for the
shared-behaviour rule: one declaration, not one per viewer. `q` is `:bd` (a
PDF opened with `:e` is an ordinary buffer, so magit's bury would be a no-op),
and `:bd` now returns the pane to the buffer it showed before, as vim's does.

The modeline shows `page 3/12` without a line of PDF code in the host. The
position element's text is the option `ui.modeline.position-format`, vim's
`'rulerformat'` (`{line}:{col}` by default, with `{lines}` for the count), and
`pdf-mode` sets it buffer-locally to `page {line}/{lines}`.

**`j` / `k` move by page, not by a scroll step.** zathura's `j` / `k` scroll a
step, and that is the one place this table departs from it. Pages are lines
here, so `5j`, `dj`-style counts, `:{n}` and the jump list all mean pages, and
making `j` scroll would break that for one viewer's muscle memory. In fit
page (the default) the two are the same keystroke anyway. The step scroll is
on `<C-e>` / `<C-y>` and the wheel; screen and half-screen scrolls cross page
boundaries the way zathura's continuous mode does.

**Screen scrolls know a page is tall.** The scroll walks charge a line the
rows its media reserves, when a peer that draws pixels is attached, so
`<C-f>` and `<Space>` turn one page in fit page rather than a screenful of
lines. A page turned to lands at the top, as in zathura. A walk that would
not move because the next line is taller than the screen moves one line, as
vim's `<C-f>` always moves. In the TUI a page is one label line and these
keys keep their meaning for lines, and `h` / `l` do nothing.

**Open: scrolling inside a page.** The core's scroll is a line index, and a
page is one line. In fit page that is exactly right, because a page is never
taller than the window. Fit width, zoom past the window and `h` / `l` all
make a page larger than the window, and something has to scroll *inside* it.
The `sub_row_px` this section first named cannot be that something: it is
peer-local, and the keys that would drive it are dispatched in the core. See
the slice plan's PF.4b for the decision this needs. Until then the mode is
fit page only (deferred, 2026-11-24), and so is rotation, whose `r` row in
the table above is the intended key.

### Fitting and zooming

**Fit page is the default.** A page sized to fill the window
(`MediaFit::Page`: `image-mode`'s `Window` without its never-upscale clamp,
because a page is vector), and one page per screen makes `j` / `k` behave
like page turns. **Fit width** makes a portrait page taller than the window,
which needs scrolling inside a page (above, open).

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
~52 MB of BGRA. GPUI's decoded-media cache becomes an LRU **by bytes**,
`media.cache-mb` (default 256), keyed by `MediaSource`, so pictures and pages
share one budget. That cache held every decoded picture forever, so an org
file full of large images grew without bound too: one mechanism fixes both,
where a page-only cache beside it would leave that and give the peer two
caches with different rules. It holds rasters only. Parsed documents are
cached separately in `lattice-media`, one per path and `mtime`, shared by all
of a document's pages. Pages scrolled far away are evicted and re-rendered on
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
  and the buffer is one line: `paper.pdf: this build of lattice does not read PDFs; the GUI build
  does` (PF.2's wording).

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

**As built (PF.5)**, in three pieces, none of them PDF-specific, so a picture
rewritten on disk reloads the same way:

- **Autoread knows a presented file is not text.** The stat gate is the
  whole check for one; the content gate's `read_to_string` used to fail on
  it and report the file gone. A change is always a reload, because a
  presented buffer is never dirty.
- **A reload carries the major's text over.** `:e!` reopens into a fresh
  buffer, and a presented file opens as one empty line its major fills in
  later, off the actor. The fresh buffer starts with the text it replaces
  (clean, as the major's own write is), so the restored cursor has its line
  and the labels are rewritten only if the page count changed.
- **Blocks carry the file's revision** (inline-media §7). The probe's
  document cache already keys on `(path, mtime)`; GPUI's pixel cache keys
  on the source, so it re-renders a page whose revision moved and draws the
  old raster until the new one lands. A failed decode is retried at a new
  revision, so a broken PDF fixed on disk draws.

`R` is the same reload by hand, on `viewer-mode`: `:e!`.

**A reload that catches the file mid-write keeps what it showed** (PF.5b).
pdfTeX writes its output as it ships pages out, so the watcher can fire on a
PDF that is half there: unreadable, or, if the reader repairs it, short of
its pages. Showing either replaces the document the user is reading with an
error line or a stub, and clamps the cursor to the stub's last page, a
pixel change to content the user did not edit. So a reload of a file that
is still being written does not happen: the labels, the blocks, the rasters
and the cursor stay as they were, and the compile's final write is a change
of its own that loads the finished PDF.

The page count cannot be the test. A reader repairs a cut file whose page
tree came first into a prefix of its pages (measured: a 12-page file cut at
half reads as 6, at 70% as 10), which is indistinguishable from a document
that lost pages, and a real rewrite that drops pages must clamp. pdfTeX
writes its tree last, so its cut files do not parse at all, but that is one
producer's order. The test is the format's own end marker: a PDF ends with
`%%EOF` (an incremental update appends another), so a file without one in
its last 1024 bytes is unfinished. `pdf-mode` answers it through
`Mode::presented_file_settled`, which autoread asks before reloading any
presented file; the default is "settled", so pictures reload as before.
Gating the reload rather than its result is what keeps all three layers
still: the media pump re-measures a fresh buffer from nothing, and the GPUI
cache redraws on a new revision, so a reload that went ahead and kept only
the labels would still have collapsed every page to its one-row stub. `R`
reloads by hand without asking: the user has said to. A file that ends with
the marker and still cannot be read is reloaded, and says so, because that
is the file the compile finished with. This is what zathura and
sioyek do. On a first open there is nothing to keep, and the error line
stands (§3).

## 5a. Lattice as the system's PDF viewer

Opening a PDF from the file manager or a browser should be able to land in
lattice. **Opt-in, never taken.** Installing lattice must not change which
program the user's PDFs open in.

- **Linux** (the `.deb` and the AppImage, the native bundles v1 ships): the
  desktop entry declares `application/pdf` among its `MimeType`s, which makes
  lattice a *candidate*: it appears under "Open with", and nothing else
  changes. Becoming the default is the user's
  `xdg-mime default com.lattice-editor.lattice.desktop application/pdf`, which
  `:help pdf-mode` gives verbatim.
- **macOS**: needs a `.app` whose `Info.plist` declares the PDF document
  type. v1 ships no `.app` (release-pipeline: the native apps are the Linux
  bundles), so this waits for one. ⛔
- **Windows**: a per-user file association (`HKCU\Software\Classes`) makes
  lattice a candidate without admin rights; Windows 10 and later then let
  only the user pick the default, in Settings. It waits for an installer that
  writes it. ⛔

**Each open is a new window.** lattice has no single-instance handoff, so a
second PDF opened from the file manager starts a second process rather than a
tab in the first. That is acceptable for a viewer, and noted, because it is
the first thing a user doing this will see.

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
`gx` hands the file to the system viewer. In the TUI a page is one label
line and reserves no rows (PF.3a), and `j` / `k` still move by page.

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
