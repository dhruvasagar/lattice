---
summary: "magit-core-mode: the shared minor mode active in every magit buffer — gr to refresh, q to close, ]]/[[ and ]f/[f to navigate, TAB to fold, and A/_/O to act on the commit under the cursor. Diff-content chords live on magit-hunk-mode."
related: [magit, magit-core]
---

# magit-core-mode

The minor mode every magit buffer inherits. It is what makes the whole
porcelain feel like one thing: refresh, close, navigate, and fold work
identically in status, log, diff, blame, stash, branch, and rebase, so
you learn the movement vocabulary once.

It activates automatically alongside each magit major mode — there is
no `:magit-core-mode` you need to turn on.

## Chords

| Chord | Action |
|---|---|
| `gr` | Refresh the current magit buffer |
| `q` | Close the buffer (bury — return to previous) |
| `]]` / `[[` | Next / previous top-level section |
| `]f` / `[f` | Next / previous file or entry within the current section |
| `TAB` | Toggle the section or hunk fold at cursor |
| `S-TAB` | Cycle section visibility (all → changed only → collapsed → all) |
| `A` | Cherry-pick the commit under the cursor |
| `_` | Revert the commit under the cursor |
| `Os` / `Om` / `Oh` | Reset `--soft` / `--mixed` / `--hard` to the commit under the cursor |

These work in **every** magit buffer, which is what this mode is for.
The chords that act on diff content — `s` / `u` / `x`, `a` / `-`,
`]c` / `[c` — live on [`magit-hunk-mode`](help:magit-hunk-mode)
instead, and are active only in the buffers that render a diff. They
used to be here, which meant they were bound (and did nothing) in a
branch list, a log, a stash list, a rebase todo and a blame.

## Operating on the commit under the cursor

`A`, `_` and `O…` work in every view that shows a commit, because each
answers "what commit is under the cursor?" for its own row format and
one shared handler acts on the answer. Keys follow Emacs magit's own.

| View | What it reads |
|---|---|
| [log](help:magit-log-mode) | the sha on the row under the cursor |
| magit-status | the sha on a Recent-commits row |
| [revision](help:magit-revision-mode) | the commit the buffer *is* — every line of it, since a `git show` is one commit |
| [rebase todo](help:magit-rebase-mode) | the sha on the todo line under the cursor |

On a row with no commit — a file entry, a stash, a `--graph` connector
line — they **ask which commit** rather than acting on a neighbour, by
opening a picker of recent commits. Same when they are fired from the
[dispatch menu](help:magit-transient), which has no cursor on a commit
at all. Magit reaches the same place: its `A` / `V` / `X` are
transients that prompt, which is why they are *not* gated to magit
buffers there.

Each also has an ex-command taking the commit directly —
`:magit-cherry-pick <sha>`, `:magit-revert <sha>`,
`:magit-reset-soft` / `-mixed` / `-hard` `<sha>`. With no argument they
open the same picker, so `:magit-revert` and the `_` chord on a
non-commit row end up in the same place.

| Chord | Runs | Keeps |
|---|---|---|
| `A` | `git cherry-pick <commit>` | — |
| `_` | `git revert --no-edit <commit>` | — |
| `Os` | `git reset --soft <commit>` | index **and** working tree |
| `Om` | `git reset --mixed <commit>` | working tree |
| `Oh` | `git reset --hard <commit>` | **nothing** — asks first |

`Oh` is the only one that destroys uncommitted work, so it is the only
one that asks — the same two-step `x`, branch-delete and stash-drop
use, where the chord itself performs no git call at all. `--soft` and
`--mixed` keep your changes, and prompting on those would just train
you to dismiss the prompt that matters.

`_` passes `--no-edit`: git would otherwise open `$EDITOR` for the
revert message, which inside lattice means waiting on a prompt you
cannot answer.

## Operating on one hunk of a commit

> `a` and `-` are [`magit-hunk-mode`](help:magit-hunk-mode)'s, not this
> mode's — they are described here because they are the hunk-scale
> counterparts of `A` and `_` above.

`a` and `-` are the hunk-scale versions of `A` and `_`. Where `A`
cherry-picks a whole commit, `a` takes the **one hunk under the
cursor** and applies it to your working tree; where `_` reverts a whole
commit, `-` takes that one hunk back out.

| Chord | Takes | Scale |
|---|---|---|
| `A` | the commit into a new commit | whole commit |
| `a` | the hunk into your working tree | one hunk |
| `_` | the commit out, as a new commit | whole commit |
| `-` | the hunk out of your working tree | one hunk |

