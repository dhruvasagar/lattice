---
summary: "version-mode: the *version* buffer `:version` opens — this build's version, commit, toolchain, renderer, plugin API and paths, with `Y` to copy it all for a bug report."
related: [ex:version, plugins, messages-mode]
---

# version-mode

The `*version*` buffer: what this build of Lattice is, what it is running
on, and where it keeps its files. `:version`.

It exists for bug reports. Open it, press `Y`, and paste — the page is
markdown, so it reads the same in an issue as it does here.

`version-mode` is a minor mode. The buffer's major is
[`markdown-mode`](help:markdown-mode), so the page is highlighted like any
markdown file; `version-mode` adds the read-only guard and `Y`.

## What it shows

| Section | |
|---|---|
| **Build** | Version; the commit it was built from, whether the tree had uncommitted changes, and the commit's date; debug or release; which renderers were compiled in; the Rust toolchain and target |
| **Runtime** | The renderer this session is running in (window or terminal), the OS and architecture, and the terminal's `TERM` / `COLORTERM` / `TERM_PROGRAM` when set |
| **Plugin API** | The plugin API generation a plugin must be built for (`lattice:plugin-host@0.4.x`), its fingerprint, and the wasmtime version that runs plugins |
| **Paths** | The config directory, options file, `init.rs`, user and core plugin directories, and the cache. There is no log file — logs go to [`*messages*`](help:messages-mode) |

The commit **date** is shown, not when the binary was built: two builds of
the same commit are the same build. A binary built outside a git checkout
with no commit supplied says `unknown` rather than guessing.

## Keys

| Key | |
|---|---|
| **`Y`** | Copy the whole page to the system clipboard |

`y` is still vim's yank, so `yiw` on the commit copies just the hash.
Motions, search and `:` work as anywhere; the buffer is read-only.

## Without starting the editor

```sh
lattice --version             # lattice 0.9.9
lattice --version --verbose   # this whole page, on stdout
```

`--verbose` (or `-V -v`) prints the same page — useful when the editor
will not start. Since nothing launches, its renderer line says what a bare
`lattice` would open in here.

## See also

- [`plugins`](help:plugins) — what is loaded, against this plugin API.
- [`messages-mode`](help:messages-mode) — where the logs are.
