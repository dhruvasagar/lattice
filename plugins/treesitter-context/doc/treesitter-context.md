# treesitter-context

Sticky scope headers. When the `impl` and the `fn` you are inside scroll off the
top, their header lines stay pinned above the text, so you can always see what
block you are in. The same idea as `nvim-treesitter-context`, or *sticky scroll*
in VSCode and Zed.

The pinned rows carry the buffer's own syntax colouring — they are built from
the same cell builder as the document, not re-highlighted — and they sit
**under** the headerline rather than replacing it, so a buffer showing LSP
status or a scan progress row keeps it and the context starts below.

## Jumping up the stack

`[u` jumps to the header of the enclosing scope. Press it again to walk further
out: each jump lands on a header, and the next press looks for a header strictly
above the cursor, so it finds the parent rather than sticking. A count works
(`3[u` goes three levels out), and at top level it does nothing.

`<C-o>` walks back — the jump records a position-history entry before it moves,
exactly like every other jump in the editor. There is deliberately no `]u`: the
inverse of walking up is `<C-o>`.

`:context-toggle` turns the strip off and back on. It flips the global
`treesitter-context.enabled` option, so it affects every buffer, not just the
one you are in.

## Options

All prefixed `treesitter-context.`:

| Option | Default | What it does |
|---|---|---|
| `enabled` | `true` | Master switch (registered by the plugin loader). |
| `anchor` | `cursor` | Which line drives the context: `cursor` (where you are) or `topline` (what you are looking at). |
| `max-lines` | `0` | Maximum context ROWS; `0` is unlimited. A wrapped signature spends more than one. The strip is bounded by `max-viewport-fraction` regardless. |
| `trim-scope` | `outer` | Which end to drop when over budget: `outer` keeps the scopes you are innermost in. |
| `multiline-threshold` | `1` | Maximum rows one scope's header may use. Raise it to see a whole wrapped signature. |
| `max-viewport-fraction` | `33` | Percent of the pane the strip may occupy, headerline included. |
| `separator` | `""` | Glyph repeated as a rule under the block. Empty disables it. |
| `line-numbers` | `true` | Show each context row's source line number in the gutter. |
| `disabled-languages` | `""` | Comma-separated grammar ids to skip. |
| `max-file-lines` | `100000` | Skip the structural query above this line count; `0` disables the guard. |

## Large files

The structural query runs over the **whole buffer** on each reparse, off the
UI thread. Its cost is linear in the file — about 1.4 µs per line: 9 ms at
5 000 lines, 15 ms at 10 000, 135 ms at 100 000, and half a second at 400 000
without ever failing.

`max-file-lines` bounds how long that background work may run on a single
reparse. The default of 100 000 is far above any realistic source file (the
largest in Lattice's own tree is 36 000 lines), so in practice the guard never
fires. Setting it to `0` removes the bound entirely, which is a reasonable
choice — the query cannot stall the editor or fail.

> Before Lattice 0.x's `run-query-ranges` seam, this query returned a handle
> per capture and went *superlinear*: 287 ms at 5 000 lines, over a second at
> 10 000, and a hard failure past 20 000 that disabled the strip in every
> buffer until reload. The default was 5 000 then, and setting `0` was a bad
> idea. Both are historical.

## Languages

Rust, Python, Go, JavaScript, TypeScript/TSX, C/C++, and Markdown ship with
context queries. A language without one simply shows no strip — nothing breaks,
and adding a query is how support grows.

## Theme elements

The strip is styled by the host's `sticky.context.*` elements, not by the
plugin's own:

| Element | What it styles |
|---|---|
| `sticky.context.background` | The backdrop behind the pinned rows |
| `sticky.context.active` | The innermost row — the scope you are actually in |
| `sticky.context.line_number` | The source line numbers in its gutter |
| `sticky.context.separator` | The rule beneath the block, when `separator` is set |

None of them colours the code itself: the rows keep the buffer's own syntax
highlighting, and these style the backdrop and gutter around it. Restyle them
like any builtin element.

They are host elements rather than the plugin's because the strip is host
chrome — the editor resolves the scopes into rows, reserves the space and
paints it, and the plugin only supplies the scopes. Earlier releases exposed
`treesitter-context.background` and friends; nothing ever read them, so they
are gone.
