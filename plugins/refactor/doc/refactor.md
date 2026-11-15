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

## Operators

Every refactor is also an operator, so it works on any range: a Visual
selection, a motion, or a text object.

| | |
|---|---|
| `cx{obj}` | exchange: the first marks the range, the second swaps the two |
| `cxx` / Visual `X` | exchange the line / the selection |
| `cxc` | clear the exchange mark |
| `<leader>rr{obj}` | raise: the node the range covers replaces the expression around it |
| `<leader>rs{obj}` | split the comma list around the range, one item per line |
| `<leader>rj{obj}` | join the comma list around the range onto one line |

`cxiw` on `bar` in `foo(bar, baz)` marks it (tinted); `cxiw` on `baz` then
gives `foo(baz, bar)`, in one undo step. The mark stays put while you edit
elsewhere; editing the marked text drops it, with a message. If one range
lies inside the other, the larger is replaced by the smaller. Exchange works
on any text, parsed or not.

`<leader>rriw` on `bar` in `foo(bar, baz)` gives `bar`. In Visual, `<leader>rr`
raises the selection — `<M-o>` a couple of times, then `<leader>rr`. The range
must be one syntax node (whitespace and a trailing `,` are ignored); a range
that is not one is refused rather than grown, since raising more than you
selected would delete text you did not point at. `:refactor-raise` is the
same edit on the node at the cursor.

Split and join act on the nearest comma list around the range, so
`<leader>rsi(` and `<leader>rsiw` on an argument both split the call's
arguments; they change only whitespace. `:refactor-split` / `:refactor-join`
do the same at the cursor.

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

## Raising a node

`:refactor-raise` replaces the expression around the node at the cursor with
the node. On `z` in `foo(bar, baz + 1)` it turns `baz + 1` into `baz`; run
again, `foo(bar, baz)` becomes `baz` — the whole call, not just its
brackets. Out of an array, `let v = [a, b];` with the cursor on `b` becomes
`let v = b;`. One `u` undoes each raise. On a top-level item there is
nothing around it but the file, and it says so.

## Splitting and joining lists

| | |
|---|---|
| `:refactor-split` | one item of the list at the cursor per line |
| `:refactor-join` | all of them back on one line |

A list here is anything comma-separated: arguments, parameters, an array, a
tuple, a struct literal. `foo(bar, baz + 1)` splits into

```
foo(
    bar,
    baz + 1
)
```

indented with the buffer's own `shiftwidth` and `expandtab`, so a Go buffer
gets tabs. A trailing comma stays if it was there and is not added if it
was not. Join puts `, ` between items and nothing inside the brackets, so
`{ a, b }` comes back as `{a, b}`. Statement blocks are not lists and are
left alone.

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
