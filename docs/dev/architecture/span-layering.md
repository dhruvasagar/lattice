# Span layering

How several independent sources of styling compose onto one line
without any of them knowing about the others.

This started as "syntax highlighting inside magit diffs", and the
useful part turned out not to be the feature. The layering contract
below already existed, implicitly, spread across three functions that
had never been described together. Writing it down is what makes the
next overlay cheap.

> Slice plan: [`magit-diff-syntax`](../operations/slice-plans/magit-diff-syntax.md).

## 1. Two axes, not one

Styling arrives on **two independent axes**, and the whole contract
rests on their independence:

| Axis | Mechanism | Carries |
|---|---|---|
| Foreground | `StyledSpan` lists, per line | Syntax, markers, headers, anything token-shaped |
| Background | The diff **sign map**, per row | Full-row tints (`diff.add.line`, `diff.remove.line`, compilation location, …) |

They never interact. A row can carry an add-tint background *and*
per-token syntax foreground because nothing composes them — they are
applied by different code at different times.

This is easy to miss, and missing it produces exactly the wrong
design. The obvious reading of "diff highlighting on top of syntax
highlighting" is that the two must be *merged* into one span list,
with interval arithmetic splitting spans wherever they overlap. That
is unnecessary: for the common case the two live on different axes and
never overlap at all.

## 2. Foreground precedence: first match wins

Within the foreground axis, a byte's style is resolved by
`cells_worker::style_at_byte`:

```rust
for s in line_spans {
    if byte >= s.start && byte < s.end {
        return s.style;   // FIRST match wins
    }
}
Style::Default
```

and `cells_worker::merge_extra_spans` concatenates in a fixed order:

```rust
merged.extend_from_slice(extra);   // caller-supplied, FIRST
merged.append(line_spans);         // syntax, SECOND
```

Together those give the layering rule:

> **Earlier spans win. Later spans show through wherever no earlier
> span covers the byte.**

So composing two layers is **concatenation in precedence order**. No
splitting, no interval arithmetic, no merged intermediate value. A
layer that wants to sit on top is simply pushed first; a layer that
wants to be a backdrop is pushed last and shows through the gaps the
top layer leaves.

The corollary is the design constraint worth remembering:

> **A top layer must cover only what it means to claim.** A span that
> covers a whole line wins the whole line, and everything below it is
> dead. Narrowing the top layer to the bytes it actually owns is what
> converts an overwrite into a layering.

## 3. Adding a layer

1. Decide the axis. Full-row tint ⇒ sign map. Token-shaped ⇒ spans.
2. Decide precedence. Push before the layer it should beat.
3. Cover only your own bytes.
4. Offset correctly if your layer sits inside a decorated line (a
   diff's `+` marker shifts every content byte by one).

Nothing else is required — no registration, no host change. The cells
worker already merges whatever the buffer's extra spans carry.

## 4. Why not merge

Merging computes a combined result, and a combined result cannot be
re-layered. Add a third overlay later — search hits inside a diff,
blame tint, inline diagnostics — and a merge implementation must grow
a case for every pair. Concatenation over an ordered list takes each
new overlay at zero marginal cost, because precedence is positional
rather than pairwise.

That is the merit argument (heuristic #1), and it is why the cheaper
implementation is also the better long-term one. The two are not
usually the same and the coincidence should not be mistaken for the
reason.

## 5. Rejected: give the buffer a real language

For the magit case specifically, the tempting shortcut is to hand the
magit buffer a `Syntax` handle and let the ordinary cells path
highlight it. It does not work: the buffer's content is a *diff*, not
source. There is no single language for a multi-file diff, and the
leading `+` / `-` / space markers are not valid syntax in any of them
— the parser would see garbage on every line.

Producing spans deliberately and handing them over as extra spans is
not a workaround for that; it is the mechanism the help-buffer system
already uses to highlight synthetic content.

## 6. First consumer — syntax inside magit diffs

Every magit view that shows a diff currently styles each content line
with **one whole-line span**: `diff.add.text` (a foreground green) or
`diff.remove.text`. Uniform colour, no syntax. The background tint is
already correct and already comes from the sign map.

### The layer stack

| # | Layer | Covers | Source |
|---|---|---|---|
| 1 | Diff headers | Whole line — `diff --git`, `@@`, `---`/`+++` | Existing classifier |
| 2 | Change marker | The single `+` / `-` / space column | Existing classifier, narrowed |
| 3 | Syntax | Everything right of the marker | Per-hunk parse, offset by 1 |
| — | Row tint | The row's background | Sign map, untouched |

Layer 2 is the whole change. Narrowing that span from the full line to
one byte is what lets layer 3 exist.

### The sign map keeps working, for free

`Editor::diff_signs_from_spans` derives a row's diff sign from the
*presence* of a `Style::DiffAdd` / `DiffRemove` span, regardless of
its range:

```rust
spans.iter().find_map(|s| match s.style {
    Style::DiffAdd => Some(DiffSignKind::Add),
    ...
})
```

A one-byte marker span satisfies it exactly as a whole-line span did,
so **the host needs no change**. That is not luck — it follows from
that function's deliberate choice to condition on the style rather
than on the producing mode.

### Hunks are fragments

A hunk begins mid-file, so tree-sitter sees an unbalanced fragment and
produces an ERROR node at the top. Two options:

- **Parse the fragment** — reconstruct each side of the hunk (context
  + added, context + removed), strip markers, parse, map back.
  Keywords, strings, comments and numbers come out right; anything
  needing enclosing context degrades to uncoloured.
- **Parse the blob** — fetch the file at that revision and slice the
  hunk's lines out of a full-file parse. Accurate, but costs a
  `git cat-file` and a full parse per file on every status refresh,
  for content that is mostly off-screen.

**Chosen: the fragment parse.** It fails in the right direction —
worst case a token is left uncoloured, never coloured *wrong*, and the
result degrades to exactly today's appearance. The blob path stays
available for `magit-diff-mode` later, where the user is deliberately
looking at one file and the cost is justified by attention.

### Ownership

`magit-hunk-mode` activates on precisely the five diff-bearing majors
— status, diff, commit, revision, stash-show — and its own module doc
already states that it owns what is inside the diff. The span
composition lives there and the five views call it, rather than each
view growing its own copy.

This is ownership by the mode that owns the content, per the
"modes own their full surface" rule. It is a presentation helper
rather than a `Mode` trait contribution, and the doc says so plainly
instead of overclaiming a seam it does not use: the spans are computed
during the async refresh and published with the text, not per frame.

## 7. Cost

Per-frame cost is **unchanged**. `style_at_byte` already walks a span
list; a longer list is the same walk. The parse happens in the refresh
task, which is already `spawn_blocking` — paramount #1 is not in the
way, and no work moves onto the actor or UI thread.
