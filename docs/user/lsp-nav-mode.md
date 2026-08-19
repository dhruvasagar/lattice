---
summary: "lsp-nav-mode: per-buffer gate for textDocument/definition, declaration, typeDefinition and implementation — turn it off and the go-to chords report no server for this buffer."
related: [lsp, lsp-mode]
---

# lsp-nav-mode

A **feature gate**, one of the set [`lsp-mode`](help:lsp-mode) turns on
for you. It carries no keys and no options of its own; what it decides is
whether this buffer talks to its language server about
`textDocument/definition, declaration, typeDefinition and implementation` — the request behind the go-to-definition family.

## Why it is a mode rather than a setting

Because it is per buffer, and because it composes. `lsp-mode` implies the
whole set, so attaching a server switches them all on together; turning
one off afterwards is a normal mode toggle rather than a special case in
the LSP client:

```
:lsp-nav-mode
```

Toggling it off means the go-to chords report no server for this buffer. Nothing else about the attachment
changes — the server stays running, and every other feature keeps
working.

That is the point of splitting the umbrella into gates. A server that is
excellent at completion and slow at definition, declaration, typeDefinition can be kept, with
the slow part switched off for the buffers where it hurts.

## Options

None. The mode's presence *is* the setting.

## Keybindings

None of its own.

## See also

- [`lsp-mode`](help:lsp-mode) — the umbrella that implies this one.
- [`lsp`](help:lsp) — attaching servers, and what each feature does.
