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
| PF.0  | Spike: hayro against a corpus — fidelity, time per page, memory      | ✅     |
| PF.1  | `lattice-media` `pdf` feature: probe and render a page               | ✅     |
| PF.2  | `pdf-mode`: presents `.pdf`, one label line per page, read-only      | ✅     |
| PF.3a | A media block per page, sized from its points before it is drawn    | ✅     |
| PF.3b | GPUI draws the pages: near the viewport, two passes, byte-bounded cache | ✅     |
| PF.4a | The keymap: page, screen and step navigation; `q`; `page N/M`      | ✅     |
| PF.4b | Fit width, zoom, rotate, `h` / `l`: scrolling inside a page        | ⛔     |
| PF.5  | Reload in place on change                                            | ✅     |
| PF.6  | System viewer, docs, benchmarks                                      | 📝     |
| PF.11 | Lattice as a candidate PDF viewer (Linux desktop entry)              | 📝     |
| PF.7  | Text: search, selection, copy                                        | ⛔     |
| PF.8  | Links and the outline                                                | ⛔     |
| PF.9  | Inline PDFs in org (first page)                                      | ⛔     |
| PF.10 | Recolour for dark themes                                             | ⛔     |
| PF.12 | Default-viewer registration on macOS and Windows                     | ⛔     |

## Ordering

PF.0 is a go / no-go on hayro: the whole plan assumes it, and the fallback
(pdfium, design §7) changes PF.1's dependency and the release artefacts. PF.1
is the renderer alone, testable without any mode. PF.2 makes `:e x.pdf` safe in
both builds before a single page draws, so a PDF is never read into a rope.
PF.3 is the first slice a user sees pages in. PF.4 and PF.5 are independent of
each other. PF.6 closes the four artefacts (design doc, bench, tests, docs).
PF.7–PF.10 wait until reading works.

## Slices

### PF.0 — spike ✅ (2026-11-24: go, with a two-pass render)

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

**Result.** The corpus was 10 public files: the *Attention* arXiv paper
(pdflatex), pdf.js's `tracemonkey`, `alphatrans`, `160F-2019` (a form),
`ArabicCIDTrueType` and `TAMReview`, the metropolis beamer demo and its
manual, a JPEG scan made from a page, and the beamer user guide twice over
(472 pages) for the probe. There was no TeX on the machine, so no CJK file,
and no JBIG2 scan; both are gaps. The reference was poppler (`pdftoppm`), the
renderer at hand, not pdfium.

| measure | result |
|---|---|
| fidelity | no visible difference from poppler on any page compared (paper, attention-map figure, transparency, form, Arabic CID, beamer gradients and icons, scan); no page panicked |
| probe | 0.1–0.4 ms per file; 3.3 ms for 472 pages |
| render at 1600 px (1×) | text pages 1–9 ms; worst page 75 ms (dense vector figure) |
| render at 3200 px (2×) | text pages 22–34 ms; three figure pages 108–330 ms |
| poppler, same pages, 3200 px | 0.2–1.9 s, including process start and PNG encoding |
| peak RSS | 59 MB at 1600 px; 191 MB over the corpus at 3200 px |

The 100 ms p99 bar holds at 1× and fails at 2× on figure-heavy pages, so the
go was conditional, and Dhruva's call: **go with hayro, rendering each page
in two passes** (design §3), a 1× pass that lands in milliseconds and a 2×
pass that replaces it. The slow page then shows as a soft page that
sharpens, never as a wait. pdfium might be faster, but nothing here measured
that, and its cost (a native library in every artefact) is permanent.

Two facts for PF.1: `hayro_syntax::Pdf` is `Send + Sync`, so one parsed
document per buffer can be shared across blocking threads. `RenderCache` holds
`Rc`s and is not `Send`, so it lives on the thread that renders and is
never shared.

### PF.1 — the renderer (`lattice-media`) ✅

