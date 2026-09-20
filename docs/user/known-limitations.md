---
summary: "What doesn't work yet at 0.9, and what is deliberately absent."
---

# Known limitations

Lattice 0.9 is an alpha. This page is the honest list — check it before
filing an issue, and file anyway if your case is worse than what's written
here.

## Distribution

- **Binaries are unsigned.** macOS quarantines browser downloads; clear it
  with `xattr -dr com.apple.quarantine <dir>`. No `.dmg`, no `.msi`, no
  notarisation — those need a paid certificate.
- **No Homebrew, no `cargo install`.** Use the install script or a release
  archive. See [installation](https://dhruvasagar.github.io/lattice/install/).
- **ARM Linux and ARM Windows GUI builds are best-effort** and may be missing
  from a release. The terminal build is available on every platform.
- **Windows executables have no embedded icon.**

## Editor

- **`--gui` is opt-in.** The terminal renderer is the default and is a
  first-class peer, not a fallback. The GPU renderer is not yet at parity.
- **Syntax colours are not fully themeable.** UI theming works; several
  syntax-style consumers still read a hardcoded palette.
- **LSP servers must be installed by hand.** There is no server manager or
  installer; point lattice at servers already on your `PATH`.
- **`:Tree` requires an explicit path — bare `:Tree` errors; use `:Tree .`
  for the current directory.**
- **No crash reporter**, and no accessibility work has been done yet.

## Grammar and commands not yet implemented

- `!` (filter through an external command) and `gq` (format motion).
- The `'<` / `'>` visual marks.
- Ex ranges are partially implemented.
- Command line: `<C-b>` / `<C-e>` cursor movement, `<C-r>` register paste,
  and completion inside `:s/.../.../`.
- `:customize`, `:autocmd` / `:add-hook`, `:describe-event`,
  `:describe-mode`, `:history-*`.
- Terminal buffers: mouse passthrough, and word motions in Terminal Visual.

## Deliberately absent

- **No vimscript, no Lua, no elisp.** WASM is the single extension
  substrate; your config is Rust compiled to WASM
  ([init](help:init)). This is a design decision, not a gap.
- **No vim/emacs config compatibility.** Explicit non-goal.
- **Rich inline media** (images, embedded widgets) is post-1.0.

## Reporting

[Open an issue](https://github.com/dhruvasagar/lattice/issues) with your
platform, `lattice --version`, and whether `:plugins` lists the three bundled
plugins. If it doesn't, say so — that is its own bug and it hides others.
