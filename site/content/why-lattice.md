+++
title = "Why Lattice?"
description = "What makes Lattice different from Vim, Neovim, Emacs, Helix, and Zed"
+++

Lattice starts from a simple observation: every existing editor makes a set of architectural trade-offs that are visible every time you use them. Lattice chooses differently.

## GPU-accelerated, not terminal-emulated

Most "modern" editors are still terminals rendering a TUI. The terminal is a 1970s serial protocol — it doesn't know about fonts, sub-pixel positioning, ligatures, or smooth scrolling. Lattice renders directly to a GPU surface (via GPUI or wgpu), with a TUI renderer as a first-class peer (not a fallback).

**What this means for you:** ligatures that work, sub-pixel text positioning, smooth scrolling, no buffer-tearing, no "my terminal font doesn't support that glyph."

## WASM plugins, not Lua/vimscript/elisp

Every extensible editor embeds a scripting language. That language determines the ceiling on plugin performance, safety, and portability. Lattice uses the WebAssembly Component Model — not Lua, not vimscript, not elisp.

| | Lattice (WASM) | Neovim (Lua) | Emacs (elisp) | Helix (no API) |
|---|---|---|---|---|
| **Performance** | Near-native compiled | Interpreted | Interpreted | N/A |
| **Memory safety** | Sandboxed, fuel-limited | Host process | Host process | N/A |
| **Language choice** | Any (Rust, Zig, Go, ...) | Lua only | elisp only | N/A |
| **Crash isolation** | Process-level | None | None | N/A |

Your `init.rs` compiles to WASM and runs with a defined capability set — no random file access, no network calls unless you grant them.

## Everything is a buffer

Open a file? It's a buffer. Open the file tree? Buffer. Run a search? Results buffer. Open a terminal? Terminal buffer. All composable in splits, all navigable with `:bn` / `:bp` / `:ls` / `:bd`. There is no "sidebar" concept — panes are whatever buffers you put in them.

This means:
- The file tree lives in a buffer you can edit, split, or close
- Search results are a buffer you can edit and save
- The terminal is a buffer you can navigate with normal vim keys
- Everything is a first-class document

## Async by construction, not by retrofit

Lattice's core is built on three layers (UI / Core / Plugins) communicating via typed message passing over tokio. Nothing blocks the UI — enforced architecturally, not by discipline. LSP communication, plugin execution, file I/O all happen off the UI thread.

Compare:
- **Vim/Neovim:** single-threaded; LSP is a separate process but UI blocks on event loop ticks
- **Emacs:** single-threaded; `M-x compile` blocks the UI
- **Helix:** async, but plugins don't exist yet
- **Zed:** async by design (Rust + gpui); closest peer

## Extensible vim grammar

Lattice implements strict vim semantics (operators, motions, text objects, registers, ranges, counts) but makes the grammar itself extensible. Adding a new motion, text object, or operator is a first-class API — including tree-sitter-driven variants.

One deliberate deviation: the `:` command line and the functional API are unified into a single typed `CommandRegistry` with one dispatcher. Every ex-command, plugin contribution, and palette entry flows through the same `execute(...)` call.

## Where Lattice is not (yet) for you

Honest caveats — Lattice is in active development (Phase 1+):

- **Stability:** breaking changes happen; there is no 1.0
- **Plugin ecosystem:** the plugin API exists but the ecosystem is sparse
- **Windows support:** not tested; macOS and Linux are the primary targets
- **GUI polish:** the TUI renderer is more mature than the GPUI renderer

If those sound fine, read the [getting started guide](./docs/getting-started/).
