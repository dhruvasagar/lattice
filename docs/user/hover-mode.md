---
summary: "hover-mode: marks the hover popup buffer so it auto-dismisses when the document cursor moves. Content is markdown; markdown-mode renders it."
related: [hover, lsp, ex:lsp-hover]
---

# hover-mode

The minor mode on the **hover popup** buffer — the floating window that
shows a symbol's documentation and type.

You don't invoke it. It comes with the popup, which comes from
[`lsp-mode`](help:lsp-mode) when you ask for hover information.

## What it's for

One behaviour hangs off it today: a hover popup that was opened but
never focused **dismisses itself when the document cursor moves**. That
is the right instinct for a hover — you asked about the symbol you were
on, so moving off it makes the answer stale — and the mode is what
tells the dispatcher which popups that rule applies to.

## Rendering

Hover content is markdown, and the popup's *major* mode is
`markdown-mode` — so the same renderer, the same syntax highlighting,
and the same link handling as any other markdown buffer. `hover-mode`
layers on top as the minor; it doesn't reimplement any of that.

## Behaviour worth knowing

- **It's a marker mode.** Its value is that hover-only behaviour has
  somewhere to attach without special-casing the popup code — an
  auto-close timer, `<Esc>`-to-dismiss, or signature-help fan-in would
  land here. Today only the auto-dismiss rule reads it.

## See also

- [`lsp-mode`](help:lsp-mode) — the gate that decides whether hover
  runs at all for a buffer.
- [`lsp`](help:lsp) — hover among the rest of the LSP surface.
