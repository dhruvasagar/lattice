---
summary: "oil-global-mode: the `-` key — from any buffer, open the directory it lives in."
related: [oil-mode, file-tree-mode, ex:Oil]
---

# oil-global-mode

One key, everywhere: `-` shows you the directory the thing in front of
you lives in, as an [oil](help:oil-mode) listing.

It is a minor mode that is on in every buffer. You do not turn it on;
it is how `-` reaches buffers that are not directory listings
themselves.

## What `-` does

| You are in | `-` |
|------------|-----|
| A file | Opens oil on the file's directory, with the cursor on that file. |
| An [oil](help:oil-mode) listing | Steps to the parent directory, with the cursor on the directory you left. |
| A [file tree](help:file-tree-mode) row | Opens oil on the row's directory — the directory itself for a directory row, the parent for a file row. |
| Anything else | Opens oil as a bare `:Oil` would: on the current file's directory when there is one, otherwise the working directory. |

Because the cursor lands on where you came from, `-` then `<CR>`
round-trips: press `-` in a file, look around, press `<CR>` on the same
row and you are back in the file.

`:Oil` with no argument does the same as `-` from a file. `:Oil
/some/dir` opens that directory with the cursor on the first row.

## Seeing it

`:describe-key -` describes the binding, and `<C-h> m` lists
`oil-global-mode` in the buffer's mode stack.
`:describe-mode oil-global-mode` describes the mode itself.

## Related

- [`oil-mode`](help:oil-mode) — the listing `-` opens, and how to edit
  a directory through it.
- [`file-tree-mode`](help:file-tree-mode) — the read-only tree view.