A `pdf` cargo feature on `lattice-media` (off by default), enabled by
`lattice-cli`'s `gui`. Two functions behind it: `probe(path) -> PdfInfo { pages:
Vec<Size<pt>> }` and `render(path, page, target_px, rotation) -> Raster`,
both blocking, both for `spawn_blocking`. Outputs the same pixel formats as the
SVG route, with the same premultiplied-alpha handling.

**As built.** `lattice_media::pdf::PdfDocument`: `open` / `from_bytes` parse
and measure (`info().pages`, points), and `render_page(page, target, format)`
draws one page on white. A handle rather than a path per call, because the
parse is shared across pages and threads (`Pdf` is `Send + Sync`). The
handle is what PF.3 keeps per buffer. `page_fit` scales up as well as down: a
page has no natural pixel size, so the picture route's never-upscale rule
does not apply. It refuses past `MAX_PIXELS` and 65 535 px a side. A panic
in the parser or on a page is caught and becomes `Undecodable`, that page's
alt text. **Rotation moved to PF.4**, with the `r` key that is its only
user. The pixmap is taken, not copied (28% faster, see
[`benchmarks.md`](../benchmarks.md)). The feature is implied by
`lattice-ui-gpui`'s `window`, as `system-clipboard` is, so `gui` gets it and a
TUI-only `lattice-cli` has no hayro in its tree. Checked with `cargo tree`.

Tests: probe a fixture's page count and sizes; render a known pixel (both
alpha directions, as for SVG); a corrupt file and an out-of-range page are
errors, not panics; a TUI-only build compiles without hayro in the tree
(`cargo tree -p lattice-cli` check). A bench for probe and render.

### PF.2 — `pdf-mode` ✅

A major in `lattice-mode` beside `image-mode`: `presents_extensions = ["pdf"]`,
the three read-only gates, the probe through the mode's `AsyncMediaSource` on
activate, and the label lines written by the owner when the count lands. Without
the `pdf` feature: one line naming the GUI and `o`.

Tests: `:e x.pdf` never reads the file into a rope; the buffer passes
`multibuffer_is_a_regular_buffer.rs`'s regular-buffer checks (`:ls`, `:bn`,
`:bd`, modeline); `dd`, `x`, insert and `:w` are refused, and `:w other.pdf`
is allowed; the label lines land without a keypress; page 1's line is on the
first frame.

**As built.** `lattice_mode::modes::PdfMode`, registered with the foundation
modes. The probe is a service, `PdfProbeHandle`: `lattice-mode` cannot depend
on `lattice-media` (the codec and PDF graph that crate keeps out of nearly
every build), so the host implements it over `lattice_media::pdf_page_sizes`
(`lattice-host/src/pdf_probe.rs`). The host registers it only when
`lattice_media::PDF_SUPPORTED`. `pdf_page_sizes` exists in every build, so the
host needs no feature of its own. `on_activate` returns at once. A task on
the LSP runtime reads the sizes on a blocking thread, replaces the buffer's
text, `mark_clean`s it (or every PDF would open modified and `:q` would
refuse) and publishes `PdfPagesLabelled`, whose boot-registered wake puts the
labels on screen. No probe service gives one line naming the build; a broken
file gives one line naming the file and the reason.

Tests changed from the list above. `:w other.pdf` is not "allowed": it would
write the page list into a file named `.pdf`, and the read-only refusal
covers it, as it does for `image-mode`. Counted `G` is not tested at the
host: counts are attached by the renderer peer's keymap before a chord
reaches `Editor`, and `G` / `gg` / `j` / `k` already show that pages are
lines. "Page 1's line on the first frame" is line 0, which the presented
buffer has before anything is read. Its *label* lands with the others.
The labels-without-a-keypress test was checked against a build with the
wake removed, and **still passed**, because boot's other wakes covered it. So a
second test pins the wake alone: the write announces itself, and the
announcement wakes a quiet editor. That one fails without the wake.
`lattice-host`'s tests enable `lattice-media/pdf` through a dev-dependency, so
the probe they open files with is the real one.

### PF.3 — pages ✅

One media block per label line, named `MediaSource::PdfPage { path, page }`
(design §1); render only the pages in the viewport plus one either side, in
two passes, 1× then the display scale (design §3); one parsed document per
path and `mtime`, cached in `lattice-media`; the byte-bounded LRU (`media.cache-mb`, one cache for pictures and
pages, keyed by `MediaSource`); every block sized from the
probe before its raster exists; the previous raster kept as the placeholder.

Tests: scrolling a 200-page file renders only nearby pages (counted at the
render seam); the cache never exceeds its budget; the document's total height
is the same before and after the pages land (no reflow); a page that fails
shows its alt text and error while its neighbours render. GPUI and TUI updated
together (the TUI's labels and alt, by snapshot).

Split in two, each landing green. **PF.3a ✅** is the substrate, and needs no
renderer code. `MediaSource::PdfPage { path, page }` and `MediaFit::Page` are
`Window`, scaling up as well as down. `MediaBlockRequest` now carries a
`source` instead of a `path`, the same type the sized block holds.
`MediaBlock::from_source` falls back to `page N of name.pdf` for its alt text,
and `MediaBlock::path()` is `None` for a page, so the picture decoder is
never handed a PDF. `pdf-mode`'s `PdfPagesMediaSource` produces one block
per labelled line, and nothing before the labels or for the one-line error.
The host's sizing pass measures pages in points, through one parse per PDF
per pass. **A page reserves no rows on a peer that draws no pixels**: the
TUI's rendering is the label line, and a picture-style alt row under every
label would make the list twice as long or more. So the TUI shows exactly
what PF.2 showed. Until PF.3b, GPUI shows each page's box with its alt text.
**PF.3b ✅** is the GPUI peer.
- **The cache.** `MediaPixels` is keyed by `MediaSource` and bounded by
  `media.cache-mb` (default 256), which is read live each frame. It evicts
  whatever was drawn least recently. Anything drawn in this frame or the
  last is spared, so a budget smaller than a screenful is exceeded rather
  than made to flicker.
- **Re-rendering.** Each entry records the block height it was decoded for.
  A resized block is decoded again while the old pixels keep drawing, and a
  landed decode releases its claim so that can happen. A failed decode stays
  claimed, so it is not retried every frame.
- **Rendering pages.** `lattice_media::render_pdf_page` exists in every
  build and draws through `pdf::DOCUMENTS`, a cache of four parsed documents
  keyed by path and mtime, so a re-export is parsed afresh. GPUI renders a
  visible page, then the one after and the one before it, borrowing the
  visible page's box.
- **Two passes.** The first draws at logical size and lands at once. The
  second draws at `window.scale_factor()` and replaces it. A 1× display
  skips the second pass.
- **Verified live.** `lattice --gui man.pdf` (a 6-page man page) rendered
  pages 1–2 at 257×363 in about 10 ms each, debug build, logged at `debug`
  as "PDF page rendered". The run turned up a bug, fixed in its own commit:
  the command line read the file as text, never asking whether a major
  presents it. `ModeRegistry::open_document` is now shared with `:e`.
  WSLg does not let X capture GPUI's surface, so the pixels themselves are
  Dhruva's to look at.
- **Deferred to PF.6.** No bench yet for the virtual-row cost of a long
  document, nor for the cache under scrolling.

### PF.4 — the keymap (PF.4a ✅, PF.4b ⛔)

**PF.4a ✅** is the navigation half, and needs nothing new from the core's
scroll model.
- **Keys.** `J` / `K`, `<Space>` / `<S-Space>` and `<PageDown>` / `<PageUp>`
  are `pdf-mode` keymap entries naming existing commands (`motion:line-*`,
  `action:page-*`), so none has a body. The builtin `<PageDown>` is a
  10-line motion, which would be ten pages, so `pdf-mode` rebinds it.
- **`q`** is on `viewer-mode`, a new minor that `image-mode` and `pdf-mode`
  both imply (the shared-behaviour rule). It is `Effect::KillBuffer`, which
  is `:bd!` for a buffer that is not an in-pane view; magit's
  `BuryBuffer` is a no-op on a buffer opened with `:e`.
- **`:bd`'s successor** is now the newest live listed buffer in the pane's
  trail (vim walks the jumplist), then the old first-listed rule. Before
  this, `:e notes.txt`, `:e paper.pdf`, `q` landed on whichever buffer had
  the lowest id.
- **`page N/M`** comes from a new option, `ui.modeline.position-format`
  (vim's `'rulerformat'`, `{line}` / `{col}` / `{lines}`), which `pdf-mode`
  overrides. No PDF code in the host's modeline.
- **Screen scrolls charge media rows.** `weighted_doc_line_cost` now charges
  a line the rows its media blocks reserve, through `WasmMediaCache`'s new
  `rows_by_anchor`, when `cell_metrics` says a pixel peer is attached. This
  fixes the GUI's `<C-f>` overshooting any document with pictures: IM.5
  built `media_row_weights` and `Editor::row_weights`, but nothing ever set
  the field. `do_page` moves at least one line, as vim's does, and with
  media it puts the line turned to at the top. `do_half_page` walks half a
  screen of rows when media is present, and is unchanged otherwise.
- **Tests** (`opening_a_pdf_file.rs`): the page keys; vim's `<C-f>` /
  `<C-b>`, `<PageDown>` / `<PageUp>`, `<C-e>` / `<C-y>` each turn one page in
  fit page; `q` closes the buffer, returns to the text file, and leaves `J`
  joining there; the modeline reads `page 3/12` and still `1:0` in text.
- **Counts on `J` / `K` are not driven by a host test.** As PF.2 found for
  `{n}G`, the peer's translate step attaches counts, and it does so for
  every normal-mode binding whatever layer resolved it
  (`lattice_host::input::translate_normal` → `attach_count`). `J` names the
  same command as `j`.

**PF.4b ⛔ deferred** (Dhruva, 2026-11-24: fit page only for now). Fit
width, `+` / `-` / `=`, `s` / `a` and `h` / `l` all make a page larger than
the window, and the core cannot scroll inside one line. The decision this
needs when it comes back: the recommended answer was a core sub-line row
offset (vim's `skipcol` / `smoothscroll`, in rows, so the TUI shares it),
against a GPUI-local offset driven by an effect (split state between core and
peer) or tiling a page into several lines (pages stop being lines).
**Rotation (`r`) is deferred with it.** It needs no sub-line scrolling, but
the page producer sees only the buffer's text, so the rotation must reach the
host's media refresh. The recommended shape was a generic per-buffer
`media.rotation` with `r` on `viewer-mode`, rotating pictures too, against a
pdf-only option the host would have to know by name. It comes back with GUI
zoom, which the same refresh-key and pixel-cache-key work serves. `R` moves
to PF.5 (reload), `gx` stays PF.6.

The original PF.4 text, kept for the test list it set:

Design §2's table, on `pdf-mode`'s major layer with its handler bodies in
`lattice-mode`: `J` / `K`; `<Space>` / `<S-Space>`, `<C-f>` / `<C-b>`,
`<PageDown>` / `<PageUp>` and `<C-d>` / `<C-u>`, which scroll by screen and
half screen and continue onto the next page; `h` / `l`; `s`, `a`, `+`, `-`,
`=`, `r`, `R`, `q`; and the modeline's `page 3/12`. `j` / `k`, `gg` / `G`,
counts, `:{n}`, marks and the jump list need nothing: pages are lines.
`pdf.zoom` is a buffer-local option in the media refresh key; fit width
scrolls within a page through `sub_row_px`. Before binding `q`, check
whether a shared minor already owns "close this view" (the shared-behaviour
rule), and use it rather than declaring `q` again.

Tests: every key in the table does what the table says, in both fits; a
screen scroll in fit width crosses from the bottom of one page to the top of
the next; each zoom key changes the option and re-renders at the new size;
zoom keeps the old raster on screen until the new one lands (no blank frame);
in fit width, `<C-e>` scrolls inside a page taller than the window and `j`
still moves to the next page; in the TUI, a screen scroll is one page; the
modeline shows the cursor's page.

### PF.5 — reload ✅

On a changed `mtime`: invalidate the probe and the path's cached pages, keep
the cursor's page and the scroll, keep old rasters as placeholders, clamp
when the page count drops.

Tests: rewriting the file re-renders at the same page with no keypress; a
shorter file clamps the cursor; an export loop (write, reload, write)
never shows a blank page.

**As built** (design §5, "As built"): autoread's presented-file gate, the
reload's text carry-over, `MediaBlock::revision`, the revision-aware GPUI
pixel cache, and `R` on `viewer-mode`. Pictures share all of it.
- **Tests.** `autoread_reloads_a_presented_file_instead_of_reading_it_as_text`
  (host unit, fails without the gate: the old "no longer available on disk");
  `a_reload_carries_the_majors_text_and_the_cursor_over` (a picture, so no
  label race; fails without the carry-over); `a_reload_stays_on_the_page_and_redraws_it`
  and `a_reload_with_fewer_pages_clamps_to_the_last` (PDF, through `R`);
  `a_new_revision_is_decoded_again_with_the_old_pixels_as_placeholder` (GPUI
  cache, including a late old-revision landing).
- **The no-jump race.** With a 12-page test PDF the probe usually relabels
  the fresh buffer before the cursor is restored, so the PDF test passes
  with or without the carry-over; the picture test is the one that pins it.
  A long PDF loses that race, which is what the carry-over is for.
- **Found after: a shrinking rewrite left the caret past the end.** With
  the carry-over the cursor is restored on line 11 of the old 12 labels;
  when the 3-page labels then land, nothing clamped it, because a bulk owner
  write is deliberately not adopted (the dashboard rule). `run_tick_pending`
  now clamps a caret that is past the last line, and only then. Pinned in
  `anchored_append_caret.rs` (deterministic); the PDF clamp test only hit it
  when it lost the label race.
- **Not built: skipping an identical relabel.** Replacing the labels with
  themselves after a same-count reload moved nothing the tests could see,
  so the check was dropped rather than shipped unproven.
- **"No keypress" is autoread's own**, already tested by AR.4; the
  PDF tests drive the reload with `R` because the watcher is a real
  filesystem watch.

### PF.6 — finish 📝

`gx` (`open-external-uri`); `:help pdf-mode`, with the keymap and the
default-viewer commands of design §5a; `docs/user/` page; CHANGELOG
Added; probe and render benches recorded in
[`benchmarks.md`](../benchmarks.md); `inline-media.md` §8.1 points to
`pdf-mode.md`.

### PF.11 — a candidate PDF viewer on Linux 📝

`application/pdf` added to the desktop entry's `MimeType`, so the `.deb` and
the AppImage put lattice under "Open with" and change no default (design
§5a). `:help pdf-mode` gives the `xdg-mime` line that makes it the default.
The `dist` leg's `.deb` assertions check the MIME type is present.

### PF.12 — macOS and Windows ⛔

Waits for a `.app` (macOS: an `Info.plist` document type) and an installer
(Windows: a per-user association, with the default left to Settings).
Neither exists in v1.

### PF.7–PF.10 — deferred ⛔

Design §6, each with what it waits for: text extraction (PF.7) before links
and the outline (PF.8); PF.9 is one line in org's media producer once PF.3
lands; PF.10 is a render option, when a dark-theme user asks.
