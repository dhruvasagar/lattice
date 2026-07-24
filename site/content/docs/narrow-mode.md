+++
title = "Narrow mode"
+++



Narrowing focuses the editor on one region of a file — a function, a
paragraph, a selected range — and hides the rest. Unlike a [fold](../folding/)
(which only collapses the *display*), a narrow view is a real, editable
buffer showing **only** the narrowed lines. You edit inside the view; the
changes flow back to the underlying file. `:widen` restores the full
buffer.

This is emacs's `narrow-to-region` reimagined on lattice's
[multibuffer](../multibuffer/) substrate: a narrow view is a one-excerpt
multibuffer over the source. Two things fall out of that for free — you
can open **several** narrow views of the same file (in splits) and they
stay live-synced as you edit, and a narrow is always exactly **one hop**
from the file (see *Stacked narrow* below).

## Entering a narrow

### The `zn` operator

`zn` is a vim **operator**: it composes with any motion or text object,
exactly like `d` / `y` / `c`. Whatever span you can delete, you can narrow.

| Keys      | Narrows to                                        |
|-----------|---------------------------------------------------|
| `znap`    | the paragraph                                     |
| `zni{`    | inside the braces                                 |
| `znaf`    | a function (tree-sitter text object)              |
| `znac`    | a class / type                                    |
| `znG`     | the cursor down to end of file                    |
| `3znj`    | the cursor plus the next three lines              |
| `znn`     | the current line (the doubled operator, like `dd`)|

The structural text objects (`af` / `ac` / `aa` / `al`, see
[modal editing](../modal-editing/#tree-sitter-text-objects)) make `zn`
structural: `znaf` narrows the enclosing function, `znac` the enclosing
type — and they work *inside* an existing narrow or search view too.

### `:narrow`

`:narrow` takes an optional ex-command range prefix:

| Command         | Narrows to                                |
|-----------------|-------------------------------------------|
| `:narrow`       | the **paragraph** around the cursor       |
| `:%narrow`      | the whole buffer                          |
| `:'<,'>narrow`  | the Visual selection                      |
| `:5,20narrow`   | lines 5–20                                |
| `:.,+10narrow`  | the cursor and the next ten lines         |

In Visual mode the `:` line pre-fills `'<,'>`, so selecting a region and
typing `:narrow<CR>` narrows the selection. Ranges follow the usual
ex-command grammar — absolute lines, `.` (cursor), `$` (last line), marks,
and `+N` / `-N` offsets.

## Editing, saving, and the headerline

The view's header row shows what you're looking at:

```
[narrow] <label> L<start>–<end>
```

`<label>` is the file or symbol name when one is known (otherwise it's
omitted); `<start>`/`<end>` are 1-based source line numbers.

Edits land in the view and propagate to the source buffer
asynchronously — nothing blocks while you type. `:w` **saves the source
file**, not the view (a narrow view has no path of its own); `:wa` saves
every dirty source buffer and naturally skips the views.

## Leaving a narrow

`:widen` closes the narrow view and returns you to the full buffer.

Like emacs, there is no narrow *stack* to pop: `widen` goes straight back
to the file, not to an enclosing narrow. (Plain emacs is the same — its
`widen` clears all narrowing at once.)

## Stacked narrow — always one hop

Narrowing again from *inside* a narrow view — or a search-results view, or
any multibuffer — does **not** stack a view-on-a-view. The new narrow
targets the **original file**: your cursor is translated back to the
source, and the fresh view is one excerpt of the real buffer. So:

- `:widen` always returns to the file, never to an intermediate narrow.
- `znaf` / `daf` / `yaf` work *inside* a narrow or a search view — the
  text object resolves against the source's syntax tree, and the result
  maps back into the view.
- Three narrows deep is still one hop from the file; edits and saves never
  thread through a chain of translations.

## See also

- [Multibuffer views](../multibuffer/) — the substrate narrow is built on,
  and the model behind side-by-side live-synced views.
- [Modal editing → tree-sitter text objects](../modal-editing/#tree-sitter-text-objects)
  — the `af` / `ac` / … objects that make `zn` structural.
- [Folding](../folding/) — collapse structure without hiding it; the
  display-only cousin of narrow.
