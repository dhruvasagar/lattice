+++
title = "Configuration & init lifecycle"
+++



How a user's configuration reaches the editor, and — the load-bearing part —
how config that targets a **plugin not yet loaded** still applies at the right
time. This fragment settles the init.rs lifecycle, the plugin-lifecycle event, and
the available-vs-enabled minor-mode model as one coherent story.

> **Status:** design settled (2026-07-19, with Dhruva). Supersedes the current
> implementation, where `init.rs` loads *async, after* other plugins like any
> discovered plugin. Slice plan: [`../operations/slice-plans/archive/config-and-init.md`](../operations/slice-plans/archive/config-and-init).

## 1. Why — two problems

**P1 — Config authority + lifecycle.** `init.rs` is the user's config, not a
third-party extension. Loading it through the async plugin-discovery path (after
boot, after other plugins) means its config races a live editor — a mode
activates a frame late, an option flips after a subsystem already read it. The
re-activation complexity this forces is a *symptom*.

**P2 — Config precedes the plugin it configures.** The vim/emacs muscle memory is
to configure a plugin *before* it loads — `let g:plugin_x = 1` / `(setq …)` /
`(with-eval-after-load 'pkg …)`. An **imperative** call (`enable-mode("auto-pair-mode")`)
has nowhere to land if auto-pair isn't loaded yet. This is the crux; it defeats a
"init.rs pokes live state" model.

## 2. The model — init-first, event-deferred, reactive

`init.rs` loads **first** (before any other plugin), does **immediate** config for
targets that exist (native options, builtin keymaps, custom commands), and
registers **deferred** handlers for targets that don't exist yet — keyed to a
typed **plugin-load event**. When a plugin finishes loading, its `plugin-loaded`
event fires and the matching handlers run, calling the plugin's now-present APIs.

```
[core services up] → [init.rs LOADS FIRST]                → [first render]
                      · immediate: set-option, keymaps         │
                      · deferred:  on-plugin-loaded(name) ──┐   │
                                                            │   ▼
[plugins load ASYNC, off the boot thread, never blocking] ─┘  each finishes →
      auto-pair, git-gutter, …                                 Event::PluginLoaded{name}
                                                                     │
                                                                     ▼
                                              matching deferred handler runs →
                                              enable-mode / set-option / bind-key
```

This is `with-eval-after-load` / vim's lazy autocmd, built on the event bus the
design already unifies (§5.10, "hooks ≡ autocmds ≡ typed event subscriptions").
The handler **calls APIs, never `:` command strings** (`event-handlers-call-apis-not-commands`).

> **Diagram names are conceptual — the literal API is the seam modules.** Above,
> `set-option` / `enable-mode` / `on-plugin-loaded` name the *operations*, not
> function names. In real `init.rs`: immediate config is `config::set_option(…)`
> in the `register_options` export; a deferred handler is a `PluginLoaded`
> subscription in `register_events` + a `match` in `on_event` calling
> `modes::enable_mode(…)` / `config::set_option(…)`. See §6's example and
> [`../../user/init.md`](../../../docs/init).

**Why this is the right shape:**
- **Async plugin loading is preserved** — plugins still load off the boot thread;
  `init.rs` registered handlers and returned. The first frame is never blocked on
  a plugin's cold-start (the plugins.md principle survives).
- **P2 dissolves** — the handler runs *when the plugin loads*, so every call hits a
  present target. No pending-value store, no adoption-on-register.
- **Graceful by construction** — a plugin that fails or isn't installed never fires
  `plugin-loaded`, so its handler never runs. No error path to write.
- **init.rs is clean** — immediate config at top level, deferred config in
  `on-plugin-loaded` handlers. The emacs init.el shape exactly.

## 3. Boot lifecycle + ordering (the one constraint)

`init.rs` MUST load — and its subscriptions register — **before** any other plugin
fires `plugin-loaded`, or a handler could miss its event. So the loader install
sequence becomes:

1. Core services up (event bus, the option / mode / command / keymap registries).
2. Apply `lattice.toml` (static values) into the config registry.
3. **Load `init.rs`** (`<config>/lattice/init/`) — runs its top-level, registering
   immediate config + `plugin-loaded` subscriptions. Awaited before step 4.
4. **Discover + load plugins** (`<config>/lattice/plugins/`) async — each fires
   `plugin-loaded` on completion → the deferred handlers run.

