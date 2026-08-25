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
> **Two *core plugins* ship today** — `auto-pair` and `treesitter-context`:
> prebuilt, discovered at boot, on by default, and each carrying its own
> `:help` page inside its component (see
> [`core-plugins`](help:core-plugins)). The frontier is more core plugins
> (`git-gutter`, rainbow-delimiters).

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
  disable them, you don't install them. See **[`core-plugins`](help:core-plugins)**.
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

> **Headless / CI boots.** Set `LATTICE_DISABLE_PLUGIN_AUTOLOAD=1` to skip the
> boot-time filesystem discovery (core plugins, `init.rs`, and the on-disk
> plugins directory). The loader and its `:plugin-load` / `:plugin-unload` /
> `:plugin-reload` commands stay available — only automatic startup discovery is
> suppressed — so a scripted or test run boots deterministically without picking
> up whatever is installed under `~/.config/lattice`.

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
| **grammar** | Register new vim motions, operators, text objects, actions, and ex-commands — extending the modal grammar itself. This is the one seam that runs *synchronously* on a keystroke (so an operator can compose with a plugin motion), under a strict sub-frame budget. A plugin's ex-commands become first-class the moment it loads: they appear in `:`-line `<Tab>` completion, `:describe-command`, `:apropos`, and `:list-commands` (and disappear when it unloads). |
| **events / hooks** | Subscribe to typed editor events (the unified autocmd/hook bus) and react off the hot path. |
| **decorations** | Produce gutter/line decorations as an off-render producer (the `git-gutter`-style seam). |
| **config** | Register typed options into the same registry `:set` reads, auto-namespaced by the plugin's id. |
| **theme** | Declare theme elements with default styles. A plugin's element lands in the same registry built-in ones do, so your colourscheme restyles it and `:customize` edits it. |
| **context** | Produce the sticky scope headers pinned above the text (what `treesitter-context` uses). |
| **error-parser** | Teach lattice to recognise diagnostics from a build tool it has never heard of — fed one line of compilation output at a time. |
| **plugin-manager** | Declare the plugins you want (`require`), which the editor then fetches, builds and loads. This is the `init.rs` seam; declaring software to install is a larger authority than setting an option, which is why it is its own entry in a manifest's `provides`. |
| **modes** | Declare a major/minor mode (kind, keymap, capabilities) that registers into the mode registry. The editor auto-generates a `:<mode-name>` toggle command for it (exactly like a built-in mode), and it shows in `:list-modes` / `:describe-mode`. |
| **keymap** | Bind user keys above the built-in grammar (the `init.rs` keybinding path). |
| **host-services** | Call back into the editor for capability-gated services (e.g. filesystem enumeration). |
| **project** | Ask which project a buffer or path belongs to, and where its root is. |
| **tree-sitter** | Query the parse tree of a buffer through a borrowed snapshot. |
| **help** | Ship its own `:help` pages. The markdown is compiled into the plugin's own `.wasm` (see below); the topic then opens, `<Tab>`-completes and cross-links exactly like a built-in doc. |
| **dashboard** | Add — or replace — a section on the `:dashboard` launch page, rendered from the live pane width, icon palette and editor version. |
| **language** | Ship a whole **language**: a tree-sitter grammar compiled to WebAssembly plus its highlight, fold, indent, injection and text-object queries. The language is then selected by file extension, highlighted, folded and reparsed exactly like a built-in one (see below). |
| **logging** | Emit the plugin's own log narrative into the boundary trace (Layer 2). |

Run `:describe-plugin-api <seam>` for the exact function signatures of any of
these; `:list-plugin-apis` lists them all, and `:export-plugin-api` dumps the
whole catalog as Markdown or JSON (useful for scaffolding).

That catalog is parsed from the WIT package at build time, so it can never
disagree with the actual API — if this table and `:list-plugin-apis` ever
differ, believe the command.

### A plugin can ship a whole language

Which languages the editor knows is **not** fixed when it is built. A plugin
that provides the `language` seam hands over a grammar and its queries once,
at load, and from then on the language behaves like any built-in one — `.org`
files select it, headings highlight, sections fold, incremental reparse works,
and unloading the plugin takes the language with it.

```rust
register_language(&LanguageSpec {
    name: "org".into(),
    extensions: vec!["org".into(), "org_archive".into()],
    grammar: GRAMMAR.to_vec(),                 // wasm, baked in
    highlights: Some(include_str!("../queries/highlights.scm").into()),
    folds: Some(include_str!("../queries/folds.scm").into()),
    ..Default::default()
});
```

**The non-obvious half is the grammar.** It has to be a tree-sitter grammar
compiled to WebAssembly, and it is *your* build artefact, not the editor's:

```sh
tree-sitter build --wasm            # the upstream tool (needs emscripten or docker)
scripts/build-wasm-grammar.sh org path/to/grammar/src   # lattice's, needs only clang + rustup
```

Then `include_bytes!` the result, exactly as `help` bakes in its markdown. The
grammar travels with the plugin and disappears with it.

Three rules worth knowing before you hit them:

- **Queries compile when the plugin loads, not on first use.** A typo in
  `folds.scm` fails the language at load with the offending file named —
  rather than quietly meaning "folding does nothing in org files", which is
  indistinguishable from the feature not existing.
