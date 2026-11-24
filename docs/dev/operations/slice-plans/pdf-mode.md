# Slice plan — PDF mode

Design: [`pdf-mode.md`](../../architecture/pdf-mode.md). Read it first; this
plan argues from it and does not repeat it. It builds on the inline-media
substrate ([`inline-media.md`](../../architecture/inline-media.md), plan
archived at [`archive/inline-media.md`](archive/inline-media.md)).

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred (not yet) · ❌ dropped (not at all).

All slices land in this tree. Every slice that touches drawing updates **both
peers in the same patch**: GPUI paints pages, the TUI paints labels and alt
text (design §8).

| Slice | Description                                                         | Status |
|-------|---------------------------------------------------------------------|--------|
| PF.0  | Spike: hayro against a corpus — fidelity, time per page, memory      | 📝     |
| PF.1  | `lattice-media` `pdf` feature: probe and render a page               | 📝     |
| PF.2  | `pdf-mode`: presents `.pdf`, one label line per page, read-only      | 📝     |
| PF.3  | A media block per page, rendered near the viewport, byte-bounded cache | 📝   |
| PF.4  | Fit, zoom, rotate; scroll within a page                              | 📝     |
| PF.5  | Reload in place on change                                            | 📝     |
| PF.6  | System viewer, docs, benchmarks                                      | 📝     |
| PF.7  | Text: search, selection, copy                                        | ⛔     |
| PF.8  | Links and the outline                                                | ⛔     |
| PF.9  | Inline PDFs in org (first page)                                      | ⛔     |
| PF.10 | Recolour for dark themes                                             | ⛔     |

## Ordering

PF.0 is a go / no-go on hayro: the whole plan assumes it, and the fallback
(pdfium, design §7) changes PF.1's dependency and the release artefacts. PF.1
is the renderer alone, testable without any mode. PF.2 makes `:e x.pdf` safe in
both builds before a single page draws, so a PDF is never read into a rope.
PF.3 is the first slice a user sees pages in. PF.4 and PF.5 are independent of
each other. PF.6 closes the four artefacts (design doc, bench, tests, docs).
PF.7–PF.10 wait until reading works.

## Slices

### PF.0 — spike 📝

Render every page of a small corpus with hayro 0.9 at 1600 px wide and 2×:

- a `latexmk` article (the export case: Type 1 / CFF fonts, maths),
- a beamer deck (shadings, transparency),
- a scanned document (JPEG and JBIG2 images),
- a fillable form, and
- a document in a non-Latin script with embedded CID fonts.

Record, per document: pages that render wrongly (compared by eye against
pdfium's output), the p50 / p99 time per page, peak memory, and the time to
probe (count plus sizes) a 500-page file. **Go** if the export case and the
deck render correctly and a page renders in under 100 ms p99 on the dev
machine. Otherwise this plan switches to pdfium before PF.1, and the decision
and the numbers are recorded here. Throwaway code.

### PF.1 — the renderer (`lattice-media`) 📝

A `pdf` cargo feature on `lattice-media` (off by default), enabled by
`lattice-cli`'s `gui`. Two functions behind it: `probe(path) -> PdfInfo { pages:
Vec<Size<pt>> }` and `render(path, page, target_px, rotation) -> Raster`,
both blocking, both for `spawn_blocking`. Outputs the same pixel formats as the
SVG route, with the same premultiplied-alpha handling.

Tests: probe a fixture's page count and sizes; render a known pixel (both
alpha directions, as for SVG); a corrupt file and an out-of-range page are
errors, not panics; a TUI-only build compiles without hayro in the tree
(`cargo tree -p lattice-cli` check). A bench for probe and render.

### PF.2 — `pdf-mode` 📝

A major in `lattice-mode` beside `image-mode`: `presents_extensions = ["pdf"]`,
the three read-only gates, the probe through the mode's `AsyncMediaSource` on
activate, and the label lines written by the owner when the count lands. Without
the `pdf` feature: one line naming the GUI and `o`.

Tests: `:e x.pdf` never reads the file into a rope; the buffer passes
`multibuffer_is_a_regular_buffer.rs`'s regular-buffer checks (`:ls`, `:bn`,
`:bd`, modeline); `dd`, `x`, insert and `:w` are refused, and `:w other.pdf`
is allowed; the label lines land without a keypress; page 1's line is on the
first frame.

### PF.3 — pages 📝

One media block per label line; render only the pages in the viewport plus one
either side; the byte-bounded LRU (`pdf.cache-mb`); every block sized from the
probe before its raster exists; the previous raster kept as the placeholder.

Tests: scrolling a 200-page file renders only nearby pages (counted at the
render seam); the cache never exceeds its budget; the document's total height
is the same before and after the pages land (no reflow); a page that fails
shows its alt text and error while its neighbours render. GPUI and TUI updated
together (the TUI's labels and alt, by snapshot).

### PF.4 — fit, zoom, rotate 📝

`s`, `a`, `+`, `-`, `=`, `r`, `J`, `K` on the major's layer; `pdf.zoom` as a
buffer-local option in the media refresh key; fit width scrolls within a page
through `sub_row_px`.

Tests: each key changes the option and re-renders at the new size; zoom
keeps the old raster on screen until the new one lands (no blank frame); in
fit width, `<C-e>` scrolls inside a page taller than the window and `j`
still moves to the next page.

### PF.5 — reload 📝

On a changed `mtime`: invalidate the probe and the path's cached pages, keep
the cursor's page and the scroll, keep old rasters as placeholders, clamp
when the page count drops.

Tests: rewriting the file re-renders at the same page with no keypress; a
shorter file clamps the cursor; an export loop (write, reload, write)
never shows a blank page.

### PF.6 — finish 📝

`o` (`open-external-uri`); `:help pdf-mode`; `docs/user/` page; CHANGELOG
Added; probe and render benches recorded in
[`benchmarks.md`](../benchmarks.md); `inline-media.md` §8.1 points to
`pdf-mode.md`.

### PF.7–PF.10 — deferred ⛔

Design §6, each with what it waits for: text extraction (PF.7) before links
and the outline (PF.8); PF.9 is one line in org's media producer once PF.3
lands; PF.10 is a render option, when a dark-theme user asks.
