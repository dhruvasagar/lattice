+++
title = "FAQ"
description = "Frequently asked questions about Lattice"
+++

## General

### Is Lattice stable?

Lattice is in active development (Phase 1+). It is usable for daily editing but breaking changes happen. There is no 1.0 release yet. See the [implementation ledger](https://github.com/dhruvasagar/lattice/blob/main/docs/dev/operations/implementation.md) for the current state.

### What platforms are supported?

macOS 14+ and Linux (kernel 5.10+) are the primary targets. Windows is not tested or supported. Both x86_64 and aarch64 are supported on macOS and Linux.

### Is there a GUI?

Yes — Lattice has two renderer backends: a TUI renderer (terminal) and a GPUI renderer (native GPU window). The TUI renderer is more mature. Use `--gui` to launch the GPU mode on macOS.

### Does it work over SSH?

The TUI renderer works in any terminal, including over SSH, tmux, or any terminal multiplexer.

### What license?

MIT. See [LICENSE](https://github.com/dhruvasagar/lattice/blob/main/LICENSE).

## Vim/Neovim users

### Is it compatible with vim/Neovim configs?

No. Lattice uses its own configuration format (`init.rs` compiled to WASM, plus TOML for static settings). There is no vimscript or Lua compatibility layer — this is an explicit non-goal.

### Does it support vim plugins?

No. Lattice has its own plugin system based on WASM Component Model. Vim plugins are not supported.

### Does it implement all of vim's grammar?

Lattice targets strict vim semantics for the core grammar (operators, motions, text objects, registers, ranges, counts). The [vim-compatibility doc](https://github.com/dhruvasagar/lattice/blob/main/docs/user/modal-editing.md) lists known gaps.

### Can I use my vim keybindings?

The default keymap is vim. Many keybindings match vim defaults. You can rebind any key in `init.rs`. There is no automatic vimrc migration path.

## Emacs users

### Does it support Emacs keybindings?

There is an `emacs-keys-mode` minor mode that provides `C-x` leader-style bindings. See the [emacs-keys-mode docs](./docs/emacs-keys-mode/).

### Does it have an equivalent of org-mode?

Not yet. The everything-is-a-buffer architecture makes this possible, but no org-mode implementation exists.

### Can I use elisp?

No. Lattice uses WASM Component Model as the single extension substrate — no elisp, no embedded languages.

## Technical

### What is "everything is a buffer"?

All UI elements (file tree, terminal, search results, diagnostics, LSP logs) are real editor buffers. There is no fixed sidebar or bottom panel. You split panes to arrange your buffers however you like. Every buffer appears in `:ls`, can be navigated with `:bn`/`:bp`, and uses the same vim grammar.

### How does the WASM plugin system work?

Plugins are WebAssembly Component Model components compiled from any language with a component-model toolchain (Rust, Zig, Go, AssemblyScript, ...). They run in a `wasmtime::Store` with capability-gated access, fuel-limited execution, and process-level crash isolation.

### Can I write plugins in languages other than Rust?

Yes — any language with WASM Component Model support. The [plugin authoring guide](./dev/guides/plugin-authoring/) covers the WIT API and multi-language setup.

### Why not Lua, vimscript, elisp, or embedded Scheme?

An explicit non-goal. A single WASM substrate means one toolchain, one API surface, one security model. Performance, safety, and portability are fundamentally better than interpreted languages. Users who want scripting have the full Rust ecosystem available through WASM compilation.

### Does Lattice support LSP?

Yes — LSP is a first-class feature. Diagnostics, completions, hover, go-to-definition, references, rename, formatting, signature help, and code actions are all implemented. See the [LSP docs](./docs/lsp/).

### Does Lattice support tree-sitter?

Yes — tree-sitter provides syntax highlighting, incremental parsing, and structured motion/text objects. See the [languages docs](./docs/languages/).

### What file size can it handle?

Lattice uses ropey (a rope data structure) for buffer content, so large files (>100MB) should load and edit without excessive memory usage. The rendering path is O(viewport-lines), not O(file-length). We do not have formal large-file benchmarks yet.

### Can I use Lattice as my daily driver?

You can try! Many features work, but you should expect rough edges, missing features, and breaking changes. Back up your work, keep a fallback editor ready, and report bugs on the [issue tracker](https://github.com/dhruvasagar/lattice/issues).

## Community

### How can I contribute?

See the [contributing guide](https://github.com/dhruvasagar/lattice/blob/main/CONTRIBUTING.md). The [developer docs](./dev/) cover architecture and development setup. Good first issues are [tagged on GitHub](https://github.com/dhruvasagar/lattice/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22).

### Where do I report bugs?

Open an issue on the [GitHub issue tracker](https://github.com/dhruvasagar/lattice/issues). Include your platform, Lattice version (`lattice --version`), and steps to reproduce.

### Is there a chat or forum?

Not yet. GitHub issues and discussions are the primary communication channels for now.
