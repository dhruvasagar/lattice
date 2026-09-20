+++
title = "Installation"
description = "Install Lattice from a release archive, the install script, or source"
+++

Lattice 0.9 is an **alpha**. The editor is usable; the distribution is new.
Binaries are unsigned — see [Gatekeeper](#macos-gatekeeper) below if macOS
refuses to open one.

## Install script (macOS, Linux)

```sh
curl -fsSL https://raw.githubusercontent.com/dhruvasagar/lattice/main/install.sh | sh
```

Installs into `~/.local` (`~/.local/bin/lattice` plus the bundled plugins
under `~/.local/share/lattice`). Override with `--prefix`:

```sh
curl -fsSL https://raw.githubusercontent.com/dhruvasagar/lattice/main/install.sh | sh -s -- --prefix /usr/local
```

Add `~/.local/bin` to your `PATH` if it isn't there already.

## Release archives

Download from the [releases page](https://github.com/dhruvasagar/lattice/releases).

| Platform | Architectures | TUI | GUI |
|---|---|---|---|
| macOS | x86_64, aarch64 | `.tar.xz` | `.tar.xz` |
| Linux | x86_64, aarch64 | `.tar.xz` | `.tar.xz`, `.AppImage`, `.deb` |
| Windows | x86_64, aarch64 | `.zip` | `.zip` (x86_64) |

The `lattice-*` archives are the terminal build — the one to use over SSH or
on a server. The `lattice-gui-*` archives are the same editor plus the
GPU-rendered window, opened with `--gui`. ARM Linux and ARM Windows GUI
builds are best-effort and may be absent from a given release.

Every archive unpacks to a relocatable prefix — put it anywhere:

```
lattice-0.9.0-aarch64-macos/
  bin/lattice
  share/lattice/plugins/…   # bundled plugins; keep these beside bin/
```

`bin/lattice` finds its plugins through `../share/lattice/plugins`, so move
the whole directory rather than just the binary. Verify your download against
the release's `SHA256SUMS`:

```sh
shasum -a 256 -c SHA256SUMS
```

### macOS Gatekeeper

Archives downloaded in a browser are quarantined, and macOS will refuse to
run an unsigned binary. Clear the flag:

```sh
xattr -dr com.apple.quarantine lattice-0.9.0-aarch64-macos
```

Downloads made with `curl` — including the install script — are not
quarantined, so this step only applies to browser downloads. There is no
signed installer at 0.9; notarisation needs a paid certificate.

## From source

```sh
git clone https://github.com/dhruvasagar/lattice
cd lattice
cargo build --release
cargo xtask build-core-plugins   # builds the bundled plugins
./target/release/lattice
```

The GPU renderer is behind a cargo feature:

```sh
cargo run --features gui -- --gui
```

Requires Rust **1.94+** (edition 2024, pinned in `rust-toolchain.toml`) and
the `wasm32-wasip2` target for the plugin build:
`rustup target add wasm32-wasip2`. Install the toolchain via
[rustup](https://rustup.rs/).

`cargo xtask build-core-plugins` is not optional — without it the editor
starts with no bundled plugins and no error, because an absent plugin
directory is indistinguishable from an empty one.

## Requirements

- **macOS** 14+, or **Linux** with kernel 5.10+, or **Windows** 10+
- **Build:** Rust 1.94+, `clang` (tree-sitter), `cmake` (some native deps)
- **GPU mode (optional):** Metal on macOS, Vulkan on Linux

## Verify your install

```sh
lattice --version
```

Prints the version. To confirm the bundled plugins were found, open the
editor and run `:plugins` — `auto-pair`, `treesitter-context` and `project`
should each be listed as `bundled`.

## Not yet available

Homebrew, `cargo install`, `.dmg` and `.msi` are all post-0.9. See
[known limitations](@/docs/start/known-limitations.md).

## Next steps

- [Getting started](@/docs/start/getting-started.md) — ten-minute orientation
- [Modal editing](@/docs/editing/modal-editing.md) — the vim grammar
- [Known limitations](@/docs/start/known-limitations.md) — what doesn't work yet
