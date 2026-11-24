---
summary: "viewer-mode: the keys every file viewer shares — `q` closes it, `R` reloads it, `gx` opens it in the system's viewer."
related: [pdf-mode, image-mode, buffers]
---

# viewer-mode

The keys every buffer that **shows** a file, rather than editing it,
has in common.

You don't turn it on. [`image-mode`](help:image-mode) and
[`pdf-mode`](help:pdf-mode) bring it with them, so a picture and a PDF
answer to the same keys, and a future viewer will too.

| | |
|---|---|
| Implied by | [`image-mode`](help:image-mode), [`pdf-mode`](help:pdf-mode) |
| Contributes | keys only |

## Keybindings

| Key | What it does |
|---|---|
| `q` | close the file's buffer, as `:bd` — the pane goes back to what it showed before |
| `R` | read the file again, as `:e!`, keeping your place |
| `gx` | open the file in the system's own viewer (`xdg-open`, `open`, `explorer`) |

These replace vim's meanings in a viewer only: there is nothing to
record a macro of (`q`) or to replace (`R`) in a buffer you cannot edit,
and the file itself is what `gx` would open. Everywhere else the keys
are vim's.

A viewer reloads on its own when its file changes on disk; `R` is for
the times a watcher cannot see the change.

## See also

- [`pdf-mode`](help:pdf-mode), [`image-mode`](help:image-mode) — the
  viewers.
- [`buffers`](help:buffers) — `:bd` and the buffer list.
