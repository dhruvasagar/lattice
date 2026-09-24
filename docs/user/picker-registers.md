---
summary: "picker-registers: see every register's contents on one line each and paste the one you pick, like `\"xp`."
related: [registers]
---

# The `registers` picker

Every register with something in it — unnamed, named, numbered and the
clipboard — each shown as one line of its contents. `<CR>` pastes the
one you pick after the cursor, exactly as `"xp` would.

Open it with `:picker registers`. `:reg` / `:registers` is not this:
it prints the registers as text.

---

## Keys in this picker

| Key | Here |
|---|---|
| *(type)* | Filter by **contents** — find the yank that had `unwrap` in it |
| `<CR>` | Paste that register **after the cursor** (`"xp`) |
| `<C-n>` / `<C-p>`, `<Down>` / `<Up>`, `<Tab>` / `<S-Tab>` | Move the selection |
| `<BS>` / `<C-w>` | Delete a character / the previous word of the query |
| `<Esc>` / `<C-c>` | Close; nothing is pasted |
| `<C-h>` | This page |

`<C-s>` / `<C-v>` / `<C-t>` behave like `<CR>`. `<C-q>` has no location
to send and says so. `<C-l>` and `<C-d>` do nothing.

---

## What is listed

In register order: the unnamed register `""` first, then `a`–`z`, then
`0`–`9`, then the clipboard `+`. Each row is the register's contents
folded onto one line — line breaks shown as ` ⏎ `, long values cut at
120 characters with `...`, whitespace-only values as `<N blank chars>`
— with the register's name on the right.

It always pastes the way `p` does, whichever mode you opened it from.
To **insert** text at the cursor while typing — or into the `:` line, a
prompt or another picker's query — use the
[`yank-ring`](help:picker-yank-ring) picker (`<C-r><C-r>` in Insert
mode), which puts the text where you were.

Registers you paste from often rank a little higher next time.

---

## See also

- [`yank-ring`](help:picker-yank-ring) — every recent yank and delete,
  inserted where you are.
- [`yank-ring`](help:yank-ring) — the yank ring itself.
- [`picker`](help:picker) — keys and options shared by every picker.
