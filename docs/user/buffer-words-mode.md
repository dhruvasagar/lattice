---
summary: "buffer-words-mode: contributes the completion source that offers words already present in your buffers."
related: [completion, complete]
---

# buffer-words-mode

Contributes the **buffer-words** completion source: the identifiers and
words already present in your open buffers, offered as completion
candidates.

It is the source that works with no language support at all — no
grammar, no language server, no configuration. In a plain text file or
a language lattice doesn't bundle, it's the only thing keeping
completion useful, and it's why completing a long variable name works
the moment you've typed it once.

Candidates flow through the same matcher and ranking as every other
source, so buffer words compete on match quality rather than being
privileged or penalised.

## Options

None.

## Keybindings

None — it contributes a completion source, nothing else.

## See also

- [`completion`](help:completion) — how sources combine, and the
  popup's keymap.
- [`tree-sitter-completion-mode`](help:tree-sitter-completion-mode) —
  the syntax-aware source alongside it.
