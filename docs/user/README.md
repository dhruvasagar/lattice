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

| Topic                      | `:help <name>`                           | Status |
|----------------------------|------------------------------------------|--------|
| Getting started            | [`getting-started`](help:getting-started) | ✅      |
| Modal editing              | [`modal-editing`](help:modal-editing)    | ✅      |
| Modes                      | [`modes`](help:modes)                    | ✅      |
| The command line           | [`command-line-mode`](help:command-line-mode) | ✅      |
| Search and substitute      | _covered in command-line + ex-commands_  | 🟡      |
| Ex-commands                | [`ex-commands`](help:ex-commands)        | ✅      |
| Buffers and panes          | [`buffers`](help:buffers)                | ✅      |
| File tree                  | [`file-tree-mode`](help:file-tree-mode)  | ✅      |
| Oil (editable directory)   | [`oil-mode`](help:oil-mode)              | ✅      |
| Multibuffer views          | [`multibuffer-mode`](help:multibuffer-mode) | ✅      |
| Project search             | [`project-search-mode`](help:project-search-mode) | ✅      |
| Compilation mode           | [`compilation-mode`](help:compilation-mode) | ✅      |
| The error list             | [`error-list`](help:error-list)          | ✅      |
| Narrow mode                | [`narrow-mode`](help:narrow-mode)        | ✅      |
| Diff & merge               | [`diff-mode`](help:diff-mode)            | ✅      |
| Magit                      | [`magit`](help:magit)                    | ✅      |
| Magit status buffer        | [`magit-status-mode`](help:magit-status-mode) | ✅      |
| Magit buffers              | [`magit-buffers`](help:magit-buffers)    | ✅      |
| Magit transient menus      | [`magit-transient`](help:magit-transient) | ✅      |
| Display & layout           | [`display`](help:display)                | ✅      |
| Modeline                   | [`modeline`](help:modeline)              | ✅      |
| Themes & colours           | [`themes`](help:themes)                  | ✅      |
| Surround (`ds`/`cs`/`ys`)  | [`surround-mode`](help:surround-mode)    | ✅      |
| Terminal buffers           | [`terminal-mode`](help:terminal-mode)    | ✅      |
| Folding                    | [`folding`](help:folding)                | ✅      |
| Insert completion          | [`completion`](help:completion)          | ✅      |
| Picker & marginalia        | [`picker`](help:picker)                  | ✅      |
| Options and configuration  | [`options`](help:options)                | ✅      |
| LSP                        | [`lsp`](help:lsp)                        | ✅      |
| `lsp-mode`                 | [`lsp-mode`](help:lsp-mode)              | ✅      |
| `emacs-keys-mode`          | [`emacs-keys-mode`](help:emacs-keys-mode) | ✅      |
| Claude Code                | [`claude-code-mode`](help:claude-code-mode) | ✅      |
| opencode                   | [`opencode-mode`](help:opencode-mode)    | ✅      |
| Languages                  | [`languages`](help:languages)            | ✅      |
| Search and substitute      | _covered in command-line + ex-commands_  | 🟡      |
| Registers, marks, macros   | _covered in modal-editing_               | 🟡      |
| Help system                | [`help`](help:help)                      | ✅      |
| Plugins                    | [`plugins`](help:plugins)                | ✅      |
| Core plugins               | [`core-plugins`](help:core-plugins)      | ✅      |
| Configuring with `init.rs` | [`init`](help:init)                      | ✅      |
| Performance posture        | _planned_                                | ⛔      |
| Tutor                      | [`tutor-mode`](help:tutor-mode)          | ✅      |

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
