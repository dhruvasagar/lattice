---
summary: "picker-dir-pick: browse the filesystem one directory at a time and choose a directory — <C-l> goes in, <C-w> comes back out."
related: [dir-pick]
---

# The `dir-pick` picker

A directory browser in the minibuffer. It lists **one directory at a
time**; you walk down into a child and back up to the parent, then
`<CR>` chooses where you are.

It answers a question rather than opening anything: the chosen path is
handed to whatever asked for it — a project to remember
(`:project-choose-dir`, the projects picker's `… (choose a dir)` row),
a command argument, a transient menu argument. Opened bare with
`:picker dir-pick [start]` it has nobody to hand the path to, and says
so.

---

## Keys in this picker

This is the picker with **depth**, so three keys mean something here
that they mean nowhere else.

| Key | Here |
|---|---|
| **`<C-l>`** | **Go into** the selected directory. The query becomes its path and the list re-lists its children |
| **`<Tab>`** | Same as `<C-l>` — go into the selected directory (in pickers without depth, `<Tab>` moves the selection) |
| **`<C-w>`** | **Go up** one level: drop the last path component. Stops at `/`; from `~/` it goes to your home's real parent |
| `<CR>` | **Choose** the selected directory and hand its path back. On the `../` row, `<CR>` goes up instead of choosing |
| *(type)* | Narrow the current directory's children by prefix — `sr` keeps `src/`. Type a `/` after a name to go into it by hand |
| `<BS>` | Delete one character of the path |
| `<C-n>` / `<C-p>`, `<Down>` / `<Up>`, `<S-Tab>` | Move the selection |
| `<C-r>` | Append something from your yank history to the path |
| `<Esc>` / `<C-c>` | Give up — nothing is chosen |
| `<C-h>` | This page |

`<C-s>` / `<C-v>` / `<C-t>`, `<C-q>` and `<C-d>` do nothing here: a
chosen directory is a value, not something to open, list or remove.

**Why these keys.** `l` and `h` are ranger / lf / nnn / vifm's "enter"
and "parent"; the picker's query takes every printable key, so "enter"
is `<C-l>`. "Up" is `<C-w>` because vim's `c_CTRL-W` deletes the word
before the cursor, and on a path that *is* the last component — and
because `<C-h>` is the help key everywhere else in the editor.
`<Tab>` follows emacs's `read-directory-name`, where it completes into
the directory.

---

## What is listed

The query **is** the directory being listed, so the prompt always shows
where you are — `~/src/` lists the children of `~/src`.

- Directories only, shown as `name/`, sorted by name. Hidden
  directories **are** listed, and symlinked directories are followed.
- A `../` row comes first whenever there is a parent to go to (never at
  `/`).
- Typing after the last `/` filters those children by **prefix**,
  case-insensitively — not fuzzily. `~/src/lat` lists `lattice/`, not
  `platform/`.
- A path that does not exist, or cannot be read, lists nothing rather
  than erroring; `<BS>` or `<C-w>` back to somewhere real.

It re-lists on every keystroke (one directory read, not a recursive
walk), so it stays fast in a directory with many children.

**Where it starts.** `:picker dir-pick` starts at `~/`;
`:picker dir-pick .` starts at the current directory;
`:picker dir-pick <path>` starts there. A caller such as the projects
picker chooses its own starting point.

---

## Accepting

`<CR>` hands the chosen path (with `~` expanded) to whoever opened the
picker, and closes it. On the `../` row it goes up instead, so the
same key walks you around and makes the choice.

There is no preview, and choices are not ranked by recency — the list
is always the directory, in name order.

---

## See also

- [`files`](help:picker-files) / [`file-pick`](help:picker-file-pick) —
  the file peers: a recursive list of files.
- [`oil`](help:oil-mode) and the [file tree](help:file-tree-mode) —
  when you want to *do* something to the directory rather than name it.
- [`picker`](help:picker) — keys and options shared by every picker.
