+++
title = "Plugins"
description = "Every lattice plugin — what ships with the editor, what you install yourself, and what it takes to write one."
weight = 3
sort_by = "weight"
template = "plugins-index.html"
+++

Lattice has **one extension substrate**. No Lua, no vimscript, no elisp. A
plugin is a WebAssembly component — written in Rust today, in any
Component-Model language tomorrow — loaded into its own `wasmtime` store,
granted only the capabilities it asks for and its trust tier permits, and
isolated well enough that a crashing plugin is quarantined rather than fatal.

Your own configuration is a plugin too: `init.rs`, compiled to WASM and loaded
at boot. Static settings stay in TOML; anything programmable is code, on the
same substrate everything else uses.

The difference between the groups below is **where a plugin comes from and
whether it is on by default** — not what it is allowed to do or which seams it
can reach.
