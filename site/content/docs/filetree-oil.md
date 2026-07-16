+++
title = "File tree & Oil"
+++


# File tree & Oil

Lattice ships two ways to browse and edit your project's filesystem from
inside the editor:

- **File tree** — a hierarchical, read-only directory view (expand /
  collapse subdirectories, jump to a file with `<CR>`). Comparable to
  NERDTree / Neo-tree.
- **Oil** — a flat, **writable** directory listing for the current
  folder. Edit the buffer like text, save it, and Lattice executes the
  diff as filesystem operations (rename, create, delete). Inspired by
  `oil.nvim`.

Both surfaces share the same icon system (nerd-font glyphs + per-type
colors) and route through the same buffer registry as documents, so
`:b *`, `<C-^>`, the buffer picker, and split commands all work
uniformly.

## File tree

### Opening

| Action | Default binding | Ex-command |
|--------|-----------------|------------|
| Open the file tree in a new pane | — | `:Tree` |
| Dismiss the file tree pane | — | `:TreeClose` |

The tree opens rooted at the workspace directory (typically the `.git`
ancestor or the launch cwd).

### Navigating + acting

| Key | Action |
|-----|--------|
| `j` / `k` | Down / up one entry |
| `gg` / `G` | First / last entry |
| `<CR>` | If on a directory: expand or collapse. If on a file: open it in the previous active pane. |
| `-` | Open `oil` for the current directory (see below). Same chord used document-side opens `oil` for the file's parent. |
| `q` / `:q` | Close the tree pane |

The tree is read-only — typing `i` or attempting `:w` is a no-op. Use
oil for editing.

### Icons & colors

Each row carries a leading icon based on file extension or the
directory marker. Folders are bold blue; hidden files (those starting
with `.`) render dim by default.

If your terminal isn't running a nerd-font patched font, set:

```
:set ui.nerd-fonts false
```

This drops the glyph column entirely and keeps only the per-type
colors.

### Per-buffer options

Inside the file tree, the `filetree.X` typed option group governs
display. Notable keys (see `:set filetree.<Tab>` for the full
catalog):

| Option | Default | Effect |
|--------|---------|--------|
| `filetree.show-hidden` | `false` | Include dotfiles + hidden entries in the listing |
| `filetree.ignore-vcs` | `true`  | Skip `.git/`, `.hg/`, `.svn/` |

## Oil — editable directory listing

### Opening

| Action | Default binding | Ex-command |
|--------|-----------------|------------|
| Open oil for the cwd | — | `:Oil` |
| Open oil for a specific directory | — | `:Oil /path/to/dir` |
| Open oil for the parent of the active file | `-` (Normal mode) | `:Oil` (when no buffer path is set) |

Oil presents the directory as a single flat list — one entry per line,
no nesting. Subdirectories carry a trailing `/`.

When `-` opens a listing, the cursor lands on the entry you came *from* —
the file you were editing (opening oil from a file buffer) or the child
directory you stepped out of (`-` inside oil). Pressing `<CR>` on that
row round-trips you straight back. When the source entry isn't in the
listing, the cursor stays on the first row.

### Editing the filesystem

Oil is a real buffer. Edit it as text:

| Buffer change | On `:w`, lattice does |
|---------------|------------------------|
| Add a new line `foo.rs` | Create empty file `foo.rs` |
| Add a new line `subdir/` | Create empty directory `subdir/` |
| Delete a line | Remove the corresponding file or directory (recursive for dirs) |
| Rename a line (`old.rs` → `new.rs`) | Rename `old.rs` to `new.rs` on disk |
| Reorder lines | No-op — order isn't persisted |

`:w` runs the diff against the snapshot taken at open-time. Conflicts
(target file already exists, permission denied, etc.) surface as
echo-area errors and leave the buffer dirty so you can retry.

### Navigation

| Key | Action |
|-----|--------|
| `<CR>` on a file | Open it in the previous active pane |
| `<CR>` on a directory | Replace the oil buffer's content with that directory |
| `-` | Navigate to the parent directory (cursor lands on the child dir you left) |
| All standard vim motions | Apply — oil is a real text buffer |

### Per-buffer options

The `oil.X` option group:

| Option | Default | Effect |
|--------|---------|--------|
| `oil.show-hidden` | `false` | Show dotfiles + hidden entries |
| `oil.confirm-delete` | `true` | Prompt before `:w` deletes a non-empty directory |

## Common workflows

**Create a new file in a subdirectory:**

```
:Oil src/
o
newfile.rs<Esc>
:w
```

**Rename a file:**

```
:Oil
/old.rs<CR>          # search for the entry
cw new.rs<Esc>       # change-word in place
:w
```

**Delete a directory tree:**

```
:Oil
/old-subdir<CR>
dd                   # delete the line
:w                   # confirm-delete prompt fires (if option is on)
```

**Bulk rename a series of files (`a-1.txt`, `a-2.txt`, ... → `b-1.txt`,
...):**

```
:Oil
:%s/a-/b-/
:w
```

That last one is the killer feature — oil lets you use vim's editing
grammar to batch-mutate the filesystem. Operators, registers, macros,
ex-commands all apply.

## Architecture notes

For developers: the iconography (`crates/lattice-ui-tui/src/icons.rs`)
is shared between the two surfaces; the buffer kinds are
`BufferKind::FileTree` (read-only) and `BufferKind::Oil` (read-write).
Both are first-class buffers in the registry — the renderer treats
them like documents with custom display providers. Design rationale
lives in [`../dev/architecture/design.md`](../dev/architecture/design)
§5.6.7 (iconography) and §5.9 (buffer model).
