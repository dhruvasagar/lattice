---
summary: "picker-lsp-locations: the list the language server's answers open in — definitions, references, symbols, call and type hierarchies, diagnostics, and code actions."
related: [lsp]
---

# The LSP result pickers

When a language-server request answers with **more than one** result,
the answers open in this picker. One answer jumps straight there; none
says so ("no definition found").

It is also the list for a few things that are not places at all — code
actions, completions, code lenses, colour alternatives — and for the
[error list](help:error-list). The keys are the same; what `<CR>` does
depends on what the rows are.

---

## What opens it

**Places** — every row is a `file:line:col`:

| From | Title |
|---|---|
| `gd` / `gD` / `gy` / `gI` with several results | `lsp:definition`, `lsp:declaration`, … |
| `gr` | `references: <symbol>` (`:lsp-references` opens an editable [multibuffer](help:multibuffer-mode) instead) |
| `:lsp-symbols` | `symbols (N)` — this file's symbols, from the server |
| `:lsp-workspace-symbol` | the project's symbols |
| `:lsp-incoming-calls` / `:lsp-outgoing-calls` | the call hierarchy |
| `:lsp-supertypes` / `:lsp-subtypes` | the type hierarchy |
| `:diagnostics` / `:diag` | `diagnostics (N)` — every diagnostic, `[E]` / `[W]` / `[I]` / `[H]` and the message |
| `:clist` / `:cl` | `error list (N)` — see [`error-list`](help:error-list) |

**Choices** — rows are things to apply, not places: `:lsp-code-action`
(`code-actions (N)`), `:lsp-complete` (`complete (N)`), `:lsp-code-lens`
(`code-lens (N)`) and `:lsp-color-presentation`
(`color alternatives (N)`). `<CR>` applies the one you pick.

---

## Keys in this picker

| Key | Here |
|---|---|
| *(type)* | Filter, fuzzily — by path, symbol name or message |
| `<CR>` | **Places:** jump there (`<C-o>` brings you back). **Choices:** apply it |
| **`<C-s>`** / **`<C-v>`** / **`<C-t>`** | **Places:** jump there in a new horizontal split / vertical split / tab. **Choices:** same as `<CR>` |
| **`<C-q>`** | **Places:** send every row still listed to the error list, then close — narrow the references to one directory, then `:cn` through them. **Choices:** nothing to send, and it says so |
| `<C-n>` / `<C-p>`, `<Down>` / `<Up>`, `<Tab>` / `<S-Tab>` | Move the selection |
| `<BS>` / `<C-w>` | Delete a character / the previous word of the query |
| `<C-r>` | Append something from your yank history to the query |
| `<Esc>` / `<C-c>` | Close; you stay where you were, and nothing is applied |
| `<C-h>` | This page |

`<C-l>` and `<C-d>` do nothing here.

---

## What is listed

Place rows read `path:line:col  <the line's text>` (symbol rows:
the symbol's icon, name and container), with a severity marker in front
where there is one. Lines and columns are 1-based. For navigation
results the prompt names the project they belong to.

**Preview** — for places only: moving the selection shows the file in
the active pane, centred on the line, without opening it — no language
server starts for a previewed file. Choices have no preview.

There is no recency ranking: rows keep the order the server sent.

---

## See also

- [`lsp`](help:lsp) — the language-server integration.
- [`lsp-nav-mode`](help:lsp-nav-mode), [`lsp-symbols-mode`](help:lsp-symbols-mode),
  [`lsp-code-action-mode`](help:lsp-code-action-mode) — the requests
  behind these lists.
- [`error-list`](help:error-list) — where `<C-q>` sends places.
- [`picker`](help:picker) — keys and options shared by every picker.