`init.rs` still loads before the first render for its *immediate* config; its
*deferred* (plugin) config lands as each plugin's cold-start completes — an
**eventual-consistency** window (a frame or two, CLAUDE.md-sanctioned: "absent
then present", never a flicker of *wrong* state).

## 4. Plugin-lifecycle events (the new primitive)

`Event::PluginLoaded { name, id }` and `Event::PluginUnloaded { name, id }` —
mirroring the existing `Event::PluginCrashed` (enum variant + `EventKind` + the
WIT `event` / `event-kind` mirror + a `plugin-loaded` events-seam subscription).

**Contract:** `plugin-loaded` fires **after a plugin's full drain** — every seam
registered (its modes, options, commands all exist) — so a handler observes a
*fully-loaded* plugin. Published by the loader at `load_discovered` completion.
`plugin-unloaded` fires after teardown reverses a plugin's contributions (the
`:plugin-unload` / crash-teardown path), so a handler can tear down its own
dependent setup.

A general primitive: lazy-loading, per-plugin setup, and plugin dependency
ordering all ride it later — not auto-pair scaffolding.

## 5. Config front-ends + read discipline

`lattice.toml` (static option overrides, declarative) and `init.rs` (programmatic:
logic, loops, conditionals, keymaps, custom commands, deferred handlers) are two
front-ends over the SAME config registry — the "static settings stay declarative,
logic stays code, one toolchain" split (CLAUDE.md). TOML covers what it can
express; `init.rs` is the escape hatch. Both **set** option values through the
same path: TOML at boot, `init.rs` via the config-seam **`set-option`** (CI.7),
which wraps the `:set name=value` machinery (coerce + validate + `OptionChanged`).
Without `set-option`, `init.rs` could only *declare* options, not override values
— so it's load-bearing for the "two value-setting front-ends" claim.

**Read discipline (the plugin contract):** a config consumer reads its options at
**use time**, or **subscribes** to `OptionChanged` — it does NOT cache-at-load.
Because config may be applied around or after a plugin loads (a `plugin-loaded`
handler runs just after the plugin registered its options), a cache-at-load
consumer would miss it. auto-pair already complies (it reads `auto-pair.style`
per keystroke, not at load).

## 6. Minor-mode enablement — available vs enabled

The user, not the plugin author, owns which extension modes are live (the emacs
global-minor-mode model: a package *provides* `electric-pair-mode`; you *enable*
it). Today a plugin declaring `ActivationPolicy::Global` auto-activates on every
document buffer with no consent — wrong for a third-party contribution.

**Declaration ≠ enablement:**
- A plugin-declared minor mode is **registered but inert**. `ActivationPolicy` now
  describes only its *eligible scope* (where it applies *when on*), not whether it
  self-activates. `auto_activatable_minors` returns a minor iff
  `admits(major, kind) AND enabled(id)`.
- **Native modes are unchanged** — `ModeRegistry::register` auto-enables them
  (snippet / LSP / help stay first-party, intrinsic, on). A new
  `register_available` (used by `register_plugin_mode`) registers **without**
  enabling — so plugin modes are available-but-off.
- **`enable-mode(id)` / `disable-mode(id)`** (modes seam) flip enablement (RCU on
  the mode-registry handle). Called from an `on-plugin-loaded` handler in init.rs,
  or a future `:mode-enable`.
- **Re-activation:** because plugins load async *after* render, enabling a mode
  must activate it on **buffers already open** — bounded (O(open buffers) whose
  major admits it). The one piece of real machinery here; legitimate anyway
  (`:reload-config`, `:mode-enable` need it too).

> **Superseded for core plugins (2026-07-20, PM.3/PM.4).** auto-pair is now a
> **core plugin**: it ships prebuilt, is discovered at boot, and is enabled by a
> config gate — its manifest declares `default_mode = "auto-pair-mode"`, and the
> loader auto-registers `auto-pair.enabled` (default `true`) that turns the mode
> on out of the box. So auto-pair needs **no** `init.rs` to enable it; the user
> *configures* it (`config::set_option("auto-pair.enabled", "false")` /
> `"auto-pair.style"` in `register_options`) at the top level, and `:set
> auto-pair.enabled=false`
> toggles it live. Beyond the config gate, every registered plugin mode also gets
> the auto-generated `:<mode-name>` toggle ex-command (built by the shared
> `mode_toggle_ex_command_spec`, registered under `SourceLayer::Plugin(id)` by
> `drain_mode`), so `:auto-pair-mode` toggles it on the active buffer and it
> resolves in `:describe-command` / `:apropos` / `:`-completion exactly like a
> native mode. See [`plugin-manager.md`](../plugin-manager/) §7. The
> `on-plugin-loaded → enable-mode` pattern below remains the tool for **user**
> plugins with off-by-default modes (and for any deferred config of a
> not-yet-loaded plugin).

