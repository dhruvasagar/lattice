---
summary: "The WASM plugin host: loading + managing plugins, the capability + fuel + crash-isolation security model, the :plugins manager view, boundary-trace observability, and the plugin API introspection commands."
related: [plugin, wasm, extension, capability, modes, trace, observability]
---

# Plugins

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

> **Status:** the plugin **host runtime** (Phase 7) and the **editor-side
> loader + manager** (Phase 8) both ship today: on-disk discovery, the
> `:plugin-load` / `:plugin-unload` / `:plugin-reload` commands, the `:plugins`
> manager view, the `init.rs` config path, and the full boundary-trace
> observability stack (`:plugin-trace`, `plugin.trace-level`, guest logging).
> **`auto-pair` now ships as the first *core plugin*** — prebuilt, discovered at
> boot, on by default (see [core-plugins.md](core-plugins.md)). The frontier is
> more core plugins (`git-gutter`, rainbow-delimiters) plus the **use-package
> layer** — declaring *user* plugins from a git/local source and building them on
> first boot (the plugin-manager PM.5–PM.8 slices).

---

## Quick reference

| Command / keystroke | Meaning |
|---|---|
| `:plugin-load <path>` | Load a `.wasm` component from `<path>` (under its manifest's capability grant). |
| `:plugin-unload <name>` | Unload a plugin by name (or numeric id): abort its tasks, reverse every registry contribution. |
| `:plugin-reload <name>` | Unload then re-load from disk — a fresh instance with an untripped quarantine. |
| `:plugins` | Open the **manager view** — a buffer listing every loaded plugin with health, tier, and capabilities. |
| `:plugin-trace` | Open `*plugin-trace*` — the live boundary-trace firehose across all plugins. |
| `:reload-config` | Re-load your `init.rs` config module. |
| `:set plugin.trace-level=debug` | Raise the global trace verbosity (see [observability](#observability-what-a-plugin-is-doing)). |
| `:list-plugin-apis` / `:describe-plugin-api [<seam>]` | Browse the plugin **API catalog** (the WIT seams). |
| `:list-plugins` / `:describe-plugin <name>` | List / describe the currently-loaded plugins. |

In the `:plugins` view: `r` reload · `x` unload · `K` / `<CR>` describe · `gr`
refresh · `t` open that plugin's trace · `T` cycle its trace verbosity.

---

## Core plugins vs. user plugins

Lattice has **two plugin roots**:

- **Core plugins** ship *with* lattice — prebuilt components in a runtime root
  beside the binary, discovered at boot at the bundled tier, and enabled by
  default via a `<id>.enabled` gate. `auto-pair` is the first. You configure or
  disable them, you don't install them. See **[core-plugins.md](core-plugins.md)**.
- **User plugins** live in `~/.config/lattice/plugins/` — you install them (drop a
  built component there, or, with the use-package layer, declare a git/local
  source that the editor builds on first boot). These load at the user-installed
  tier under a capability grant.

Everything below applies to both; the difference is only *where they come from* and
*whether they're on by default*.

## Loading a plugin

A plugin is a `wasm32-wasip2` component plus a `manifest.toml` declaring its id,
the seams it provides, and the capabilities it requests. Load one explicitly
with `:plugin-load <path>`, or drop it into the plugins directory for automatic
discovery at startup. On load the host computes the plugin's capability grant
(what it requested ∩ what its trust tier permits), creates its private data dir,
and drives the `compile → instantiate → activate` spine — off the boot thread, so
a plugin's cold-start never delays startup.

`:plugin-unload <name>` reverses everything: it aborts the plugin's running
actor tasks and undoes every registry contribution (its motions, options, modes,
keymaps, decorations) by provenance. `:plugin-reload <name>` does an unload +
fresh load from the same on-disk source — the way to pick up a rebuilt `.wasm`,
and the way to revive a plugin that crashed (a reloaded instance gets a fresh,
untripped quarantine).

### `init.rs` — configuration as a plugin

Your user configuration is itself a plugin: `init.rs` compiled to WASM, loaded
at boot with a trusted (bundled) capability set. Anything *programmable* —
keymaps, autocmds/hooks, custom commands — lives there as code; static option
overrides stay in TOML. `:reload-config` re-loads it, and a rebuilt `init.wasm`
is auto-detected and reloaded without a manual command.

---

## The `:plugins` manager view

`:plugins` opens a read-only buffer listing every loaded plugin — its name,
health (`ok`, or `quarantined` after a crash), trust tier, and the capabilities
granted (with any denied ones noted). It updates live: a plugin that crashes
while the view is open flips to `quarantined` immediately.

It is a real buffer (everything-is-a-buffer), so ordinary motions work, and it
carries in-view chords on the row under the cursor:

| Key | Action |
|---|---|
| `r` | Reload the plugin under the cursor |
| `x` | Unload it |
| `K` / `<CR>` | Open its documentation (`:describe-plugin`) |
| `gr` | Refresh the list (pick up out-of-band loads) |
| `t` | Open that plugin's boundary trace (`*plugin-trace:<name>*`) |
| `T` | Cycle that plugin's trace verbosity (off → error → … → trace) |

---

## Observability: what a plugin is doing

Because every plugin interaction is host-mediated, the host can show you **every
call in and out of a plugin** — its name, timing, fuel cost, result or trap,
capability denials — *independent of the plugin's source language*. This
"boundary trace" is lattice's plugin debugger: it is a property of the
message-passing architecture, not a language-specific add-on.

**The trace buffers.** `:plugin-trace` opens `*plugin-trace*`, the interleaved
firehose across all plugins. Pressing `t` on a `:plugins` row opens
`*plugin-trace:<name>*`, filtered to that one plugin. Both are read-only buffers
that seed from the in-memory ring and live-tail new records. A line reads:

```
debug [plugin:3] grammar »apply-motion → ok 34µs
error [plugin:3] grammar »apply-motion → trap(fuel)
info  [plugin:3] logging index: reindexed 40 files
```

**Verbosity.** Tracing is **off the keystroke hot path** and **off by default** —
at the default `info` level you see only lifecycle/crash signal, not per-call
noise. Raise it globally with `:set plugin.trace-level=debug` (or `trace`), or
per-plugin with `T` in the `:plugins` view. The change is live on the next
keystroke, and raising one plugin's verbosity never costs another anything.

**Guest logging.** A plugin can also emit its *own* narrative through a
`wasi:logging`-shaped host import (`log(level, context, message)`) — "parsing
X", "reindexed 40 files". Those lines are captured into the same trace buffer,
tagged by plugin and level, interleaved with the boundary trace, so a plugin
author sees both the host's observed behavior and the guest's stated intent in
one place. Guest logs obey the same per-plugin verbosity gate.

---

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
| **keymap** | Bind user keys above the built-in grammar (the `init.rs` keybinding path). |
| **host-services** | Call back into the editor for capability-gated services (e.g. filesystem enumeration). |
| **logging** | Emit the plugin's own log narrative into the boundary trace (Layer 2). |

Run `:describe-plugin-api <seam>` for the exact function signatures of any of
these; `:list-plugin-apis` lists them all, and `:export-plugin-api` dumps the
whole catalog as Markdown or JSON (useful for scaffolding).

---

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
`/data`); denied capabilities are surfaced, not silently dropped. A plugin's
manifest id must be a single safe path component — it keys that writable data
dir, so a path-escaping id is rejected at load.

**Trust tiers.** *Bundled* plugins (shipped with lattice) are pre-granted.
*User-installed* plugins receive only the capabilities their tier permits;
anything the tier withholds is denied and surfaced.

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
Revive it with `:plugin-reload` (or `r` in the `:plugins` view).

---

## Writing a plugin

**Scaffold one:** `lattice --scaffold-plugin <name>` writes a complete, buildable
plugin project into `~/.config/lattice/plugins/<name>/` — a grammar action, a
minor mode (`<name>-mode`) that binds a key to it, the `<name>.enabled` gate, and
a `wit/` copy of the editor's API. Build it (`cargo build --target wasm32-wasip2`,
then copy the component in as `<name>.wasm`) and it's discovered at boot with its
mode on by default. The command prints the exact steps; the name must be lowercase
kebab-case.

For the toolchain (`wasm32-wasip2`, `wit-bindgen`), a seam-by-seam walkthrough,
the manifest format, fuel budgets, and a worked `fuzzy-finder` example, see the
**[plugin authoring guide](../dev/guides/plugin-authoring.md)**.

## See also

- [Options / configuration](options.md) — the typed-option system plugins register into (and where `plugin.trace-level` lives).
- [Modes](modes.md) — major/minor modes, shipping as components.
- [Completion](completion.md) and [LSP](lsp.md) — seams a completion-source plugin mirrors.
