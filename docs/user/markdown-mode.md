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
| Link rendering | `[text](url)` shows as **text**, `<https://…>` as the bare URL |

Markdown is the one bundled language whose highlight query lattice
writes itself rather than taking from the grammar crate. Markdown's
parse tree splits into a block grammar and an inline grammar, and the
useful captures for an editor don't line up with what the upstream
query emits.

## Links render as their text

Outside Insert mode a link shows only its label: `[the guide](guide.md)`
reads as `the guide`, and `<https://example.com>` as
`https://example.com`. Insert mode shows the raw markup again, so you
can edit the target. Nothing in the file changes; this is display only
(the same concealment org links use).

A few cases stay raw on purpose: images (`![alt](src)`), escaped
brackets (`\[…`), and a link written as the first thing in a code span.
A link further inside a code span, or inside a fenced code block, does
get concealed, because concealment works line by line and can't tell
it's in code.

## Not provided

- **No symbols query**, so `:picker outline` has nothing to list in a
  markdown buffer. Headings would be the obvious outline and this is a
  real gap, not a deliberate omission.
- **No text objects query**, so `af` / `if` and friends fall back to
  their non-syntactic behaviour here.

Both exist for the other eighteen bundled languages; see
[`languages`](help:languages).

## Keybindings

| Key | Mode | Does |
|---|---|---|
| `<CR>` | Normal | Follow the link under the cursor |

The cursor counts as on a link anywhere from its `[` to its `)`. What happens depends on where the link points:

| Link | `<CR>` |
|---|---|
| `[text](#install)` | Jump to the heading `# Install` in this buffer |
| `[text](guide.md)` | Open `guide.md`, relative to *this file's* directory |
| `[text](guide.md#setup)` | Open `guide.md` at its `## Setup` heading |
| `[text](https://…)`, `<https://…>` | Open in your browser (the OS handler) |

Heading anchors use GitHub's slugs: lowercase, punctuation dropped,
spaces become `-`. So `## 1. Getting Started` is `#1-getting-started`.
Every follow is recorded as a jump, so `<C-o>` takes you back to the
link. A link to a file that doesn't exist reports `no such file`
instead of opening an empty buffer.

Off a link, `<CR>` does nothing. Reference links (`[text][ref]`) and
wiki links (`[[note]]`) aren't followed yet.

In a help buffer, `<CR>` follows help's own links (from
[`help-mode`](help:help-mode)), as it always has.

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
