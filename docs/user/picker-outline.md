---
summary: "picker-outline: jump to a function, type or other symbol in the current buffer, from its tree-sitter outline."
related: [outline]
---

# The `outline` picker

The symbols of the buffer you are in — functions, types, methods and
the like — as the tree-sitter parse sees them. Filter by name; `<CR>`
jumps to the symbol.

Open it with `:picker outline`. It takes no arguments.

It needs no language server; for the server's view of the same buffer
(richer kinds, containers), use `:lsp-symbols` — see
[`lsp-symbols-mode`](help:lsp-symbols-mode).

---

## Keys in this picker

| Key | Here |
|---|---|
| *(type)* | Filter symbol names, fuzzily |
| `<CR>` | Jump to the symbol (its line and column) |
| **`<C-s>`** / **`<C-v>`** / **`<C-t>`** | Open the same buffer in a new horizontal split / vertical split / tab, at the symbol |
| **`<C-q>`** | Send every symbol still listed to the error list, then close (a buffer with a file on disk only) |
| `<C-n>` / `<C-p>`, `<Down>` / `<Up>`, `<Tab>` / `<S-Tab>` | Move the selection — the buffer scrolls to each symbol |
| `<BS>` / `<C-w>` | Delete a character / the previous word of the query |
| `<C-r>` | Append something from your yank history to the query |
| `<Esc>` / `<C-c>` | Close, with the buffer scrolled back to where it was |
| `<C-h>` | This page |

`<C-l>` and `<C-d>` do nothing here.

---

## What is listed

Symbol names in the order they appear in the file, coloured as they are
in the buffer, with the 1-based line number on the right.

**It depends on the language having a symbols query.** A language
without one — or a buffer whose parse is empty — opens nothing and
says so: "outline: no symbols (language … has no tree-sitter query, or
the parse tree is empty)". The bundled languages have one: Bash, C,
C++, CSS, Go, HTML, Java, JavaScript, JSON, Lua, Python, Ruby, Rust,
SQL, TOML, TypeScript and YAML.

Jumping records where you were, so `<C-o>` brings you back.

---

## See also

- [`lines`](help:picker-lines) — jump by line text instead.
- [`lsp-symbols-mode`](help:lsp-symbols-mode) — the language server's
  outline.
- [`picker`](help:picker) — keys and options shared by every picker.
