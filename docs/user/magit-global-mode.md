---
summary: "magit-global-mode: the always-active entry chords — C-x g opens magit-status, C-c g the repo dispatch transient, C-c f the file dispatch transient."
related: [magit, magit-transient]
---

# magit-global-mode

The three chords that get you *into* magit. This minor mode is active
on every buffer of every kind — documents, help, the file tree, oil,
terminals — because "show me the state of this repository" is a
question you ask from wherever you happen to be, not from a place you
first have to navigate to.

## Chords

| Chord | Opens |
|---|---|
| `C-x g` | [`magit-status-mode`](help:magit-status-mode) for the current repo |
| `C-c g` | The repo [dispatch transient](help:magit-transient) |
| `C-c f` | The file [dispatch transient](help:magit-transient) |

The keys follow Emacs magit's own, so the muscle memory transfers.

## Repo dispatch versus file dispatch

The two transients differ in what they act on, which is the thing worth
internalising:

- **`C-c g`** is repo-scoped. Its items open views (status, commit,
  log, branch, stash, rebase, diff) or run whole-repo operations
  (fetch, pull, push, stash).
- **`C-c f`** is file-scoped, and the file it means is the one in
  *your current buffer* — not an entry under the cursor somewhere
  else. Pressed in `src/main.rs`, `d` diffs `src/main.rs`, `l` logs
  it, `b` blames it.

That distinction is why `C-c f` is worth reaching for from an ordinary
editing buffer: it answers questions about the file you are already
looking at.

## Operations without a buffer

`C-c g`'s fetch / pull / push / stash items run git rather than opening
a view. They return immediately with a `magit: pushing…` echo; the
outcome arrives as a [notification](help:notifications) when it
finishes — success and failure both — because the operation outlives
the keystroke that started it. The full output is in the log and in
`*messages*`; the notification carries the first line. Git runs with
`GIT_TERMINAL_PROMPT=0`, so a missing or expired credential fails fast
instead of hanging on a prompt that can never be answered.

Each also has an ex-command — `:magit-fetch`, `:magit-pull`,
`:magit-push`, `:magit-stash` — reaching the same implementation, for
when you want to script or rebind one. See [`magit`](help:magit).

## See also

- [`magit-transient`](help:magit-transient) — every item in both menus.
- [`magit-core-mode`](help:magit-core-mode) — the chords shared *inside*
  magit buffers, once you're there.
