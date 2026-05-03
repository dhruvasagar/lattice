# Buffers and panes

A *buffer* in lattice is anything you can navigate with motions:
a code file, a help view, a file tree. Documents and trees both
live in the same registry under one `BufferId` keyspace, so the
same `:bn` / `:bp` / `:ls` / `:bd` / `:b N` commands work uniformly
across kinds. *Panes* are the visible viewports the buffers render
into; you arrange them with vim-style window splits.

## Quick reference

### Buffer commands

| Command                     | Effect                                                          |
|-----------------------------|-----------------------------------------------------------------|
| `:e[dit] FILE`              | Open `FILE` (or switch to it if already open).                  |
| `:e folder`                 | Defers to `:Tree folder` -- editing a directory shows it.       |
| `:Tree [path]`              | Open a file-tree buffer rooted at `path` (default: cwd).        |
| `:b[uffer] N`               | (planned) Switch the active pane to buffer `#N`.                |
| `:bn[ext]`                  | Cycle to the next listed buffer (any kind).                     |
| `:bp[rev]`                  | Cycle to the previous listed buffer.                            |
| `:ls` / `:buffers`          | List every open buffer; `%` marks the one in the active pane.   |
| `:bd[elete][!]`             | Close the active buffer. `!` discards unsaved document changes. |
| `:TreeClose`                | Close the active pane's tree (alternative to `:bd`).            |

### Pane commands (vim's `<C-w>` family)

`<C-w>` arms a chord; the second key resolves the action. Both
"Ctrl held throughout" (`<C-w><C-l>`) and "release then press"
(`<C-w>l`) forms work.

| Chord                                     | Effect                                       |
|-------------------------------------------|----------------------------------------------|
| `<C-w>s`                                  | Split pane horizontally (new pane below).    |
| `<C-w>v`                                  | Split pane vertically (new pane right).      |
| `<C-w>c` / `<C-w>q`                       | Close the active pane.                       |
| `<C-w>h` / `<C-w>j` / `<C-w>k` / `<C-w>l` | Navigate to spatial neighbour.               |
| `<C-w>w` / `<C-w><C-w>` / `<C-w><Tab>`    | Cycle to next pane.                          |
| `<C-w>W` / `<C-w><BackTab>`               | Cycle to previous pane.                      |

`<C-o>` / `<C-i>` walk the unified position-history ring across
buffer boundaries -- so `<C-o>` from inside a help or tree pane
returns to the document spot you came from.

## Semantics

### The buffer registry

Every open buffer (document, file tree) gets a stable `BufferId`
when it's created. The id is monotonic, never reused, and outlives
any one pane. Two panes can show the same buffer (vim's `:vsplit`
+ no `:e`); each pane carries its own viewport stash (cursor +
scroll), so the cursors are independent even though the underlying
text is shared.

A buffer always knows its kind:

- **Document** -- backed by a file (or `[no name]` if not yet
  written). Mutations route through the per-document actor;
  saving with `:w` writes to disk; `:bd` checks the dirty flag
  unless `!` is given.
- **FileTree** -- a hierarchical view of a directory. Read-only
  -- mutating operators echo "buffer is read-only". `<CR>` on a
  directory toggles expansion; on a file opens it via `:e FILE`
  (which switches to / spawns a Document buffer in the active
  pane).

### Buffer flags

Every entry in the registry carries a small set of flags:

| Flag       | Default | Meaning                                                          |
|------------|---------|------------------------------------------------------------------|
| `listed`   | `true`  | Whether the buffer appears in `:bn` / `:bp` / `:ls`.             |
| `hidden`   | `false` | Reserved for "keep loaded without a window" (vim's `'hidden'`).  |

Setting `listed = false` (vim's `:setlocal nobuflisted`) makes the
buffer skip cycling but still reachable by id (`:b N`). v1 doesn't
yet expose a per-buffer `:set` interface for these flags --
they're API knobs for plugins / future config.

### Multiple file trees

`:Tree path` adds a *new* tree buffer rooted at `path`. If a tree
at the same root is already open, the active pane switches to it
instead of spawning a duplicate. This means you can have several
file trees open simultaneously (one per project root) and cycle
between them with `:bn` like any other buffer.

### Pane visuals

Each pane renders its actual buffer content -- there are no
decorative borders. When more than one pane is visible:

- A one-row **per-pane status line** at the bottom of each pane
  shows the buffer label + cursor position. The active pane's
  status line is reverse-videoed; inactive ones are dim.
- A `│` **separator column** is drawn between vertically-split
  panes. Horizontal splits don't get an explicit separator -- the
  upper pane's status line at its bottom edge is the visual
  delimiter.
- **Inactive panes keep their syntax highlights** (`refresh_pane_highlights`
  reparses lazily by document text version). A `DIM` overlay is
  layered on top so the active pane stands out without losing
  color.

### Theme customization

Style knobs live in the typed-options registry; `:set ui.*` writes
through to `App.theme`. Available:

| Option                          | Type   | Default      | Effect                                                       |
|---------------------------------|--------|--------------|--------------------------------------------------------------|
| `ui.dim_inactive`               | bool   | `true`       | Apply DIM modifier on inactive panes' content.               |
| `ui.separator`                  | string | `│`          | Single character drawn in the vertical-split separator col.  |
| `ui.separator_color`            | string | `darkgray`   | Foreground color name for the separator.                     |
| `ui.statusline_active_fg`       | string | `default`    | Foreground for the active pane's status line.                |
| `ui.statusline_inactive_fg`     | string | `darkgray`   | Foreground for inactive panes' status lines.                 |

Color names: the 16 ANSI palette (`red`, `green`, `lightblue`,
`darkgray`, ...) plus `default` / `reset` for the terminal default.
Hex colors arrive post-1.0.

`:describe-option ui.dim_inactive` opens the spec inline. `:options`
lists every registered option and its current value.

## Edge cases

- **Closing the last buffer.** `:bd` rejects when the registry has
  one entry; the App always has at least one buffer.
- **Closing the last pane.** `<C-w>c` rejects when only one pane
  is open; vim's behaviour is the same.
- **Following a tree entry while the tree is the active pane.** The
  tree buffer stays in the registry; the active pane switches to
  the file's Document buffer. The user can `<C-w>v :Tree path`
  first if they want to keep the tree visible alongside the file.
- **Two panes showing the same file.** Each pane has its own
  cursor / scroll stash. The active pane's hot-path fields
  (`App::cursor`, `App::scroll`) mirror the active pane's stash;
  switching panes round-trips through the stash.
- **Stale position-history entries.** If a buffer is closed via
  `:bd` and the position-history ring has entries from it, those
  entries are filtered out by the `<C-o>` / `<C-i>` walker --
  reachability is checked against the live registry.

## Anchors in DESIGN.md

- §5.9 -- everything-is-a-buffer principle and the pane / window
  composition model.
- §5.6 -- rendering / theming layers.
- §5.12 -- typed options registry.
- §5.1.1 -- unified position history (jump list + mark ring).
