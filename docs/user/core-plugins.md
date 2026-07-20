# Core plugins

**Core plugins ship with lattice.** They are the batteries-included set — prebuilt
WebAssembly components that live *beside* the editor (not compiled into the
binary), discovered at boot, and enabled by default. You don't install them, you
don't build them, and they work offline with no toolchain. You *can* turn any of
them off.

This is distinct from **user plugins**, which you declare from a git repo or a
local directory and which the editor builds on first boot — see
[plugins.md](plugins.md) for that (the use-package model).

## The list

| Plugin | Default mode | Enable option | What it does |
|---|---|---|---|
| **auto-pair** | `auto-pairs-mode` | `auto-pair.enabled` (default `true`) | Auto-closes brackets/quotes; a `manual` style closes the nearest unmatched opener on one key. See [below](#auto-pair). |

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

## auto-pair

Auto-closes brackets and quotes as you type; a `manual` style instead closes the
nearest unmatched opener when you press one key.

**On by default.** Type `(` and you get `()` with the caret between; type `)`
before an existing `)` and the caret steps over it; backspace inside an empty pair
deletes both. The full set is `() [] {}` and `"" '' \`\``.

**Options** (set via `:set`, `lattice.toml`, or `config::set_option` in `init.rs`):

| Option | Values | Default | Meaning |
|---|---|---|---|
| `auto-pair.enabled` | bool | `true` | Enable/disable `auto-pairs-mode`. |
| `auto-pairs-style` | `auto` \| `manual` | `auto` | `auto` completes pairs on the opening key; `manual` self-inserts and closes on the close key. |
| `auto-pairs-close-key` | key | `<C-j>` | The manual-style close key. |

**The `manual` style.** With `auto-pairs-style=manual`, the pair keys just insert
themselves; a single key (`<C-j>`) closes the nearest *unmatched* opener above the
caret, scanning only the enclosing lexical scope (via the tree-sitter seam, so it
stays fast on large files) and falling through to whatever else the key is bound to
when nothing is open. This is the [`vim-pairify`](https://github.com/dhruvasagar/vim-pairify)
model.

```toml
# lattice.toml — manual pairing
auto-pairs-style = "manual"
```

**Testing it.** Open any file, enter insert mode, and:

- `auto` (default): type `(` → `()`, caret between; type `"` → `""`; type `)` over
  an existing `)` → steps over; `<BS>` inside `()` → deletes both.
- `manual` (`:set auto-pairs-style=manual`): type `(` → just `(`; move on, then
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
