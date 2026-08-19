---
summary: "lsp-references-mode: identity marker for a references view — a multibuffer with this minor active IS the references list, and it is editable."
related: [lsp, multibuffer-mode]
---

# lsp-references-mode

An identity marker. A multibuffer with this minor active **is** a
references view — that is the whole of what the mode asserts, and it is
what lets the rest of the editor recognise the buffer without anything
branching on a buffer kind.

## It is editable, deliberately

There is no read-only override here. Edits you make in the references
list propagate to the source files through the ordinary multibuffer
pipeline, which is the point of the surface: find every caller, fix them
where you are looking at them, without opening seven files.

## Keybindings

None of its own. The view is a multibuffer, so excerpt motions
(`]e` / `[e`, `]E` / `[E`) and `<CR>` to jump to source all work as they
do in any other multibuffer.

## Options

None.

## See also

- [`multibuffer`](help:multibuffer-mode) — excerpts, jumping, and editing.
