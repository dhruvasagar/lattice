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

| Topic                      | File                                     | Status |
|----------------------------|------------------------------------------|--------|
| Getting started            | [getting-started.md](getting-started.md) | ✅     |
| Modal editing              | [modal-editing.md](modal-editing.md)     | ✅     |
| Modes                      | [modes.md](modes.md)                     | ✅     |
| The command line           | [command-line.md](command-line.md)       | ✅     |
| Search and substitute      | _covered in command-line + ex-commands_  | 🟡     |
| Ex-commands                | [ex-commands.md](ex-commands.md)         | ✅     |
| Buffers and panes          | [buffers.md](buffers.md)                 | ✅     |
| File tree & Oil            | [filetree-oil.md](filetree-oil.md)       | ✅     |
| Multibuffer views          | [multibuffer.md](multibuffer.md)         | ✅     |
| Project search             | [project-search.md](project-search.md)   | ✅     |
| Compilation mode           | [compilation.md](compilation.md)         | ✅     |
| The error list             | [error-list.md](error-list.md)           | ✅     |
| Narrow mode                | [narrow-mode.md](narrow-mode.md)         | ✅     |
| Diff & merge               | [diff.md](diff.md)                       | ✅     |
| Magit                      | [magit.md](magit.md)                     | ✅     |
| Magit status buffer        | [magit-status.md](magit-status.md)       | ✅     |
| Magit buffers              | [magit-buffers.md](magit-buffers.md)     | ✅     |
| Magit transient menus      | [magit-transient.md](magit-transient.md) | ✅     |
| Display & layout           | [display.md](display.md)                 | ✅     |
| Modeline                   | [modeline.md](modeline.md)               | ✅     |
| Themes & colours           | [themes.md](themes.md)                   | ✅     |
| Folding                    | [folding.md](folding.md)                 | ✅     |
| Insert completion          | [completion.md](completion.md)           | ✅     |
| Picker & marginalia        | [picker.md](picker.md)                   | ✅     |
| Options and configuration  | [options.md](options.md)                 | ✅     |
| LSP                        | [lsp.md](lsp.md)                         | ✅     |
| `lsp-mode`                 | [lsp-mode.md](lsp-mode.md)               | ✅     |
| `emacs-keys-mode`          | [emacs-keys-mode.md](emacs-keys-mode.md) | ✅     |
| Claude Code                | [claude-code.md](claude-code.md)         | ✅     |
| opencode                   | [opencode.md](opencode.md)               | ✅     |
| Languages                  | [languages.md](languages.md)             | ✅     |
| Search and substitute      | _covered in command-line + ex-commands_  | 🟡     |
| Registers, marks, macros   | _covered in modal-editing_               | 🟡     |
| Help system                | [help.md](help.md)                       | ✅     |
| Plugins                    | [plugins.md](plugins.md)                 | ✅     |
| Core plugins               | [core-plugins.md](core-plugins.md)       | ✅     |
| Configuring with `init.rs` | [init.md](init.md)                       | ✅     |
| Performance posture        | _planned_                                | ⛔     |
| Tutor                      | [tutor.md](tutor.md) · [lessons](tutor/) | ✅     |

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
