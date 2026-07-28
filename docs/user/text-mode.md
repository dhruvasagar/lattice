---
summary: "text-mode: the fallback major mode for plain text — full vim grammar, no tree-sitter parser, no LSP, no mode-scoped option overrides."
related: [modes, text]
---

# text-mode

The catch-all major mode. Any buffer whose content lattice can't
identify as a specific language lands here — a scratch buffer, a
`.txt`, a file with an extension nothing claims.

It is deliberately the *least* opinionated mode in the editor:

- **Full vim grammar.** Every motion, operator, text object, register,
  and macro works exactly as documented in
  [`modal-editing`](help:modal-editing). Nothing is gated.
- **No tree-sitter parser**, so no syntax highlighting, no
  syntax-driven folds, and no tree-sitter text objects.
- **No LSP attachment.** Nothing to attach to.
- **No option overrides.** Every option resolves to its global or
  default value, so what you set in `init.rs` or with `:set` is what
  you get, unmodified.

That last point is what makes it the useful baseline: if a behaviour
differs between two buffers, comparing against a text-mode buffer tells
you whether a major mode is contributing an override.

## Getting a better mode

If a file *should* have a language mode and doesn't, the detection
didn't recognise it. Set the major mode directly with `:rust-mode`,
`:python-mode`, and so on — every major mode has an auto-generated
toggle named after it. See [`languages`](help:languages) for the
bundled set and how detection works.

## See also

- [`modes`](help:modes) — majors versus minors, and how activation
  works.
- [`languages`](help:languages) — the language majors and how a file
  gets one.
