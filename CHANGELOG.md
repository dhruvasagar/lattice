# Changelog

## Unreleased

### Added
- **Install a plugin from its release, without building it.** `init.rs` can
  `require_release` a plugin published on GitHub — `repo`, an optional tag
  and an optional `sha256` pin — and lattice downloads the build for its own
  plugin API instead of cloning and compiling, so no Rust toolchain is
  needed. The download is checked against the release's published checksum
  (and your pin), must carry its own manifest naming the plugin, and only
  then replaces what is installed; once installed, startup makes no network
  request. Without a tag it takes the newest release built for your editor,
  and `:plugin-update <name>` (or `:plugin-update-all`) installs a newer one
  when there is one; with a tag it stays on that release until you change
  the tag. A release with no build for your editor is refused by name in `:plugins`,
  saying what it ships instead. `:plugins` shows the source as
  `release owner/name@v0.2.0`. A new function in the plugin API, so plugins
  built from source rebuild once.
- **A plugin's options can go in `lattice.toml`.** `[org]` /
  `directory = "~/notes"` used to be reported as an unknown option at
  startup and ignored: the file is read before any plugin has declared its
  options. Such a value is now held and applied when the plugin declares the
  option (and again if it reloads), and a plugin's file access sees it from
  its first load. A typo in a namespace that is already registered
  (`ui.nerd_fontz`) still warns at startup; one in a plugin's namespace is
  reported once every plugin has loaded. Where `lattice.toml` and `init.rs`
  both set an option, `init.rs` wins.
- **`init.rs` can set a plugin's options before the plugin loads.** Your
  config loads first, so a user plugin's options did not exist yet and
  `set_option` on them was rejected — they could only be set from the
  plugin's `pre-plugin-loaded` handler. Now the value is held and applied
  when the plugin declares the option, and again if it reloads, so plain
  `config::set_option("org.directory", "~/notes")` in `register_options`
  works. For an option a plugin's file access follows, that also saves a
  full reload of the plugin at every start. A name no plugin declares —
  usually a typo — is reported in `*messages*`. Only `init.rs` can do this;
  another plugin naming an option that does not exist is still refused.
  The plugin API's documentation of `set-option` changed with it, so
  plugins built from source rebuild once.
