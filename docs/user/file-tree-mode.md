---
summary: "file-tree-mode: a hierarchical, read-only directory view — expand/collapse subdirectories, <CR> to open a file, `-` to hand off to oil-mode for editing."
related: [filetree, tree, ex:Tree, ex:TreeClose]
---

# file-tree-mode

A hierarchical, read-only view of your project's directory tree —
expand and collapse subdirectories, jump to a file with `<CR>`.
Comparable to NERDTree / Neo-tree.

It is read-only by design. When you want to *change* the filesystem,
press `-` to hand the current directory to
[oil-mode](help:oil-mode), which is the writable surface. The two are
peers, not layers: the tree is for finding things, oil is for editing
them.

The tree routes through the same buffer registry as documents, so
`:b *`, `<C-^>`, the buffer picker, and split commands all work
uniformly. Press `<C-h> m` inside one to see its live mode stack and
the chords each mode contributes, or `<C-h> K` for every chord that
fires there. `:describe-mode file-tree-mode` describes the mode
itself — its options and capabilities — whether or not you are in a
tree buffer.

## Opening

| Action | Default binding | Ex-command |
|--------|-----------------|------------|
| Open the file tree in a new pane | — | `:Tree` |
| Dismiss the file tree pane | — | `:TreeClose` |

The tree opens rooted at the workspace directory (typically the `.git`
ancestor or the launch cwd).

## Navigating and acting

| Key | Action |
|-----|--------|
| `j` / `k` | Down / up one entry |
| `gg` / `G` | First / last entry |
| `<CR>` | On a directory: expand or collapse. On a file: open it in the previous active pane. |
| `-` | Open [oil-mode](help:oil-mode) for the current directory. The same chord used in a document opens oil for that file's parent. |
| `q` / `:q` | Close the tree pane |

Typing `i` or attempting `:w` is a no-op — see
[oil-mode](help:oil-mode) for editing.

## Icons and colours

Each row carries a leading icon based on file extension or the
directory marker. Folders are bold blue; hidden files (those starting
with `.`) render dim by default.

If your terminal isn't running a nerd-font patched font, set:

```
:set ui.nerd-fonts false
```

This drops the glyph column entirely and keeps only the per-type
colours. The icon system is shared with [oil-mode](help:oil-mode), so
the toggle affects both.

## Per-buffer options

The `filetree.X` typed option group governs display. Notable keys (see
`:set filetree.<Tab>` for the full catalog):

| Option | Default | Effect |
|--------|---------|--------|
| `filetree.show-hidden` | `false` | Include dotfiles + hidden entries in the listing |
| `filetree.ignore-vcs` | `true`  | Skip `.git/`, `.hg/`, `.svn/` |

## Architecture notes

For developers: the buffer kind is `BufferKind::FileTree` (read-only);
the iconography (`crates/lattice-ui-tui/src/icons.rs`) is shared with
oil. Both are first-class buffers in the registry — the renderer
treats them like documents with custom display providers. Design
rationale lives in
[`../dev/architecture/design.md`](../dev/architecture/design.md)
§5.6.7 (iconography) and §5.9 (buffer model).
