---
summary: "foldable-view-mode: gives a grouped view the shared <Tab> / <S-Tab> fold chords; each view declares which of its own actions <Tab> should run."
related: [multibuffer, magit, folding]
---

# foldable-view-mode

`<Tab>` folds the block under the cursor, and `<S-Tab>` folds every block,
in the generated views that are built out of blocks — the org agenda, magit
status and diffs, project search results, the references view, `*problems*`
and `*compilation*`. This mode is where those two chords live, once.

## Why one mode instead of a chord per view

The same reason [refreshable-view-mode](help:refreshable-view-mode) exists, and
the same evidence. Magit had both chords; the org agenda grew its own copy;
and project search, the references view, `*problems*` and `*compilation*` had
neither — four views made of foldable blocks with no way to collapse one. That
went unnoticed because a gap in a copied set does not announce itself: there
was no single place the chord should have been, so nothing was missing from
anywhere in particular.

So the chord is declared here and the *body* mostly is not. `<S-Tab>` is
shared outright, because "fold everything in this buffer" means the same thing
everywhere. `<Tab>` resolves to whichever action the view declared, so a view
with a genuine specialisation keeps it: in magit, pressing `<Tab>` on a file
line in the status buffer expands its diff on the first press, which is what
`=` does too. Everywhere else it is the plain fold toggle.

## Activation

Manual, never automatic. In an ordinary file `<Tab>` is the terminal's name for
`<C-i>` — [jump forward](help:jumps) through the jump list — and that must keep
working, so this mode attaches only to the generated views that opt in.

The trade is deliberate: inside one of those views, `<Tab>` folds rather than
jumps. You navigate those views with `<CR>`, `]]` and `[[` instead, and folding
a block is the thing you actually reach for.

## Keybindings

- `<Tab>` — fold or unfold the block at the cursor.
- `<S-Tab>` — fold or unfold every block in the view.

Both are ordinary folds, so the `z` chords still work alongside them: `zM` to
close everything, `zR` to open it, `za` to toggle one.

## Options

None. Whether a view *opens* folded is the view's own business — the org
agenda opens collapsed to its blocks, search results open expanded — and is
controlled by `foldlevel`, which you can set per buffer with
`:setlocal foldlevel=99`.

## See also

- [refreshable-view-mode](help:refreshable-view-mode) — the same shape, for `gr`.
- [folding](help:folding) — the fold model these chords drive.
