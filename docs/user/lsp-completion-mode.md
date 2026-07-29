---
summary: "lsp-completion-mode: contributes the language-server completion source — the type-aware candidates, produced asynchronously."
related: [completion, lsp]
---

# lsp-completion-mode

Contributes the **language-server** completion source: the candidates
that know types, imports and scope, because a real language server
computed them.

It is asynchronous by construction. The server is a separate process
and may take a moment, so candidates arrive into an already-open popup
rather than the popup waiting on them — the other sources fill it
immediately and LSP results join when they land. That is what keeps a
slow server from making completion feel slow.

Requires [`lsp-mode`](help:lsp-mode) to be active on the buffer with a
server attached; without one it contributes nothing.

## Options

None.

## Keybindings

None — it contributes a completion source, nothing else.

## See also

- [`lsp-mode`](help:lsp-mode) — the per-buffer gate.
- [`lsp`](help:lsp) — servers, attach lifecycle, every `:lsp-*`
  command.
- [`completion`](help:completion) — how sources combine and rank.
