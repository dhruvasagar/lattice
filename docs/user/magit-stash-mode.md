---
summary: "magit-stash-mode: the stash list — a apply, p pop, d drop (asks first), z create, <CR> to preview a stash's patch."
related: [magit, magit-stash, ex:magit-stash-list, ex:magit-stash]
---

# magit-stash-mode

Every stash you're holding, one per row. `:magit-stash-list`, or `z l`
in the repo [dispatch transient](help:magit-transient).

Rows read `  stash@{N} <message>` — the same shape magit-status uses
for its Stashes section, so a stash looks the same wherever you meet
it. The `stash@{N}` label is not decoration: every chord in this buffer
locates its stash by reading that label back out of the row under the
cursor.

The headerline carries the count.

## Chords

| Chord | Action |
|---|---|
| `a` | Apply the stash at cursor (keeps it in the list) |
| `p` | Pop the stash at cursor (apply, then drop) |
| `d` | Drop the stash at cursor — **asks first** |
| `z` | Create a stash from the working tree (so `z` is not the fold prefix in this buffer — use `TAB` / `S-TAB`) |
| `<CR>` | Show this stash's patch in [`magit-stash-show-mode`](help:magit-stash-show-mode) |
| `gr` | Refresh (re-run `git stash list`) |

## Why only `d` asks

A dropped stash is gone — there is no reflog for stash content once the
entry is removed. `a` and `p` both put the content somewhere you can
still see it (your working tree), so they act immediately. `d` routes
through a confirmation that names the stash (`Drop stash@{2}?`), and
the chord itself performs no git call at all — answering `n` cannot
mutate anything, because the mutating code is on the other side of the
prompt rather than behind a flag that could be forgotten.

The same two-step applies to discarding a file in magit-status,
deleting a branch, and aborting a rebase.

## Preview before you apply

`<CR>` answers "what would `a` actually put in my working tree?" before
you press `a`. It opens the stash's patch in its own buffer.

magit-status keeps a different behaviour for the same key: `<CR>` on a
stash row *there* toggles the patch inline, because there a stash is
one row among many and the surrounding sections are the context. Here
the stash is the subject.

## Behaviour worth knowing

- **`z` always runs plain `git stash push`** — no flag yet for
  including untracked files or attaching a message.
- **Dropping or popping renumbers the others.** `stash@{N}` is a
  position, not an identity, so after a drop the list is the source of
  truth — refresh (`gr`) rather than trusting an index you read a
  moment ago.

## See also

- [`magit-stash-show-mode`](help:magit-stash-show-mode) — the patch
  view `<CR>` opens.
- [`magit-status-mode`](help:magit-status-mode) — the Stashes section
  and its inline toggle.
