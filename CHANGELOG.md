# Changelog

## 0.9.1 — unreleased

A fourth bundled plugin, one data-loss fix, and an honesty pass over the
docs the first 0.9 users read.

### Fixed
- **Plugin data survives an upgrade (Linux).** A plugin's private store
  lived in `~/.local/share/lattice/plugins/`, which is also where
  `install.sh --prefix ~/.local` puts the bundled plugins — and the
  installer replaces that directory. Every reinstall deleted every
  plugin's saved state. Data now lives beside the plugin, under
  `~/.config/lattice/plugins/<name>/data/`, and existing directories are
  carried over on first start. **If you reinstalled 0.9.0 on Linux, that
  state is already gone and cannot be recovered.** macOS was unaffected.
- **The docs named config files that do not exist.** `options.md` told you
  to write `~/.config/lattice/init.toml`; the file lattice reads is
  `~/.config/lattice/lattice.toml` (a project's is
  `.lattice/config.toml`). Thanks to the reporter of issue #3.
- **`lsp.md` documented an `lsp.toml` that nothing loads.** The page now
  says what is true: which servers are built in and what each needs on
  your `PATH`, that the list is not configurable yet, that server settings
  come from `[lsp.<section>]` in `lattice.toml`, and the `lsp-*` command
  names that are actually registered.
- **SQL comments.** The comment text objects looked for `//` in `.sql`
  files, where a line comment is `--`.
- **`:describe-key`** reports a composed chord as its operator, keeps the
  layer and source of every row, and no longer mangles bracket chords.
- **Plugin provenance.** A plugin's chords, seams and describe-views name
  the plugin that contributed them, and every chord an operator composes
  (`gcap`, `gcF{c}`) stays in its plugin's mode layer rather than leaking
  into the universal one.

### Added
- **`comment`, the fourth bundled plugin.** `gc` toggles line comments as a
  real operator, so it composes with any motion or text object — `gcc`,
  `gcap`, `gci{`, `3gcc`, and `gc` over a Visual selection. It is
  contributed from WASM: the first operator to cross the plugin boundary.
- **The plugin API grew what that needed:** an operator receives the
  document it operates on, and a plugin can declare its own chord, bound
  in its own mode's layer so `:set comment.enabled=false` takes the keys
  with it.
- **A `/plugins/` section on the site**, with each plugin's page built from
  the manual the plugin itself ships.
- **Command-line completion for command arguments.**

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
- Coding-agent integrations: Claude Code attaches over MCP, and `:opencode`
  runs opencode's own TUI in a terminal buffer. An ACP-buffer alternative
  gives opencode a lattice-owned conversation buffer with interactive diff
  review; it is not the default path at 0.9.

### Distribution
- Release archives for macOS, Linux and Windows on x86_64 and aarch64, plus
  Linux `.AppImage` and `.deb`, checksums and build provenance.
- `install.sh` for macOS and Linux.

### Known limitations
See [known limitations](https://dhruvasagar.github.io/lattice/docs/start/known-limitations/). The short version:
binaries are unsigned, LSP servers must be installed by hand, syntax colours
are not fully themeable, and the GPU renderer is not yet at parity with the
terminal one.
