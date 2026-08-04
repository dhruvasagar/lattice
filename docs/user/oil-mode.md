---
summary: "oil-mode: a flat, writable directory listing — edit the buffer like text, :w runs the diff as filesystem operations (rename / create / delete)."
related: [oil, ex:Oil]
---

# oil-mode

A flat, **writable** directory listing for one folder. Edit the buffer
like ordinary text, save it, and lattice executes the diff as
filesystem operations — rename, create, delete. Inspired by
`oil.nvim`.

This is the editing half of the filesystem surface;
[file-tree-mode](help:file-tree-mode) is the read-only browsing half.
Press `-` in either to move between them.

Oil routes through the same buffer registry as documents, so `:b *`,
`<C-^>`, the buffer picker, and split commands all work uniformly.
Press `<C-h> m` inside one to see its live mode stack and the chords
each mode contributes, or `<C-h> K` for every chord that fires there.
`:describe-mode oil-mode` describes the mode itself — its options and
capabilities — whether or not you are in an oil buffer.

## Opening

| Action | Default binding | Ex-command |
|--------|-----------------|------------|
| Open oil for the cwd | — | `:Oil` |
| Open oil for a specific directory | — | `:Oil /path/to/dir` |
| Open oil for the parent of the active file | `-` (Normal mode) | `:Oil` (when no buffer path is set) |

Oil presents the directory as a single flat list — one entry per line,
no nesting. Subdirectories carry a trailing `/`.

When `-` opens a listing, the cursor lands on the entry you came
*from* — the file you were editing (opening oil from a file buffer) or
the child directory you stepped out of (`-` inside oil). Pressing
`<CR>` on that row round-trips you straight back. When the source
entry isn't in the listing, the cursor stays on the first row.

## Editing the filesystem

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

## Navigation

| Key | Action |
|-----|--------|
| `<CR>` on a file | Open it in the previous active pane |
| `<CR>` on a directory | Replace the oil buffer's content with that directory |
| `-` | Navigate to the parent directory (cursor lands on the child dir you left) |
| All standard vim motions | Apply — oil is a real text buffer |

## Per-buffer options

The `oil.X` option group:

| Option | Default | Effect |
|--------|---------|--------|
| `oil.show-hidden` | `false` | Show dotfiles + hidden entries |
| `oil.confirm-delete` | `true` | Prompt before `:w` deletes a non-empty directory |

Icons and the `ui.nerd-fonts` toggle are shared with
[file-tree-mode](help:file-tree-mode).

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

**Bulk rename a series of files (`a-1.txt`, `a-2.txt`, … → `b-1.txt`,
…):**

```
:Oil
:%s/a-/b-/
:w
```

That last one is the point of oil: vim's editing grammar batch-mutates
the filesystem. Operators, registers, macros, and ex-commands all
apply.

## Architecture notes

For developers: the buffer kind is `BufferKind::Oil` (read-write); the
iconography (`crates/lattice-ui-tui/src/icons.rs`) is shared with the
file tree. Both are first-class buffers in the registry — the renderer
treats them like documents with custom display providers. Design
rationale lives in
[`../dev/architecture/design.md`](../dev/architecture/design.md)
§5.6.7 (iconography) and §5.9 (buffer model).
