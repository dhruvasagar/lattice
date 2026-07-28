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
│ [read-only] diff of staged changes          │
│ (scrollable, background-tinted)             │
│ ─────────── message area ────────────────   │
│ │ Add user authentication endpoint          │  ← subject
│ │                                           │
│ │ Implements OAuth2 flow with...            │  ← body
│ ──────────────────────────────────────────  │
│ C-c C-c commit   C-c C-k abort              │
└─────────────────────────────────────────────┘
```

The headerline shows the branch you're committing to and what's staged
— `main  3 files +120 −18` — plus `AMEND` when this will rewrite the
previous commit rather than add one.

## Chords

| Chord | Action |
|---|---|
| `C-c C-c` | Create the commit with the entered message |
| `C-c C-k` | Abort (close the buffer without committing) |
| `<CR>` | Visit the file at cursor, **as staged** — only in the diff region |

`gr` / `q` / `]]` / `[[` and the rest come from
[`magit-core-mode`](help:magit-core-mode).

## The two regions

**Diff region** (top, read-only) — populated by `git diff --cached`
when the buffer opens. This is the one place magit loads a diff eagerly
rather than on demand: you opened this buffer specifically to review
what you're about to commit, so an empty pane waiting for `=` would
defeat the point.

`<CR>` on a file line there opens the file's **staged** content (the
index blob, via `git show :<path>`) in
[`magit-file-revision-mode`](help:magit-file-revision-mode) — not the
working-tree file. The diff you're reviewing describes what's staged,
which may already differ from a copy you've edited since.

**Message region** (below the marker, editable) — ordinary text. First
line is the subject, a blank line separates it from the body. This is a
real buffer with the full vim grammar: normal-mode editing, visual
selections, registers, macros.

## Amend

Opened via `ca` from magit-status (buffer name `*magit:amend*`), the
previous commit's message is pre-populated so you're editing it rather
than retyping it. `C-c C-c` amends; the commit count doesn't change,
and the headerline carries `AMEND` throughout so you can't mistake
which one you're in.

## Behaviour worth knowing

- **Empty subject is rejected.** Pressing `C-c C-c` with a blank
  subject line reports an error rather than silently doing nothing.
- **The buffer closes when the commit is *kicked off*,** not when git
  confirms it landed. A failure is logged rather than reported back
  into a buffer that has already gone. magit-status auto-refreshes
  behind it.

## See also

- [`magit-status-mode`](help:magit-status-mode) — where `cc` / `ca`
  come from, and where staging happens.
- [`magit-revision-mode`](help:magit-revision-mode) — a *historical*
  commit's detail, which is a different buffer from this one despite
  the similar name.
