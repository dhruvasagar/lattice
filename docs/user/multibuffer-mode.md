---
summary: "Multibuffer views: excerpts from many files composed into one editable buffer — the substrate behind search results and project-wide diff."
related: [multibuffer, excerpt]
---

# multibuffer-mode

A **multibuffer** composes excerpts drawn from one or more source
files into a single buffer you can scroll, navigate, fold, and
**edit** as if it were an ordinary file. Editing an excerpt writes
straight back to its source. It is the substrate that powers several
features that look different on the surface but are the same
mechanism underneath.

> **Status:** the multibuffer substrate — excerpts, headers, the
> excerpt motions, context expand/contract, edit-propagation, and
> excerpt / file folding (`za` over an excerpt or a whole file) — is
> shipped (M-series, through M.8). The richer headerline status line
> is tracked but not all landed yet; this doc marks that inline.

---

## One mechanism, three scenarios

The point of a multibuffer is **orthogonality**: "excerpt of a file"
and "what produced the excerpts" are independent axes.

```
            ┌─────────────────────────────────────────────┐
            │  multibuffer = [excerpt, excerpt, …]         │
            │  each excerpt = (source file, line range)    │
            └─────────────────────────────────────────────┘
                      ▲              ▲              ▲
       single-file diff      search results   project-wide diff
       (excerpts = hunks     (excerpts = the   (excerpts = hunks
        of one file)          matched lines     across many files)
                              across files)
```

- **Search results** ([`project-search-mode`](help:project-search-mode)) — each
  match is a one-line excerpt under its file's header.
- **Diff** ([`diff-mode`](help:diff-mode)) — changed hunks become excerpts; a
  single-file diff draws hunks from one file, a project-wide diff
  draws them from many.

Because they share the substrate, the same motions, folds, and
edit-propagation work identically in all of them.

---

## Quick reference

| Keystroke / command          | Meaning                                                         |
|------------------------------|-----------------------------------------------------------------|
| `<CR>`                       | Jump to the **source** file/line of the excerpt under the cursor |
| `]e` / `[e`                  | Jump to the next / previous **excerpt** start                  |
| `]E` / `[E`                  | Jump to the next / previous **file boundary** (excerpt whose source file differs) |
| `:multibuffer-expand [n]`    | Grow the excerpt under the cursor by `n` source lines (default `5`) |
| `:multibuffer-contract [n]`  | Shrink the excerpt under the cursor by `n` source lines (default `5`) |
| *(edit normally)*            | Edits propagate to the source file                              |
| `:w`                         | Saves through to the underlying source file(s)                  |
| `:ls` / `:b N`               | A multibuffer is a listed buffer like any other                 |

The excerpt motions compose with operators and counts, exactly like
the built-in `]`/`[` family: `d]e` deletes through the next excerpt
start, `3]e` jumps three excerpts ahead.

---

## Concepts

### Excerpts and headers

An **excerpt** is a `(source file, line range)` pair. The view
renders each excerpt's source lines in order, preceded by a
**header** virtual row naming the source file. The header is a
*virtual row* — it occupies a display line but is not a document
line, so `j`/`k` step over the source content and the cursor never
lands "on" a header. The gutter shows the **source** line numbers for
each excerpt (not the composed-buffer row), so the numbers match what
you'd see opening the file directly.

### Composed vs. source coordinates

Internally the view keeps two coordinate spaces — the *composed*
buffer you edit and the *source* files it's assembled from — and
translates between them. You never deal with this directly: motions,
edits, and `:w` all speak source coordinates under the hood.

### Edit propagation

A multibuffer is **editable**. Type into an excerpt and the change is
applied to the underlying source buffer; `:w` persists it to disk.
Edits that span an excerpt boundary are clipped to the excerpt's own
source range (the tail beyond the boundary is dropped) so you can't
accidentally rewrite a region you can't see. `u` / `<C-r>` undo and
redo fan out across every source the view touches.

### Live updates

If a source file changes underneath the view (you edited it in
another pane, or a provider re-scanned), the excerpts recompose and
their line positions slide to stay anchored to the right content.

---

## Navigating

`<CR>` jumps to the **source** of the excerpt under the cursor — it
opens (or switches to) the source file at that line and records a jump,
so `<C-o>` brings you back. `]e` / `[e` walk excerpt starts; `]E` /
`[E` walk *file* boundaries — the first excerpt of each distinct
source file — which is the fast way to move through a many-file view.
Landing past the last excerpt (or before the first) is a no-op: the
cursor stays put rather than wrapping, so an operator like `d]e` at the
end deletes nothing unexpected.

These work in **every** multibuffer — search results
([`project-search-mode`](help:project-search-mode)), the
[`*problems*`](help:compilation-mode#the-problems-view) error view,
[narrow](help:narrow-mode) views, and any future one — because `<CR>`
jump-to-source and the excerpt motions belong to the shared multibuffer
substrate, not to any one producer.

## Expanding context

Search and diff excerpts are deliberately tight (often a single
line). `:multibuffer-expand` grows the excerpt under the cursor by
`n` lines on each side (default `5`, the Zed precedent);
`:multibuffer-contract` shrinks it. Expansion clips to the source
file's bounds (you can't expand past line 0 or past EOF), and a
contract that would invert the range is a no-op.

---

## Interactions and edge cases

- **It's a real buffer.** A multibuffer shows up in `:ls`, is
  reachable with `:b N` / `:bn` / `:bp`, has a name, and saves
  through normal `:w`. There is no separate "multibuffer mode" you
  have to learn — the vim grammar applies unchanged. See
  [`buffers`](help:buffers).
- **Folding** — the `z*` vocabulary folds an excerpt to its header
  row, and folds a whole file's excerpts to a single summary row, so a
  50-file view collapses to a 50-row outline. This rides the standard
  fold commands (see [`folding`](help:folding)); the per-excerpt (M.7)
  and per-file (M.8) fold providers register automatically when a
  multibuffer is active.
- **Async status** — providers that build a view in the background
  (search) report progress on the view's **headerline** (a header
  row at the top), not in the status line or as a notification.
- **Read-only views** — some providers present a read-only view; the
  substrate itself is editable, and read-only is a per-view policy.
