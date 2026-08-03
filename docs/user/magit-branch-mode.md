---
summary: "magit-branch-mode: the local branch list — <CR> checks out, c runs the create wizard, d deletes (asks first), m merges into the current branch."
related: [magit, magit-branch, ex:magit-branch, ex:magit-branch-create]
---

# magit-branch-mode

Your local branches, with the checked-out one marked `*` and coloured.
`:magit-branch`, or `b` `L` in the repo [dispatch
transient](help:magit-transient) — the other branch operations live
alongside it in that submenu, and several of them (checkout, create,
rename, delete) no longer need this buffer at all.

The headerline carries the current branch and the total.

## Chords

| Chord | Action |
|---|---|
| `<CR>` | Check out the branch at cursor |
| `c` | Create a branch — a two-step wizard |
| `d` | Delete the branch at cursor — **asks first** |
| `m` | Merge the branch at cursor into the current branch |
| `gr` | Refresh (re-list branches) |

`q` and navigation come from
[`magit-core-mode`](help:magit-core-mode).

## The create wizard (`c`)

`c` opens the Emacs-magit-style two-step flow:

1. A picker lists your local branches — choose one as the **base**.
2. Submitting opens a prompt, `New branch name (from <base>):` — type
   the name and press Enter.

The branch is created from that base and checked out
(`git checkout -b <name> <base>`). `<Esc>` at either step cancels
cleanly.

If you want the quick path instead, `:magit-branch-create <name>`
creates from HEAD with no base choice and no prompt — the scriptable
one-shot. The wizard is an additional interactive path, not a
replacement.

`:magit-branch-delete <name>` is the matching one-shot for deletion. It
still asks — the confirmation is the point of it, not a step the
scriptable form skips.

## Why `d` asks

`d` is a **force** delete (`git branch -D`), which discards unmerged
commits. That is irreversible in the way that matters — the commits
become unreachable — so it routes through a confirmation naming the
branch (`Delete branch feature/foo?`), and the chord itself performs no
git call at all. Answering `n` cannot mutate anything.

`<CR>` (checkout) and `m` (merge) act immediately: neither destroys
work.

## See also

- [`magit-log-mode`](help:magit-log-mode) — what's actually on a branch
  before you delete it.
- [`magit-rebase-mode`](help:magit-rebase-mode) — the other way to move
  commits between branches.
