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

`auto-pair`, `treesitter-context` and `project` should each be listed with
SOURCE `bundled`. If the list is empty, the editor found no plugin
directory. Lattice looks in this order:

1. `$LATTICE_RUNTIME/plugins`
2. `<install-prefix>/share/lattice/plugins`
3. `<directory-of-the-binary>/../share/lattice/plugins`
4. `<workspace>/runtime/plugins` (when running from `target/`)

The usual causes are moving `bin/lattice` out of its extracted archive
(which orphans it from `../share/lattice/plugins`), or building from source
without running `cargo xtask build-core-plugins`. An absent plugin directory
is treated as "no plugins installed" and does not raise an error, which is
why this fails silently.

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
lattice --log-level debug 2>/tmp/lattice.log
```

Never use `println!`/`eprintln!` while the terminal UI is up — it corrupts
the alternate screen.

## Filing a bug

Include your platform, `lattice --version`, whether `:plugins` shows three
`bundled` rows, and the relevant `:messages` output.
[Open an issue](https://github.com/dhruvasagar/lattice/issues).
