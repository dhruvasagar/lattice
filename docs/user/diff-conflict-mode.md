---
summary: "diff-conflict-mode: activates on buffers whose diff session carries conflict regions. Currently a marker — the resolution chords are not implemented yet."
related: [diff, conflict, merge]
---

# diff-conflict-mode

The minor mode for **conflict resolution**, separate from the two-way
diffing surface of [`diff-mode`](help:diff-mode). It activates on a
buffer whose diff session actually carries conflict regions.

> **Status: a marker, not a feature yet.** The mode exists and
> activates on the right buffers. The conflict-resolution chords —
> keep-ours, keep-theirs, keep-both, next-conflict — and the conflict
> gutter are **not implemented**. Pressing anything expecting smerge
> behaviour will do nothing. This page documents a surface that exists
> so you can see what's real; see [`diff-mode`](help:diff-mode) for
> what you can actually do with conflicts today.

## Why it's separate from diff-mode

Resolving a conflict and diffing two files are different jobs that
happen to share a rendering. Diffing is symmetric — two versions, and
you move hunks between them. Conflict resolution is a decision per
region with a fixed vocabulary (ours / theirs / both), against a
three-way base.

Giving them separate modes means the conflict chords can exist without
appearing in every ordinary `:diffthis`, and the gutter can distinguish
a conflict from a change without `diff-mode` growing a conditional.
The decomposition is the part that landed; the behaviour is the part
that hasn't.

## What activates it

A diff session whose sign map contains conflict regions. In practice:
a merge or rebase that left conflict markers in a file you then diff.

## See also

- [`diff-mode`](help:diff-mode) — the two-way surface, hunk navigation
  with `]c` / `[c`, and `do` / `dp`.
- [`magit-rebase-mode`](help:magit-rebase-mode) — where conflicts
  during a rebase come from.
