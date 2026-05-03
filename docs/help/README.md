# Help

User-facing reference for lattice features, organized by topic. The
goal here is what `:help` does in vim and `C-h i` does in emacs:
every feature has a deep-dive doc you can read end-to-end when you
need to understand it, and skim when you just need a keystroke.

This is **user documentation**, not internal notes. For the design
spec see [`../DESIGN.md`](../DESIGN.md); for current build status
see [`../IMPLEMENTATION.md`](../IMPLEMENTATION.md).

In-editor lookup will eventually surface these via
`:help <topic>` (rendered into a help buffer the same way
`:describe-command` works today). For now, browse the markdown
directly.

## Conventions

- Keystrokes use vim notation: `j`, `<C-d>`, `<Esc>`, `gg`, `dap`.
- Ex-commands use the `:cmd` form.
- Tables list keystrokes with their behavior. Sections build from
  the simple case to the edge cases.
- Every doc has a "Quick reference" section near the top for the
  keystroke-lookup case and a deeper "Semantics" / "Edge cases"
  section for the read-it-once case.
- Code samples use fenced blocks with the relevant language tag.

## Topics

| Topic                                                                                                 | File                     | Status |
|-------------------------------------------------------------------------------------------------------|--------------------------|--------|
| Folding (manual + indent + markdown + tree-sitter, operator interaction, navigation, auto-open)       | [folding.md](folding.md) | ✅     |
| Buffers and panes (registry, splits, file tree, navigation, theme)                                    | [buffers.md](buffers.md) | ✅     |
| Modal editing (Normal / Insert / Visual / Op-pending / Command / Search / Replace, the chord grammar) | _planned_                | ⛔     |
| Operators (`d` / `y` / `c` / `>` / `<` / `gU` / `gu` / `g~`)                                          | _planned_                | ⛔     |
| Motions (cursor, word, paragraph, sentence, viewport, find-char, search)                              | _planned_                | ⛔     |
| Text objects (`iw` / `aw` / `ip` / `ap` / `i{` / `a{` etc.)                                           | _planned_                | ⛔     |
| Search and substitute (`/` / `?` / `:s` / live preview / regex syntax / backrefs)                     | _planned_                | ⛔     |
| Registers and macros                                                                                  | _planned_                | ⛔     |
| Marks and position history                                                                            | _planned_                | ⛔     |
| Block-visual mode (`Ctrl-V`, `I` / `A` / `>` / `<`, replicate-on-Esc)                                 | _planned_                | ⛔     |
| Ex-commands (`:w`, `:e`, `:s`, `:g`, `:d`, alias resolution, surface forms)                           | _planned_                | ⛔     |
| Help system (`:describe-*`, `:apropos`, `:keymap`, missing-arg prompts)                               | _planned_                | ⛔     |
| Options and theme (`:set name=value`, `:describe-option`, `ui.*` styling)                             | _planned_                | ⛔     |
| Plugins (WASM Component Model, capabilities, fuel)                                                    | _planned_                | ⛔     |
| Performance posture (latency budgets, what's safe in a hot loop)                                      | _planned_                | ⛔     |

Topics with `_planned_` aren't drafted yet — open an issue or send
a PR if you want one prioritized.

## When to read which

- **You want to do X right now:** jump to the topic and search for
  the keystroke.
- **You want to understand how X composes with Y:** read the
  topic's "Edge cases" / "Interaction" sections.
- **You want to know if a feature exists yet:**
  [`../IMPLEMENTATION.md`](../IMPLEMENTATION.md) is the ledger.
- **You want to know why it works the way it does:**
  [`../DESIGN.md`](../DESIGN.md) is the spec.