- **A failed language costs only itself.** A bad grammar or query leaves the
  rest of your plugin — and your other languages — registered and working.
- **You cannot replace a built-in language.** Registering the name `rust` is
  refused. Claiming a built-in *extension* is allowed but never wins: the
  built-in table is consulted first.

If your grammar's entry point does not match the language name — lattice's own
`sql` rides the `tree-sitter-sequel` grammar, which exports
`tree_sitter_sequel` — set `grammar-name` to the grammar's name and leave
`name` as what users type.

[`lattice-org-plugin`](https://github.com/dhruvasagar/lattice-org-plugin) in the lattice repo is a complete worked example.

### A plugin's docs live inside the plugin

The `help` seam has one non-obvious rule: **there is no docs directory.** A
plugin's markdown is compiled into its own component with `include_str!` and
handed to the editor once, at load.

```rust
fn register_help_topics() {
    // `:help fugitive` — an empty name means the bare plugin id, so a
    // one-page plugin does not answer to `fugitive.fugitive`.
    let _ = register_topic("", "Git from inside lattice.",
                           include_str!("../doc/index.md"), &[]);
    // `:help fugitive.status`
    let _ = register_topic("status", "The status buffer.",
                           include_str!("../doc/status.md"), &[]);
}
```

Two consequences worth knowing before you write the first page:

- **Topic names are namespaced for you.** The editor prefixes every name with
  your plugin's id, using the id it read from your manifest rather than
  anything you pass. You cannot shadow a built-in page or collide with another
  plugin, and you do not have to think about it.
- **Docs have the plugin's lifetime.** Uninstalling removes the pages; a
  plugin that failed to load leaves none behind. This is the reason the docs
  travel *in* the artefact instead of being copied into a shared directory at
  install time.

The `related-commands` argument takes substring patterns matched against
command names — `:describe-command` uses them to emit a *See also* link back to
your page, the same way a built-in doc's frontmatter does.

### Dashboard sections are rendered, not stored

The `dashboard` seam works the other way round from `help`, and the difference
matters when you write one. You declare your section ids once at load, and the
editor calls you back **every time the page composes** — passing the current
pane width, whether Nerd Font glyphs are available, and the editor version. So
a section can show something live (recent projects, repository state) rather
than a frozen block of text.

Because it runs while the page is being built, a section should return
promptly; the editor budgets the call and a section that overruns or crashes
simply renders nothing while the rest of the page composes normally.

Section ids are **not** namespaced, deliberately: registering `getting-started`
replaces the built-in section of that name. That is a supported thing to do —
and unloading your plugin puts the original back.

If your section draws icons, honour the `nerd-fonts` flag and keep both
glyphs the same display width, or the page's columns will shift when the user
toggles `ui.nerd_fonts`.

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
| `fs:read:<path>` / `fs:write:<path>` | Read/write access under a specific path prefix only. `fs:write` also authorises a plugin to **file text into another buffer** — see below. |
| `net:http:<host>` | HTTP to a named host (serviced through a gated host-service, not raw sockets). |
| `proc:spawn` | Spawn a subprocess — **bundled plugins only**. |
| editor caps (`lsp`, `tree-sitter`, `folds`, `diagnostics`, `writable`, `buffer-uri`) | Access to specific editor subsystems, for plugin-declared modes. |

**A plugin can file text into a file you have not opened**, if it holds
`fs:write` over that path. This is what an org plugin's archive, refile and
capture commands do: take a subtree from where you are and put it somewhere
else.

Two things are worth knowing about how that works, because they are what stop
it being alarming:

- **It edits a buffer, not the disk.** Lattice opens the target (reusing the
  buffer if you already have it open, so your unsaved changes are what the
  write lands on), inserts, and leaves it **modified and unsaved**. It appears
  in `:ls`, `u` undoes it, and nothing reaches your disk until you `:w`. A
  plugin writing files behind your back is a larger authority than this, and
  lattice does not grant it.
- **The path is checked against the grant, not trusted.** A plugin granted
  `~/org` cannot write outside it, including by spelling the path with `..`;
  the attempt is refused and reported. A plugin with only `fs:read` over a
  directory cannot write into it — reading a tree and changing it are separate
  permissions.

The write also never steals focus: your cursor stays where it was.

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
mode on by default — toggle it any time with `:<name>-mode`. The command prints the
exact steps; the name must be lowercase kebab-case.

**Options are auto-namespaced.** A plugin registers config options with SHORT
names (`register_option("style", …)`); the host prefixes them with the plugin id,
so they land as `<id>.style` (`auto-pair.style`) and no two plugins can collide in
the global option namespace. `get`/`set-option` resolve the same way — a plugin
reads its own options by short name, and users set them by the full `auto-pair.style`.

For the toolchain (`wasm32-wasip2`, `wit-bindgen`), a seam-by-seam walkthrough,
the manifest format, fuel budgets, and a worked `fuzzy-finder` example, see the
**[plugin authoring guide](../dev/guides/plugin-authoring.md)**.

## See also

- [Options / configuration](help:options) — the typed-option system plugins register into (and where `plugin.trace-level` lives).
- [Modes](help:modes) — major/minor modes, shipping as components.
- [Completion](help:completion) and [LSP](help:lsp) — seams a completion-source plugin mirrors.
