---
summary: "picker-colorscheme: try every theme live as you move through the list; <CR> keeps one, <Esc> puts yours back."
related: [colorscheme, ex:colorscheme]
---

# The `colorscheme` picker

Every registered theme. **The whole editor recolours as you move the
selection** — you are looking at the real thing, not a swatch. `<CR>`
keeps the theme you are on; `<Esc>` puts back exactly what you had.

Open it with `:colorscheme` (or `:colo`) with no name, or
`:picker colorscheme`. `:colorscheme <name>` switches directly, with
`<Tab>` completing the names.

---

## Keys in this picker

| Key | Here |
|---|---|
| *(type)* | Filter theme names |
| `<C-n>` / `<C-p>`, `<Down>` / `<Up>`, `<Tab>` / `<S-Tab>` | Move the selection — **and recolour the editor to that theme** |
| `<CR>` | Keep the theme |
| `<Esc>` / `<C-c>` | Close and **restore** the theme you had before opening |
| `<BS>` / `<C-w>` | Delete a character / the previous word of the query |
| `<C-h>` | This page |

`<C-s>` / `<C-v>` / `<C-t>` behave like `<CR>`. `<C-q>` has no location
to send. `<C-l>` and `<C-d>` do nothing.

---

## What is listed

Theme names in the order they were registered. The editor recolours as soon as
the picker opens, to the first row.

Themes you keep often rank a little higher next time. Keeping one here
lasts for the session; it is not saved across restarts yet, so set it
from your `init` to make it stick — see [`themes`](help:themes).

---

## See also

- [`themes`](help:themes) — theme files, defaults and overrides.
- [`picker`](help:picker) — keys and options shared by every picker.
