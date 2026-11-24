---
summary: "image-mode: the major mode for a buffer whose file is a picture — `:e diagram.png` shows the image."
related: [buffers, read-only-mode, viewer-mode, pdf-mode, display]
---

# image-mode

The major mode for a buffer whose file is a **picture**.

You don't invoke it. Open an image the way you open anything else —
`:e diagram.png`, `<CR>` on it in the file tree, a picker — and the
buffer comes up in `image-mode`, showing the image instead of failing
on bytes that are not text.

| | |
|---|---|
| Activates on | `png`, `jpg`, `jpeg`, `gif`, `webp`, `svg`, `bmp` |
| Implies | [`read-only-mode`](help:read-only-mode), [`viewer-mode`](help:viewer-mode) |
| Contributes | ``ReadOnly`` |

## It is an ordinary buffer

An image buffer is listed by `:ls`, reached by `:bn` / `:bp` / `:b`,
named in the modeline and closed with `:bd`, like any other. It splits
and moves between panes the same way, so a diagram can sit beside the
code it describes.

The picture is drawn at its own size and scaled **down** to fit the
window when it is larger — in both directions, so a tall screenshot or a
portrait photo shows whole instead of running off the bottom of the pane.
It is never scaled up: an icon blown up to fill the window is worse than
the icon. Resize the pane and the picture is fitted again.

## Read-only, and why

The buffer does not hold the file's bytes — it holds one empty line,
with the picture drawn below it. So the buffer's text is *not* the
file, and writing it back would replace the image with nothing.

That is why every way of changing it is refused:

- typing in Insert mode,
- the operators — `x`, `dd`, `cw`, `p`,
- `:w` with no argument, which answers *buffer is read-only*.

To change an image, edit it in an image editor (`gx` opens it in the
system's viewer). When the file changes on disk, the buffer reloads by
itself and draws the new picture, with the old one shown until it is
ready; `R` reloads by hand.

## In the terminal

The GUI build draws the picture. The terminal build cannot, and shows
the file's **name** in its place — the buffer still opens, lists and
closes normally, so a picture in your project never breaks navigation,
it just isn't drawn.

## Keybindings

None of its own. [`viewer-mode`](help:viewer-mode) brings the ones every
viewer shares: `q` closes the picture, `R` reloads it, `gx` opens it in
the system's viewer. Motions, splits, and buffer commands are the usual
ones.

## See also

- [`buffers`](help:buffers) — listing, switching and closing buffers.
- [`read-only-mode`](help:read-only-mode) — the minor that refuses the
  operators here.
- [`viewer-mode`](help:viewer-mode) — `q`, `R` and `gx`.
- [`pdf-mode`](help:pdf-mode) — the same shape, one page per line.
- [`display`](help:display) — display options, per buffer.
