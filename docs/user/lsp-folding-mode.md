---
summary: "lsp-folding-mode: sets foldmethod=lsp on its buffers, so folds come from textDocument/foldingRange instead of indentation."
related: [lsp, lsp-mode, folding]
---

# lsp-folding-mode

Makes folds come from the language server. Active, it contributes
`foldmethod = lsp` **to this buffer**, so fold regions are the ones
`textDocument/foldingRange` reports — which understand your language's
constructs rather than inferring them from indentation.

## Turning it off

```
:lsp-folding-mode
```

The contribution is dropped and `foldmethod` falls back to whatever the
buffer would otherwise have had — your `:set foldmethod`, a major mode's
contribution, or the default. Nothing is stashed and restored; the
fallback is just the option resolver with one fewer layer.

That last detail was once a real bug rather than a footnote. This mode
used to write `foldmethod` through the **global** registry from its
activation, so attaching a server to one buffer set `foldmethod = lsp`
for every buffer in the editor, including ones with no server at all.
A mode contribution is scoped to its buffers and reverts itself, which
is why the stash-and-restore machinery is gone rather than fixed.

## Options

Contributes `foldmethod = lsp`. See [`folding`](help:folding) for the
fold commands themselves and for `foldlevel`.

## Keybindings

None of its own — the ordinary fold chords (`za`, `zR`, `zM`) work on
LSP folds like any other.

## See also

- [`folding`](help:folding) — fold methods, `foldlevel`, and the chords.
- [`lsp-mode`](help:lsp-mode) — the umbrella that implies this one.
