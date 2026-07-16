+++
title = "Installation"
description = "Install Lattice from source, Homebrew, or pre-built binaries"
+++

## From source (Rust toolchain)

```sh
git clone https://github.com/dhruvasagar/lattice
cd lattice
cargo build --release

# Run (TUI mode)
./target/release/lattice

# Run (GPU mode, macOS)
cargo run --features gui -- --gui
```

Requires Rust 1.85+ (edition 2024). Install the toolchain via [rustup](https://rustup.rs/).

## Homebrew (macOS)

```sh
brew install dhruvasagar/lattice/lattice
```

(coming soon — tap not yet published)

## Pre-built binaries

Download the latest release from the [releases page](https://github.com/dhruvasagar/lattice/releases).

| Platform | Architecture | Format |
|---|---|---|
| macOS | x86_64, aarch64 | .tar.gz |
| Linux | x86_64, aarch64 | .tar.gz |

Pre-built binaries are signed and checksummed.

## From source (no Rust toolchain)

If you don't have Rust installed but want the latest development build:

```sh
curl -fsSL https://github.com/dhruvasagar/lattice/releases/latest/download/lattice-x86_64-linux.tar.gz \
  | tar xz
./lattice
```

## Build dependencies

- **Runtime:** macOS 14+ or Linux (kernel 5.10+)
- **Build:** Rust 1.85+, `clang` (for tree-sitter), `cmake` (for some native deps)
- **GPU mode (optional):** Metal (macOS) or Vulkan (Linux); enabled via `--features gui`

## Verify your install

```sh
lattice --version
```

Should print the version number and commit hash.

## Next steps

- [Getting started](./docs/getting-started/) — ten-minute orientation
- [Modal editing](./docs/modal-editing/) — the vim grammar
