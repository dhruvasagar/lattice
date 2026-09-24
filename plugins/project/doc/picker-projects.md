# The `projects` picker

Every project you have worked in, most recent first. Pick one, then
pick **what to do there** — find a file, see its buffers, grep it,
open a shell, open magit — without leaving the buffer you are in.

Open it with `:project-switch`, `<leader>pp` or `<C-x>pp`, or
`:picker projects`. See `:help project` for the whole idea.

---

## Keys in this picker

| Key | Here |
|---|---|
| *(type)* | Filter by project name **or path** |
| `<CR>` | Open the project's menu: `f` find file, `b` buffers, `d` browse the tree, `g` grep, `s` shell, `m` magit |
| **`<C-d>`** | **Forget** the selected project. The picker stays open and re-lists, so you can clear out several stale ones in a row. Nothing on disk is touched and there is no confirmation — opening a file there again brings it back |
| `<C-n>` / `<C-p>`, `<Down>` / `<Up>`, `<Tab>` / `<S-Tab>` | Move the selection |
| `<BS>` / `<C-w>` | Delete a character / the previous word of the query |
| `<C-r>` | Append something from your yank history to the query |
| `<Esc>` / `<C-c>` | Close |
| `<C-h>` | This page |

`<C-s>` / `<C-v>` / `<C-t>` and `<C-q>` do nothing useful here — a
project is a place to choose a verb in, not a file. `<C-l>` does
nothing.

---

## What is listed

Each row is the project's directory name, with its full path beside it
(the path is searchable too, so `work api` finds `~/work/api`). A
project joins the list when you open a file inside it; the list keeps
the 256 most recent.

Two rows at the bottom are not projects:

- **`… (choose a dir)`** — always there. `<CR>` opens the
  [`dir-pick`](help:picker-dir-pick) directory browser (`<C-l>` in,
  `<C-w>` out, `<CR>` chooses), and the directory you choose is
  remembered and switched to.
- **`… (remember <what you typed>)`** — appears once you have typed
  something. `<CR>` remembers that path as a project and switches to it.

---

## See also

- `:help project` — projects, roots and the project menu.
- [`project-buffers`](help:project.picker-project-buffers) — the open
  buffers of the current project.
- [`picker`](help:picker) — keys and options shared by every picker.
