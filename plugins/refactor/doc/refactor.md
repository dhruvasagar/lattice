# refactor — select and reshape code

Works from the syntax tree the editor already parses for highlighting, so it
works in every language lattice highlights, with or without a language server.

| | |
|---|---|
| `<M-o>` (Alt-o) | select the syntax node at the cursor; in Visual, grow to the node around the selection |
| `<M-i>` (Alt-i) | in Visual, step back to the selection before the last `<M-o>`; in Normal it says there is nothing to shrink |
| `3<M-o>` | grow three levels at once |

Press `<M-o>` repeatedly to walk outward — an argument, the argument list, the
call, the statement, the block, the function — and `<M-i>` to walk back the
way you came. Once you are back at the selection you started from, one more
`<M-i>` returns to the cursor and leaves Visual.

`<M-i>` retraces `<M-o>`; it does not guess. If you change the selection any
other way — a motion, `o`, a mouse drag — the way back is forgotten, and
`<M-i>` says so rather than shrinking somewhere you did not come from.

Both are also in the command palette, as `refactor-expand` and
`refactor-shrink`. They are not `:` commands: typing `:` leaves Visual, so a
`:` command could never see the selection it is meant to grow or shrink.

## Swapping list items

| | |
|---|---|
| `:refactor-swap-next` | swap the item at the cursor with the next one |
| `:refactor-swap-prev` | swap it with the previous one |

An *item* is an argument, a parameter, an array or tuple element, a struct
field, a statement in a block — anything in a list whose items are separated
by commas, semicolons or line breaks. On `z` in `f(bar, baz + 1)` the item is
`baz + 1`, not `baz`: `+` joins operands, it does not separate items. The
separators stay where they were, so `, ` and the indentation between two
statements are untouched, and the cursor follows the item you moved. One `u`
undoes a swap.

They have no default keys; bind them in your `init.rs`. With a count (from a
key you bind), the item moves that many places.

## Where it works

Any buffer with a syntax tree. In one without (plain text, a language lattice
has no grammar for) the keys say there is no syntax tree rather than doing
nothing.

"Syntax node" means a named node of the language's tree-sitter grammar, so the
steps follow the grammar's idea of structure. It is a different source from
`:lsp-expand-region`, which asks the language server: the two can disagree on
where an expression begins, which is why they are separate commands rather
than one that picks whichever happens to be available.

## Turning it off

`:set refactor.enabled=false` turns off `refactor-mode`, and the keys with
it.
