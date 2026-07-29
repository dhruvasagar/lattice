---
summary: "markdown-mode: the major mode for .md files AND for every help buffer — hand-written highlight queries, folds by section."
related: [markdown, languages, help]
---

# markdown-mode

The major mode for Markdown. It reaches buffers by two different
routes, which is what makes it unlike the other language modes:

- **By extension** — `.md`, `.markdown`, `.mdown`, `.mkd`.
- **By buffer kind** — every `Help` buffer. `:help`, `:describe-*`,
  `:apropos` and the hover popup are all markdown, so they get the
  same renderer, the same highlighting, and the same link handling as
  a markdown file you opened yourself.

That second route is why this mode is load-bearing beyond editing
prose: change how markdown renders and you change the entire help
system with it.

## What it gives you

| Capability | Source |
|---|---|
| Syntax highlighting | a **hand-written** `queries/markdown/highlights.scm`, plus `tree-sitter-md`'s inline query |
| Folds (`zc` / `zo` / `zM` / `zR`) | `queries/markdown/folds.scm` — by section, so a heading folds its body |

Markdown is the one bundled language whose highlight query lattice
writes itself rather than taking from the grammar crate. Markdown's
parse tree splits into a block grammar and an inline grammar, and the
useful captures for an editor don't line up with what the upstream
query emits.

## Not provided

- **No symbols query**, so `:picker outline` has nothing to list in a
  markdown buffer. Headings would be the obvious outline and this is a
  real gap, not a deliberate omission.
- **No text objects query**, so `af` / `if` and friends fall back to
  their non-syntactic behaviour here.

Both exist for the other eighteen bundled languages; see
[`languages`](help:languages).

## Keybindings

None of its own. Markdown-mode names the language and supplies queries;
it contributes no chords. In a help buffer, `<CR>`-follows-link comes
from [`help-mode`](help:help-mode) layered on top, not from here — which
is why `<CR>` follows a link in `:help` and inserts a newline in a
markdown file you are editing.

## Options

None. Every option resolves to its global or default value. Note that
help buffers *are* read-only and gutterless — but that comes from
[`help-mode`](help:help-mode) and the buffer kind, not from this mode,
so editing a `.md` file is unaffected.

## Activating it manually

```
:markdown-mode
```

## See also

- [`languages`](help:languages) — every bundled language.
- [`help`](help:help) — the help system this mode renders.
- [`folding`](help:folding) — section folding.
