---
summary: "magit-stash-show-mode: one stash's patch — git stash show -p, opened by <CR> in the stash list. Read-only, fixed content."
related: [magit, magit-stash-show]
---

# magit-stash-show-mode

One stash's patch, in `*magit:stash:<repo>:<n>*`. `<CR>` on a row in
[`magit-stash-mode`](help:magit-stash-mode).

It answers the question you have before pressing `a`: *what would this
actually apply to my working tree?* The headerline names which stash
and its subject — `stash@{2}  WIP on main: fix the thing`.

## Behaviour worth knowing

- **No mode-specific chords.** `q` / navigation come from
  [`magit-core-mode`](help:magit-core-mode), and everything that acts
  on the patch — `s` / `u` / `x`, `a` / `-`, `]c` / `[c` and `<CR>` —
  from [`magit-hunk-mode`](help:magit-hunk-mode), which is active in
  every buffer that renders a diff. This view had *none* of them before
  that mode existed; `<CR>` here now opens the file as the stash left
  it (`git show stash@{n}:<path>`).
- **`gr` is a deliberate no-op** — `stash@{n}`'s patch does not change
  under a fixed index.
- **Indices renumber.** Dropping or popping a stash shifts the *others*
  down, so a detail buffer opened earlier still names the index it was
  opened at. After a drop, re-open from the refreshed list rather than
  trusting an open detail buffer's title.
- **An empty patch says so.** A stash of untracked files only has no
  patch to show without `-u`; the buffer reports that rather than
  rendering blank, which would read as a failure.

## See also

- [`magit-stash-mode`](help:magit-stash-mode) — the list, and the
  apply / pop / drop chords.
- [`magit-diff-mode`](help:magit-diff-mode) — how added and removed
  lines are coloured, and the theme elements that control it.
