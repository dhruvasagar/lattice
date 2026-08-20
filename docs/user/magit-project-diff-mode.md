---
summary: "magit-project-diff-mode: identity marker for the project-diff view — every changed file as one editable multibuffer."
related: [magit, multibuffer]
---

# magit-project-diff-mode

An identity marker for the project-diff view: every changed file in the
working tree, composed into one multibuffer you can edit.

## The gap it fills

Magit already diffs in two shapes and neither is this one. The status
buffer's sections are patch text built for staging;
`*magit:diff:<repo>:<path>*`
reads well but is one file at a time, and is still patch text. Missing
was *every changed file at once, as real source you can edit* — you spot
a typo in file 19 of a 30-file review, and otherwise you must leave the
diff, open the file, fix it, and come back.

## Editable, when the thing being compared is a file

An excerpt is a hunk's range **in the working-tree file**, so editing it
lands in the file through the ordinary propagation pipeline — no patch
application, no separate write-back.

That anchoring is also the limit. Only the working tree is a file:

| Comparison | View |
|---|---|
| working tree vs `HEAD` | editable |
| working tree vs index | editable |
| index vs `HEAD` (staged) | read-only |

An index blob is not a file, so there is nothing for an edit to land in.
Those comparisons open read-only and the headerline says which you are
looking at. That is the correct rendering of a comparison between two
things that are not the file on disk, rather than a degraded mode.

## What happens to the colours once you edit

Fix something in an excerpt and that file's `+` colouring, gutter marks
and grey deleted-line rows **go away**, and the headerline gains a note:

```
[project-diff: working tree] 12 hunks in 5 files · 1 edited file — gr to refresh
```

That is deliberate. The diff was computed against the file as it was
when the view opened; the moment you insert a line, every mark below
your edit describes the line above or below the one it is drawn on. The
view would rather show you no diff colouring for a file it can no longer
describe than colouring that is quietly one row out.

Only the file you edited loses its colouring — the other four in the
example keep theirs. `gr` re-scans and brings it all back, including
your edit as part of the diff.

## Folding a large diff

The view has two levels of structure, so `:set foldlevel=0` collapses it
to one row per file and `:set foldlevel=1` to one row per hunk. On a
fifty-file review that is the difference between a list and a wall.

## Keybindings

None of its own. It joins the magit family and takes the family's
chords: `gr` (re-scan, keeping whichever comparison you are looking at),
`q`, and `]]` / `[[` to walk file boundaries.

It joins `magit-nav-mode` rather than `magit-core-mode`, which matters
here: core's single letters (`i`, `C`, `D`, `S`, `U`, `yr`) are meant
for read-only lists, and in a buffer you can type in, `i` has to mean
Insert.

## See also

- [`magit`](help:magit) — the rest of the family.
- [`multibuffer`](help:multibuffer-mode) — excerpts, jumping, editing.
- [`folding`](help:folding) — `foldlevel` and the fold chords.
