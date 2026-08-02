---
summary: "magit-log-mode: the commit history graph — git log --oneline --graph --decorate, <CR> on a row opens that commit's detail. Repo-wide or scoped to one file."
related: [magit, magit-log, ex:magit-log]
---

# magit-log-mode

The commit history, as a graph. `:magit-log` opens the repo-wide view;
content comes from `git log --oneline --graph --decorate -50`.

The headerline names what's being logged, how many commits are shown,
and the path filter when there is one: `HEAD  50 commits  src/main.rs`.

## Chords

| Chord | Action |
|---|---|
| `<CR>` | Show the commit at cursor in [`magit-revision-mode`](help:magit-revision-mode) |
| `gr` | Refresh (re-run `git log`) |
| `D` | Log arguments — `-a` all refs, `-n` commit count, `-A` filter by author |

`q` / `]]` / `[[` / `]f` / `[f` come from
[`magit-core-mode`](help:magit-core-mode).

`<CR>` finds the SHA as the first hex-looking token on the row,
wherever the graph drawing characters happen to end — so it works on
any commit row regardless of how deeply indented the graph is at that
point. On a pure connector row (`|\`, `|/` — no commit) it does
nothing, which is the right answer: there is no commit there.

## File-scoped logs

`l` in the [file dispatch transient](help:magit-transient) (`C-c f`)
opens `*magit:log:<path>*` — the same mode, scoped to one file's
history (`git log -- <path>`). A file with no commits touching it says
so rather than leaving a blank buffer, which matters because an
untracked or brand-new file is a common and legitimate case.

## Reading the graph

- SHAs are abbreviated and coloured (`magit.sha`).
- Ref decorations — `(HEAD -> main, origin/main)` — get their own
  colour (`magit.ref.decoration`).
- Subjects are left plain: the graph and refs are already visually
  busy, and colouring the subject too would compete with them for
  attention on the same line.

## Behaviour worth knowing

- **The count, graph, and decoration are hardcoded** — `-50`,
  `--graph`, `--decorate`. `magit.log.count` and friends read like
  options but are **not registered**: `:set magit.log.count=100` fails
  with `unknown option`. Use `D` to change them for this view. See
  [`magit`](help:magit#options) for the two magit options that *are*
  registered.
- **Log arguments aren't interactively configurable either.** There is
  no Log submenu inside `C-c g` — it's a flat menu, and `l` just opens
  this buffer.

## See also

- [`magit-revision-mode`](help:magit-revision-mode) — where `<CR>`
  lands.
- [`magit-blame-mode`](help:magit-blame-mode) — the other way into a
  commit, starting from a line rather than from history.
