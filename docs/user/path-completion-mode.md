---
summary: "path-completion-mode: contributes filesystem-path completion, so typing a path in a buffer offers real directory entries."
related: [completion, complete]
---

# path-completion-mode

Contributes the **path** completion source: when what you're typing
looks like a filesystem path, candidates come from the real directory.

Useful wherever paths appear in text — an import, a config value, a
shell script, a markdown link — without you leaving the buffer to check
what a directory contains.

## Options

None.

## Keybindings

None — it contributes a completion source, nothing else.

## See also

- [`completion`](help:completion) — sources, triggers, and the popup.
- [`file-tree-mode`](help:file-tree-mode) — browsing the filesystem as
  a buffer instead.
