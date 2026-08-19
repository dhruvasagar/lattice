---
summary: "lsp-semantic-tokens-mode: per-buffer gate for textDocument/semanticTokens — turn it off and highlighting falls back to tree-sitter alone."
related: [lsp, lsp-mode]
---

# lsp-semantic-tokens-mode

A **feature gate**, one of the set [`lsp-mode`](help:lsp-mode) turns on
for you. It carries no keys and no options of its own; what it decides is
whether this buffer talks to its language server about
`textDocument/semanticTokens` — the request behind server-supplied highlighting layered over tree-sitter's.

## Why it is a mode rather than a setting

Because it is per buffer, and because it composes. `lsp-mode` implies the
whole set, so attaching a server switches them all on together; turning
one off afterwards is a normal mode toggle rather than a special case in
the LSP client:

```
:lsp-semantic-tokens-mode
```

Toggling it off means highlighting falls back to tree-sitter alone. Nothing else about the attachment
changes — the server stays running, and every other feature keeps
working.

That is the point of splitting the umbrella into gates. A server that is
excellent at completion and slow at semanticTokens can be kept, with
the slow part switched off for the buffers where it hurts.

## Options

None. The mode's presence *is* the setting.

## Keybindings

None of its own.

## See also

- [`lsp-mode`](help:lsp-mode) — the umbrella that implies this one.
- [`lsp`](help:lsp) — attaching servers, and what each feature does.
