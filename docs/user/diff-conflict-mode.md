---
summary: "diff-conflict-mode: the conflict-resolution surface. Activates on buffers whose diff session carries conflict regions, and contributes the fugitive-style d2o/d3o/d2p/d3p/dB chords."
related: [diff, conflict, merge, rebase]
---

# diff-conflict-mode

The minor mode for **conflict resolution**, separate from the two-way
diffing surface of [`diff-mode`](help:diff-mode). It activates on a
buffer whose diff session actually carries conflict regions.

## The chords

Vim-fugitive's three-way family, so the muscle memory carries over.
Each acts on the conflict region **under the cursor**:

| Chord | Action |
|---|---|
| `d2o` | Keep **ours** — the local side, the one already in the buffer |
| `d3o` | Keep **theirs** — take the other side into the local buffer |
| `d2p` | Put **ours** — push the local side into the ours buffer |
| `d3p` | Put **theirs** — push the local side into the theirs buffer |
| `dB` | Keep **both** — retain the two sides, conflict markers removed |

`d2o` and `d2p` are degenerate on purpose: the local buffer already
*is* "ours", so there is nothing to apply or push. They report that
rather than doing nothing silently, so a mistyped `d3o` does not look
like a mode that ignored you.

## Which side is "ours"?

**This inverts depending on how the conflict was created**, and it is
the single most confusing thing about resolving one:

- In a **merge**, "ours" is the branch you are on and "theirs" is the
  branch being merged in. What everyone expects.
- In a **rebase**, **cherry-pick**, **revert** or **`git am`**, git
  replays your work *onto* the other side. So "ours" is the
  **upstream** you are replaying onto, and "theirs" is **your own
  commit**. The names are backwards from the intuition.

The magit status headerline says which operation is in flight —
`MERGING`, `REBASING`, `CHERRY-PICKING`, `REVERTING`, `APPLYING` — so
you can tell which reading applies before you press `d3o`. The
unmerged file labels ("added by us", "deleted by them") follow git's
wording and therefore the same inversion.

## Why it's separate from diff-mode

Resolving a conflict and diffing two files are different jobs that
happen to share a rendering. Diffing is symmetric — two versions, and
you move hunks between them. Conflict resolution is a decision per
region with a fixed vocabulary (ours / theirs / both), against a
three-way base.

Giving them separate modes means the conflict chords exist without
appearing in every ordinary `:diffthis`, and the gutter can
distinguish a conflict from a change without `diff-mode` growing a
conditional.

## What activates it

A diff session whose sign map contains conflict regions. In practice:
a merge or rebase that left conflict markers in a file you then diff.

## Not implemented yet

- **Conflict navigation.** There is no next-conflict / prev-conflict
  motion. `]c` / `[c` ([`diff-mode`](help:diff-mode)) walk diff hunks,
  which in a conflicted file will step through conflict regions among
  the rest, but nothing jumps conflict-to-conflict.
- **A dedicated conflict gutter.** Conflict regions carry the diff
  sign map's conflict kind, but there is no separate marker column.

## See also

- [`diff-mode`](help:diff-mode) — the two-way surface, hunk navigation
  with `]c` / `[c`, and `do` / `dp`.
- [`magit-rebase-mode`](help:magit-rebase-mode) — where conflicts
  during a rebase come from.
- [`magit`](help:magit) — the status buffer, where a stopped
  operation is announced and `--continue` / `--skip` / `--abort` live.