A **user** plugin that declares no default mode is available-but-off; the user's
`init.rs` turns it on when it loads. There is **no `setup()` / `on_plugin_loaded()`
/ `set_option()`** — those are the *model*; the real API is the wit-bindgen `Guest`
trait (`register_events` / `on_event`) plus the seam modules (`modes::enable_mode`,
`config::set_option`). You subscribe to `PluginLoaded` in `register_events` and
react in `on_event`:

```rust
// init.rs — a combined world providing `events`, importing `modes` + `config`.
use lattice::plugin_host::types::{Event, EventFilter, EventKind};
use lattice::plugin_host::{config, events, modes};

impl Guest for Component {
    fn register_events() {
        events::subscribe(
            &EventFilter {
                kinds: Some(vec![EventKind::PluginLoaded]),
                path_globs: None,
                major_modes: None,
            },
            1, // your handler id
        );
    }

    fn on_event(handler: u32, ev: Event) {
        if let (1, Event::PluginLoaded(p)) = (handler, ev) {
            if p.name == "my-linter" {
                modes::enable_mode("my-linter-mode");
                config::set_option("my-linter.strict", "true"); // optional
            }
        }
    }
}
```

The full annotated shape (imports, `plugin.toml`, the combined world) is in the
user guide, [`../../user/init.md`](../../../docs/init); `lattice --scaffold-init`
writes a compilable starter.

## 7. Paramount-goal alignment

- **#2 Extensibility** — init.rs (and any plugin) reacts to any plugin's lifecycle
  via typed events; configuring a plugin never requires it present when init.rs
  runs.
- **#4 Async** — async plugin loading preserved (no boot block); the event bus is
  the async coupling; eventual-consistency is the accepted, bounded compromise.
- **#1 Performance** — init.rs registers handlers and returns; zero per-frame /
  per-keystroke cost; deferred config runs off the hot path.
- **UX (higher court)** — matches vim/emacs muscle memory; no surprise
  auto-activation of third-party behavior; "absent then present", never wrong.

## 8. Rejected alternatives

- **Pending-value config store** (buffer config for not-yet-registered options,
  adopt on register) — rejected: the event-deferred handler achieves the same
  *and* covers imperative refs (keymaps, commands), not just option values, with
  no persistent unclaimed-values layer. The store solves a more general problem we
  don't have; premature (heuristic #1).
- **Blocking boot** (load all bundled plugins + init.rs before first render so
  every reference resolves) — rejected: sacrifices fast startup and violates "no
  plugin cold-start delays startup". The event model keeps async loading.
- **init.rs as a normal async-discovered plugin** (loaded after other plugins) —
  rejected: its subscriptions must precede the events it reacts to, so it loads
  first; and it's config authority, not a discovered extension.
- **Imperative `enable-mode` at init.rs top level** (against a maybe-absent plugin)
  — rejected: the target may not exist; the deferred `on-plugin-loaded` handler is
  the fix.

## 9. API / seam surface touched

- **events** — new `plugin-loaded` (+ `plugin-unloaded`) `event-kind` / `event`
  arm; `init.rs` subscribes with a name filter.
- **modes** — `enable-mode(id)` / `disable-mode(id)`; `register_available`
  (host-side) for plugin modes.
- **config** — `set-option(name, value)` (CI.7): the value-setting front-end that
  makes `init.rs` symmetric with `lattice.toml`. Backed by the SAME
  `ConfigRegistry::parse_and_set_command` path `:set name=value` uses (type-coerce
  + validate + publish `OptionChanged`); `false` on unknown option / invalid value
  / no registry. Without it `init.rs` could only *declare* options, not set values
  — a crippled front-end; it's the missing half, not a bolt-on.
- **loader** — publishes `plugin-loaded` / `plugin-unloaded`; loads `init.rs`
  before plugin discovery.
- **world** — lattice ships no combined "init" world (a mega-world would force a
  user to implement every `register-*` export even for a keymap-only config). A
  multi-seam `init.rs` declares its OWN combined world listing exactly the seams
  it uses — the AP.1.0 multi-seam pattern; `provides` names the seams it
  contributes into (`config`/`keymap`/`events`/`grammar`), while a seam it only
  *calls* (`modes`, for `enable-mode`) is imported but not provided.
