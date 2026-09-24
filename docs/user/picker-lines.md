---
summary: "picker-lines: fuzzy-filter the current buffer's lines and jump to one, with the buffer previewed in place."
related: [lines]
---

# The `lines` picker

Every line of the buffer you are in, filtered as you type. Moving the
selection scrolls the buffer to that line in place; `<CR>` puts the
cursor there.

Open it with `:picker lines`. It takes no arguments and reads the buffer
that was active when you opened it.

---

## Keys in this picker

| Key | Here |
|---|---|
| *(type)* | Filter lines, fuzzily; space separates fragments, `!word` excludes lines containing `word` |
| `<CR>` | Put the cursor at the start of that line |
| **`<C-s>`** / **`<C-v>`** / **`<C-t>`** | Open the **same buffer** in a new horizontal split / vertical split / tab, at that line — keep your place here and look at the match beside it |
| **`<C-q>`** | Send every line still listed to the error list, then close. Only for a buffer with a file on disk; a scratch buffer has no location to send |
| `<C-n>` / `<C-p>`, `<Down>` / `<Up>`, `<Tab>` / `<S-Tab>` | Move the selection — the buffer scrolls with it |
| `<BS>` / `<C-w>` | Delete a character / the previous word of the query |
| `<C-r>` | Append something from your yank history to the query |
| `<Esc>` / `<C-c>` | Close, with the buffer scrolled back to where it was |
| `<C-h>` | This page |

`<C-l>` and `<C-d>` do nothing here.

---

## What is listed

One row per line, **blank lines included**, in buffer order — the text
of the line (syntax-highlighted) with its 1-based line number on the
right. Unlike `/`, the match is fuzzy and multi-word: `err ret` finds
`return Err(e)`.

Jumping records where you were, so `<C-o>` brings you back.

---

## See also

- [`outline`](help:picker-outline) — jump by symbol rather than by line.
- [`grep`](help:picker-grep) — the same idea across every file.
- [`picker`](help:picker) — keys and options shared by every picker.
