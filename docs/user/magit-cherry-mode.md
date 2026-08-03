---
summary: "magit-cherry-mode: which of your commits are not upstream yet, and which already are under a different SHA. <CR> opens a commit; A cherry-picks it."
related: [magit, magit-log-mode, ex:magit-cherries]
---

# magit-cherry-mode

"Which of my commits has upstream not taken yet?"

`C-c g Y`, or `:magit-cherries <upstream> [<head>]`.

```
Cherries  HEAD vs origin/main

+ a1b2c3d  Add the refs buffer
- e4f5g6h  Fix the retry loop
+ 9z8y7x6  Document the notes menu
```

| Mark | Meaning |
|---|---|
| `+` | Not upstream. This is yours to push, rebase or cherry-pick. |
| `-` | **Already upstream, under a different SHA.** |

## Why not just `git log origin/main..HEAD`

Because of the `-` rows, and they are the whole point.

When a commit is cherry-picked or rebased upstream it arrives with a
*different* SHA. A range walk compares SHAs, so it still lists your
original as missing — and you go to push something that is already
there. `git cherry` compares **patch-ids** instead: same change, same
id, regardless of SHA. A `-` row is "this landed; your copy is
redundant."

That is usually the signal to drop your local copy rather than push it.

## Chords

| Chord | Action |
|---|---|
| `<CR>` | Show the commit at cursor |
| `A` | Cherry-pick it onto the current branch |
| `_` | Revert it |
| `O` | Reset to it (`s` soft / `m` mixed / `h` hard) |
| `gr` | Re-run the comparison |

`A` is the reason the command is called *cherry*: a `+` row is exactly
the thing you reach for cherry-pick with. `q` and navigation come from
[`magit-core-mode`](help:magit-core-mode).

The headerline carries what is being compared and the two counts
separately — `3 ahead  1 already upstream`. They are not summed, because
they call for opposite actions.

## Behaviour worth knowing

- **Commits are passed on by full SHA**, never the seven characters
  shown. An abbreviation is ambiguous in principle and git resolves the
  ambiguity by refusing, which would surface as an `A` that did nothing.
- **The default `<head>` is `HEAD`.** `:magit-cherries origin/main
  feature/x` compares a branch you are not on.
- **No upstream, no answer.** "Not upstream yet" is meaningless without
  naming the upstream, so it is required rather than guessed.

## See also

- [`magit-log-mode`](help:magit-log-mode) — the full history, when you
  want commits rather than the comparison.
- [`magit-refs-mode`](help:magit-refs-mode) — ahead/behind **counts**
  for every branch at once, when you want the summary rather than the
  commits.
