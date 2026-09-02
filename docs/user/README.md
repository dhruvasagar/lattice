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
| The launch dashboard       | [`dashboard-mode`](help:dashboard-mode) | ✅      |
| Modal editing              | [`modal-editing`](help:modal-editing)    | ✅      |
| Yank ring                  | [`yank-ring`](help:yank-ring)            | ✅      |
| Cancelling an operation    | [`cancellation`](help:cancellation)      | ✅      |
| Modes                      | [`modes`](help:modes)                    | ✅      |
| The command line           | [`command-line-mode`](help:command-line-mode) | ✅      |
|   — expanded `:` band      | [`command-line-expand-mode`](help:command-line-expand-mode) | ✅      |
| The search line            | [`search-line-mode`](help:search-line-mode) | ✅      |
| One-line prompts           | [`prompt-line-mode`](help:prompt-line-mode) | ✅      |
| Search and substitute      | _covered in command-line + ex-commands_  | 🟡      |
| Ex-commands                | [`ex-commands`](help:ex-commands)        | ✅      |
| Buffers and panes          | [`buffers`](help:buffers)                | ✅      |
| File tree                  | [`file-tree-mode`](help:file-tree-mode)  | ✅      |
| Oil (editable directory)   | [`oil-mode`](help:oil-mode)              | ✅      |
|   — listing presentation   | [`directory-listing-mode`](help:directory-listing-mode) | ✅      |
| Tables (markdown + org)    | [`table-mode`](help:table-mode)          | ✅      |
| Multibuffer views          | [`multibuffer-mode`](help:multibuffer-mode) | ✅      |
|   — `gr` refresh          | [`refreshable-view-mode`](help:refreshable-view-mode) | ✅      |
|   — `<Tab>` fold a block   | [`foldable-view-mode`](help:foldable-view-mode) | ✅      |
| Projects and roots         | [`project`](help:project)                | ✅      |
| Project search             | [`project-search-mode`](help:project-search-mode) | ✅      |
| Scan views                 | [`scan-view-mode`](help:scan-view-mode) | ✅      |
|   — `cr` clock report      | [`scan-view-clockreport-mode`](help:scan-view-clockreport-mode) | ✅      |
| Compilation mode           | [`compilation-mode`](help:compilation-mode) | ✅      |
| The error list             | [`error-list`](help:error-list)          | ✅      |
| The problems view          | [`problems-minor-mode`](help:problems-minor-mode) | ✅      |
| Narrow mode                | [`narrow-mode`](help:narrow-mode)        | ✅      |
| Diff & merge               | [`diff-mode`](help:diff-mode)            | ✅      |
|   — conflict resolution    | [`diff-conflict-mode`](help:diff-conflict-mode) | ✅      |
| Notifications              | [`notifications`](help:notifications)    | ✅      |
| Notifications buffer       | [`notifications-mode`](help:notifications-mode) | ✅      |
| Magit                      | [`magit`](help:magit)                    | ✅      |
| Magit status buffer        | [`magit-status-mode`](help:magit-status-mode) | ✅      |
| magit — commit buffer      | [`magit-commit-mode`](help:magit-commit-mode) | ✅      |
| magit — commit detail      | [`magit-revision-mode`](help:magit-revision-mode) | ✅      |
| magit — file at revision   | [`magit-file-revision-mode`](help:magit-file-revision-mode) | ✅      |
| magit — diff buffer        | [`magit-diff-mode`](help:magit-diff-mode) | ✅      |
| magit — log buffer         | [`magit-log-mode`](help:magit-log-mode)  | ✅      |
| magit — blame annotations  | [`magit-blame-mode`](help:magit-blame-mode) | ✅      |
| magit — stash list         | [`magit-stash-mode`](help:magit-stash-mode) | ✅      |
| magit — stash detail       | [`magit-stash-show-mode`](help:magit-stash-show-mode) | ✅      |
| magit — branch list        | [`magit-branch-mode`](help:magit-branch-mode) | ✅      |
| magit — remote list        | [`magit-remote-mode`](help:magit-remote-mode) | ✅      |
| magit — submodule list     | [`magit-submodule-mode`](help:magit-submodule-mode) | ✅      |
| magit — rebase todo        | [`magit-rebase-mode`](help:magit-rebase-mode) | ✅      |
| magit — refs (branches, remotes, tags) | [`magit-refs-mode`](help:magit-refs-mode) | ✅      |
| magit — commit notes       | [`magit-notes-mode`](help:magit-notes-mode) | ✅      |
| magit — cherries (not upstream yet) | [`magit-cherry-mode`](help:magit-cherry-mode) | ✅      |
| magit — project diff (editable) | [`magit-project-diff-mode`](help:magit-project-diff-mode) | ✅      |
| magit — shared chords      | [`magit-core-mode`](help:magit-core-mode) | ✅      |
| magit — shared navigation   | [`magit-nav-mode`](help:magit-nav-mode) | ✅      |
| magit — hunk chords        | [`magit-hunk-mode`](help:magit-hunk-mode) | ✅      |
| magit — entry chords       | [`magit-global-mode`](help:magit-global-mode) | ✅      |
| Magit transient menus      | [`magit-transient`](help:magit-transient) | ✅      |
| Display & layout           | [`display`](help:display)                | ✅      |
| Line numbers               | [`line-numbers-mode`](help:line-numbers-mode) | ✅      |
| Relative line numbers      | [`relative-line-numbers-mode`](help:relative-line-numbers-mode) | ✅      |
| Soft wrap                  | [`wrap-mode`](help:wrap-mode) | ✅      |
| Read-only                  | [`read-only-mode`](help:read-only-mode) | ✅      |
| Whitespace markers         | [`whitespace-show-mode`](help:whitespace-show-mode) | ✅      |
| Current-line highlight     | [`current-line-highlight-mode`](help:current-line-highlight-mode) | ✅      |
| Modeline                   | [`modeline`](help:modeline)              | ✅      |
| Themes & colours           | [`themes`](help:themes)                  | ✅      |
| Surround (`ds`/`cs`/`ys`)  | [`surround-mode`](help:surround-mode)    | ✅      |
| Terminal buffers           | [`terminal-mode`](help:terminal-mode)    | ✅      |
|   — motions over output    | [`terminal-normal-mode`](help:terminal-normal-mode) | ✅      |
|   — typing at the shell    | [`terminal-insert-mode`](help:terminal-insert-mode) | ✅      |
| REPL input buffers         | [`repl-mode`](help:repl-mode) | ✅      |
| Folding                    | [`folding`](help:folding)                | ✅      |
| Insert completion          | [`completion`](help:completion)          | ✅      |
| Snippets                   | [`snippet-mode`](help:snippet-mode) | ✅      |
|   — as completions         | [`snippet-completion-mode`](help:snippet-completion-mode) | ✅      |
|   — while expanding        | [`active-snippet-mode`](help:active-snippet-mode) | ✅      |
| Picker & marginalia        | [`picker`](help:picker)                  | ✅      |
| Options and configuration  | [`options`](help:options)                | ✅      |
| LSP                        | [`lsp`](help:lsp)                        | ✅      |
| `lsp-mode`                 | [`lsp-mode`](help:lsp-mode)              | ✅      |
|   — subsystem log          | [`lsp-log-mode`](help:lsp-log-mode) | ✅      |
|   — one server's log       | [`lsp-server-log-mode`](help:lsp-server-log-mode) | ✅      |
|   — protocol trace         | [`lsp-trace-log-mode`](help:lsp-trace-log-mode) | ✅      |
|   — diagnostics           | [`lsp-diagnostics-mode`](help:lsp-diagnostics-mode) | ✅      |
|   — go-to definition      | [`lsp-nav-mode`](help:lsp-nav-mode) | ✅      |
|   — hover gate            | [`lsp-hover-mode`](help:lsp-hover-mode) | ✅      |
|   — signature help        | [`lsp-signature-mode`](help:lsp-signature-mode) | ✅      |
|   — symbols               | [`lsp-symbols-mode`](help:lsp-symbols-mode) | ✅      |
|   — code actions          | [`lsp-code-action-mode`](help:lsp-code-action-mode) | ✅      |
|   — formatting            | [`lsp-format-mode`](help:lsp-format-mode) | ✅      |
|   — rename                | [`lsp-rename-mode`](help:lsp-rename-mode) | ✅      |
|   — occurrence highlight  | [`lsp-document-highlight-mode`](help:lsp-document-highlight-mode) | ✅      |
|   — selection range       | [`lsp-selection-range-mode`](help:lsp-selection-range-mode) | ✅      |
|   — inlay hints           | [`lsp-inlay-hint-mode`](help:lsp-inlay-hint-mode) | ✅      |
|   — semantic tokens       | [`lsp-semantic-tokens-mode`](help:lsp-semantic-tokens-mode) | ✅      |
|   — server-driven folds   | [`lsp-folding-mode`](help:lsp-folding-mode) | ✅      |
|   — progress reporting    | [`lsp-progress-mode`](help:lsp-progress-mode) | ✅      |
|   — references view       | [`lsp-references-mode`](help:lsp-references-mode) | ✅      |
| Hover popups               | [`hover-mode`](help:hover-mode) | ✅      |
| `emacs-keys-mode`          | [`emacs-keys-mode`](help:emacs-keys-mode) | ✅      |
| Claude Code                | [`claude-code-mode`](help:claude-code-mode) | ✅      |
| opencode                   | [`opencode-mode`](help:opencode-mode)    | ✅      |
| pi                         | [`pi-mode`](help:pi-mode) | ✅      |
| Agent conversation         | [`ai-conversation-mode`](help:ai-conversation-mode) | ✅      |
|   — permission prompts     | [`ai-permission-mode`](help:ai-permission-mode) | ✅      |
|   — agent process log      | [`ai-log-mode`](help:ai-log-mode) | ✅      |
| Languages                  | [`languages`](help:languages)            | ✅      |
| Plain text (fallback)      | [`text-mode`](help:text-mode) | ✅      |
| Rust                       | [`rust-mode`](help:rust-mode) | ✅      |
| Python                     | [`python-mode`](help:python-mode) | ✅      |
| JavaScript                 | [`javascript-mode`](help:javascript-mode) | ✅      |
| TypeScript                 | [`typescript-mode`](help:typescript-mode) | ✅      |
| TSX (TS + JSX)             | [`tsx-mode`](help:tsx-mode) | ✅      |
| Go                         | [`go-mode`](help:go-mode) | ✅      |
| C                          | [`c-mode`](help:c-mode) | ✅      |
| C++                        | [`cpp-mode`](help:cpp-mode) | ✅      |
| Java                       | [`java-mode`](help:java-mode) | ✅      |
| Ruby                       | [`ruby-mode`](help:ruby-mode) | ✅      |
| Lua                        | [`lua-mode`](help:lua-mode) | ✅      |
| Bash                       | [`bash-mode`](help:bash-mode) | ✅      |
| HTML                       | [`html-mode`](help:html-mode) | ✅      |
| CSS                        | [`css-mode`](help:css-mode) | ✅      |
| JSON                       | [`json-mode`](help:json-mode) | ✅      |
| TOML                       | [`toml-mode`](help:toml-mode) | ✅      |
| YAML                       | [`yaml-mode`](help:yaml-mode) | ✅      |
| SQL                        | [`sql-mode`](help:sql-mode) | ✅      |
| WIT                        | [`wit-mode`](help:wit-mode) | ✅      |
| Markdown                   | [`markdown-mode`](help:markdown-mode) | ✅      |
| Search and substitute      | _covered in command-line + ex-commands_  | 🟡      |
| Registers, marks, macros   | _covered in modal-editing_               | 🟡      |
| Help system                | [`help`](help:help)                      | ✅      |
| Help buffers               | [`help-mode`](help:help-mode) | ✅      |
| Completion gate            | [`completion-mode`](help:completion-mode) | ✅      |
| Completion popup keys      | [`completion-popup-mode`](help:completion-popup-mode) | ✅      |
|   — buffer-words source    | [`buffer-words-mode`](help:buffer-words-mode) | ✅      |
|   — path source            | [`path-completion-mode`](help:path-completion-mode) | ✅      |
|   — tree-sitter source     | [`tree-sitter-completion-mode`](help:tree-sitter-completion-mode) | ✅      |
|   — LSP source             | [`lsp-completion-mode`](help:lsp-completion-mode) | ✅      |
| Picker preview             | [`preview-mode`](help:preview-mode) | ✅      |
| The `*messages*` log       | [`messages-mode`](help:messages-mode) | ✅      |
| Plugins                    | [`plugins`](help:plugins)                | ✅      |
|   — the manager buffer     | [`plugins-mode`](help:plugins-mode) | ✅      |
|   — the boundary trace     | [`plugin-trace-mode`](help:plugin-trace-mode) | ✅      |
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