- **A plugin's file access can follow your configuration.** A plugin may
  declare `fs:write:{org.directory|~/org}` instead of a fixed path: it gets
  the directory you set in that option, or its default until you do, so no
  plugin ships with a path of its own (or its author's) baked in. The option
  must be the plugin's own. Changing the option reloads that plugin with the
  new access — setting it to the value it already has does nothing — and a
  plugin cannot change such an option itself, so it cannot widen its own
  access; only your configuration can. `:plugins` shows each grant as
  resolved, with where it came from (`fs:write ~/notes (org.directory)`, or
  `…, default`), and a write refused for falling outside it names the option
  to change.
- **A plugin can be a single `.wasm` file.** Its manifest — what
  `plugin.toml` holds — can be embedded in the component itself, as a
  `lattice-manifest` custom section; a plugin directory with the component
  alone loads. `plugin.toml` still works, and when both are present the
  embedded one is used. A manifest may also say which editor it needs
  (`min_lattice = "0.9.10"`): an older Lattice refuses it by name in
  `:plugins` — *"org needs lattice ≥ 0.9.10; this is 0.9.9"* — rather than
  failing on a field it doesn't understand.
  A plugin's source directory no longer needs a `plugin.toml` either: a
  cargo project under `~/.config/lattice/plugins/` (or your `init/`) is
  built in place and named from the manifest its component embeds.
  `--scaffold-init` and `--scaffold-plugin` now write the manifest into
  `src/lib.rs` instead of a `plugin.toml`; a config scaffolded earlier keeps
  its `plugin.toml` and keeps working.
- **Publish a plugin as a release users install without building it.**
  For plugin authors: `scripts/build-plugin.sh` builds a plugin, checks it
  carries its manifest, and packages `<id>-<version>-api<M>.<m>.wasm` with
  its `.sha256` — the API generation read from the plugin's locked
  `lattice-wit`. A plugin repository publishes on a version tag by calling
  lattice's reusable `plugin-release.yml` workflow, which checks the tag is
  the crate version and attaches both files to a GitHub release. The plugin
  authoring guide's "Publishing a release" walks through it.
- **`lattice_plugin_sdk::manifest!`** (plugin API crates 0.3.1) embeds a plugin's
  manifest in its component, so a Rust plugin needs no `plugin.toml`. It
  builds in a crate that denies unsafe code.
- **`:version`** opens a `*version*` page with what a bug report needs: the
  version, the commit it was built from (and whether the tree was modified),
  debug or release, the Rust toolchain and target, the renderer you are in,
  the plugin API generation, and where Lattice keeps its config, plugins and
  cache. `Y` copies the whole page to the clipboard as markdown.
  `lattice --version --verbose` prints the same page without starting the
  editor; plain `--version` is unchanged.
- **`<C-o>` re-reads an open `:files` list, and `:files-refresh` does it from
  anywhere.** For files created or removed while the picker is open; your
  query and the selected file stay put.

### Changed
- **`lattice --scaffold-init` writes a config that can install plugins.** Its
  world imports `plugin-manager` and it has a `register_plugins` with a
  `require_release` example commented out, so installing a plugin is
  uncommenting one block. A config scaffolded earlier needs the seam added by
  hand; `:help plugins` says how.
- **A plugin you add with `require` loads on `:reload-config`.** It used to
  be recorded and not installed until the next restart. A reload now
  installs new ones, reloads one whose build changed — or whose download
  did, as when you switch a plugin to a release or to another tag — and
  leaves the rest alone; at startup each required plugin is loaded once, by the scan of your
  plugins directory, rather than by `require` and then again by the scan.
- **The core plugins are one file each.** auto-pair, treesitter-context,
  project, comment and lighthouse carry their manifests inside their
  components; a release no longer ships a `plugin.toml` beside each.
- **Unfocused popups dim in the GUI, as they do in the terminal.** A
  `:help` or `:describe-*` popup you have not focused into is drawn dimmed,
  like any pane without focus — the GUI never dimmed popups before. Hover and
  signature-help popups are the exception in both, and stay undimmed.

### Fixed
- **A git operation pressed twice runs once.** Pushing (or fetching,
  pulling, `rebase --continue`, …) again while the first is still running
  used to start a second copy — two pushes raced, and one was rejected by
  the remote. Now the second press says *"push is already running in
  lattice"* and the first carries on. Re-running it with different flags —
  a push with `-f` after a rejection — stops the first and runs the new
  one. Operations a kill could leave half-done (pull, stash, the rebase /
  merge / cherry-pick / revert / `am` steps) are never interrupted: a
  second press is refused until they finish. To restart identical work on
  purpose, press `<C-g>`, then run it again.
- **Magit operations on one repository take turns.** Different operations
  pressed in quick succession — a branch created, then checked out; a
  rebase step, then a commit operation; `C-c f` on one file, then another —
  used to run at once and could fail with *"Unable to create
  '.git/index.lock': File exists"*. They now run one after another, in the
  order you pressed them; the same operation pressed again while it runs
  is refused, as above.
- **`:plugin-update` updates a plugin installed from a URL.** A `Prebuilt`
  plugin was refused with "has no buildable source" instead of being
  downloaded again; `:plugin-rebuild` still says that, since there is nothing
  to build.
- **Magit pushes a branch that has no upstream yet.** `C-c g p`, `-u`, `p`
  (and `:magit-push`) ran `git push --set-upstream` with nowhere to push, which
  git refuses for a new branch — so a branch could only be pushed the first
  time from a shell. Lattice now names the destination: the push remote git is
  configured with, or the repository's only remote; `-u` then sets the
  upstream. With several remotes and none configured it says so and points you
  at `e` (push elsewhere) rather than guessing.
- **A `Prebuilt` plugin is not downloaded again at every start.** A plugin
  `require`d from a URL was fetched on each boot even when it was already on
  disk; it is now fetched only when missing, when the URL in your config
  changed, or on update. The download also has timeouts and a size limit, so
  a server that stops answering cannot hang plugin installation.
- **The dashboard no longer shows `[+]`.** Its modeline marked it modified
  from the moment it opened — its own content counted as an edit — until
  something redrew it.
- **A plugin whose file access follows your settings no longer reloads
  forever.** With `org.directory` or `org.roam-directory` set from `init.rs`,
  org loaded, reloaded onto your directory, and reloaded again, endlessly:
  each reload dropped org's options, so the next copy started on the
  defaults and your setting moved it once more. It now reloads once and
  stays loaded.
- **A plugin is loaded once, not twice.** At startup a plugin your
  `init.rs` requires could appear twice in `:plugins` — or once beside a
  failure reading "plugin declares no loadable seam". When a setting that
  decides the plugin's file access changed while it loaded, it was reloaded,
  and the startup scan of your plugins directory could look in the moment
  between unload and load, find nothing, and load a second copy. Loads and
  reloads now take turns.
- **A plugin refused at startup shows in `:plugins`.** One that needs a newer
  Lattice, or whose manifest does not parse, was listed nowhere when it came
  from `require`; it now appears under *Failed to load* with the reason.
- **A colour name works in a style even when no palette defines it.** An org
  TODO keyword styled `fg: "magenta"` in `init.rs` showed no colour — no
  built-in palette has a `magenta` role — and logged "unknown palette key"
  on every theme rebuild. A name that isn't a palette role now falls back to
  the terminal colour of that name, or a `#rrggbb` value; palette roles still
  win, so a theme's own `green` is unchanged.
- **`*notifications*` keys work.** `<CR>` (run a notification's action), `d`
  (dismiss) and `gr` (refresh) did nothing: the actions they name were never
  registered, so they were dropped at startup with a warning in the log.
- **`<C-c>` interrupts Claude again** in a Claude Code terminal (Normal mode).
  It named a command that no longer exists under that name.
- **Semantic highlighting appears without `:e!`.** Opening a file sometimes
  asked the language server for semantic tokens and inlay hints before it had
  finished starting; the empty answer was kept, and nothing asked again until
  you edited the file or ran `:e!`. Whether you hit it depended on startup
  timing. Both are now requested again as soon as the server is ready for the
  file, and still update when it finishes indexing.
- **Hover documentation is no longer dimmed.** A hover popup (`K`, LSP hover)
  is read while the cursor stays in your file, so it never has focus — and it
  was dimmed like any unfocused pane until you focused it. `ui.dim_inactive`
  is now resolved per buffer, and hover popups turn it off, in the terminal
  and the GUI alike (see *Changed* for the other popups).
- **`:files` shows files created outside the editor the next time you open
  it.** The list is remembered for speed, and a stale one used to be corrected
  only for the open *after* next, so the first `:files` after a build or a
  `git checkout` still missed the new files. It now opens on the remembered
  list at once and replaces the rows a moment later, when the re-read lands.
- **Visual-block `c` changes every row.** After `<C-v>` and `c`, the text you
  typed replaced the block on the top row only; the rows below were left with
  the block deleted and nothing in its place. It now lands on every row the
  block covers, as `I` already did, and one `u` takes it back off all of them.
  Rows too short to reach the block are left alone, as in Vim.
- **Visual-block `I` skips rows that end where the block starts.** A row
  exactly as long as the block's left column got the inserted text added to
  its end; Vim leaves it alone, and now so does Lattice.
- **`cc` in magit-status commits in the status buffer's repository.** The
  commit buffer it opened showed nothing staged and `C-c C-c` committed
  nothing, unless the editor had been started in that same repository.
  `cc` and `ca` now open the same commit buffer as `C-c g c` / `C-c g a`.
- **An image opened directly fits the window.** `:e photo.png` in the GUI
  scaled a large picture to the pane's width only, so a tall screenshot or
  portrait photo ran off the bottom of the window. It is now scaled down to
  fit both the width and the height, and re-fitted when the pane is resized.
  Small images still show at their own size.
- **The cursor ignored hidden link markup in the terminal.** With markdown
  links shown as their text, `$` on a line ending in a link put the terminal
  cursor where the hidden URL would have been, and on a line with wrapping
  off it could scroll the view sideways past the text. The cursor now lands
  just past the link's text, in the terminal and the GUI alike, and the GUI's
  block cursor no longer shows the hidden `)` under it.

### Documentation
- **The org plugin installs by release.** `org.md` leads with
  `require_release`, which installs a published org without a Rust
  toolchain; building it yourself now checks out a release tag (the README
  names the current one) and copies one `.wasm` — no `plugin.toml`. A new
  section says where org may write (`org.directory`, `org.roam-directory`)
  and that setting those options is the permission.
- **Installing plugins from `init.rs`** is documented in `:help plugins` —
  a released build with `require_release`, or building from source with
  `require` — which lattice's own docs had never shown.
- **Plugin options in `lattice.toml`** — `options.md` documents setting a
  plugin's options there, when it is held until the plugin loads, how typos
  are reported, and that `init.rs` wins over it.
- **`:help plugins` explains grants that follow your configuration**
  ("Capabilities and your configuration"): that setting an option like
  `org.directory` is also what decides where the plugin may write, how to
  see the resolved paths in `:plugins`, and why a plugin cannot change them
  itself. The plugin authoring guide documents `manifest!`, option-bound
  grants, `min_lattice`, and the `lattice-manifest` section format for
  toolchains other than Rust.

## 0.9.9 — 2026-10-15

### Added
- **Markdown links render as their text.** Outside Insert mode,
  `[the guide](guide.md)` shows as `the guide` and `<https://…>` as the bare
  URL; Insert mode shows the markup again. Display only, so the file is
  unchanged.
- **`<CR>` follows markdown links.** In a markdown file, `<CR>` on
  `[text](#heading)` jumps to that heading, on `[text](other.md)` or
  `[text](other.md#heading)` opens the file (relative to the current file),
  and on `[text](https://…)` or `<https://…>` opens your browser. `<C-o>`
  returns to the link. `:help markdown-mode` has the details.
- **Language servers for HTML, CSS, JSON, YAML, TOML, Bash, Markdown and
  Ruby.** Opening one of these files now starts its standard server when
  it is on your `PATH`: the VS Code HTML / CSS / JSON servers,
  `yaml-language-server`, `taplo`, `bash-language-server`, `marksman` and
  `ruby-lsp`. `:help lsp` lists the binaries and how to install them.
- **`:lsp-install` for Lua, TOML and Markdown.** `lua-language-server`,
  `taplo` and `marksman` join `rust-analyzer`: downloaded, checked against a
  pinned SHA-256 and used in place of any copy on your `PATH`, on Linux and
  macOS.
- **`:lsp-install typescript-language-server`.** Servers with no download
  can now be installed by their package manager: lighthouse runs `npm` with
  an exact version pin into its own directory, shows npm's output in the
  install buffer, and uses the result like any other install. TypeScript,
  JavaScript, TSX and JSX files then get the managed server. Needs `npm` and
  Node 20 or later.
- **`:lsp-install` for Python, Go, Bash, YAML, HTML, CSS and JSON.**
  `pyright`, `bash-language-server`, `yaml-language-server` and the VS Code
  HTML / CSS / JSON servers install with `npm`; `gopls` with `go install`.
  Every language lattice has a server for can now get one from
  `:lsp-servers`, except C / C++ (clangd) and Ruby (`gem install ruby-lsp`).

### Fixed
- **`$` in a popup scrolls it sideways.** Help pages don't wrap (so tables
  keep their columns), but moving to the end of a line wider than the popup
  left the cursor past the popup's right edge instead of scrolling the
  content. The popup now scrolls horizontally to keep the cursor in view, in
  the terminal and the window.
- **GUI: horizontal scroll moves the text, not just the cursor.** With
  ligatures on (the default), scrolling sideways (`zl`, `zh`, or a cursor
  past the edge with `nowrap`) moved the cursor and highlights while the
  text stayed put, so the cursor landed in the wrong place on the line.
- **TypeScript and C++ syntax highlighting is complete.** In `.ts`, `.tsx`,
  `.cpp` and `.hpp` files, strings, comments, numbers, function names and
  most keywords rendered as plain text; only the type annotations (and C++'s
  own additions) were coloured. They now highlight like JavaScript and C.
- **JSX is highlighted, and `.jsx` files are recognised.** HTML tags and
  attributes inside JSX are coloured in `.js`, `.jsx` and `.tsx` files, and
  `.jsx` files open as JavaScript instead of plain text.
- **The TypeScript server understands `.tsx`, `.js` and `.jsx` files.**
  Every file it handled was announced as TypeScript, so JSX in `.tsx`
  files was reported as errors and plain JavaScript was type-checked as
  TypeScript. Each file is now sent as what it is (`typescriptreact`,
  `javascript`, `javascriptreact`), and `clangd` hears `c` for a `.c`
  file instead of `cpp`.

## 0.9.8 — 2026-10-14

### Added
- **GUI on macOS: accented characters, emoji and input methods.** In the
  window, dead keys now compose (`Option-e` then `e` types `é`), holding a letter
  opens the macOS accent picker, the emoji panel inserts, and an input
  method's in-progress text is shown underlined at the cursor. This applies
  wherever you type text: Insert, Replace, the `:` line, search and picker
  queries. Normal and Visual mode still take keys as pressed, so an active
  input method never intercepts a command.
- **GUI on macOS: `Option` types its character unless a binding claims it.**
  `Option-3` types `£` where it used to type `3`. A chord a mode binds, such
  as org's `<M-CR>`, still runs the binding, and a terminal pane still gets
  `Option` as Meta.
- **GUI: open files from the operating system.** Drop a file on the window,
  or open one with Lattice from Finder or `open -a`, and it opens as `:e`
  would. The window title shows the active file's name, and macOS has a
  Lattice menu with Quit.
- **GUI: an Edit menu on macOS.** Undo, Redo, Cut, Copy and Paste, with the
  usual Cmd shortcuts. Each is the vim command it names on the system
  clipboard register: Copy and Cut act on the Visual selection (`"+y`,
  `"+d`), and Paste is `"+p` (over the selection in Visual) and `<C-r>+`
  while typing.
- **The rest of vim's put commands.** Only `p` and `P` in Normal mode
  existed. Now, in the terminal and the window alike:
  - `3p` puts the text three times.
  - `gp` / `gP` put and leave the cursor just after the new text.
  - `]p` / `[p` put linewise text re-indented to the current line.
  - `p` over a Visual selection replaces it, and the replaced text becomes
    the unnamed register; `P` replaces it and leaves the registers alone.
  - `:put`, `:put!`, `:5put a` put a register as a whole line below or
    above a line.

### Changed
- **Plugin API 0.3.0.** `app-effect` gains the put family: `put` (with a
  `put-spec` for `gp` / `]p` and friends), `put-over-selection` and
  `put-line`. Adding cases to a variant changes its shape, so this is a new
  ABI generation: a plugin pinned to `lattice-wit = "0.2"` must move its pin
  to `"0.3"` and rebuild, and one that matches on `app-effect` exhaustively
  needs arms for the new cases. Plugins without a pin rebuild on their own.
- **`lattice` opens a window.** The GPU renderer is the default; `lattice
  --tui` opens the terminal renderer, which stays first-class. With no
  display reachable (an SSH session, a server, a Linux console) a bare
  `lattice` opens in the terminal on its own. `lattice --gui` insists on a
  window and reports an error if it cannot have one. The terminal-only
  build is unchanged: it has no window, so it opens the terminal.
- **Motions and operators are one round-trip to the editor core**, in both
  the terminal and the window. Only typed characters took the fast path
  before; `j`, `w`, `x`, `dd` and the rest each made several round-trips,
  which is most noticeable with a key held down.

### Fixed
- **Backspace, `~` and Replace mode on accented letters and emoji.** These
  acted on one byte of the character instead of the whole character.
  Backspacing the last `é` on a line removed the line and left the cursor
  on a row that no longer existed; `~` and `R` corrupted the character.
  All three now work on whole characters, in the terminal and the window.
- **GUI: the cursor no longer slips under the bottom edge below large
  headings.** A markdown or org heading is drawn taller than a normal row,
  and scrolling counted it as one row, so with headings on screen the last
  line or two could be clipped. Scrolling, paging, `zb` and `H`/`M`/`L` now
  count a heading at the height it is drawn.
- **Putting a line below the last line no longer leaves a blank line at the
  end of the file.** `yyGp` in a file ending with a newline added one.
- **`p` after a multi-byte character puts after the whole character.** It
  stepped one byte, which is not a place text can go.
- **GUI: LSP semantic highlighting works.** The window showed only
  tree-sitter colours; a language server's semantic tokens were painted in
  the terminal alone. Both now show them, and in the theme's colours rather
  than fixed terminal ones. Deprecated symbols are struck through, and
  `static` / read-only ones italic, in both.
- **Terminal: a code block or cursor line holding non-ASCII text is tinted
  to the pane's edge.** The fill was measured in bytes, so a row of
  box-drawing characters (a diagram in a fenced block) left a ragged strip
  of the terminal's own background down the right-hand side.
- **GUI: the command line shows a caret.** The `:` line, the search line
  and prompts drew the text you typed and nothing to say where you were in
  it. They now draw a caret at the cursor, and the `:` line shows the same
  trailing hints as the terminal: `press a chord`, the completion count, a
  live error, the command's parameter hint.
- **Terminal: the `:` line's caret is in the right column after a
  multi-byte character.** It was placed by byte count.
- **GUI: file-tree, oil and help panes are labelled in the modeline**
  (`[tree] <root>`, `[oil] <dir>`, `[help] <title>`), as they are in the
  terminal, and an oil listing with unapplied edits is marked `[+]` in both.
- **GUI: the completion popup opens at the text you are completing**
  instead of in the window's top-right corner, with its documentation panel
  beside it.
- **GUI: `<Esc>` inside a focused popup cancels a search there first**
  instead of closing the popup with the search half-typed. Moving between
  splits with `<C-w>` also uses the window's real width now.
- **GUI: terminal panes show bold, italic, dim and strikethrough text**, and
  search hits are underlined instead of looking like the current match.
- **GUI: completion ghost text.** With `completion.ghost_text` on, the rest
  of the top candidate is shown dimmed after the cursor, as in the terminal.
- **GUI: completion and hover popups line up with the text in a split or
  beside a gutter.** They were placed as if the pane started at the window's
  left edge with no line numbers.
- **GUI: the expanded command line (`<C-x><C-e>`) is syntax-coloured** on its
  first line, as the one-row `:` line is.
- **GUI: symbol highlights distinguish reads from writes.** With a language
  server attached, the other occurrences of the symbol under the cursor were
  all tinted as reads. Writes and plain textual occurrences now get their own
  theme colours, as in the terminal.
- **GUI: a `:s` preview strikes through the text it will replace**, as the
  terminal does.
- **GUI: the modeline's hover tooltip follows the theme.** It was a fixed
  dark box, on light themes too.
- **GUI: `:set relativenumber` works.** The window's gutter always showed
  absolute line numbers.
- **GUI: closing the window no longer discards unsaved work.** The close
  button and Cmd+Q closed the window without asking the editor. They now
  behave like `:qa`: with an unsaved buffer the window stays open and the
  message line says which buffer.
- **GUI: `lattice --gui --tutor` opens the tutor.** The flag was ignored in
  the window.
- **GUI: the completion popup's keys work, and so do snippet tabstops.** In
  the window (`--gui`) the insert-completion popup opened and `<C-n>`, `<C-p>`
  and `<C-y>` went to the buffer underneath; `<Tab>` did not move between a
  snippet's placeholders. The terminal was unaffected.
- **GUI: a macro that opens a file and then edits it replays in order.** It
  replayed as "edit, then open", so the text landed in the buffer the macro
  was started from.
- **GUI: typing is one round-trip to the editor core per character**, down
  from four, and the window no longer does editor work while painting a frame.
- **GUI: results that arrive on their own no longer wait for a keypress.** A
  file opened by a picker whose accept finished in the background appeared
  only when you next pressed a key, and so did a quit raised by a mouse
  click.
- **GUI: toggling `ui.nerd_fonts` redraws the file tree's icons** instead of
  leaving the old glyphs in place.
- **The keys in `:plugins` work on a plugin that failed.** `b`, `r`, `u` and
  `<CR>` did nothing on an entry under *Failed to load* or *Build failed* —
  the plugins you most want to rebuild were the ones no key reached, and
  getting one back took `:plugin-load <path>` or a restart. The cursor on a
  plugin's name there, or anywhere in the error under it, is now that plugin:
  `b` rebuilds and loads it, `r` tries the load again, `u` fetches first. A
  plugin `init.rs` requires that has never built can be rebuilt this way too.

## 0.9.7 — 2026-10-11

### Added
- **Lattice can install language servers.** `:lsp-install rust-analyzer`
  downloads the server, checks it against a pinned SHA-256, unpacks it into
  a directory lattice manages, and uses it from then on — no `PATH` entry
  and no toolchain. It runs in the background and reports each step live in
  `*lsp-install:<server>*`; a download that fails its check installs nothing
  and leaves nothing behind. `:lsp-servers` lists the servers with their
  state and has keys on each row (`i` install, `u` update, `x` uninstall,
  `<CR>` its log). `:lsp-update` and `:lsp-update-all` move to a newer
  pinned version without a moment with no server, and `:lsp-uninstall`
  removes one. Installed servers are used for files opened afterwards and
  survive restarts. rust-analyzer ships in the list, for Linux and macOS on
  x86_64 and aarch64; add your own, or pin a different version, with a
  `registry.toml` in the plugin's data directory. This is a new bundled
  plugin, `lighthouse` — `:help lighthouse`.
- **Plugins can download, unpack and run things, and show their progress.**
  Plugin API 0.2.0 adds `http-download` (to hosts the manifest names, with a
  mandatory SHA-256), `extract-archive`, `set-executable`, `spawn-process`
  (bundled plugins only) and `register-server`, all of which return at once
  and report through events; `output-append` / `output-status` /
  `output-reset`, for a read-only buffer a plugin fills while it works; and
  `host-platform` and `data-dir`. A plugin's own data directory no longer
  needs an `fs:` capability to be used by these calls. Existing plugins need
  rebuilding against 0.2.0.

## 0.9.6 — 2026-10-10

### Added
- **Ex ranges.** A `:` line takes vim's addresses: line numbers, `.`, `$`,
  `%`, marks (`'a`, `'<`, `'>`), `/pattern/` and `?pattern?`, and `+n` / `-n`
  offsets, alone or as a `from,to` pair. `:d`, `:y`, `:>`, `:<`, `:s`, `:g`
  and `:narrow` act on the range (`:2,5d`, `:.,$s/a/b/`, `:'a,/end/>`), and a
  range with no command jumps to its last line (`:$`, `:/TODO/`). Before,
  only `:42`, `%s` and the `:'<,'>` prefix were understood.
- **`:!` runs shell commands.** With a range it filters: `:%!sort`,
  `:.!date`, `:'<,'>!jq .` pipe those lines through the command and replace
  them with its output, as one undo step. It runs in the background, so a
  slow command does not freeze the editor; if the buffer changes before it
  finishes, the output is discarded rather than written over your edit.
  With no range, `:!cmd` runs the command and streams its output into its
  own `*shell-command*` buffer (`<C-c>` stops it, `gr` runs it again). It
  is separate from `:compile`: the output is not parsed for errors, and a
  one-off command never replaces a build's output, its error list or the
  command `:recompile` re-runs. The buffer is highlighted as shell.
- **`'<` and `'>` are motions.** They jump to the start and end of the last
  Visual selection (`` `< `` and `` `> `` to the exact column), and they
  compose with operators, so `d'>` and `y'<` work. Before, they were
  understood only in the `:'<,'>` prefix.

### Fixed
- **Highlighting waited for a keypress after a background change.** When
  a buffer's text changed on its own (a filter's result, a formatter, a
  server's edit, streamed output), its syntax colours were not refreshed
  until the next key. They now catch up as the change lands.
- **A buffer with no file could not take a built-in language from its
  mode.** A mode, including a plugin's, that names a built-in language such
  as `bash` now has its file-less buffers highlighted with it. Before, only
  plugin-provided languages worked this way.
- **`:describe-key` could not take a mode prefix.** Typing `i_` and then
  a chord described `i` as soon as it was pressed. The prompt now waits
  after a mode letter, so `i_<C-n>`, `n_j` and the rest work again; press
  `<CR>` to describe the letter on its own.
- **`:format` timed out on large files.** An external formatter whose
  output ran past 64 KiB was reported as timed out when it was only waiting
  to be read.
- **`:'<,'>s/…/…/` substituted on the cursor line only.** It now covers the
  selected lines, as the prefix says.
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
