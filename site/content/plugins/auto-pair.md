+++
title = "auto-pair"
description = "Closes brackets and quotes as you type, and closes the nearest unmatched opener on one key."
weight = 10
[extra]
kind = "bundled"
+++

Type `(` and get `()`, with the cursor between them. Delete the opener and the
close goes with it. The ordinary, invisible convenience every editor has —
here it is a WebAssembly component, because it was the first one, written to
prove the plugin host could carry something real.

A second style, `manual`, does not insert anything as you type; instead one
key closes the **nearest unmatched opener**, whatever it is. Useful if
auto-inserted pairs get in your way more than they help.

## Getting it

Nothing to do — it ships with lattice and is on by default. To turn it off:

```
:set auto-pair.enabled=false
```

It contributes the minor mode `auto-pair-mode`, and the `<id>.enabled` option
is the switch for that mode, so the plugin never forces itself on.

## Documentation

`:help auto-pair` inside the editor, or
[core plugins](@/docs/config/core-plugins.md) for how bundled plugins are
discovered, granted capabilities and gated.
