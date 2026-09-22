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
- **LSP servers must be installed by hand.** There is no server manager or
  installer; point lattice at servers already on your `PATH`.
- **Org-mode is a separate build.** It ships as an out-of-repo WASM plugin
  ([`dhruvasagar/lattice-org-plugin`](help:org)) you clone and build yourself
  — it is not bundled with the editor or the installer.
- **The Claude Code and opencode integrations need their own CLI installed.**
  [`:claude`](help:claude-code-mode) requires the `claude` CLI on your
  `PATH`; [`:opencode`](help:opencode-mode) requires the `opencode` CLI.
  Lattice provides the editor-side integration, not the agent itself.
- **Concealment has no settings.** Markup hiding works and both renderers
  paint it, but there is no `conceallevel` / `concealcursor` equivalent:
  the cursor line reveals in Insert and Replace, always, and only org
  declares conceal rules today.
- **No crash reporter**, and no accessibility work has been done yet.

## Grammar and commands not yet implemented

- `!` — filtering a range through an external command (`:%!sort`).
- The `'<` / `'>` visual marks as motions — `'<` does not jump. The
  `:'<,'>` command-line prefix does work, and is what Visual mode inserts
  for you.
- Ex ranges are partially implemented. What works: `:42` (go to a line),
  the `:'<,'>` prefix, `%s/` / `s/` for substitute, and `:g/` / `:v/`.
  What does not parse yet: `1,5`, `.` and `$`, `'a,'b`, `+n` / `-n`
  offsets, and `/pattern/` addresses.
- Completion inside `:s/.../.../`.
- `:autocmd` / `:add-hook`.
- `:customize` — browsing and picking groups/modes works, and
  `:customize-edit <name>` opens the option in the `:` line via `:set`; there
  is no TOML write-back, so a change made this way is not persisted back to
  your config file.
- `:history-*` (the dashed spelling) does not exist; use `:history
  [commands|searches|pane-buffers]` instead — it ships today with all three
  kinds.
- Terminal buffers: mouse passthrough.

## Deliberately absent

- **No vimscript, no Lua, no elisp.** WASM is the single extension
  substrate; your config is Rust compiled to WASM
  ([init](help:init)). This is a design decision, not a gap.
- **No vim/emacs config compatibility.** Explicit non-goal.
- **Rich inline media** (images, embedded widgets) is post-1.0.

## Reporting

[Open an issue](https://github.com/dhruvasagar/lattice/issues) with your
platform, `lattice --version`, and whether `:plugins` lists the four bundled
plugins. If it doesn't, say so — that is its own bug and it hides others.
