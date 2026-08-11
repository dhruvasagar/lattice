# Intra-line diff refinement

How a diff shows **which part of a line changed**, not merely that it
changed.

Companion to [`span-layering.md`](span-layering.md) (the composition
contract this builds on) and [`diff-system.md`](diff-system.md) (hunk
computation). Sequencing:
[`../operations/slice-plans/diff-refinement.md`](../operations/slice-plans/diff-refinement.md).

## 1. The gap

Every diff surface in lattice colours a changed line **uniformly**.
`magit-status`'s inline hunks, `magit-diff-mode`, the commit buffer's
staged region, `stash-show` — all route through
`highlight::diff_styled_spans`, which emits one span per line:

```rust
diff.lines().map(|line| spans_for(classify_diff_line(line), line.len()))
```

So a one-character change renders identically to a fully-rewritten
line. The reader has to diff the pair by eye — exactly the work the
tool exists to do.

Every comparable tool refines: magit (`magit-diff-refine-hunk`), git
(`--word-diff`), delta, difftastic, GitHub, VS Code, Zed. This is a
**convention gap**, not a missing nicety, and the UX-convention rule
says convention leads on a surface this well-established.

## 2. Refinement is a background, not a foreground

The layering contract (`span-layering.md` §1) puts styling on two
independent axes:

| Axis | Mechanism | Carries |
|---|---|---|
| Foreground | `StyledSpan` lists | Syntax, markers, headers |
| Background | Sign map, per row | Full-row diff tints |

**Refinement must ride the background axis**, and this is the
load-bearing decision of the whole design.

The tempting cheap route is a foreground span: push a "refined" colour
before the syntax layer and it wins those bytes. It works with the
existing mechanism and needs no new plumbing. **Reject it.** DS.1–DS.5
just gave diffs syntax highlighting; a foreground refinement would
destroy that colouring on precisely the bytes the user is looking
hardest at, trading one signal for another. Convention agrees — magit,
delta, difftastic and GitHub all emphasise refinement with a
*stronger background*, keeping token colour intact.

On the background axis the two compose with no interaction at all,
which is the property §1 exists to state.

## 3. The mechanism gap

The background axis is **per row** today. `Cell.bg` is set by the
renderer from the row's `DiffSignKind`; `resolve(style)` returns only
`(fg, mods)`. There is no way to say "these bytes have a different
background from the rest of the row".

So refinement needs one genuinely new thing: **a per-range background
overlay**, parallel to the existing per-line span list.

```
RefineSpan { start: usize, end: usize, kind: RefineKind }
RefineKind { Added, Removed }
```

Published with the buffer's text like the foreground spans, consumed
by the cells worker when it sets `Cell.bg` — after the row tint, so a
refined range overrides its row's uniform tint and everything else
keeps it.

**Why a parallel list rather than a `bg` field on `StyledSpan`:**
adding a background to the foreground type would force every existing
span producer to decide about a concern it does not have, and would
couple first-match-wins foreground precedence to background
precedence, which is a different question. Two lists on two axes stay
two.

Two theme elements, following the existing naming:
`diff.add.refine.bg`, `diff.remove.refine.bg` — a stronger version of
the row tint they sit inside.

## 4. What gets refined

Refinement compares a **removed line against its added counterpart**,
so it only means anything for *substitutions*. A hunk that only adds,
or only removes, has nothing to pair.

Pairing rule: within a hunk, take each maximal run of consecutive `-`
lines followed immediately by a run of `+` lines. Pair them **by
position** — the first removed with the first added, and so on — and
refine only where the two runs are the **same length**.

Unequal runs are left unrefined. Magit does the same. Pairing 3
removals against 5 additions requires guessing which addition replaced
which removal, and a wrong guess produces confident, incorrect
emphasis — worse than none, because the reader trusts it. Similarity
scoring could do better and is deliberately deferred until the simple
rule proves insufficient in use.

A pair whose lines are wholly dissimilar is also left alone: if the
refined ranges would cover most of both lines, the "refinement" is
noise, and the uniform tint already says "this line changed". A
threshold (refined bytes > ~70% of the line) drops back to no
refinement.

## 5. Token granularity

**Word-level**, not character-level. Character-level diffing of source
code produces confetti — matching brackets and single letters scattered
through a rename read worse than no refinement. Word-level is what
magit, delta and GitHub use.

"Word" is a run of `[A-Za-z0-9_]`, with each other character its own
token. That keeps identifiers whole (the common rename case) while
letting punctuation-only changes still refine.

`imara-diff` is already a dependency and is generic over its token
type, so the same engine that computes the hunks computes the
refinement — no new dependency, no second diff implementation with
different behaviour.

## 6. Where it computes

In `lattice-diff`, beside the hunk computation, **not** in
`lattice-magit`.

`magit` is the first consumer but not the only future one:
`diff-mode`'s side-by-side panes have exactly the same gap, and a
refinement living in magit would have to be reimplemented or reached
across for them. The computation is pure — two strings in, ranges out —
which is what `lattice-diff` is for.

Cost sits in the refresh task, which is already `spawn_blocking`.
Nothing moves onto the actor or UI thread, and per-frame cost is
unchanged: the cells worker gains one list lookup per line, the same
shape as the span walk it already does.

Bounded by construction: refinement runs only on paired lines, only
within hunks already computed, and a pathological pair (a minified
line thousands of tokens long) is skipped by the same threshold §4
uses.

## 7. Rejected alternatives

- **Foreground spans** (§2). The cheap mechanism, but it destroys
  syntax colour where the user is looking hardest, and contradicts
  every comparable tool.
- **Character-level granularity** (§5). Confetti on real code.
- **Similarity-scored pairing** for unequal runs (§4). Better in
  principle; a wrong pairing is confidently misleading, so the simple
  rule ships first.
- **Refine in `lattice-magit`** (§6). First consumer is not owner;
  `diff-mode` has the same gap.
- **A `bg` field on `StyledSpan`** (§3). Couples two independent axes
  and taxes every existing producer.
- **Git's own `--word-diff`.** Shelling out for something already
  computable from the hunks, in a format that then needs parsing back.

## 8. Paramount-goal alignment

- **UX (higher court).** The whole point: the reader sees *what*
  changed. Degrades to today's appearance whenever refinement is
  declined (§4), so the failure direction is "no worse than now".
- **#1 Performance.** Computed in the existing `spawn_blocking`
  refresh; per-frame cost unchanged; bounded by the pairing rule.
- **#3 Everything-is-a-buffer.** No kind-branching: the overlay rides
  with the buffer's published spans like every other layer.
