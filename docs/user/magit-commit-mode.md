---
summary: "magit-commit-mode: the commit compose buffer — a read-only staged diff on top, an editable message below, C-c C-c to commit and C-c C-k to abort."
related: [magit, magit-commit, magit-amend, ex:magit-commit]
---

# magit-commit-mode

The buffer you write a commit message in. `cc` from
[`magit-status-mode`](help:magit-status-mode), or `:magit-commit` from
anywhere.

```
┌─────────────────────────────────────────────┐
│ Add user authentication endpoint            │  ← subject (line 0)
│                                             │
│ Implements OAuth2 flow with...              │  ← body
│ --- Staged diff (review only) ---           │
│ [read-only] diff of staged changes          │
│ (scrollable, background-tinted)             │
│                                             │
│ C-c C-c commit   C-c C-k abort              │
└─────────────────────────────────────────────┘
```

The message is on **top**, so the cursor opens where you type rather
than below a diff that may be hundreds of lines long. The diff sits
underneath as reference while you write. Same arrangement as
`git commit --verbose` and Emacs magit.

The headerline shows the branch you're committing to and what's staged
— `main  3 files +120 −18` — plus `AMEND` when this will rewrite the
previous commit rather than add one.

## Chords

| Chord | Action |
|---|---|
| `C-c C-c` | Create the commit with the entered message |
| `C-c C-k` | Abort (close the buffer without committing) |
| `<CR>` | Visit the file at cursor, **as staged** — only below the diff marker; in the message it does nothing rather than jumping away mid-sentence. From [`magit-hunk-mode`](help:magit-hunk-mode) |

`gr` / `q` / `]]` / `[[` and the rest come from
[`magit-core-mode`](help:magit-core-mode).

## The two regions

**Message region** (top, editable) — ordinary text, starting at line 0.
First line is the subject, a blank line separates it from the body.
This is a real buffer with the full vim grammar: normal-mode editing,
visual selections, registers, macros.

Everything above the `--- Staged diff ---` marker is the message.
Extraction stops *at* the marker rather than skipping past it, so a
diff line can never end up in your commit message even if you edit or
delete the marker line; a buffer with no marker at all is treated as
entirely message.

**Diff region** (below the marker, read-only) — populated by
`git diff --cached` when the buffer opens. This is the one place magit
loads a diff eagerly rather than on demand: you opened this buffer
specifically to review what you're about to commit, so an empty pane
waiting for `=` would defeat the point.

`<CR>` on a file line there opens the file's **staged** content (the
index blob, via `git show :<path>`) in
[`magit-file-revision-mode`](help:magit-file-revision-mode) — not the
working-tree file. The diff you're reviewing describes what's staged,
which may already differ from a copy you've edited since.



## What the buffer will do when you confirm

The same compose buffer backs several operations. Which one `C-c C-c`
runs is decided by the buffer's **name**, so `:ls` always shows what
you are about to do — there is no hidden state to get out of step with
the text on screen.

| Buffer | Opened by | `C-c C-c` runs | Pre-filled? |
|---|---|---|---|
| `*magit:commit*` | `cc`, `:magit-commit` | `git commit` | no |
| `*magit:amend*` | `ca` | `git commit --amend` | previous message |
| `*magit:reword*` | commit menu `w` | `git commit --amend --only` | previous message |
| `*magit:augment:<sha>*` | commit menu `A`, `:magit-augment` | `git commit --squash=<sha>` | no |
| `*magit:merge-edit:<branch>*` | merge menu `e` | `git merge <branch>` | no |

**Amend and reword are not the same.** Amend sweeps in whatever you
have staged; reword passes `--only` and changes the message alone. If
you have staged work you are not ready to commit, reword is the one
that leaves it staged. The headerline carries `AMEND` for both, so
check the buffer name when it matters.

**Augment writes a note onto a squash marker.** It records a *new*
commit whose subject git generates as `squash! <target's subject>`,
with whatever you type appended below it. Nothing is rewritten yet —
the fold-in happens the next time you rebase with `--autosquash`. What
you write here is a note to yourself for that moment, which is why the
buffer starts empty rather than seeded with the target's message.

**Merge-edit finishes the merge.** It is the merge menu's `e`, and it
differs from that menu's `n` (don't commit) precisely in that it
*does* commit, using the message you write. If the branches merge
cleanly you end up with a merge commit; a conflict leaves you in the
usual conflicted state and your message is not used.

Both targeted forms carry their target in the buffer name because the
buffer is opened well before the commit runs. A sha or branch that
lives only in the name cannot drift out of sync with the buffer you
are looking at.

## Behaviour worth knowing

- **Empty subject is rejected.** Pressing `C-c C-c` with a blank
  subject line reports an error rather than silently doing nothing.
- **The buffer closes when the commit is *kicked off*,** not when git
  confirms it landed. A failure is logged rather than reported back
  into a buffer that has already gone. The commit action refreshes
  magit-status behind it as part of its own completion — not a
  background watcher, of which there is none.

## See also

- [`magit-status-mode`](help:magit-status-mode) — where `cc` / `ca`
  come from, and where staging happens.
- [`magit-revision-mode`](help:magit-revision-mode) — a *historical*
  commit's detail, which is a different buffer from this one despite
  the similar name.
- [`magit-diff-mode`](help:magit-diff-mode) — how added and removed
  lines are coloured, and the theme elements that control it.
