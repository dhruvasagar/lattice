---
summary: "tree-sitter-completion-mode: contributes a syntax-aware completion source drawn from the buffer's parse tree."
related: [completion, syntax]
---

# tree-sitter-completion-mode

Contributes a completion source drawn from the buffer's **parse tree**
rather than its raw text. Where
[`buffer-words-mode`](help:buffer-words-mode) offers any word it has
seen, this one knows which of them are actually symbols.

It needs a language major mode to have parsed the buffer — see
[`languages`](help:languages) — so it contributes nothing in
[`text-mode`](help:text-mode) or a language lattice doesn't bundle.
The buffer-words source covers that case.

## Options

None.

## Keybindings

None — it contributes a completion source, nothing else.

## See also

- [`completion`](help:completion) — how sources combine.
- [`languages`](help:languages) — which languages parse.
