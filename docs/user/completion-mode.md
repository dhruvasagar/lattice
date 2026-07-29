---
summary: "completion-mode: the per-buffer gate for insert-mode completion — active on writable buffers, which is why <C-Space> is a no-op in help or oil."
related: [completion, complete]
---

# completion-mode

The gate that decides whether a buffer participates in insert-mode
completion at all. It activates automatically on writable buffers when
they're created and stays for the buffer's life.

Its whole job is to be checked: the completion trigger consults it
before opening the popup. Read-only kinds — help, file tree, oil —
never activate it, which is why `<C-Space>` there is a silent no-op
rather than a popup with nothing useful in it.

## The pair

`completion-mode` is persistent; its twin
[`completion-popup-mode`](help:completion-popup-mode) is transient and
exists only while the popup is open. The split mirrors
[`lsp-mode`](help:lsp-mode) (umbrella, persistent) and its per-feature
sub-modes: one answers "is this available here?", the other owns the
keys that only mean something while a popup is up.

Neither contributes options.

## Options

None.

## Keybindings

None. It is a gate that other code consults; the popup's keys belong to [`completion-popup-mode`](help:completion-popup-mode).

## See also

- [`completion`](help:completion) — sources, triggers, configuration.
- [`completion-popup-mode`](help:completion-popup-mode) — the popup's
  own keymap.
