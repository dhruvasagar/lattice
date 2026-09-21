+++
title = "Plugins"
description = "Every lattice plugin — what ships with the editor, what you install yourself, how to manage them from inside lattice, and what it takes to write one."
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

## Manage them from inside the editor

`:plugins` opens the manager — a live buffer, not a dialog, so the whole vim
grammar works in it.

| | |
|---|---|
| **See the state** | every loaded plugin with its health (`ok`, or `quarantined` after a crash), trust tier, and the capabilities it was granted — with any denied ones noted |
| **Read the header** | sticky counts of loaded, running, quarantined and **failed to load**. That last number matters: a plugin that fails to load has no row in the table, so the header is the only place it appears |
| **Act on a row** | `<CR>` describes it, `r` reloads, `b` rebuilds from source, `u` updates, `x` unloads, `t` opens its boundary trace |
| **Act on all of them** | the same keys uppercased — `R`, `B`, `U`, `X` — plus `gr` to refresh |
| **Watch a build** | a third header row appears while a rebuild runs and disappears when it finishes |

It updates live. A plugin that crashes changes state under your cursor without
you asking for it, and the table is coloured by meaning rather than by syntax —
`quarantined` and failed builds are highlighted because they are the rows you
have to act on, while a screen of healthy plugins is left plain.

When something loads but does not behave, `:plugin-trace` shows what it is
actually saying across the host boundary.

Full reference: [the plugins manager](@/docs/reference/plugins-mode.md) and
[the plugin host](@/docs/config/plugins.md).

## The plugins themselves

The difference between the groups below is **where a plugin comes from and
whether it is on by default** — not what it is allowed to do or which seams it
can reach. Each page carries that plugin's own manual: the same text
`:help <name>` shows you inside the editor.
