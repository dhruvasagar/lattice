---
summary: "pdf-mode: the major mode for a buffer whose file is a PDF — `:e paper.pdf` shows its pages, one per line."
related: [viewer-mode, image-mode, buffers, read-only-mode]
---

# pdf-mode

The major mode for a buffer whose file is a **PDF**.

You don't invoke it. Open a PDF the way you open anything else —
`:e paper.pdf`, `lattice paper.pdf`, `<CR>` on it in the file tree, a
picker — and the buffer comes up in `pdf-mode`, showing its pages.

| | |
|---|---|
| Activates on | `pdf` |
| Implies | [`read-only-mode`](help:read-only-mode), [`viewer-mode`](help:viewer-mode) |
| Contributes | ``ReadOnly``, ``ui.modeline.position-format`` |

## A page is a line

The buffer's text is one label per page — `Page 3 of 12` — and each
page is drawn below its own label, sized to fill the window. So the line
number **is** the page number, and everything vim does with lines it does
with pages, unchanged:

| Keys | What they do |
|---|---|
| `j` / `k`, `J` / `K` | next / previous page (a count moves that many) |
| `gg` / `G`, `{n}G`, `:{n}` | first / last / page *n* |
| `<Space>` / `<S-Space>`, `<C-f>` / `<C-b>`, `<PageDown>` / `<PageUp>` | a screen down / up: one page, with that page at the top |
| `<C-d>` / `<C-u>` | half a screen down / up |
| `<C-e>` / `<C-y>`, the wheel | a step down / up |
| `m{a-z}` / `` `{a-z} ``, `<C-o>` / `<C-i>` | marks and the jump list |
| `q` | close the PDF, as `:bd` ([`viewer-mode`](help:viewer-mode)) |
| `R` | read it again, as `:e!` |
| `gx` | open it in the system's PDF viewer |

The modeline shows `page 3/12` for the page under the cursor.

## It reloads by itself

When the file changes on disk — an export that writes it again, a
`latexmk` loop — the buffer reloads **where you were reading**: the same
page, the same scroll, and the old pages stay on screen until the new
ones are drawn. If the new file has fewer pages, the cursor moves to its
last page. `R` does the same by hand, for a file the watcher cannot see
(a network mount).

## Read-only, and why

The buffer holds the page labels, not the PDF. Writing that text back
would replace the PDF with a list of page numbers, so every way of
changing it is refused: typing, the operators, and `:w`.

## In the terminal

The GUI build draws the pages. The terminal build shows the labels
alone, one line per page, and no blank space where pages would be — so
`j` / `k` still move by page and `:42` still goes to page 42. `gx` hands
the file to the system's viewer to see it.

## Making lattice your PDF viewer

**Linux.** The `.deb` and the AppImage list lattice under "Open with"
for PDF files. Installing it changes nothing else: your PDFs keep opening
where they did. To make lattice the default:

```
xdg-mime default com.lattice-editor.lattice.desktop application/pdf
```

and `xdg-mime query default application/pdf` to see what it is now.
Each file opened this way starts its own window.

**macOS and Windows** are not offered yet: both need an application
bundle or an installer that v1 does not ship.

## Options

| Option | Default | What it does |
|---|---|---|
| `media.cache-mb` | `256` | GUI: megabytes of drawn pages and pictures kept, across all buffers. Past it, what was drawn longest ago goes first; what is on screen never does. |
| `ui.modeline.position-format` | `page {line}/{lines}` here | what the modeline's position shows, as vim's `'rulerformat'`: `{line}`, `{col}`, `{lines}` |

## Not yet

Fit width, zoom, rotation, and scrolling left and right within a page;
searching, selecting and copying text; links and the outline. A page is
always fitted to the window for now.

## See also

- [`viewer-mode`](help:viewer-mode) — the keys every file viewer shares.
- [`image-mode`](help:image-mode) — the same shape for a picture.
- [`buffers`](help:buffers) — listing, switching and closing buffers.
