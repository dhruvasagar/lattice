---
summary: "lsp-progress-mode: marks a buffer whose server reports $/progress; the detail rides in the `lsp` modeline element."
related: [lsp, lsp-mode, modeline]
---

# lsp-progress-mode

A marker. It records that this buffer's server sends `$/progress`
notifications — indexing, building, "loading crate graph" — so the
in-flight work can be surfaced while it happens.

The reporting itself is **not** here. It ships as part of the `lsp`
[modeline](help:modeline) element, which is where a long-running
background task belongs: a status area you can glance at, rather than a
notification that interrupts. The mode is kept as a distinct id so
activation and gating key off it unchanged.

## Turning it off

```
:lsp-progress-mode
```

Progress stops being reported for this buffer. The work still happens —
you simply stop being told about it.

## Options

None.

## Keybindings

None.

## See also

- [`modeline`](help:modeline) — where the progress detail renders.
- [`lsp-mode`](help:lsp-mode) — the umbrella that implies this one.
