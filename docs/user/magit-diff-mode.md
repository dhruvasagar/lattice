---
summary: "magit-diff-mode: a read-only diff buffer with line-, hunk- and file-level stage/unstage (s/u) — repo-wide against HEAD, or scoped to one file and one baseline."
related: [magit, magit-diff, ex:magit-diff]
---

# magit-diff-mode

A read-only diff in its own buffer, with staging on top.
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
| `s` | Stage the hunk or file at cursor |
| `u` | Unstage the hunk or file at cursor |
| `s` / `u` in Visual | Stage / unstage the selected lines only |
| `<CR>` | Visit the file at cursor |
| `gr` | Refresh (re-run the underlying `git diff`) |

`]]` / `[[` / `]f` / `[f` / `]c` / `[c` / `TAB` / `q` come from
[`magit-core-mode`](help:magit-core-mode), the same as every other
magit buffer — `]c` / `[c` for hunk-to-hunk movement is the one you'll
use most here.

`s` and `u` act on the **hunk under the cursor** when there is one, so
`]c` then `s` stages exactly the hunk you landed on and leaves the
file's others alone. With the cursor on a file header — or anywhere
outside a hunk body — they fall back to the whole file, resolved from
the nearest `diff --git a/<path> b/<path>` header above.

In Visual mode they act on the **selected lines** rather than the whole
hunk — select, press `s`, and only those lines are staged. See
[`magit-status-mode`](help:magit-status-mode) for the details; the rules
are identical here.

Which chord applies depends on the buffer's scope. A
`*magit:diff:unstaged:*` buffer stages with `s`; a `*magit:diff:staged:*`
one unstages with `u`; pressing the other says so rather than failing
in git.

**`*magit:diff*` (against HEAD) stages files, not hunks.** Its hunks
combine staged and unstaged changes, so a single hunk there is not a
patch against either the index or the working tree. `s` inside one
reports that hunk staging isn't available in this view; move to the
file header for the whole file, or open the scoped view (`d` on the
file in magit-status) to work hunk by hunk.

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
- **`s` / `u` stage a hunk in the scoped views, a file in the HEAD
  view** — see the note above. The same rule holds in magit-status.
- **`x` (discard) isn't bound here**, only in magit-status.
- **Staging keeps your place.** After `s` or `u` the buffer re-runs its
  diff and the cursor lands on the hunk that took the staged one's
  place, so you can work down a file hunk by hunk without hunting for
  where you were.
- **Not yet implemented:** no side-by-side pane layout, no
  `do` / `dp` hunk transfer between panes, no visual-mode partial-hunk
  staging across MORE than one hunk (within one hunk it works — see
  above). For
  side-by-side diffing of two *files* (as opposed to reviewing git
  state), see [`diff-mode`](help:diff-mode), which is a separate
  feature.

## See also

- [`magit-status-mode`](help:magit-status-mode) — inline `=` diffs and
  the sections `d` is pressed from.
- [`diff-mode`](help:diff-mode) — lattice's general diff/merge surface,
  unrelated to git porcelain.
