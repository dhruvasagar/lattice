# Changelog

## 0.9.0 — 2026-09-20

The first installable release. Alpha: the editor is usable; the
distribution is new.

### Editing
- Vim modal grammar: operators, motions, text objects, registers, counts,
  macros recorded as command invocations, folds, marks, a unified position
  history (jump list + mark ring), surround, narrowing, soft wrap.
- Unified command dispatch — the `:` line, the palette, plugin
  contributions and the grammar all flow through one registry.

### Code intelligence
- LSP: completion, diagnostics, hover, rename, references, inlay hints,
  document symbols, code actions, signature help, semantic tokens,
  selection ranges, folding ranges.
- Tree-sitter highlighting for 20 languages, incremental and O(viewport).

### Git
- A magit port: status, hunk-level staging, commit, amend, rebase, blame,
  log, branches, stashes, submodules, notes, cherry-pick, with transients.
- A diff and merge subsystem — inline, side-by-side, three-way, `]c` / `[c`,
  `do` / `dp`.

### Extensibility
- A WebAssembly Component Model plugin host: capability-gated, fuel-limited,
  crash-isolated, one store per plugin instance.
- Configuration is Rust compiled to WASM (`lattice --scaffold-init`), with
  TOML for static option overrides.
- Three bundled plugins ship with every build: auto-pair,
  treesitter-context, project.

### Interface
- Two renderers: a first-class terminal peer and a GPU-rendered window
  (`--gui`, opt-in at 0.9).
- Everything is a buffer — file tree, diagnostics, search results, terminal,
  git views, help.
- Pickers, which-key, a dashboard, notifications, themes, a tutor
  (`:tutor`), and self-documenting help for every command, option, mode and
  key.
- Coding-agent integrations: Claude Code over MCP and opencode over ACP,
  both as buffers with interactive diff review.

### Distribution
- Release archives for macOS, Linux and Windows on x86_64 and aarch64, plus
  Linux `.AppImage` and `.deb`, checksums and build provenance.
- `install.sh` for macOS and Linux.

### Known limitations
See [known limitations](https://dhruvasagar.github.io/lattice/docs/start/known-limitations/). The short version:
binaries are unsigned, LSP servers must be installed by hand, syntax colours
are not fully themeable, and the GPU renderer is not yet at parity with the
terminal one.
