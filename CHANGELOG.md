# Changelog

## Unreleased

### Fixed
- **A table with links in it did not line up.** table-mode measured a
  `[label](url)` cell as the width of its label, which is how a help page
  shows it and not how a buffer does, so every other row was padded short
  by the length of the URL and the pipes drifted apart. Cells are now
  measured as written.
- **A proper markdown table lost its highlighting.** With a header and a
  `|---|---|` row, bold, code spans and links inside the cells were shown as
  plain text; the same rows without the rule were coloured. Cells are now
  highlighted either way.

## 0.9.5 — 2026-10-09

A reading release. Build output and diffs that had been arriving as plain
text are coloured: `*compilation*` now reads a compiler error the way
`:plugins` does, for rustc and for tools that report on one line, and the
project diff shows what was added. Two of these were not missing features
but colour that was computed and then never reached the screen.

### Fixed
- **`*compilation*` showed a compiler error as plain text.** A build's
  output is captured through a pipe, which makes cargo and rustc drop
  their colours, so an error looked like the lines around it. The buffer
  now reads a diagnostic the way `:plugins` does: severity-coloured
  labels, a dimmed gutter, carets in their diagnostic's colour, the
  `-->` location as a link, styled backtraces.
  One-line diagnostics from other tools (`main.c:10:5: error: …` from
  gcc, clang, eslint; a Rust panic; any `file:line`) are styled as well,
  from the same match that makes them jumpable.
- **Colour a tool forced on never reached `*compilation*`.** Output from
  `cargo build --color=always` had its escape codes stripped, as
  documented, but was then shown uncoloured. It is painted now.
- **The project diff was never coloured.** `:magit-project-diff` drew
  its deleted lines but left added and changed lines in plain syntax
  colours, with no green and no row tint. They are styled now.
- **Indent guides ran through a build log.** They drew a bar beside
  rustc's own gutter; `*compilation*` no longer shows them.
- **A secondary `--` beside a primary `^^^` coloured the whole line
  blue**, in `:plugins` as well. Each underline now takes its own colour
  and the message follows the primary one.

### Changed
- **A jumpable line in `*compilation*` looks the same in the terminal and
  the GPU window.** The row keeps its background tint; the location on it
  is now a link in both, where before only the terminal coloured the path
  and did so with a colour no theme could change.
- A search match or a selection on a jumpable line is no longer painted
  over by the row tint.

### Project
- The repository has a Sponsor button, and the README, site and issue
  chooser link the Discord server.

## 0.9.4 — 2026-10-09

A first-config release. `lattice --scaffold-init` produced a config that
did not compile in 0.9.2 and 0.9.3; that is fixed, the scaffolds now check
for the Rust toolchain they need, the editor builds a plugin you are
writing the way it builds your config, and a build that fails — any
plugin's — is shown in `:plugins` in full instead of being left in
`:messages`.

### Fixed
- **`--scaffold-init` and `--scaffold-plugin` wrote code that did not
  build.** The starter `src/lib.rs` had fallen behind the plugin API, so a
  fresh config failed to compile and never loaded. Both are fixed, and CI
  now builds what each scaffold writes.
- **A build that failed was invisible.** An `init.rs` that had never built
  did not appear in `:plugins` at all, and one running its previous build
  read `cached`; the reason was only in `:messages`. `:plugins` now lists it
  under *Failed to load* or *Build failed* with the whole compiler report.
  This holds for every plugin the editor builds: your config, a failed
  `:reload-config`, the plugins your config `require`s, and plugin projects
  in your plugins directory. A plugin directory with a manifest and nothing
  loadable in it is reported too, instead of skipped with a log line.
- **Your config rebuilt on every start.** A config built in its own
  directory counted the build's output — the component, `Cargo.lock`, the
  plugin's saved state — as a source change, so cargo ran at every start
  even when nothing had changed. An unchanged config is now a plain load.
- **A scaffolded plugin could not be built by the editor.** Its world was
  written to `wit/plugin.wit`, a file the editor rewrites before each
  build. New scaffolds use `wit/user-plugin.wit`; an existing one is moved
  there automatically.
- **`-` in a file buffer opens oil at that file's directory again.** It
  had stopped doing anything outside a listing.
