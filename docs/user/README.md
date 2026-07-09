---
summary: "Topic index -- start here when you don't know what to look up."
---

# Help

User-facing reference for lattice features, organized by topic. The
goal here is what `:help` does in vim and `C-h i` does in emacs:
every feature has a deep-dive doc you can read end-to-end when you
need to understand it, and skim when you just need a keystroke.

This is **user documentation**, not internal notes. For the design
spec see [`../dev/architecture/design.md`](../dev/architecture/design.md); for current build status
see [`../dev/operations/implementation.md`](../dev/operations/implementation.md).

In-editor lookup is `:help [topic]` -- with no arg it opens this
index page; with a topic name it opens the matching doc rendered
through the same markdown-highlighting path `:describe-command`
uses. `<Tab>` after `:help ` lists registered topics. The bodies
are embedded into the binary at build time, so no filesystem
dependency is needed at runtime; the registry is also pluggable
(future LSP-driven and plugin-supplied topics flow through the
same surface).

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

| Topic                                                                                                       | File                                | Status |
|-------------------------------------------------------------------------------------------------------------|-------------------------------------|--------|
| Getting started (the ten-minute orientation: the modal loop, open/save, the command line, splits, next steps) | [getting-started.md](getting-started.md) | ✅ |
| Modal editing (Normal / Insert / Visual / Op-pending / Command / Search / Replace + the vim grammar)        | [modal-editing.md](modal-editing.md)| ✅     |
| Modes (major + minor; `:<mode-name>` toggle; LSP umbrella + sub-modes; display modes; convergence with `:set`) | [modes.md](modes.md)             | ✅     |
| Ex-commands (`:w`, `:e`, `:s`, `:g`, `:d`, arg schemas, completion, ranges, aliases)                        | [ex-commands.md](ex-commands.md)    | ✅     |
| Buffers and panes (registry, splits, file tree, navigation, theme)                                          | [buffers.md](buffers.md)            | ✅     |
| File tree & Oil (browse / edit the filesystem; oil-style writable directory listing; icons + colors)        | [filetree-oil.md](filetree-oil.md)  | ✅     |
| Multibuffer views (excerpts composed into one editable buffer; the substrate behind search + project diff)  | [multibuffer.md](multibuffer.md)    | ✅     |
| Project search (`:search`, streaming results multibuffer, `<CR>` jump-to-source, `gr` refresh)              | [project-search.md](project-search.md) | ✅  |
| Narrow mode (`zn` operator, `:narrow` / `:widen`, edit-in-view → source, stacked one-hop, `znaf` in view)   | [narrow-mode.md](narrow-mode.md)    | ✅     |
| Diff & merge (`:diffthis` / `:diffsplit`, `]c` / `[c`, `do` / `dp`, sign column, two- + three-way)          | [diff.md](diff.md)                  | ✅     |
| Display & layout (soft-wrap, tab width, scroll-off, whitespace markers)                                     | [display.md](display.md)            | ✅     |
| Modeline (per-pane status row: zones, the modal tag + showmode echo, `ui.modeline.*` layout config)         | [modeline.md](modeline.md)          | ✅     |
| Themes & colours (`:colorscheme` + the live-preview picker, the builtin theme catalog, `:customize`, `register_theme`) | [themes.md](themes.md)   | ✅     |
| Folding (manual + indent + markdown + tree-sitter, operator interaction, navigation, auto-open)             | [folding.md](folding.md)            | ✅     |
| Insert completion (sources, popup keymap, ranking, ghost text, snippets)                                    | [completion.md](completion.md)      | ✅     |
| Picker & marginalia (`:picker <source>` fuzzy finder — files/grep/buffers/lines/outline/…; typed annotation columns; syntax-highlighted previews; frecency ranking) | [picker.md](picker.md) | ✅ |
| Options and configuration (`:set`, layered resolver, TOML, groups, live `:options` reference)               | [options.md](options.md)            | ✅     |
| LSP (servers, capabilities, attach lifecycle, every `:lsp-*` command in context)                            | [lsp.md](lsp.md)                    | ✅     |
| `lsp-mode` (the umbrella minor + 9 sub-modes that gate per-feature LSP traffic)                             | [lsp-mode.md](lsp-mode.md)          | ✅     |
| `emacs-keys-mode` (the `<C-x>` leader: emacs-style buffer / file / window chords layered over vim)          | [emacs-keys-mode.md](emacs-keys-mode.md) | ✅ |
| Claude Code (the `:claude` agent IDE peer: reads context, edits via reviewable side-by-side diffs; wire shapes provisional) | [claude-code.md](claude-code.md) | ✅ |
| Languages (bundled set, coverage roadmap, add new language tree-sitter or otherwise)                        | [languages.md](languages.md)        | ✅     |
| Search and substitute (`/` / `?` / `:s` / live preview / regex syntax / backrefs)                           | _covered in modal-editing + ex-commands_ | 🟡 |
| Registers, marks, macros                                                                                    | _covered in modal-editing_          | 🟡     |
| Help system (`:describe-*`, `:apropos`, `:keymap`, `<C-h>` map, mode-prefix syntax for `:describe-key`)    | [help.md](help.md)                  | ✅     |
| Plugins (WASM Component Model, capabilities, fuel)                                                          | _planned (Phase 7+)_                | ⛔     |
| Performance posture (latency budgets, what's safe in a hot loop)                                            | _planned_                           | ⛔     |
| Tutor (the gamified `:tutor` lesson sequence — lives/score/HUD; 7 lessons: motions → grammar → visual → modes/help → splits/diff/LSP → advanced editing → customization) | [tutor.md](tutor.md) · [lessons](tutor/) | ✅ |

Topics with `_planned_` aren't drafted yet — open an issue or send
a PR if you want one prioritized.

## When to read which

- **You want to do X right now:** jump to the topic and search for
  the keystroke.
- **You want to understand how X composes with Y:** read the
  topic's "Edge cases" / "Interaction" sections.
- **You want to know if a feature exists yet:**
  [`../dev/operations/implementation.md`](../dev/operations/implementation.md) is the ledger.
- **You want to know why it works the way it does:**
  [`../dev/architecture/design.md`](../dev/architecture/design.md) is the spec.
