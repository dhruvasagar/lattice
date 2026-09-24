---
summary: "picker-files: fuzzy-find a file under the project root and open it — `:files [root]`."
related: [files, ex:files]
---

# The `files` picker

Every file under the active buffer's project, filtered as you type.
`<CR>` opens the one you picked.

Open it with `:files`, `:picker files`, or `<C-x><C-f>` with
[`emacs-keys-mode`](help:emacs-keys-mode) on. `:files <root>` walks
another directory instead (`~` is expanded); the prompt shows the
project root either way.

---

## Keys in this picker

| Key | Here |
|---|---|
| *(type)* | Filter by path. Space separates fragments: `pick refil` and `refil pick` both find `lattice-picker/src/refilter.rs` |
| `<CR>` | Open the file (or switch to it if it is already open) |
| **`<C-s>`** / **`<C-v>`** / **`<C-t>`** | Open it in a horizontal split / vertical split / new tab instead |
| **`<C-q>`** | Send every file still listed to the error list, then close — walk them with `:cn` / `:cp` |
| `<C-n>` / `<C-p>`, `<Down>` / `<Up>`, `<Tab>` / `<S-Tab>` | Move the selection |
| `<BS>` / `<C-w>` | Delete a character / the previous word of the query |
| `<C-r>` | Append something from your yank history to the query |
| `<Esc>` / `<C-c>` | Close without opening anything |
| `<C-h>` | This page |

`<C-l>` and `<C-d>` do nothing here: a file has no inside to go into,
and nothing is removed from a file listing but the file itself (use
[`oil`](help:oil-mode) for that).

---

## What is listed

Paths relative to the root, with three columns on the right:
permissions (`drwxr-xr-x`, each bit coloured), size (`ls -h` style)
and when the file was last modified ("3 hours ago").

The walk is deliberately simple and fast:

- **Hidden entries are skipped** — anything whose name starts with `.`,
  files and directories alike.
- `.git`, `target`, `node_modules`, `dist` and `.cache` are never
  entered.
- `.gitignore` is **not** read. Use [`grep`](help:picker-grep) or
  [`:search`](help:project-search-mode), whose backends respect it, when
  ignore rules matter.
- Regular files only; symlinks are not followed.
- At most **5000** files. A larger tree is cut off, so pass a narrower
  root: `:files crates/lattice-host`.

---

## Preview

Moving the selection shows the file in the active pane, from the top,
without opening it — no language server starts. Large files are read
partially (256 KiB / 2000 lines), and a binary file shows
`<binary file — no preview>`.

---

## Ranking

Files you open from this picker come back sooner: a frecency bonus
(recent and frequent picks) breaks ties between equally good matches.
It never outranks a better match. See [MRU ranking](help:picker#mru-ranking)
for the `picker.mru.*` options.

---

## See also

- [`recent`](help:picker-recent) — just the files you edited this session.
- [`file-pick`](help:picker-file-pick) — the same list, but choosing a
  path supplies it as a value instead of opening it.
- [`grep`](help:picker-grep) — search file *contents*.
- [`picker`](help:picker) — keys and options shared by every picker.
