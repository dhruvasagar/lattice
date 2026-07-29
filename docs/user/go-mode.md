---
summary: "go-mode: the major mode for Go files (`.go`) — tree-sitter highlighting, folds, symbols, and text objects."
related: [go, languages]
---

# go-mode

The major mode for Go. Activates automatically on `.go`.

## What it gives you

| Capability | Source |
|---|---|
| Syntax highlighting | `tree-sitter-go`'s highlight query |
| Folds (`zc` / `zo` / `zM` / `zR`) | `queries/go/folds.scm` |
| Symbols (outline, `:picker outline`) | `queries/go/symbols.scm` |
| Text objects (`af` / `if`, `ac` / `ic`, …) | `queries/go/textobjects.scm` |

All four come from tree-sitter, so they follow the real parse tree
rather than indentation or regex heuristics — a fold or a function
text object is the actual syntactic node, not a guess.

## Keybindings

None of its own. A language major mode names the language and supplies
queries; it contributes no chords. Everything you press in one of these
buffers is the universal vim grammar
([`modal-editing`](help:modal-editing)) plus whatever minors are
active. The *syntax* text objects the queries feed — `af` / `if` for a
function, `ac` / `ic` for a class — are bound by the grammar, not here;
this mode is what makes them resolve to real syntax nodes.

## Options

None. This mode contributes no option overrides, so every option in a
Go buffer resolves to its global or default value. Tab width,
wrapping, line numbers and the rest are set globally or per buffer —
see [`options`](help:options) and [`display`](help:display).

There is no per-language settings mechanism today: you cannot say
"tabs in Go, spaces in Python" through the mode. That is a real gap,
not an omission from this page.

## Activating it manually

Detection is by file extension, so a Go file with an unusual name
lands in [`text-mode`](help:text-mode). Set the mode directly:

```
:go-mode
```

Every major mode has an auto-generated toggle named after it. To see
what a buffer currently has, `:describe-mode` with no argument.

## What it does not give you

- **No LSP by itself.** Language-server features — completion,
  diagnostics, go-to-definition — come from
  [`lsp-mode`](help:lsp-mode), which is a separate per-buffer gate
  with its own activation. A major mode names the language; it does
  not start a server.
- **No formatting or indentation rules.** Neither is wired to the
  major mode today.

## See also

- [`languages`](help:languages) — every bundled language and how
  detection works.
- [`folding`](help:folding) — the fold chords these queries feed.
- [`lsp-mode`](help:lsp-mode) — turning on language-server features.
