---
summary: "magit-diff-mode: a read-only diff buffer with file-level stage/unstage (s/u) — repo-wide against HEAD, or scoped to one file and one baseline."
related: [magit, magit-diff, ex:magit-diff]
---

# magit-diff-mode

A read-only diff in its own buffer, with file-level staging on top.
`:magit-diff` opens the repo-wide view — `git diff HEAD`, staged and
unstaged changes combined.

The headerline names the scope, and the path when the buffer is
file-scoped: `staged  src/main.rs`. Without it, three buffers showing
three different baselines would be indistinguishable.

Unlike magit-status's inline `=`, this is a whole-buffer view — useful
when a diff is too large to read comfortably expanded inside the status
list.

## Chords

| Chord | Action |
|---|---|
| `s` | Stage the file at cursor |
| `u` | Unstage the file at cursor |
| `<CR>` | Visit the file at cursor |
| `gr` | Refresh (re-run the underlying `git diff`) |

`]]` / `[[` / `]f` / `[f` / `]c` / `[c` / `TAB` / `q` come from
[`magit-core-mode`](help:magit-core-mode), the same as every other
magit buffer — `]c` / `[c` for hunk-to-hunk movement is the one you'll
use most here.

`s` and `u` resolve the file from the nearest `diff --git a/<path>
b/<path>` header **above** the cursor, so they work from anywhere
inside that file's diff, not only on its header line.

## The three scopes

| Buffer | Baseline | Opened by |
|---|---|---|
| `*magit:diff*` | `git diff HEAD` | `:magit-diff`, or `d` in the repo dispatch |
| `*magit:diff:<path>*` | that file against HEAD | `d` in the [file dispatch](help:magit-transient) (`C-c f`) |
| `*magit:diff:staged:<path>*` | `git diff --cached` | `d` on a file in magit-status's **Staged** section |
| `*magit:diff:unstaged:<path>*` | `git diff` (worktree vs index) | `d` on a file in magit-status's **Unstaged** section |

The scope decides what `<CR>` opens: from a **staged**-scoped buffer it
opens the index blob (read-only,
[`magit-file-revision-mode`](help:magit-file-revision-mode)), because
that's what the diff you're reading describes. From the others it opens
the live working-tree file.

## How the diff is coloured

Added and removed lines get a **full-width background tint** as well as
a coloured foreground, so a hunk reads as a block at a glance rather
than as text that happens to start with `+` or `-`. The tint spans the
whole row, past the end of the text — which is what makes a run of
added lines look like one shape.

Four theme elements control it, and they apply anywhere a diff is
shown, including magit-status's inline `=` expansions, the
[commit](help:magit-commit-mode) buffer's staged diff, a
[revision](help:magit-revision-mode) and a
[stash](help:magit-stash-show-mode):

| Element | What it colours |
|---|---|
| `diff.add.text` | foreground of an added line |
| `diff.add.line` | **background** of an added line |
| `diff.remove.text` | foreground of a removed line |
| `diff.remove.line` | **background** of a removed line |

Hunk headers (`@@`), file headers and `diff --git` lines are
deliberately *not* tinted — they describe the diff's structure rather
than changed content, so tinting them would break the blocks apart.

There is no gutter `+` / `-` sign column here, unlike a file being
edited under a diff session: the text already starts with the marker,
so a second one would be redundant.

## Behaviour worth knowing

- **Populated once on open**; `gr` re-runs the diff.
- **`s` / `u` are file-level only.** There is no hunk-level staging
  here — the same caveat as magit-status. File-level is as granular as
  staging gets anywhere in magit today.
- **Not yet implemented:** no side-by-side pane layout, no
  `do` / `dp` hunk transfer between panes, no visual-mode partial-hunk
  staging. For side-by-side diffing of two *files* (as opposed to
  reviewing git state), see [`diff-mode`](help:diff-mode), which is a
  separate feature.

## See also

- [`magit-status-mode`](help:magit-status-mode) — inline `=` diffs and
  the sections `d` is pressed from.
- [`diff-mode`](help:diff-mode) — lattice's general diff/merge surface,
  unrelated to git porcelain.
