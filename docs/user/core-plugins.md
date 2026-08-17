# Core plugins

**Core plugins ship with lattice.** They are the batteries-included set — prebuilt
WebAssembly components that live *beside* the editor (not compiled into the
binary), discovered at boot, and enabled by default. You don't install them, you
don't build them, and they work offline with no toolchain. You *can* turn any of
them off.

This is distinct from **user plugins**, which you declare from a git repo or a
local directory and which the editor builds on first boot — see
[`plugins`](help:plugins) for that (the use-package model).

## The list

| Plugin | Default mode | Enable option | What it does |
|---|---|---|---|
| **auto-pair** | `auto-pair-mode` | `auto-pair.enabled` (default `true`) | Auto-closes brackets/quotes; a `manual` style closes the nearest unmatched opener on one key. See [below](#auto-pair). |
| **treesitter-context** | `treesitter-context-mode` | `treesitter-context.enabled` (default `true`) | Pins the enclosing `impl` / `fn` / `if` above the text once their headers scroll away. See [below](#treesitter-context). |

More core plugins land over time (a git-gutter, a file-tree, …); each appears here
with its mode, its `<id>.enabled` option, and its own options.

## How a core plugin loads

1. **Ships prebuilt.** A release stages each core plugin's `.wasm` + `plugin.toml`
   into lattice's **runtime root** — a directory beside the binary, found via a
   search path (`$LATTICE_RUNTIME` → the install prefix's `share/lattice` →
   next to the executable → the dev workspace's `runtime/`).
2. **Discovered at boot.** The editor scans `<runtime>/plugins/` and loads each
   at the *bundled* trust tier (pre-granted — no capability prompt).
3. **Enabled by a config gate.** A core plugin's manifest names the minor mode it
   enables by default; the editor auto-registers a bool option `<id>.enabled`
   (default `true`) that turns that mode on. So the plugin is active out of the
   box, and the mode never forces itself on — you own the switch.

## Where core plugins live — and pointing lattice elsewhere

The **runtime root** is resolved by a search path; the **first existing** location
wins (except `$LATTICE_RUNTIME`, which always wins if set):

| Priority | Location | For |
|---|---|---|
| 1 | **`$LATTICE_RUNTIME/plugins/`** | An explicit override — you set this. |
| 2 | `<install-prefix>/share/lattice/plugins/` | Packaged installs (the prefix is baked in at build time via `LATTICE_INSTALL_PREFIX`). |
| 3 | `<exe-dir>/../share/lattice/plugins/` | Relocatable tarballs / macOS `.app` bundles. |
| 4 | `<workspace>/runtime/plugins/` | Running from a source checkout (`cargo run`). |

**To point lattice at a different runtime root, set `$LATTICE_RUNTIME`:**

```sh
LATTICE_RUNTIME=/opt/my-lattice-runtime cargo run -p lattice-cli
# core plugins are then discovered under /opt/my-lattice-runtime/plugins/
```

**Why an environment variable, not a config option?** Core plugins are discovered
and loaded at boot **before** `lattice.toml` and `init.rs` are read — so the
location has to be known without loading config first. `$LATTICE_RUNTIME` (and the
build-time `LATTICE_INSTALL_PREFIX`) are available that early; a config option
would be a chicken-and-egg. (The *user* plugins root is different — it's always
`~/.config/lattice/plugins/`, honoring `$XDG_CONFIG_HOME`.)

## Turning a core plugin off

Every core plugin has an `<id>.enabled` option. Turn it off any of three ways:

- **Live**, this session: `:set auto-pair.enabled=false` (toggles the mode off
  immediately; back on with `=true`).
- **Statically**, in `lattice.toml`:

  ```toml
  auto-pair.enabled = false
  ```

- **Programmatically**, in `init.rs`:

  ```rust
  config::set_option("auto-pair.enabled", "false");
  ```

Disabling the mode leaves the plugin **loaded** — it just deactivates its mode, so
re-enabling is instant. (You cannot un-*ship* a core plugin from config; if you
truly never want it, a future `core-plugins.exclude` list will cover that.)

**Per-buffer toggle.** Independently of the `enabled` gate, the plugin's mode has an
auto-generated `:auto-pair-mode` command — every registered mode gets a
`:<mode-name>` toggle — that flips it on the **active buffer** only. Use
`auto-pair.enabled` for the editor-wide default; `:auto-pair-mode` for a one-off
buffer.

## auto-pair

Auto-closes brackets and quotes as you type; a `manual` style instead closes the
nearest unmatched opener when you press one key.

**On by default.** Type `(` and you get `()` with the caret between; type `)`
before an existing `)` and the caret steps over it; backspace inside an empty pair
deletes both. The full set is `() [] {}` and `"" '' \`\``.

**Options** (set via `:set`, `lattice.toml`, or `config::set_option` in `init.rs`):

| Option | Values | Default | Meaning |
|---|---|---|---|
| `auto-pair.enabled` | bool | `true` | Enable/disable `auto-pair-mode`. |
| `auto-pair.style` | `auto` \| `manual` | `auto` | `auto` completes pairs on the opening key; `manual` self-inserts and closes on the close key. |
| `auto-pair.close-key` | key | `<C-j>` | The manual-style close key. |

**The `manual` style.** With `auto-pair.style=manual`, the pair keys just insert
themselves; a single key (`<C-j>`) closes the nearest *unmatched* opener above the
caret, scanning only the enclosing lexical scope (via the tree-sitter seam, so it
stays fast on large files) and falling through to whatever else the key is bound to
when nothing is open. This is the [`vim-pairify`](https://github.com/dhruvasagar/vim-pairify)
model.

```toml
# lattice.toml — manual pairing
auto-pair.style = "manual"
```

**Testing it.** Open any file, enter insert mode, and:

- `auto` (default): type `(` → `()`, caret between; type `"` → `""`; type `)` over
  an existing `)` → steps over; `<BS>` inside `()` → deletes both.
- `manual` (`:set auto-pair.style=manual`): type `(` → just `(`; move on, then
  press `<C-j>` → the matching `)` is inserted at the nearest unmatched `(`.

## For contributors — building core plugins from source

Running from a source checkout, stage the core plugins into the dev runtime root
once (and after changing one):

```bash
cargo xtask build-core-plugins   # builds plugins/<name>/ → runtime/plugins/<name>/
```

Then `cargo run -p lattice-cli` (or `--features gui -- --gui`) discovers them. The
`runtime/` directory is git-ignored — it's regenerated build output. A released
build stages the same artifacts into the packaged runtime root, so end users never
run this step.

See the design fragment
[`plugin-manager.md`](../dev/architecture/plugin-manager.md) for the two-roots
model, the runtime search path, and how user plugins (git/local, build-on-boot)
differ from core plugins.


## treesitter-context

Sticky scope headers. When the `impl` and the `fn` you are inside scroll off the
top, their header lines stay pinned above the text, so you can always see what
block you are in. The same idea as `nvim-treesitter-context`, or *sticky scroll*
in VSCode and Zed.

The pinned rows carry the buffer's own syntax colouring — they are built from
the same cell builder as the document, not re-highlighted — and they sit
**under** the headerline rather than replacing it, so a buffer showing LSP
status or a scan progress row keeps it and the context starts below.

### Jumping up the stack

`[u` jumps to the header of the enclosing scope. Press it again to walk further
out: each jump lands on a header, and the next press looks for a header strictly
above the cursor, so it finds the parent rather than sticking. A count works
(`3[u` goes three levels out), and at top level it does nothing.

`<C-o>` walks back — the jump records a position-history entry before it moves,
exactly like every other jump in the editor. There is deliberately no `]u`: the
inverse of walking up is `<C-o>`.

`:context-toggle` turns the strip off for the current buffer and back on.

### Options

All prefixed `treesitter-context.`:

| Option | Default | What it does |
|---|---|---|
| `enabled` | `true` | Master switch (registered by the plugin loader). |
| `anchor` | `cursor` | Which line drives the context: `cursor` (where you are) or `topline` (what you are looking at). |
| `max-lines` | `0` | Maximum context ROWS; `0` is unlimited. A wrapped signature spends more than one. The strip is bounded by `max-viewport-fraction` regardless. |
| `trim-scope` | `outer` | Which end to drop when over budget: `outer` keeps the scopes you are innermost in. |
| `multiline-threshold` | `1` | Maximum rows one scope's header may use. Raise it to see a whole wrapped signature. |
| `max-viewport-fraction` | `33` | Percent of the pane the strip may occupy, headerline included. |
| `separator` | `""` | Glyph repeated as a rule under the block. Empty disables it. |
| `line-numbers` | `true` | Show each context row's source line number in the gutter. |
| `disabled-languages` | `""` | Comma-separated grammar ids to skip. |
| `max-file-lines` | `5000` | Skip the structural query above this line count; `0` disables the guard. |

### Large files

The structural query runs over the **whole buffer** on each reparse, and its
cost grows faster than the file does — about 25 ms at 1 000 lines, 287 ms at
5 000, over a second at 10 000, and past roughly 20 000 it exceeds the plugin
budget and traps. A trap quarantines the plugin, which stops the strip for
*every* buffer until reload, so `max-file-lines` skips the query rather than
risk that: above the limit a file simply shows no context.

The default of 5 000 is set from those measurements. Raising it trades editor
responsiveness on a background thread for context in big files; setting it to
`0` removes the guard entirely and is not recommended until the whole-buffer
query is replaced.

### Languages

Rust, Python, Go, JavaScript, TypeScript/TSX, C/C++, and Markdown ship with
context queries. A language without one simply shows no strip — nothing breaks,
and adding a query is how support grows.

### Theme elements

`treesitter-context.background`, `.separator`, `.line-number`, and `.active`
(the innermost row). None of them colours the code itself: the rows keep the
buffer's own syntax highlighting, and these style the backdrop and gutter around
it. Restyle them like any builtin element.