- **The `.deb` is named `lattice-gui` inside as well as out.** The package
  was published as `lattice-gui-<ver>-<arch>.deb` but declared itself
  `lattice-cli`, with a placeholder description. It now provides, conflicts
  with and replaces `lattice-cli`, so an existing install upgrades in
  place. ([#6](https://github.com/dhruvasagar/lattice/issues/6))
- **The site's version badge follows the release.** It stayed on the
  previous version after 0.9.3 shipped.

### Added
- **The editor builds the plugin you are writing.** A plugin project in
  `~/.config/lattice/plugins/` — a manifest beside a `Cargo.toml`, which is
  what `--scaffold-plugin` writes — is compiled at start when its source
  has changed, like `init.rs`. `b` on its row in `:plugins` rebuilds and
  reloads it, and `:plugin-load <dir>` builds one that is not loaded yet.
  No more `cargo build` and copying the component by hand.
- **The scaffolds check the toolchain.** A release archive needs no Rust,
  but `init.rs` is compiled on your machine. `--scaffold-init` and
  `--scaffold-plugin` now say whether `cargo` and the `wasm32-wasip2`
  target are present, run `rustup target add wasm32-wasip2` for you when
  rustup is installed, and otherwise print the commands. `install.sh`
  mentions it when `cargo` is not on your `PATH`.
- **Highlighted error reports in `:plugins`.** Compiler output and trap
  backtraces are shown the way a terminal would show them: the `error[…]`
  label in the error colour, `-->` locations as links, the gutter dimmed,
  carets in the colour of their diagnostic. The header counts build
  failures.
- **`:plugins` updates itself.** The view re-renders when a plugin loads or
  unloads and when a build starts, fails or finishes, so a failing
  `:reload-config` puts its error on screen without a keypress.
- **Opening oil with no directory lands on the file you came from**,
  including a bare `:Oil`.

### Changed
- **A long build log keeps its first eighty lines, not its last twenty**,
  and drops cargo's `Compiling …` progress. The first error is usually the
  cause; the tail was the summary.
- **A build failure names the toolchain only when the toolchain is at
  fault.** "Is the target installed?" used to follow every compile error;
  a missing `cargo` now says "Rust is not installed" with the commands to
  fix it.
- **`-` is bound once, by `oil-global-mode`**, in every buffer rather than
  separately in each listing mode. Behaviour is unchanged.

### Documentation
- `init.md` opens with what a programmable config needs (Rust, the
  `wasm32-wasip2` target, network for the first build) and no longer tells
  you to run `cargo build` and copy `init.wasm` by hand — the editor builds
  it on first start and on `:reload-config`.
- New troubleshooting entry for a config that does not load;
  `plugins-mode.md` documents the failure sections and live refresh.

## 0.9.3 — 2026-10-07

A fixes release: Windows paths work end to end, a plugin write grant can
no longer be escaped, `:files` opens instantly, and the plugin API and
Rust API now have generated reference documentation.

### Added
- **Generated API documentation.** The plugin API reference is now built
  from the WIT itself: one page per seam with full signatures and types,
  an index, a JSON export, and examples extracted from guests that CI
  compiles. The Rust API is published as rustdoc at `/api/`, with a
  generated crate map. For agents there is `llms.txt`, `llms-full.txt`
  and a Markdown mirror of the site.
- **`<Esc>` closes help everywhere.** In a help or describe buffer `<Esc>`
  now closes it in every display mode, including a split, where it used
  to do nothing.

### Changed
- **`install.sh` installs the GUI build by default.** That binary is a
  superset: it runs the terminal UI unless started with `--gui`. Where a
  release has no GUI build for the platform the script falls back to the
  terminal build. `--cli` pins the terminal build, `--gui` now requires
  the GUI one, and `LATTICE_MODE` sets either from the environment.
- **`:files` opens instantly.** The walk is parallel and honours
  `.gitignore` / `.ignore` properly instead of a hardcoded skip list, and
  the project's file list is warmed in the background at startup, so the
  first open no longer waits on a cold filesystem.

### Fixed
- **A plugin could write outside its write grant.** A path that did not
  exist yet and contained `..` (`<grant>/new/../../elsewhere`) was
  compared without being normalised, so the write was permitted outside
  the granted directory. It is now resolved before the comparison.
- **Windows paths.** A sweep across the editor:
  - LSP locations open: a `file:///C:/…` URI converts back to a path
    Windows can open, so go-to-definition, references and `*problems*`
    land on the file.
  - The grep picker opens (`rg.exe` / `ag.exe` / `grep.exe` are found on
    `PATH`), and the directory picker lists a canonical `\\?\C:\…` root
    and walks up from `~`.
  - Paths under home display as `~\…`, and `:cd` / completion list a
    directory named by a canonical path.
  - Magit's interactive rebase (drop, edit, reword) works: the editor
    path git is handed is quoted.
  - The project plugin names projects by their folder, inline media
    accepts a rooted path without a drive, and source links in help keep
    their separators.
- **Bundled plugins load when `lattice` is a symlink.** With
  `~/.local/bin/lattice` linked to a binary elsewhere, the plugins in
  `~/.local/share/lattice/plugins` were never found, silently. The lookup
  now tries beside the path you ran, then beside the file it points to.
- **A far-away edit in a large file could abort the editor.** An edit
  below the rendered window of a large file (`:2500d` from the top of a
  5000-line file, or an LSP edit far from the viewport) attempted a
  multi-hundred-gigabyte allocation.
- **Help in a split shows the help.** With `help.describe-display` set to
  a split or in-pane mode, the pane painted the buffer underneath; and in
  the GUI an empty floating popup was drawn beside it.
- **`<C-g>` cancels a stuck `:` line, search or prompt again.**
- **`*problems*` and the references view are syntax-highlighted**,
  including for languages a plugin provides.
- **Inline code is legible on the cursor line** in markdown and org, in
  every theme.
- **Inline images stay current.** When an edit or resize overtook a slow
  image refresh, the older result could land last and leave the images
  stale until the next edit.
- **A finished `git bisect` is recognised** on a git that quotes the term
  (`is the first 'bad' commit`).
- **Agenda and other scanned views are deterministic.** A root is walked
  in file-name order, so the same directory gives the same view on every
  machine.

## 0.9.2 — 2026-09-28

Inline images and SVG, any picker's results into the error list, and a
wide sweep of path-handling and GPUI-parity fixes.

### Added
- **Send any picker's results to the error list.** `<C-q>` in any picker
  sends the rows that survived your query to the error list — telescope's
  `send_to_qflist` — and opens the `*problems*` view over them. Narrow a
  grep or references list, `<C-q>`, then walk it with `:cnext` / `]q`. The
  row text you saw becomes each entry's line. Turn the auto-open off with
  `picker.send-opens-problems=false` to populate the list silently.
- **Inline images and SVG.** `:e diagram.png` opens a picture, and images
  and SVG render inline in a document, sized to their block.
- **Open a listing entry into a split or tab.** In oil and the file tree,
  `<C-s>` / `<C-v>` / `<C-t>` open the entry under the cursor in a
  horizontal split, a vertical split, or a new tab — the chords the picker
  already uses.
- **Every picker has a help page.** `<C-h>` inside a picker opens that
  picker's own page, and each built-in source — files, grep, jumps,
  history, marks, LSP, magit — now has one.
- **Full-width code-block background in markdown**, in both renderers,
  driven by a `@codeblock` capture; and a distinct colour for INFO lines
  in `*messages*`.
- **Taller help popups** — the 40-row cap on centered help popups is gone.

### Changed
- **The plugin ABI is versioned on its own.** `lattice-wit` and the plugin
  SDK crates now carry their own versions, independent of the editor's,
  and the `wit/` package moved into the crate that publishes it — so an
  editor patch release no longer churns a new version at every plugin
  author. The ABI contract a plugin author signs is now documented.

### Fixed
- **Path handling honours what you typed.** `~` expands in the rooted file
  pickers, `:w <path>`, `:Tree <root>`, `:plugin-load` and inline media
  paths; a bare `:cd` finds `HOME` on Windows; caches moved under the
  config home.
- **GPUI parity.** Syntax now reparses on every keystroke (it was lost on
  the first edit and never returned); a declined chord falls through, so a
  plugin's keys (auto-pair) are no longer dead; the transient `<CR>` fires
  the `<C-n>` / `<C-p>`-selected row; the gutter stays parallel to content
  across image rows; and a picker accept's follow-up effects (the
  branch-delete confirm) run.
- **Oil / file-tree focus.** The active pane follows its buffer, and a file
  keeps its syntax highlighting when focus moves to a non-Document pane.
- **Unreachable commands.** `:oil`, `:format` and `:reload-snippets` are
  reachable again, and `<CR>` in `:history pane-buffers` now walks.
- **`:reload-config` recompiles `init.rs`**, so a config change takes
  effect on reload.
- **Read-only buffers** gate on the buffer's read-only property, not its
  kind, and folding no longer counts as a mutation the gate rejects.
- **A minibuffer resolves keys in its own context**, not Insert's.
- **Git.** Reads no longer take the index lock out from under writes, and
  finishing a magit commit starts the next one clean.
- **A plugin grammar registered after its major mode re-attaches**, so its
  syntax highlighting appears instead of staying plain.

## 0.9.1 — 2026-09-23

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