They work wherever a *committed* patch is shown — the
[revision view](help:magit-revision-mode) (`<CR>` on a log entry) and
the [stash detail](help:magit-stash-show-mode) view. In magit-status
and magit-diff, where the patch describes your current changes rather
than history, `a` says the change is already in the working tree and
`-` points you at `x`.

A Visual-mode selection narrows them the same way it narrows `s` — pick
the lines you want out of a commit's hunk and only those move.

Both write the **working tree only**, never the index: what you get is
an ordinary unstaged change, which `s` then stages in the usual way.
Neither asks first, because each is the other's exact inverse — `a`
puts a change in that `-` takes straight back out, and `-` removes one
that is still in the commit it came from. And `git apply` refuses
outright when the surrounding lines have drifted, so neither can
quietly damage an edit in progress.

Put the cursor inside a hunk. There is no whole-file fallback here on
purpose: the file-scale meaning of these keys is cherry-pick and
revert, and doing that because the cursor missed a hunk would be a much
larger action than the key promises. Outside a hunk they say so.

### Why these keys, and not magit's

They are **evil-collection-magit's**, not raw magit's. Magit is not a
modal editor, so it can afford `V` for revert and `X` for reset; in a
vim-modal editor those are grammar. evil-magit is the reference set
because it already resolved exactly these collisions:

| Command | Magit | Here (evil-magit) |
|---|---|---|
| Revert | `V` | `_` — "you are subtracting a commit" |
| Reverse one hunk | `v` | `-` — the same category, one scale down |
| Reset | `X` | `O` |
| Discard | `k` | `x` |
| Apply / cherry-pick | `A` | `A` |
| Apply one hunk | `a` | `a` |

`a` and `-` are magit's own pair too, reached the same way magit
reaches them: `magit-mode-map` binds `a` to cherry-apply and `v` to
revert-no-commit, and inside a diff section
`magit-diff-section-base-map` remaps *both* to their hunk-level
versions. So the hunk pair genuinely rides on the commit-level keys —
which is why `-` sits next to `_` here, `v` being unavailable for the
same reason `V` is.

`V` is deliberately left alone so it still means linewise Visual, which
is what [region staging](help:magit-status-mode) needs — select lines
inside a hunk, stage only those. vim-fugitive keeps `V` free for the
same reason.

(An earlier revision bound revert to `V`, which swallowed the chord in
every magit buffer and made region staging unreachable. If you have
muscle memory for `V`, it is `_` now.)

## What `gr` actually does

Each view supplies its own refresh body — the log re-runs `git log`,
the branch list re-runs `git branch`, status re-runs its whole section
scan. `magit-core-mode` owns the *chord*; the buffer under it owns the
*work*. That split matters in one visible way: `gr` is a deliberate
no-op in the fixed-content views
([`magit-revision-mode`](help:magit-revision-mode),
[`magit-file-revision-mode`](help:magit-file-revision-mode),
[`magit-stash-show-mode`](help:magit-stash-show-mode)), because a fixed
sha or a fixed stash index cannot change under you.

## Navigation granularity

The three pairs step at three different scales, so you can move at
whichever one matches what you're doing:

- `]]` / `[[` — section headers (`Staged changes`, `Unstaged changes`,
  `Recent commits`). The coarsest jump.
- `]f` / `[f` — entries within a section: one file, one stash, one
  commit row.
- `]c` / `[c` — hunks (`@@` headers) inside an expanded diff.

In views without sections — a log, a blame — `]]` and `]f` degrade to
whatever rows that buffer does have rather than erroring.

## Folding

`TAB` and `S-TAB` route through the ordinary fold engine, not a
magit-specific one, so `za` / `zM` / `zR` and every other fold chord
work here too. See [`folding`](help:folding).

One exception: in [`magit-stash-mode`](help:magit-stash-mode) `z`
creates a stash, so it is not available as the fold prefix there — use
`TAB` / `S-TAB` in that buffer.

In [`magit-status-mode`](help:magit-status-mode) folds nest: closing a
file's fold hides its inline diff, and each `@@` hunk inside that diff
is independently foldable within it.

## See also

- [`magit`](help:magit) — the subsystem overview and entry points.
- [`magit-global-mode`](help:magit-global-mode) — the chords that open
  magit from anywhere (`C-x g`, `C-c g`, `C-c f`).
- `:describe-mode magit-core-mode` — the live keymap for the buffer
  you're in.
