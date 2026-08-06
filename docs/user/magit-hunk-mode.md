---
summary: "magit-hunk-mode: the shared minor mode active in every magit buffer that renders a diff — s/u/x to stage, unstage and discard the hunk at cursor, a/- to move one hunk of a commit, ]c/[c to navigate, <CR> to visit the right version at the right line. It also owns the folds inside a diff: per file and per @@ hunk, nothing finer."
related: [magit, magit-core-mode, magit-status-mode, magit-diff-mode, diff-mode]
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
| `]f` / `[f` | Next / previous file |
| `dv` | Open the file at cursor side-by-side against its baseline |
| `<CR>` | Visit the file at cursor — the version this buffer describes, on the line under the cursor |

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

## What `<CR>` opens

The same key, but the *version* differs — because what you are looking
at differs:

| Buffer | `<CR>` opens |
|---|---|
| magit-diff, staged | the file as staged (the index blob) |
| magit-diff, unstaged or HEAD | the live working-tree file |
| magit-commit | the index blob — its diff *is* the index |
| magit-revision | the file as of that commit |
| magit-stash-show | the file as that stash left it |
| magit-status | on a diff line, as above; on a file, stash or commit row, that row's own target |

Opening the working-tree copy when you asked about a historical one is
the mistake [`magit-file-revision-mode`](help:magit-file-revision-mode)
exists to prevent, so a buffer with no sensible answer declines rather
than guessing.

### And on the line you were reading

`<CR>` inside a hunk lands on the line the code under the cursor lives
on, not at the top of the file. The `@@` header declares where the new
side starts; counting the added and context rows between it and the
cursor gives the rest. `-` rows are skipped — a removed line has no
position in the file being opened, so the cursor lands on the row that
replaced it.

On a row that is not inside a hunk — a file entry in magit-status, a
`diff --git` header — the file opens at the top. Nothing failed; there
is simply no line to compute, and Emacs magit opens those at the top
too.

## Folding inside a diff

**This mode owns the folds in diff content**, and what it declares is
deliberately coarse:

- each **file** in the diff folds, and
- each **`@@` hunk** folds independently inside it.

Nothing finer. In a magit buffer `foldmethod` is `manual`, which turns
off the indent- and syntax-driven folds you get in an ordinary source
buffer, so the only folds present are the ones above (plus
[magit-status's own sections](help:magit-status-mode), which that major
declares).

That is not a limitation working around a bug — a diff is *fragments*
of a file. Folding it by code structure means folding a function whose
opening brace is in the hunk and whose closing brace is in a part of
the file the diff never showed, which produces a fold that swallows
everything after it, including the next file's diff and every section
below. Hunk-scale is the largest unit a diff actually contains.

A file's fold ends where its **last hunk** ends, and a hunk's fold ends
where the `@@` header says it does — the counts in `@@ -a,b +c,d @@`
are how many lines the hunk is, so the fold stops there rather than
running to the next marker or the end of the buffer.

`TAB` and `S-TAB` toggle these, and so does every ordinary fold chord —
`za`, `zR`, `zM` and the rest work here because these are ordinary
folds. See [`folding`](help:folding).

## `dv` — side by side

`dv` on a diff row opens two scroll-bound panes: the file as it was at
the version this diff describes on the left, your working-tree copy on
the right. Once they are open, everything the
[diff subsystem](help:diff-mode) provides applies — `]c` / `[c` to move
between hunks, and **`do` / `dp` to pull a hunk in or push it across**,
exactly as in vim's diff mode. Magit has no side-by-side view; the key
is vim-fugitive's.

Which version ends up on the left depends on what you are looking at:

| You are in | Left-hand side |
|---|---|
| A staged or unstaged diff | The file as the index has it |
| A commit's or a stash's diff | The file at that commit |
| `:magit-diff` (against HEAD) | The file at HEAD |

That last row is the one place `dv` works where `s` / `u` / `x` will
not. A diff against HEAD mixes staged and unstaged changes, so there is
no single place to *apply* a hunk — but there is no ambiguity at all
about which version to *show*.

A file with no working-tree copy (deleted in the commit you are
looking at) says so rather than opening an empty pane.
