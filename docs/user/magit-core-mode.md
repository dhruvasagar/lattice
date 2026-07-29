---
summary: "magit-core-mode: the shared minor mode active in every magit buffer — gr to refresh, q to close, ]]/[[ and ]f/[f and ]c/[c to navigate, TAB to fold."
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
| `]c` / `[c` | Next / previous hunk |
| `TAB` | Toggle the section or hunk fold at cursor |
| `S-TAB` | Cycle section visibility (all → changed only → collapsed → all) |
| `A` | Cherry-pick the commit under the cursor |
| `V` | Revert the commit under the cursor |
| `Os` / `Om` / `Oh` | Reset `--soft` / `--mixed` / `--hard` to the commit under the cursor |

## Operating on the commit under the cursor

`A`, `V` and `O…` work in every view that shows a commit — the
[log](help:magit-log-mode), magit-status's Recent commits, the
[revision](help:magit-revision-mode) view, the
[rebase todo](help:magit-rebase-mode) — because each view answers "what
commit is under the cursor?" for its own row format and one shared
handler acts on the answer. Keys follow Emacs magit's own.

On a row with no commit — a file entry, a stash, a `--graph` connector
line — they do nothing rather than acting on a neighbour.

| Chord | Runs | Keeps |
|---|---|---|
| `A` | `git cherry-pick <commit>` | — |
| `V` | `git revert --no-edit <commit>` | — |
| `Os` | `git reset --soft <commit>` | index **and** working tree |
| `Om` | `git reset --mixed <commit>` | working tree |
| `Oh` | `git reset --hard <commit>` | **nothing** — asks first |

`Oh` is the only one that destroys uncommitted work, so it is the only
one that asks — the same two-step `x`, branch-delete and stash-drop
use, where the chord itself performs no git call at all. `--soft` and
`--mixed` keep your changes, and prompting on those would just train
you to dismiss the prompt that matters.

`V` passes `--no-edit`: git would otherwise open `$EDITOR` for the
revert message, which inside lattice means waiting on a prompt you
cannot answer.

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

In [`magit-status-mode`](help:magit-status-mode) folds nest: closing a
file's fold hides its inline diff, and each `@@` hunk inside that diff
is independently foldable within it.

## See also

- [`magit`](help:magit) — the subsystem overview and entry points.
- [`magit-global-mode`](help:magit-global-mode) — the chords that open
  magit from anywhere (`C-x g`, `C-c g`, `C-c f`).
- `:describe-mode magit-core-mode` — the live keymap for the buffer
  you're in.
