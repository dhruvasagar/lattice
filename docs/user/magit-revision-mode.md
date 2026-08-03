---
summary: "magit-revision-mode: one historical commit in full — git show --stat -p, opened by <CR> on a SHA anywhere one appears. Read-only; <CR> visits a file as of that commit."
related: [magit, magit-revision, ex:magit-log-merged]
---

# magit-revision-mode

One commit, in full: metadata, message, a file-change summary, then the
diff. `git show --stat -p <sha>`, read-only, in `*magit:commit:<sha>*`.

This is **not** the compose buffer — that's
[`magit-commit-mode`](help:magit-commit-mode), where you write a *new*
commit. This one shows a commit that already exists. The buffer names
are similar because git's vocabulary is; the two are unrelated in use.

## The two questions it answers

| Buffer | Shows |
|---|---|
| `*magit:commit:<sha>*` | That commit |
| `*magit:merged:<sha>*` | The **merge** that brought that commit into `HEAD` |

The second is `C-c f M` (or `:magit-log-merged <commit>`), and it is
worth reading carefully: the SHA in the name is the *question*, and the
commit on screen is a **different one** — the merge that landed it,
which is usually the pull request. Everything that acts on a commit
here (`A` cherry-pick, `_` revert, `O` reset, `<CR>`) acts on the merge
you are looking at, not on the commit you asked about.

A commit made straight onto your branch was never merged in. That is
the ordinary case for most history, not a failure, and the buffer says
so in those words rather than coming up empty.

You never open the plain form directly. `<CR>` on a SHA reaches it from everywhere
a SHA appears: [`magit-log-mode`](help:magit-log-mode),
[`magit-blame-mode`](help:magit-blame-mode),
[`magit-rebase-mode`](help:magit-rebase-mode), and magit-status's
Recent commits section. That uniformity is deliberate — every view that
shows a commit navigates to the same place for its detail.

The headerline carries the short SHA, author, relative date, and
subject, so the identity of what you're reading survives scrolling past
the header lines.

## Chords

| Chord | Action |
|---|---|
| `<CR>` | Visit the file at cursor **as of this commit** — from [`magit-hunk-mode`](help:magit-hunk-mode) |

`<CR>` works on either shape a path appears in: a `--stat` summary row
(`" src/main.rs | 12 +++---"`) or a `diff --git a/… b/…` header. From
anywhere inside a file's diff it walks up to that file's header, so you
don't have to land on the header line itself.

`q` / `]]` / `[[` / `]c` / `[c` and the rest come from
[`magit-core-mode`](help:magit-core-mode).

## Behaviour worth knowing

- **`gr` is a deliberate no-op.** A fixed SHA's content never changes,
  so there is nothing to refresh.
- **`<CR>`'s target is always historical, never the working tree.** The
  commit you're reading is fixed, so opening anything other than the
  file exactly as it was in this commit would be misleading — even when
  the working tree happens to match right now. The file opens in
  [`magit-file-revision-mode`](help:magit-file-revision-mode) as
  `*magit:file:<sha>:<path>*`.

## See also

- [`magit-log-mode`](help:magit-log-mode) — the history you reach this
  from.
- [`magit-file-revision-mode`](help:magit-file-revision-mode) — where
  `<CR>` lands.
- [`magit-diff-mode`](help:magit-diff-mode) — how added and removed
  lines are coloured, and the theme elements that control it.
