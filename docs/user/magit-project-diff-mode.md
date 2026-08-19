---
summary: "magit-project-diff-mode: identity marker for the project-diff view — every changed file as one editable multibuffer."
related: [magit, multibuffer]
---

# magit-project-diff-mode

An identity marker for the project-diff view: every changed file in the
working tree, composed into one multibuffer you can edit.

## The gap it fills

Magit already diffs in two shapes and neither is this one. The status
buffer's sections are patch text built for staging; `*magit:diff:<path>*`
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

## Folding a large diff

The view has two levels of structure, so `:set foldlevel=0` collapses it
to one row per file and `:set foldlevel=1` to one row per hunk. On a
fifty-file review that is the difference between a list and a wall.

## Keybindings

None of its own — it joins `magit-core-mode`, so `gr` (refresh), `q`,
and `]]` / `[[` work here exactly as in every other magit buffer.

## See also

- [`magit`](help:magit) — the rest of the family.
- [`multibuffer`](help:multibuffer-mode) — excerpts, jumping, editing.
- [`folding`](help:folding) — `foldlevel` and the fold chords.
