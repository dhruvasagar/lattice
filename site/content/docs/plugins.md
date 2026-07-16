+++
title = "Plugins"
+++



Extensibility is one of lattice's four paramount goals (CLAUDE.md #2). The
extension substrate is a **WebAssembly Component Model** host: plugins ship as
`.wasm` components, run sandboxed with explicit capabilities and a fuel budget,
and are crash-isolated so a misbehaving plugin can never stall or take down the
editor. There is one substrate — no Lua, no vimscript, no elisp. Any language
with Component Model toolchain support (Rust today; Zig, Go, AssemblyScript,
… tomorrow) can target it.

The design principle is **"the boundary is a seam we already built, not a wall
we bolt on"**: every plugin interface mirrors a native Rust trait the editor
already uses internally. A plugin that contributes a picker source, a completion
source, a motion, or an event hook plugs into the *same* seam the built-in
feature uses — it is not a second-class bolt-on.

> **Status:** Phase 7 (the plugin **host runtime**) is complete — the
> `lattice-plugin-host` crate, the WIT API package, the capability/fuel/
> crash-isolation model, every extension seam (exercised end-to-end by guest
> fixtures), the `fuzzy-finder` validation plugin, and the CI overhead gates all
> ship today.
>
> **What is not wired yet: loading a plugin into the running editor.** There is
> no `:load-plugin` command, no on-disk plugin discovery, and no `init.rs`
> config path — those are **Phase 8** (the plugin *manager*). So today you can
> *introspect* the plugin API and understand the model (below), and — if you
> write plugins — build and test a guest against the host library (see the
> [authoring guide](../dev/guides/plugin-authoring)); you cannot yet drop a
> `.wasm` into a config directory and have the editor pick it up.

---

## What you can do today: introspect the API

The plugin API is **self-documenting from day one** (design §5.11). The `wit/`
interface package is compiled into a browsable catalog, so you can see exactly
what a plugin *will* be able to do without reading source. These commands work
now:

| Command | What it does |
|---|---|
| `:list-plugin-apis` | List every plugin-API interface (*seam*) the WIT package exposes — one row per seam with its direction and capability. |
| `:describe-plugin-api [<seam>]` | Open a help view for one seam (`host-services`, `picker-source`, `grammar`, `events`, …) showing its functions, direction, and required capability. Omit the seam to list all of them. `<Tab>`-completes the seam name. |
| `:export-plugin-api [markdown\|json]` | Export the whole API catalog as Markdown (default) or JSON — useful for generating plugin scaffolding or offline reference. |

Two more commands exist for *loaded* plugins, but until the Phase-8 loader lands
they report an empty set:

| Command | Today's behavior |
|---|---|
| `:list-plugins` | *"No plugins are loaded. (The plugin loader is wired in at Phase 8.)"* |
| `:describe-plugin <name>` | Same — nothing is loaded to describe yet. |

## The extension seams

Each seam is a typed interface a plugin implements or calls. They mirror the
native traits the editor already exercises, so the plugin path and the built-in
path are the same code shape.

| Seam | A plugin can… |
|---|---|
| **picker-source** | Contribute a fuzzy-picker source (like the file picker) — produce candidates, handle accept, route to an editor action. |
| **completion-source** | Contribute an Insert-mode completion source that generates candidates off-keystroke (the same async pattern LSP completion uses). |
| **grammar** | Register new vim motions, operators, text objects, actions, and ex-commands — extending the modal grammar itself. This is the one seam that runs *synchronously* on a keystroke (so an operator can compose with a plugin motion), under a strict sub-frame budget. |
| **events / hooks** | Subscribe to typed editor events (the unified autocmd/hook bus) and react off the hot path. |
| **decorations** | Produce gutter/line decorations as an off-render producer (the `git-gutter`-style seam). |
| **config** | Register typed options into the same registry `:set` reads. |
| **modes** | Declare a major/minor mode (kind, keymap, capabilities) that registers into the mode registry. |
| **host-services** | Call back into the editor for capability-gated services (e.g. filesystem enumeration). |

Run `:describe-plugin-api <seam>` for the exact function signatures of any of
these.

## The security model

A plugin is untrusted by default. Three mechanisms — all load-bearing for
paramount goal #1 (the editor stays responsive no matter what a plugin does) —
keep it contained:

**Capabilities (deny by default).** A plugin declares the capabilities it wants
in its manifest; it receives only the *intersection* of what it requested and
what its trust tier permits. With no grant a plugin reaches **no filesystem at
all** — no ambient authority, no network sockets, no subprocess. Capabilities
are explicit and prefix-scoped:

| Capability | Grants |
|---|---|
| `fs:read:<path>` / `fs:write:<path>` | Read/write access under a specific path prefix only. |
| `net:http:<host>` | HTTP to a named host (serviced through a gated host-service, not raw sockets). |
| `proc:spawn` | Spawn a subprocess — **bundled plugins only**. |
| editor caps (`lsp`, `tree-sitter`, `folds`, `diagnostics`, `writable`, `buffer-uri`) | Access to specific editor subsystems, for plugin-declared modes. |

Every plugin also gets a private, always-writable data directory (mounted at
`/data`); denied capabilities are surfaced, not silently dropped.

**Trust tiers.** *Bundled* plugins (shipped with lattice) are pre-granted.
*User-installed* plugins would prompt for consent before a grant — the
prompt-and-narrow flow arrives with the Phase-8 manager.

**Fuel + time budget.** Every plugin call runs under both a *fuel* cap (a bound
on how much work it may do) and an *epoch* deadline (wall-clock, ~1 ms tick). A
plugin that loops forever or runs long is cut off — it cannot hang the editor.
Budgets are per-seam: the synchronous grammar seam gets a tight sub-frame
"Reflex" budget; async seams (events, decorations) get larger ones. Each call
gets a fresh allowance.

**Crash isolation / quarantine.** If a plugin traps (runs out of fuel, blows its
deadline, panics, or hits an out-of-bounds access) it is **quarantined**: a
single `PluginCrashed` event is published, and every later call to that instance
short-circuits instead of re-entering the dead instance. Only the offending
plugin is affected — the event bus, other plugins, and the editor keep running.
(Automatic reload of a quarantined plugin is a later slice.)

## Coming in Phase 8

Phase 7 built and proved the runtime; Phase 8 makes it reachable from a running
editor:

- **The plugin manager / loader** — on-disk discovery of plugins + their
  `manifest.toml`, and the load path itself.
- **Bundled reference plugins** — `git-gutter`, `auto-pair`, rainbow-delimiters,
  and the built-in major modes shipping *as components*.
- **`init.rs` as configuration** — your user config compiled to WASM and loaded
  with a boot-capability set (keymaps, autocmds, custom commands as code; TOML
  stays for static option overrides).
- **Decoration rendering** — the renderer reading plugin-produced decorations
  (the producer half already works).

## Writing a plugin

You can already write a Component Model guest against the WIT package and test it
against the host library today — the editor-side loader is the only missing
piece. For the toolchain (`wasm32-wasip2`, `wit-bindgen`), a seam-by-seam
walkthrough, the manifest format, fuel budgets, and a worked `fuzzy-finder`
example, see the **[plugin authoring guide](../dev/guides/plugin-authoring)**.

## See also

- [Options / configuration](options) — the typed-option system plugins register into.
- [Modes](modes) — major/minor modes, which Phase 8 ships as components.
- [Completion](completion) and [LSP](lsp) — seams a completion-source plugin mirrors.
