---
summary: "magit-refs-mode: every branch, remote-tracking branch and tag in one buffer, with each branch's ahead/behind against its upstream. <CR> shows the commit a ref points at; c checks out."
related: [magit, magit-branch-mode, ex:magit-refs]
---

# magit-refs-mode

Every ref in the repository, grouped: local branches, remote-tracking
branches, tags. `:magit-refs`, or `y` in the repo [dispatch
transient](help:magit-transient) — magit's own key for it.

```
Branches (2)
* main                        a1b2c3d   ahead 2             Add the refs buffer
  feature/x                   e4f5g6h                       Work in progress

Remotes (1)
  origin/main                 9z8y7x6                       Add the refs buffer

Tags (1)
  v1.0.0                      9z8y7x6                       Release 1.0
```

Four columns: the ref's name (with `*` on the checked-out branch), the
commit it points at, how far it is from its upstream, and that commit's
subject.

The headerline carries the count of each kind. A group with nothing in
it is left out entirely — a repository with no remotes is ordinary, and
a `Remotes (0)` heading is a row that says nothing.

## What this is, and what `magit-branch-mode` is

They look adjacent and answer different questions.

| | Answers |
|---|---|
| [`magit-branch-mode`](help:magit-branch-mode) | "What local branches do I have, and let me act on one" — checkout, create, delete, merge |
| **`magit-refs-mode`** | "What refs exist, and where does each point" — including tags and remote-tracking branches, which the branch list never shows |

The ahead/behind column is the one people open this buffer for: it is
how you see that `main` is 2 commits ahead of `origin/main` without
running anything.

`gone` in that column means the upstream branch was deleted on the
remote — usually a merged pull request whose branch was cleaned up.

## Chords

| Chord | Action |
|---|---|
| `<CR>` | Show the commit this ref points at |
| `c` | Check out the ref at cursor |
| `gr` | Refresh (re-walk the refs) |
| `A` / `_` / `O` | Cherry-pick / revert / reset to the commit this ref points at |

`q` and navigation come from
[`magit-core-mode`](help:magit-core-mode).

## Why `<CR>` shows rather than checks out

`magit-branch-mode`'s `<CR>` checks out, and this one does not. That is
deliberate, and it goes the way the rest of the editor already goes:
across [magit-log](help:magit-log-mode),
[magit-blame](help:magit-blame-mode),
[magit-rebase](help:magit-rebase-mode) and magit-status's recent
commits, `<CR>` means **show the commit detail**. The branch list is the
one buffer that departs from it.

There is also a concrete cost here that the branch list does not have.
Two of the three groups in this buffer — tags and remote-tracking
branches — have no local branch to move, so checking one out leaves you
on a **detached HEAD**: a state that is easy to enter, hard to
recognise, and hard to leave. Putting that behind the most reflexive key
in a buffer whose purpose is looking things up is the wrong default.

So `c` checks out, and on a tag or a remote-tracking branch it refuses
and says why, pointing at what you almost certainly wanted — a branch
made from that ref:

```
magit: checking out v1.0.0 would detach HEAD — make a branch from it
instead (:magit-branch-create <name>, or `b` `c` in the dispatch)
```

## Behaviour worth knowing

- **The commit a ref points at is passed around by its full id**, never
  the abbreviation shown in the column. An abbreviation is ambiguous in
  principle, and git resolves the ambiguity by refusing — which would
  surface as a `<CR>` that opened nothing.
- **Long ref names push the columns right rather than being cut.** A
  truncated ref name is not a ref name, and naming things is what this
  buffer is for.
- **`refs/stash`, `refs/notes/*` and `refs/bisect/*` are not shown.**
  They are real refs, but they belong to features with their own
  surfaces — see [`magit-stash-mode`](help:magit-stash-mode).

## See also

- [`magit-branch-mode`](help:magit-branch-mode) — the local branch list,
  and where branch operations live.
- [`magit-revision-mode`](help:magit-revision-mode) — where `<CR>`
  lands.
- [`magit-remote-mode`](help:magit-remote-mode) — the remotes
  themselves, with their URLs, rather than the branches on them.
