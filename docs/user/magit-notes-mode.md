---
summary: "magit-notes-mode: the note attached to one commit, as an editable buffer. C-c C-c saves, C-c C-k closes without saving; clearing the buffer removes the note."
related: [magit, magit-revision-mode, ex:magit-note-edit, ex:magit-note-remove, ex:magit-note-merge]
---

# magit-notes-mode

A git note is a scrap of text attached to a commit *after* the fact —
review comments, a ticket link, "this is the one that broke CI". The
commit's SHA never changes, so notes are how you annotate history
without rewriting it.

This buffer is one commit's note, editable. `C-c g T T` opens it, or
`:magit-note-edit <commit>`.

```
new note                    a1b2c3d  Jane Doe  3 days ago  Fix the thing
────────────────────────────────────────────────────────────────────────
Reviewed by Sam. The retry loop here is load-bearing — see #412 before
touching it.
```

The headerline says **which commit** and whether you are creating or
overwriting. That matters more here than in other magit buffers: the
body is bare text with nothing in it naming what it is attached to, so
a note written against the wrong commit would look exactly like one
written against the right one.

## Chords

| Chord | Action |
|---|---|
| `C-c C-c` | Save the note |
| `C-c C-k` | Close without saving |

Editing is ordinary vim — this is one of the few magit buffers that is
**not** read-only, because typing in it is the point.

## Behaviour worth knowing

- **Clearing the buffer removes the note.** An empty note is not a
  thing git stores, so saving an empty buffer is how you delete one.
  Doing that on a commit that never had a note is not an error either.
- **Saving overwrites.** The buffer was seeded with the existing note,
  so refusing to replace it would refuse every edit after the first.
- **The buffer closes as soon as the write is kicked off,** not when it
  finishes — the same optimistic close
  [`magit-commit-mode`](help:magit-commit-mode) makes. A failure is
  logged rather than reported back into a buffer that has gone.
- **You never see `$EDITOR`.** `git notes edit` would open one, which
  inside an editor means a child process waiting on a terminal that
  isn't there. This buffer *is* the editor.

## Where notes show up

Nowhere special — `git show` prints them by default, so a commit's note
appears under its message in the
[revision view](help:magit-revision-mode) with no extra step.

## The notes menu (`C-c g T`)

| Key | Action |
|---|---|
| `T` | Edit the note on a commit — opens this buffer |
| `r` | Remove the note from a commit |
| `m` | Merge another notes ref into this one |
| `p` | Prune notes whose commit no longer exists — **asks first** |

`T` and `r` need a commit. From a magit buffer with one under the
cursor they use it; from anywhere else they open the commit picker —
the same way `A` / `_` / `O` behave.

`r` does not ask, and `p` does. Removing one note loses only that note
and it is one `T` away from being retyped; prune drops an unbounded
number and names none of them.

### Merging notes

Notes live on a ref (`refs/notes/commits` by default), and two clones
can grow different notes for the same commit. `m` opens a
[picker of refs](help:picker#the-magit-sources) to merge in — every
branch, remote-tracking ref and tag the repository has — optionally
with a strategy:

| Strategy | Resolution |
|---|---|
| `manual` (default) | Stop on a conflict and let you resolve it |
| `ours` / `theirs` | Keep one side |
| `union` | Concatenate both |
| `cat_sort_uniq` | Concatenate, sort, drop duplicates |

`:magit-note-merge refs/notes/other theirs` is the scriptable form. A
**misspelled strategy is refused** rather than quietly falling back to
`manual` — `ours` and `theirs` resolve in opposite directions, and a
silent fallback would stop a merge you asked to resolve.

A `manual` conflict leaves the merge in progress, and `C-c g T` then
shows only the two ways out:

| Key | Action |
|---|---|
| `c` | Commit the merge, keeping the resolved notes |
| `a` | Abort, restoring the notes ref |

The menu is gated the same way [`B` bisect](help:magit-transient) is:
outside a merge those two error, so showing them would be showing rows
that fail when pressed.

## Not here

The four **configure** rows magit has (`c` / `d` / `C` / `D`, setting
`core.notesRef` and `notes.displayRef`) are absent. They are transient
*variable rows* — a menu row that renders a config value and edits it in
place — which lattice's transients don't have, the same gap that makes
remote URLs a buffer rather than a menu. Per-repo git config is more
likely to end up under `:customize` than in a hand-rolled menu.

## See also

- [`magit-revision-mode`](help:magit-revision-mode) — where a commit's
  note is displayed.
- [`magit-commit-mode`](help:magit-commit-mode) — the other editable
  magit buffer, and the one this is modelled on.
