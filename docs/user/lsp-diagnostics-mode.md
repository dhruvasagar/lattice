---
summary: "lsp-diagnostics-mode: the diagnostic surfaces — gutter severity marks, `gl` for the message under the cursor, and the `]d` / `[d` walk."
related: [lsp, lsp-mode, error-list]
---

# lsp-diagnostics-mode

Owns everything you *see* and *do* about diagnostics in a buffer with a
language server attached: the severity marks in the gutter, the message
for the diagnostic under the cursor, and moving between them.

Unlike most of its siblings this is not a bare gate — it carries a keymap
and the `gl` handler, which is why it exists as its own mode rather than
as one more marker in the `lsp_sub_mode!` set.

## Keybindings

- `gl` — show the diagnostic under the cursor.
- `]d` / `[d` — next / previous diagnostic.

## Turning it off

```
:lsp-diagnostics-mode
```

The gutter marks disappear and the chords stop resolving for this
buffer. The server keeps publishing — the diagnostics are still there,
still feeding `:diagnostics` and the error list — so this hides the
in-buffer surface rather than stopping the analysis.

## Options

None of its own. Whether diagnostics reach the error list is
`lsp.diagnostics-to-error-list`, which is separate and global.

## See also

- [`error-list`](help:error-list) — the list view and its navigation.
- [`lsp-mode`](help:lsp-mode) — the umbrella that implies this one.
