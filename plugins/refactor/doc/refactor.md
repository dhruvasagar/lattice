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
selection, a motion, or a text object. They are the `c` family: `cx` is
vim-exchange's, and the rest start with `cr` ("change: refactor"). In
Visual, `c` changes the selection at once, so there each takes `<leader>r`
with the same letter instead.

| Normal | Visual | |
|---|---|---|
| `cx{obj}` | `X` | exchange: the first marks the range, the second swaps the two |
| `cxx` | | exchange the line |
| `cxc` | | clear the exchange mark |
| `crr{obj}` | `<leader>rr` | raise: the node the range covers replaces the expression around it |
| `cre{obj}` | `<leader>re` | extract: the language server's extractions, else the expression into a variable declared before its statement |
| `cri{obj}` | `<leader>ri` | inline: the language server's inlines, else every use of the variable becomes its value and the declaration goes |
| `cr<CR>{obj}` | `<leader>r<CR>` | split the comma list around the range, one item per line |
| `crj{obj}` | `<leader>rj` | join the comma list around the range onto one line |

The letters vim-abolish uses after `cr` for case changes (`s`, `c`, `m`,
`u`, `-`, `.`, `t`) are left free. All of these keys belong to
`refactor-mode`: `:set refactor.enabled=false` removes them, and `c` is
plain `c` again.

`cxiw` on `bar` in `foo(bar, baz)` marks it (tinted); `cxiw` on `baz` then
gives `foo(baz, bar)`, in one undo step. The mark stays put while you edit
elsewhere; editing the marked text drops it, with a message. If one range
lies inside the other, the larger is replaced by the smaller. Exchange works
on any text, parsed or not.

`crriw` on `bar` in `foo(bar, baz)` gives `bar`. In Visual, `<leader>rr`
raises the selection — `<M-o>` a couple of times, then `<leader>rr`. The range
must be one syntax node (whitespace and a trailing `,` are ignored); a range
that is not one is refused rather than grown, since raising more than you
selected would delete text you did not point at. `:refactor-raise` is the
same edit on the node at the cursor.

`cre` on `baz + 1` in `let x = foo(bar, baz + 1);` gives

    let value = baz + 1;
    let x = foo(bar, value);

with `value` selected: type the name once and it changes in both places.
The range must be one whole expression. The declaration form is the
language's (`let`, `const`, `:=`, `local`, plain assignment …); for a
language without one, or to use another, set `refactor.declaration` —
`:set refactor.declaration=const {name} = {value};` for every buffer, or
`:setlocal` for one (`:setlocal refactor.declaration&` puts the language's
form back). Extracting a function needs a language server and is not
offered without one.

The declaration goes before the innermost statement holding the
expression, so inside a closure's or a branch's own block when it is in
one. Lifting can change what the code does, and native extract refuses
rather than guess when it would:

- the expression only runs sometimes: the right of `&&`, `||`, `and`,
  `or` or `??`, one branch of an `if` or `?:`, or a `match` arm;
- it runs more than once: a loop's condition, step or body, or a
  comprehension;
- it runs later: a closure or lambda body with no block of its own, or a
  default argument;
- it reads a name defined earlier in the same statement, such as `v` in
  Go's `if v := f(); v > 0`.

Each refusal says which. Native extract knows Rust, Python, Ruby, Go,
JavaScript, TypeScript / TSX, C++, Java and Lua; in other languages it
says a language server can do it.

**Server first.** `cre` and `cri` first ask the buffer's language server
for its extractions or inlines over the same range. If it offers any, they
open in a picker (extract function, extract constant, whatever the server
knows). If it offers none (no server, a server without that kind, or an
empty answer), the plugin's own extract runs on the text as it was; if the
text changed meanwhile, nothing runs and it says so. A server error is
shown, and nothing else runs. `refactor.source` sets the order:

| `refactor.source` | |
|---|---|
| `lsp,native` (default) | the server first, native when it has nothing |
| `native` | native only; the server is not asked |
| `lsp` | the server only; nothing when it has nothing |
| `native,lsp` | native only, as `native`: it always answers, so the server would never be asked |

`criiw` on a variable, at its declaration or at any use, gives

    let q = x + 1;          →    g(x + 1, (x + 1) * 2);
    g(q, q * 2);

the value parenthesised only where an operator could bind tighter, and
`S { q }` becoming `S { q: x + 1 }`. One undo step. Native inline refuses,
saying why, when copying the value would change what the code does:

- the variable is assigned again, or declared `mut`;
- the value runs code or makes a new object (a call, `new`, a list or
  object literal, a closure) and is used more than once, or its one use is
  in a branch, a loop or a closure;
- a name the value reads means something else at a use (it is shadowed
  there), or is assigned after the declaration;
- a format string may read the name (Rust's `"{q}"`), which inline cannot
  see;
- the declaration names more than one variable, or a pattern.

A call used once moves to its use: if code between the two depends on
when it runs, check it. Native inline knows the same languages as native
extract.

**From the command line.** The same, by name, for a selection or at the
cursor:

| Command | |
|---|---|
| `:'<,'>refactor-extract` | extract what is selected, as `cre` does: the server's extractions, else natively into a variable |
| `:refactor-inline` | inline the variable under the cursor, as `cri` does |
| `:lsp-refactor` | every refactoring the server offers at the cursor, or over the range; needs a server |

Visual `:` fills in the `'<,'>`. With any other range (`:3,5`), they act
on its whole lines.

Split and join act on the nearest comma list around the range, so
`cr<CR>i(` and `cr<CR>iw` on an argument both split the call's
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
