+++
title = "treesitter-context"
description = "Pins the enclosing function, impl or branch above the text once its header scrolls off."
weight = 11
[extra]
kind = "bundled"
+++

You are two hundred lines into a function and the signature is long gone.
treesitter-context keeps the enclosing `impl`, `fn` or `if` pinned at the top
of the window, so the thing you are reading always says what it belongs to.

It reads the same incremental tree-sitter parse the highlighter uses, off the
UI thread. There is no second parse and no per-keystroke walk — the context
line is derived from a snapshot that already exists.

## Getting it

Ships with lattice, on by default. To turn it off:

```
:set treesitter-context.enabled=false
```

## Documentation

`:help treesitter-context`, or
[core plugins](@/docs/config/core-plugins.md).
