+++
title = "Documentation"
description = "Lattice user documentation — topics organized by subject"
weight = 1
template = "section.html"
+++

Lattice user-facing reference, organized by topic. The goal here is what `:help` does in vim and `C-h i` does in emacs: every feature has a deep-dive doc you can read end-to-end when you need to understand it, and skim when you just need a keystroke.

## Start here

- [Getting Started](@/docs/getting-started.md) — the ten-minute orientation
- [Modal Editing](@/docs/modal-editing.md) — the vim grammar
- [Modes](@/docs/modes.md) — major + minor modes

## All topics

| Topic | Description |
|---|---|
| [Getting Started](@/docs/getting-started.md) | Ten-minute orientation: open/save, command line, splits |
| [Modal Editing](@/docs/modal-editing.md) | Normal/Insert/Visual/Search/Replace, operators, motions, text objects |
| [Modes](@/docs/modes.md) | Major + minor modes, activation, option resolution |
| [Ex-commands](@/docs/ex-commands.md) | `:w`, `:e`, `:s`, `:g`, ranges, aliases |
| [Buffers and Panes](@/docs/buffers.md) | Registry, splits, file tree, navigation |
| [File tree & Oil](@/docs/filetree-oil.md) | Browse/edit filesystem, oil-style writable listings |
| [Multibuffer](@/docs/multibuffer.md) | Excerpts, composed views, search results |
| [Project Search](@/docs/project-search.md) | `:search`, streaming results, jump-to-source |
| [Narrow Mode](@/docs/narrow-mode.md) | `zn` operator, edit-in-view, stacked |
| [Diff & Merge](@/docs/diff.md) | `:diffthis`, `]c`/`[c`, `do`/`dp`, two/three-way |
| [Display & Layout](@/docs/display.md) | Soft-wrap, tab width, scroll-off |
| [Modeline](@/docs/modeline.md) | Per-pane status row, zones, customization |
| [Themes](@/docs/themes.md) | `:colorscheme`, live preview, customization |
| [Folding](@/docs/folding.md) | Manual/indent/tree-sitter folds |
| [Completion](@/docs/completion.md) | Insert-mode completion, snippets, ghost text |
| [Picker](@/docs/picker.md) | Fuzzy finder, file/grep/buffer/outline, frecency |
| [Options](@/docs/options.md) | `:set`, TOML, groups, live reference |
| [LSP](@/docs/lsp.md) | Language servers, capabilities, commands |
| [lsp-mode](@/docs/lsp-mode.md) | Umbrella minor mode, sub-modes, gating |
| [emacs-keys-mode](@/docs/emacs-keys-mode.md) | `C-x` leader over vim |
| [Help](@/docs/help.md) | `:describe-*`, `:apropos`, `:keymap` |
| [Plugins](@/docs/plugins.md) | WASM Component Model, capabilities, API |
| [init.rs](@/docs/init.md) | Rust/WASM config: commands, events, keybinds |
| [Terminal](@/docs/terminal.md) | PTY buffer |
| [Tutor](@/docs/tutor.md) | Interactive lesson sequence |
| [Claude Code](@/docs/claude-code.md) | AI agent IDE peer |
| [OpenCode](@/docs/opencode.md) | AI agent TUI integration |
| [Languages](@/docs/languages.md) | Bundled grammars, adding new ones |

## Where to look next

- **I want to do X right now** — jump to the topic and search for the keystroke
- **I want to understand how X composes** — read the topic's sections
- **I want to know if a feature exists** — check the [implementation ledger](https://github.com/dhruvasagar/lattice/blob/main/docs/dev/operations/implementation.md)
- **I want to know why it works this way** — read the [design spec](https://github.com/dhruvasagar/lattice/blob/main/docs/dev/architecture/design.md)
