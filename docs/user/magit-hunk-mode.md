---
summary: "magit-hunk-mode: the shared minor mode active in every magit buffer that renders a diff — s/u/x to stage, unstage and discard the hunk at cursor, a/- to move one hunk of a commit, ]c/[c to navigate."
related: [magit, magit-core-mode, magit-status-mode, magit-diff-mode]
---

# magit-hunk-mode

The minor mode that owns diff **content**. Where
[`magit-core-mode`](help:magit-core-mode) is every magit buffer, this
one is every magit buffer that renders a diff — so the keys for acting
on a hunk mean the same thing wherever a hunk appears.

It activates automatically alongside the majors below; there is no
`:magit-hunk-mode` to turn on.

## Where it's active

| Buffer | The diff it shows |
|---|---|
| [magit-status](help:magit-status-mode) | the inline diffs `=` expands |
| [magit-diff](help:magit-diff-mode) | the whole buffer |
| [magit-commit](help:magit-commit-mode) | the staged changes under your message |
| [magit-revision](help:magit-revision-mode) | the commit's own patch |
| [magit-stash-show](help:magit-stash-show-mode) | the stash's patch |

Not the list views — a [log](help:magit-log-mode),
[branch list](help:magit-branch-mode),
[stash list](help:magit-stash-mode),
[rebase todo](help:magit-rebase-mode) or [blame](help:magit-blame-mode)
has no hunks, so these keys stay free there rather than being bound to
do nothing.

## Chords

| Chord | Action |
|---|---|
| `s` | Stage the hunk or file at cursor |
| `u` | Unstage the hunk or file at cursor |
| `x` | Discard the hunk or file at cursor (asks first) |
| `s` / `u` / `x` in Visual | Act on the selected lines only |
| `a` | Apply a committed hunk to the working tree |
| `-` | Reverse a committed hunk out of the working tree |
| `]c` / `[c` | Next / previous hunk |

`]c` then `s` stages exactly the hunk you landed on. In Visual mode,
select lines inside a hunk and only those move — see
[region staging](help:magit-status-mode).

## Which key applies where

A hunk's patch only means something against the tree it was diffed
from, so each key checks before acting and declines with a sentence
rather than handing git a patch it will refuse:

| Where the hunk came from | `s` | `u` | `x` | `a` | `-` |
|---|---|---|---|---|---|
| Unstaged changes | ✓ | — | ✓ | — | — |
| Staged changes | — | ✓ | — | — | — |
| A commit or a stash | — | — | — | ✓ | ✓ |

`x` on a **staged** hunk is refused rather than performed: reversing it
out of the working tree while leaving it in the index would make the
change vanish from the file and still be committed by your next `cc`.
The message says to `u` first.

`a` and `-` write the working tree only, never the index — see
[magit-core-mode](help:magit-core-mode#operating-on-one-hunk-of-a-commit)
for what they're for.

## Why this is a mode and not a per-buffer keymap

Each of those five buffers used to declare these chords itself, and the
sets had drifted: magit-diff had `s` and `u` but no `x`, and the
commit, revision and stash-detail buffers had none at all. Nobody
noticed, because with the chords copied into five places there was no
single place the missing one should have been.

The rule that came out of it: behaviour wanted in more than one buffer
belongs to a mode that carries it, not to each buffer separately. A gap
in a copied set doesn't announce itself.

`<CR>` is deliberately **not** here. In magit-status it opens whatever
is under the cursor — a file, a stash, a commit — not only diff
content, so it stays with each buffer until that resolution is shared
too.
