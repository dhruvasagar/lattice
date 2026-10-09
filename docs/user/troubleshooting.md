---
summary: "When something doesn't work: plugins, LSP, colours, logs, and first-run problems."
related: [plugins, lsp-status, messages]
---

# Troubleshooting

Start with [known limitations](help:known-limitations) — if it's listed
there, it isn't broken, it's absent.

## The bundled plugins aren't working

Symptom: typing `(` doesn't insert a closing paren, or there's no
tree-sitter context header.

```
:plugins
```

`auto-pair`, `treesitter-context`, `project` and `comment` should each be listed with
SOURCE `bundled`. If the list is empty, the editor found no plugin
directory. Lattice looks in this order:

1. `$LATTICE_RUNTIME/plugins`
2. `<install-prefix>/share/lattice/plugins`
3. `<directory-of-the-binary>/../share/lattice/plugins`
4. `<workspace>/runtime/plugins` (when running from `target/`)

If `lattice` is a symlink, rules 3 and 4 look beside the link first and
beside the file it points to second. So `~/.local/bin/lattice` linked to
`~/.cargo/bin/lattice` finds plugins in `~/.local/share/lattice/plugins`.

The usual causes are moving `bin/lattice` out of its extracted archive
(which orphans it from `../share/lattice/plugins`), or building from source
without running `cargo xtask build-core-plugins`. An absent plugin directory
is treated as "no plugins installed" and does not raise an error, which is
why this fails silently.

## My `init.rs` config isn't loading

Your config is compiled on your machine, so this is almost always the Rust
toolchain rather than the config. Open `:plugins`: `init` is listed under
*Failed to load* (or *Build failed*) with the full error. Its first lines
say one of:

| The message says | What it means | Fix |
|---|---|---|
| `Rust is not installed (no working cargo on PATH)` | lattice came from a release archive and this machine has no Rust — or Rust was installed after this shell was opened | Install from [rustup.rs](https://rustup.rs), run `rustup target add wasm32-wasip2`, then start lattice from a **new** shell |
| `the wasm32-wasip2 target is not installed` | Rust is there, the WebAssembly target is not | `rustup target add wasm32-wasip2` |
| `…and this Rust did not come from rustup` | Rust came from a distribution package, which has no `rustup` and usually no wasm target | Install the target from the same package source, or switch to rustup |
| `cargo build failed` followed by compiler errors | The toolchain is fine; `src/lib.rs` does not compile | Fix the error shown. The previous config, if there was one, keeps running |

`lattice --scaffold-init` runs the same check before you ever start the
editor, and adds the target for you when it can. The full list of
requirements is in [`init`](help:init#what-you-need).

A build that fails on a field the compiler says is *missing* right after an
editor upgrade means the plugin API gained a field — add it (usually `None` or
an empty list). The API your config compiles against is refreshed from the
editor on every build, so it is always the current one.

## An LSP server won't start

```
:lsp-status
```

Lattice does not install servers — the binary must already be on your
`PATH`. Check the server's own log:

```
:lsp-log
```

and the editor's messages buffer:

```
:messages
```

For a deeper trace, start with `--log-level debug`. Per-keystroke and
per-frame diagnostics are debug-level by design, so `info` stays readable.

## Colours look wrong

If the UI is themed but code is monochrome, the buffer's language may have
no tree-sitter grammar — check `:set filetype?`. If glyphs in the file tree
are boxes, your font isn't a Nerd Font: leave `ui.nerd_fonts` off and the
BMP fallback palette is used instead. Both palettes are the same cell width,
so nothing shifts.

## A key does nothing

See [troubleshooting keys](help:troubleshooting-keys), and:

```
:describe-key
```

Then press the chord. Note that a bound prefix consumes its longer chords --
if `gD` is bound, `gDd` can never fire.

## Where the logs are

`:messages` is the in-editor log. For a file, redirect stderr:

```sh
lattice --stderr-logs --log-level debug 2>/tmp/lattice.log
```

In TUI mode stderr tracing is off by default — stderr is the alternate
screen, and writing to it would corrupt the paint — so `--stderr-logs` is
required to turn it on. It only takes effect when stderr is redirected (as
above); with stderr still attached to the terminal, Lattice prints a note
and ignores the flag rather than corrupting the display. The GUI (`--gui`)
enables stderr logging unconditionally.

Never use `println!`/`eprintln!` while the terminal UI is up — it corrupts
the alternate screen.

## Filing a bug

Include your platform, `lattice --version`, whether `:plugins` shows four
`bundled` rows, and the relevant `:messages` output.
[Open an issue](https://github.com/dhruvasagar/lattice/issues).
