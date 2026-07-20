---
summary: "Configuring lattice in Rust/WASM with init.rs: where it lives, how it loads and reloads, and worked examples for event handlers, custom grammar (commands / motions / text-objects / operators / ex-commands), keybindings, and options."
related: [init, config, configuration, plugin, wasm, autocmd, keymap, event, command, motion]
---

# Configuring lattice with `init.rs`

lattice has **one** extension substrate — WebAssembly. There is no vimscript, no
Lua, no elisp. Your configuration is a small Rust program, `init.rs`, compiled to
a `wasm32-wasip2` component and loaded at boot. It registers keymaps, custom
commands, new motions and text objects, options, and event handlers through the
*same* seams a plugin uses — your config is simply the first plugin the editor
loads.

TOML covers static option *values* (`ui.nerd_fonts = true`); anything
programmable — a keybind, a custom `:command`, a new motion, an event hook —
lives in `init.rs`. Static settings stay declarative; logic stays code; one
toolchain.

> **Prerequisite:** this is the how-to for *users*. The full plugin substrate
> (the capability/fuel/crash-isolation model, every seam, the introspection
> commands) is documented in [plugins.md](plugins.md) and the
> [authoring guide](../dev/guides/plugin-authoring.md). If you've written a
> plugin, `init.rs` is the same thing pointed at your config directory.

---

## Where it lives and how it loads

Your compiled config is a plugin directory at:

```
<config>/lattice/init/
├── plugin.toml      # the manifest (id = "init", provides = [...])
└── init.wasm        # your init.rs, compiled to a component
```

`<config>` is `~/.config` on **both Linux and macOS** (honoring
`$XDG_CONFIG_HOME`), and `%APPDATA%` on Windows — a consistent
`~/.config/lattice/` tree across Unix, not the macOS-native Application Support.
Your on-disk plugins live alongside it at `~/.config/lattice/plugins/`.

- **Loaded at boot**, *after* the built-in vim grammar and modes register (so your
  keymaps, commands, and options **layer on top of** the defaults — your
  `<leader>f` sits above the builtin grammar; your `:mycmd` joins the same command
  registry as `:write`) but *before* the plugins in `~/.config/lattice/plugins/`,
  so your `PluginLoaded` handlers are subscribed and ready when those plugins load
  (see [Configuring plugins that load after you](#configuring-plugins-that-load-after-you-pluginloaded)).
- **Loaded with boot capabilities** — `init.rs` is your own trusted config, so
  it gets the pre-granted (`Bundled`) trust tier, not the consent-prompted tier
  a downloaded plugin gets.
- **Reload without restarting** with `:reload-config` — it unloads the old
  config (reversing every keymap, command, option, and subscription it added)
  and re-instantiates `init.wasm` from disk with a fresh, clean sandbox. Edit,
  rebuild, `:reload-config`, done.

An absent `<config>/lattice/init/` is the normal "no custom config" case — the
editor boots with defaults, silently.

## The model: config is a plugin, each capability is a *seam*

Every kind of configuration maps to a **seam** — a typed interface mirroring a
native editor subsystem:

| You want to… | Seam | WIT world you implement |
|---|---|---|
| bind a key (`<leader>f` → a command) | `keymap` | `keymap-plugin` |
| add a `:command`, motion, text object, operator | `grammar` | `grammar-plugin` |
| declare a typed option (`:set my.opt=…`) | `config` | `config-plugin` |
| observe editor events (save, mode change, …) | `events` | `events-plugin` |
| define a minor mode | `modes` | `modes-plugin` |

> **One component implements one seam world today.** A WebAssembly component has
> a single world type, so a single `init.wasm` targets one `wit_bindgen::generate!`
> world. If your config spans several seams (say, custom commands **and** event
> handlers), ship the extra concerns as additional plugins under
> `<data>/lattice/plugins/<name>/` — each a one-seam component — alongside the
> `init/` dir. A unified `init` world that lets one component register across
> every seam at once is planned; until then, one component = one seam.
>
> The good news: **commands, motions, text objects, operators, and ex-commands
> are all the _same_ seam** (`grammar`), so a single grammar component can define
> all of them together (the big example below).

`plugin.toml` declares which seam(s) the component provides:

```toml
id = "init"                 # required; the :reload-config target
provides = ["grammar"]      # the seam this component implements
doc = "My lattice config."
```

Everything else in the manifest (`capabilities`, `editor_capabilities`) is the
sandbox grant — see [the safety model](#the-safety-model) and
[plugins.md](plugins.md#the-security-model).

---

## Event handlers

**Observe editor state transitions** — a document saved, the modal mode changed,
a major mode entered. This is the `:autocmd` / `add-hook` equivalent, unified
into one typed event bus.

> **Handlers act via APIs, not command strings** — but only the APIs their
> **world imports**. A standalone `events-plugin` imports `events` + `host-services`
> + `logging`, so its handler can `walk` the fs, emit its own plugin event, or log
> — nothing more. To call `enable-mode` / `set-option` / `register-binding` from a
> handler, your world must *also* import `modes` / `config` / `keymap` — a
> **combined world** (see the [complete annotated example](#a-complete-annotated-initrs)).
> A handler never runs a `:` command string or returns an effect (the standing
> `event-handlers-call-apis-not-commands` rule: `:` commands are user-facing
> front-ends; a handler calls the underlying API). Direct **buffer mutation** and
> **before-class veto** (a handler that rewrites content or aborts a save) are
> still deferred.

You implement the `events-plugin` world: subscribe your handlers in
`register_events`, then dispatch them in `on_event`.

### The event catalog

Subscribe with an `EventFilter` — any combination of event **kinds**, path
**globs**, and major-**modes** (all `None` ⇒ every event):

| `EventKind` | Payload | Fires when |
|---|---|---|
| `DocumentOpened` | `{ id, path?, version, text }` | a buffer opens (path `None` for scratch) |
| `DocumentClosed` | `id` | a buffer closes |
| `BeforeSave` | `{ id, path }` | just before a write |
| `DocumentSaved` | `{ id, path }` | after a write |
| `DocumentChanged` | `{ id, path?, version }` | buffer content changed |
| `SelectionsChanged` | `{ … }` | cursor/selection moved |
| `ModalModeChanged` | `{ from_state, to_state }` | Normal↔Insert↔Visual↔… |
| `BeforeQuit` | — | `:q` on the last window |
| `OptionChanged` | `{ name, old?, new_value }` | a `:set` landed |
| `MajorEntered` / `MajorExiting` | `{ buffer, mode }` | major mode lifecycle |
| `MinorActivated` / `MinorDeactivated` | `{ buffer, mode }` | minor mode lifecycle |
| `PluginLoaded` / `PluginUnloaded` | `{ name, id }` | a plugin finished loading / was unloaded — the hook for **deferred plugin config** (see below) |
| `Plugin` | `{ name, payload }` | a plugin-defined event (filter by `name` yourself) |

### A worked event-handler config

```rust
// init.rs — an events-plugin. plugin.toml: provides = ["events"]
wit_bindgen::generate!({ world: "events-plugin", path: "../../wit" });

use lattice::plugin_host::events;
use lattice::plugin_host::host_services;
use lattice::plugin_host::types::{EventFilter, EventKind};

struct Component;

// Handler ids are yours to choose — they route deliveries in `on_event`. Give
// them names so the dispatch reads clearly.
const ON_SAVE: u32 = 1;
const ON_MODE: u32 = 2;
const ON_RUST_OPEN: u32 = 3;

impl Guest for Component {
    // Called once at load. Subscribe each handler with a filter.
    fn register_events() {
        // Every save, any file.
        events::subscribe(
            &EventFilter { kinds: Some(vec![EventKind::DocumentSaved]), path_globs: None, major_modes: None },
            ON_SAVE,
        );
        // Modal transitions (Normal↔Insert↔…).
        events::subscribe(
            &EventFilter { kinds: Some(vec![EventKind::ModalModeChanged]), path_globs: None, major_modes: None },
            ON_MODE,
        );
        // A document opening, narrowed to Rust files by a path glob.
        events::subscribe(
            &EventFilter {
                kinds: Some(vec![EventKind::DocumentOpened]),
                path_globs: Some(vec!["**/*.rs".into()]),
                major_modes: None,
            },
            ON_RUST_OPEN,
        );

        // Declare a plugin-defined event other plugins can observe (optional).
        host_services::register_event("myconfig.saved", "Fired after every save.");
    }

    // Deliver one matching event. Dispatch on the handler id you registered.
    fn on_event(handler: u32, ev: Event) {
        match handler {
            ON_SAVE => {
                if let Event::DocumentSaved(doc) = ev {
                    log(&format!("saved {}", doc.path));
                    // Fan out a plugin event carrying the path (any consumer of
                    // "myconfig.saved" — another plugin — receives it).
                    host_services::emit_event("myconfig.saved", doc.path.as_bytes());
                }
            }
            ON_MODE => {
                if let Event::ModalModeChanged(m) = ev {
                    log(&format!("mode {} -> {}", m.from_state, m.to_state));
                }
            }
            ON_RUST_OPEN => {
                if let Event::DocumentOpened(doc) = ev {
                    log(&format!("opened rust file (version {})", doc.version));
                }
            }
            _ => {}
        }
    }
}

// The only side effect a handler can have without extra capabilities: write to
// its own private, always-writable `/data` dir. (An `fs:read:<prefix>` grant in
// plugin.toml also lets a handler read outside it via the gated `walk`.)
fn log(line: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/data/events.log") {
        let _ = writeln!(f, "{line}");
    }
}

export!(Component);
```

A handler that traps (panics, overruns its fuel budget) is **quarantined** — the
host logs it, skips that one delivery, and every other subscriber is untouched.
Return early on the arms you don't handle; never panic to signal "not for me."

### Core plugins: configure, don't enable

**Core plugins ship with lattice and are on by default** — you don't enable them.
`auto-pair` is a core plugin: `auto-pairs-mode` is active out of the box, gated by
a bool option `auto-pair.enabled` (default `true`). To configure or disable it,
set options at the **top level** of `init.rs` — the option exists as soon as the
core plugin loads (before your other config runs):

```rust
fn setup() {
    // auto-pair is ON by default. Turn it off:
    config::set_option("auto-pair.enabled", "false");

    // …or keep it on and switch to the manual close-key style:
    config::set_option("auto-pair.style", "manual");
    config::set_option("auto-pair.close-key", "<C-l>");
}
```

You can equally set these in `lattice.toml` (`auto-pair.enabled = false`) or live
with `:set`. See [core-plugins.md](core-plugins.md) for the full list and each
plugin's options.

### Configuring *user* plugins that load after you: `PluginLoaded`

Your `init.rs` loads **first** — before the *user* plugins in
`~/.config/lattice/plugins/`. So config that targets a user plugin (enable its
mode, set its options, bind keys to its commands) can't run at the top level: the
plugin isn't there yet. Instead, **subscribe to `PluginLoaded` and react when it
arrives** — the `with-eval-after-load` / lazy-autocmd pattern:

```rust
fn on_event(_handler: u32, ev: Event) {
    if let Event::PluginLoaded(p) = ev {
        match p.name.as_str() {
            // Enable a USER plugin's minor mode the moment it loads.
            "my-plugin" => {
                modes::enable_mode("my-plugin-mode");
                config::set_option("my-plugin.option", "value");
            }
            _ => {}
        }
    }
}
```

Why this shape, not a top-level `enable_mode(...)`:

- **User-plugin minor modes are available-but-off.** A user plugin *provides* a
  mode; **you** enable it — the plugin author doesn't turn it on for you (the emacs
  global-minor-mode model). `enable-mode` flips it on globally and activates it on
  your open buffers immediately. (Core plugins differ: their `<id>.enabled` gate
  turns their default mode on for you — see above.)
- **The target must exist when you configure it.** `PluginLoaded` fires *after*
  the plugin's seams all registered, so `enable-mode` / `set-option` always hit a
  present target.
- **Graceful by construction.** If a plugin never loads (not installed, failed to
  compile), its `PluginLoaded` never fires and your handler never runs — no error,
  no guard to write. Config for a plugin you removed simply goes dormant.

Plugins still load asynchronously (they never block startup), so an enabled mode
becomes active a frame or two after that plugin's cold-start finishes — the same
"absent, then present" you feel on emacs/vim startup, never a flicker of wrong
state.

---

## Custom grammar: commands, motions, text objects, operators, ex-commands

The `grammar` seam is where you *extend the editing language itself*. A motion
you add composes with operators (`d` + your-motion), repeats with counts, and is
recordable in macros — because a plugin-contributed motion is registered into the
*same* `CommandRegistry` a builtin lives in and is indistinguishable to the
dispatcher.

You implement two halves of the `grammar-plugin` world:

- `register_grammar` — declare each contribution (name, doc, spec) with a
  **callback id** you choose;
- the `grammar-callbacks` exports — the behavior, dispatched by that callback id.

> **Grammar is the one _synchronous_ seam.** A motion/operator/text-object
> `apply` runs *on the keystroke* (so `d{motion}` stays atomic and dot-repeat
> works). Keep it cheap arithmetic over the projected context — it runs under a
> tight fuel budget, and a runaway call is trapped. No I/O, no long loops.

### One config with several grammar contributions

```rust
// init.rs — a grammar-plugin. plugin.toml: provides = ["grammar"]
wit_bindgen::generate!({ world: "grammar-plugin", path: "../../wit" });

use exports::lattice::plugin_host::grammar_callbacks::Guest as Callbacks;
use lattice::plugin_host::grammar;
use lattice::plugin_host::types::{
    ActionContext, Args, EchoLevel, EchoPayload, Effect, ExCommandContext, LatencyClass,
    MotionContext, MotionResult, MotionSpec, OperatorContext, Position, Range, SurfaceForm,
    TextObjectContext, TextObjectSpec, OperatorSpec, ActionSpec, ExCommandSpec,
};

struct Component;

// Callback ids — your own dispatch keys, one per contribution.
const MOT_DOWN5: u32 = 1;
const TXO_TO_BOL: u32 = 2;
const OP_SHOUT: u32 = 3;
const ACT_HELLO: u32 = 4;
const EXC_GREET_PARSE: u32 = 5;
const EXC_GREET_APPLY: u32 = 6;

impl Guest for Component {
    fn register_grammar() {
        // 1. A MOTION: `gld` jumps 5 lines down. Composes with operators — `dgld`
        //    deletes 5 lines. `jump: true` records it in the jump list.
        grammar::register_motion(
            "leap-down",
            "Jump five lines down",
            &MotionSpec { jump: true, exclusive: false, args_schema: Vec::new() },
            MOT_DOWN5,
        );

        // 2. A TEXT OBJECT: "to beginning of line" — usable as an operator target
        //    (`d{txo}`), resolves to a range the operator consumes.
        grammar::register_text_object(
            "to-bol",
            "From the cursor back to the start of the line",
            &TextObjectSpec { args_schema: Vec::new() },
            TXO_TO_BOL,
        );

        // 3. An OPERATOR: acts on a range (from a following motion/text object)
        //    and returns Effects. `repeatable: true` ⇒ dot-repeatable.
        grammar::register_operator(
            "shout",
            "Uppercase the operated range (demo: echoes instead)",
            &OperatorSpec { repeatable: true, args_schema: Vec::new(), blockwise_per_row: false },
            OP_SHOUT,
        );

        // 4. An ACTION: a standalone command bound to a chord/`:` with no motion —
        //    it just runs and returns Effects.
        grammar::register_action(
            "say-hello",
            "Echo a greeting",
            &ActionSpec { args_schema: Vec::new() },
            ACT_HELLO,
        );

        // 5. An EX-COMMAND: `:greet <name>` — two callbacks, one to parse the
        //    argument string into typed Args, one to apply.
        grammar::register_ex_command(
            "greet",
            "Greet someone: :greet <name>",
            &ExCommandSpec {
                latency_class: LatencyClass::Reflex,
                accepts_bang: false,
                accepts_range: false,
                args_schema: Vec::new(),
                surface_form: SurfaceForm::Keyword,
            },
            EXC_GREET_PARSE,
            EXC_GREET_APPLY,
        );
    }
}

impl Callbacks for Component {
    // A motion returns the target position (+ whether it's linewise). Pure
    // arithmetic over the projected context — cursor is `ctx.from`, count is
    // `ctx.count` (honor `2gld` → 10 lines).
    fn apply_motion(callback: u32, ctx: MotionContext) -> Result<MotionResult, String> {
        match callback {
            MOT_DOWN5 => Ok(MotionResult {
                target: Position { line: ctx.from.line + 5 * ctx.count.max(1), byte: 0 },
                linewise: true,
            }),
            other => Err(format!("no motion {other}")),
        }
    }

    // A text object returns the range it resolved (here: line start → cursor).
    fn apply_text_object(callback: u32, ctx: TextObjectContext) -> Result<Range, String> {
        match callback {
            TXO_TO_BOL => Ok(Range {
                start: Position { line: ctx.at.line, byte: 0 },
                end: ctx.at,
            }),
            other => Err(format!("no text object {other}")),
        }
    }

    // An operator receives the resolved `ctx.range` and returns Effects. (This
    // demo just echoes; a real one would emit edit Effects over the range.)
    fn apply_operator(callback: u32, ctx: OperatorContext) -> Result<Vec<Effect>, String> {
        match callback {
            OP_SHOUT => Ok(vec![Effect::Echo(EchoPayload {
                level: EchoLevel::Info,
                text: format!("SHOUT over {} lines", ctx.range.end.line - ctx.range.start.line + 1),
            })]),
            other => Err(format!("no operator {other}")),
        }
    }

    fn apply_action(callback: u32, _ctx: ActionContext) -> Result<Vec<Effect>, String> {
        match callback {
            ACT_HELLO => Ok(vec![Effect::Echo(EchoPayload {
                level: EchoLevel::Info,
                text: "hello from init.rs".into(),
            })]),
            other => Err(format!("no action {other}")),
        }
    }

    // Parse `:greet <rest>` — the raw string after the command word — into Args.
    fn parse_ex_args(callback: u32, rest: String, _bang: bool) -> Result<Args, String> {
        match callback {
            EXC_GREET_PARSE => Ok(Args::String(rest.trim().to_string())),
            other => Err(format!("no parser {other}")),
        }
    }

    // Apply `:greet` — read the parsed Args from the context, return Effects.
    fn apply_ex_command(callback: u32, ctx: ExCommandContext) -> Result<Vec<Effect>, String> {
        match callback {
            EXC_GREET_APPLY => {
                let who = match ctx.args { Args::String(s) if !s.is_empty() => s, _ => "world".into() };
                Ok(vec![Effect::Echo(EchoPayload { level: EchoLevel::Info, text: format!("Hello, {who}!") })])
            }
            other => Err(format!("no ex-command {other}")),
        }
    }
}

export!(Component);
```

An `apply` that returns `Err(..)` is a graceful no-op (logged, the dispatcher
commits nothing) — distinct from a trap. Return errors as values.

Bind your new motion/action to a key with a companion `keymap` config (next), or
invoke a command directly (`:greet Dhruva`).

---

## Keybindings

The `keymap` seam binds a chord to an **existing** command (a builtin, or one
your grammar config added), landing in the user keymap layer — above the builtin
vim grammar, so your bindings win.

```rust
// init.rs — a keymap-plugin. plugin.toml: provides = ["keymap"]
wit_bindgen::generate!({ world: "keymap-plugin", path: "../../wit" });

use lattice::plugin_host::keymap;
use lattice::plugin_host::keymap::BindingMode;

struct Component;

impl Guest for Component {
    fn register_keymap() {
        // <C-s> in Normal → :write. The command is named by its registry name.
        keymap::register_binding(BindingMode::Normal, "<C-s>", "ex:write");
        // <leader>g → the :greet command a grammar config added.
        keymap::register_binding(BindingMode::Normal, "<leader>g", "greet");
    }
}

export!(Component);
```

A binding to an unregistered command, or an unparseable chord, is skipped
(logged) — it binds nothing rather than mis-binding.

## Options

The `config` seam does two things: **`register-option`** declares a *new* typed
option into the *same* registry `:set`, `:describe-option`, and `:customize` read
(no special-casing), and **`set-option(name, value)`** overrides an *existing*
option's value — exactly what `:set name=value` does (the value is type-coerced +
validated, and `OptionChanged` fires). So `init.rs` is a value-setting config
front-end alongside `lattice.toml`: `lattice.toml` for static values, `init.rs`
for values you compute or set conditionally (per filetype, on a plugin loading).
`set-option` returns `false` (a logged no-op) for an unknown option or an invalid
value — it never mis-sets.

```rust
// init.rs — a config-plugin. plugin.toml: provides = ["config"]
wit_bindgen::generate!({ world: "config-plugin", path: "../../wit" });

use lattice::plugin_host::config;
use lattice::plugin_host::config::OptionType;

struct Component;

impl Guest for Component {
    fn register_options() {
        // Now `:set myconfig.greeting=hi` works, and :describe-option shows it.
        config::register_option("myconfig.greeting", OptionType::String, "hello", "Default greeting.");
        config::register_option("myconfig.max-items", OptionType::Integer, "50", "Cap on items.");
    }
}

export!(Component);
```

---

## A complete annotated `init.rs`

A realistic config that uses several seams at once. Because it's multi-seam, you
declare **one combined world** with the seams you use (the same pattern a bundled
plugin like `auto-pair` uses) — put this in your own `wit/init.wit`:

```wit
// wit/init.wit
package my:init;
world init {
    // Seams you contribute into (you implement their register-* export):
    export register-options: func();   // config
    export register-keymap: func();    // keymap
    export register-events: func();    // events
    export on-event: func(handler: u32, ev: event);
    // Host functions you call — `config`/`keymap`/`events` you both provide AND
    // call; `modes` you only CALL (`enable-mode`), so it's imported, not provided:
    import config; import keymap; import events; import modes;
    use lattice:plugin-host/types.{event};
}
```

`plugin.toml`: `provides = ["config", "keymap", "events"]` — the seams you
register into. (`modes` is *not* listed: you don't declare a mode, you only call
`enable-mode` on one another plugin declared.) Read the guest top to bottom —
**immediate** config runs when `init.rs` loads; **deferred** config runs from the
`PluginLoaded` handler.

```rust
wit_bindgen::generate!({ world: "init", path: "wit" });

use lattice::plugin_host::keymap::BindingMode;
use lattice::plugin_host::types::{Event, EventFilter, EventKind};
use lattice::plugin_host::{config, events, keymap, modes};

struct Component;

impl Guest for Component {
    // ── IMMEDIATE: override option values (the config front-end; also settable
    //    in lattice.toml, but here you might compute them) ──────────────────────
    fn register_options() {
        config::set_option("tabstop", "4");
        config::set_option("plugin.trace-level", "info");
        // CORE plugins (auto-pair) are on by default — configure them here at the
        // top level; their options exist as soon as the core plugin loads.
        config::set_option("auto-pair.style", "manual"); // manual close-key pairing
        // config::set_option("auto-pair.enabled", "false"); // …or turn it off
        // set-option returns false (a logged no-op) for an unknown option or an
        // invalid value — it never mis-sets.
    }

    // ── IMMEDIATE: keybindings above the builtin grammar ─────────────────────
    fn register_keymap() {
        // <C-s> in Normal → :write ; gd → an LSP command (if lsp is loaded).
        keymap::register_binding(BindingMode::Normal, "<C-s>", "ex:write");
        keymap::register_binding(BindingMode::Normal, "gd", "lsp-definition");
    }

    // ── Subscribe the DEFERRED + event-flow hooks ────────────────────────────
    fn register_events() {
        events::subscribe(&kind(EventKind::PluginLoaded), 1); // deferred plugin config
        events::subscribe(&kind(EventKind::DocumentOpened), 2); // options by filetype
    }

    fn on_event(handler: u32, ev: Event) {
        match (handler, ev) {
            // DEFERRED: configure each USER plugin the moment it loads. (Core
            // plugins like auto-pair are on by default — configure them in
            // `register_options` above, not here.)
            (1, Event::PluginLoaded(p)) => match p.name.as_str() {
                // A user plugin you installed that provides an off-by-default mode:
                "my-linter" => {
                    modes::enable_mode("my-linter-mode");             // turn it on
                    config::set_option("my-linter.strict", "true");   // and configure it
                }
                _ => {}
            },
            // EVENT FLOW: set options as buffers open (e.g. wrap for markdown).
            (2, Event::DocumentOpened(d)) => {
                if d.path.as_deref().is_some_and(|p| p.ends_with(".md")) {
                    config::set_option("wrap", "on");
                }
            }
            _ => {}
        }
    }
}

fn kind(k: EventKind) -> EventFilter {
    EventFilter { kinds: Some(vec![k]), path_globs: None, major_modes: None }
}

export!(Component);
```

The pattern to internalize: **immediate config for what exists at load (option
values via `set-option`, builtin/LSP keymaps via `register-binding`); deferred
`PluginLoaded` handlers for anything that belongs to a plugin** (`enable-mode`,
its options). Handlers call APIs (`enable-mode`, `set-option`), never `:` command
strings. Add a custom command by also `provide`-ing `grammar` (see
[Custom grammar](#custom-grammar-commands-motions-text-objects-operators-ex-commands)).

---

## Building and installing

### Scaffold it: `lattice --scaffold-init`

The fastest start — let lattice write a **buildable** starter config for you:

```bash
lattice --scaffold-init
```

This creates `~/.config/lattice/init/` with a complete WASM-component config crate
— `Cargo.toml`, `plugin.toml`, `src/lib.rs` (a minimal config: an option, a
keybinding, an event handler), and a `wit/` copy of *this editor's* API (so it
builds with no separate checkout, matched to your version). It refuses to
overwrite an existing config. Then build + install as below (the command prints
these steps too):

```bash
rustup target add wasm32-wasip2                      # once
cd ~/.config/lattice/init
cargo build --release --target wasm32-wasip2
cp target/wasm32-wasip2/release/lattice_init.wasm init.wasm
```

Edit `src/lib.rs`, rebuild, and `:reload-config`. The rest of this section is the
manual setup, if you'd rather assemble it yourself.

### Manual setup

`init.rs` is a standalone crate compiled to a component. A minimal setup:

```toml
# Cargo.toml — a standalone [workspace] so it doesn't inherit an outer toolchain
[package]
name = "lattice-init"
version = "0.0.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]      # a component, not an rlib

[dependencies]
wit-bindgen = "0.58"

[profile.release]
opt-level = "s"
strip = true

[workspace]
```

Build and install:

```bash
rustup target add wasm32-wasip2                    # once
cargo build --target wasm32-wasip2 --release
cp target/wasm32-wasip2/release/lattice_init.wasm \
   ~/.config/lattice/init/init.wasm                # place beside plugin.toml
```

Then in a running editor, `:reload-config` — no restart. (Or start the editor;
it loads `init/` at boot.)

Point `wit_bindgen::generate!(path: …)` at wherever you keep the
[`wit/`](../../wit) package (a copy in your config repo, or a checkout path).
Browse the exact signatures for any seam with `:describe-plugin-api <seam>`, or
dump scaffolding with `:export-plugin-api markdown`.

## The safety model

`init.rs` runs in the same sandbox as any plugin, at the pre-granted `Bundled`
tier (it's your own config):

- **Capabilities are deny-by-default.** No filesystem beyond your private `/data`
  dir unless `plugin.toml` requests an `fs:` prefix; no network, no process spawn.
- **Fuel + deadline bounded.** Every call has a budget; a runaway loop is trapped,
  not hung. The synchronous `grammar` seam has the tightest budget — keep `apply`
  cheap.
- **Crash-isolated.** A trap quarantines the config and fires one `PluginCrashed`
  event; the editor never stalls or crashes. **Return errors as values** (`Err(..)`),
  don't panic.

See [plugins.md](plugins.md#the-security-model) for the full model.

## Reference

- [plugins.md](plugins.md) — the plugin substrate, security model, introspection
  commands.
- [authoring guide](../dev/guides/plugin-authoring.md) — the per-seam surface in
  depth, building/testing guests.
- `:describe-plugin-api <seam>` / `:list-plugin-apis` / `:export-plugin-api` —
  the live, self-documenting API in the editor.
- `:reload-config` — reload `init.rs` without restarting.
