<!-- @generated from wit/ by crates/lattice-plugin-api (render.rs).
     Do not edit: run `UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api`. -->

# `config`

**Direction:** guest calls into the host through it · **Capability:** none (pure data / dispatch) · **Worlds:** `auto-pair-plugin` (imports), `comment-plugin` (imports), `config-plugin` (imports), `project-plugin` (imports), `scanned-excerpt-source-plugin` (imports), `treesitter-context-plugin` (imports)

Mirrors `ConfigRegistry` (lattice-config). The guest declares an option
(name + type + default + doc); the host registers it into the *same* registry
core options live in, so `:set` / `:describe-option` / `:customize` /
`gen:options` completion treat plugin options uniformly (no host kind-branch).
Values round-trip as strings via the native `OptionType` parse/format
contract. Populated in PH7.10.

This interface is the CANONICAL, language-agnostic option API — any
component-model language (Go, JS, Zig, Python, ...) calls these directly. The
Rust `lattice-plugin-sdk` `#[derive(PluginOption)]` (PH7.10b) is optional
ergonomics that expands to these same calls; it adds no capability not here.

## Functions (8)

### `get-option`

```wit
get-option: func(name: string) -> option<string>
```

Read an option's current value, formatted as a string (the `OptionType`
`format` contract). `none` if no option by that name is registered.
Resolves the plugin's OWN namespace first (`style` → `<id>.style`), then
the raw name — so a plugin reads its own options with short names AND can
still read a core option (`tabstop`) that isn't in its namespace.

**Example — Read the plugin's own option on every call, so `:set` takes effect without re-registering** · [`plugins/auto-pair/src/lib.rs`](../../../../plugins/auto-pair/src/lib.rs)

```rust
/// Read the live style option (AP.3). `auto` (default) or `manual`. The plugin
/// uses the SHORT name `style`; the host auto-namespaces it to `auto-pair.style`
/// (the name a user sets). The grammar guest reads the SHARED editor config
/// registry (wired at instantiate time), so `:set auto-pair.style=manual` flips
/// behavior live — no keymap re-registration.
fn is_manual() -> bool {
    config::get_option("style").as_deref() == Some("manual")
}
```

### `get-option-value`

```wit
get-option-value: func(name: string) -> option<config-value>
```

Read an option's current value as a tree. `none` if no option by that
name is registered. Resolves the caller's OWN namespace first, exactly
like `get-option`.

Works for scalar options too — a scalar is a degenerate schema, so a
guest that wants typed reads everywhere can use this one call rather
than choosing per option.

**Example — Read a structured option's current value, falling back to defaults** · [`plugins/project/src/lib.rs`](../../../../plugins/project/src/lib.rs)

```rust
/// The configured rows, or the defaults.
fn switch_commands() -> Vec<switch::SwitchCommand> {
    match lattice::plugin_host::config::get_option_value(switch::OPTION) {
        Some(value) => switch::from_value(&value),
        // Unregistered or unreadable — the same answer either way, and it is
        // the useful one: a menu with no rows looks exactly like a broken chord.
        None => switch::defaults(),
    }
}
```

### `option-diagnostic`

```wit
option-diagnostic: func(name: string) -> option<config-diagnostic>
```

**Did the last assignment to `name` fail, and what did it say?**

A failed assignment is a no-op — vim's rule, which lattice keeps — so
the option is left holding whatever it had, and for one that was never
successfully set that is its registered DEFAULT. Reading the value
therefore cannot distinguish "the user configured this and it did not
parse" from "the user never configured this". This can.

org-capture is the case that forced it: a `capture-templates` whose
TOML did not fit its schema read back as the empty default, so capture
filed through the legacy `capture-file` believing nothing had been
configured — and the user's note went somewhere they thought they had
stopped using.

**This is not a status the option carries.** The option has no such
state; an assignment errored, which is an event, and this is the record
of it. `none` means the last assignment succeeded, or there was never
one — those two are not distinguished, and deliberately: a plugin's
question is "can I trust this value", and both answers are yes.

Cleared for a name as soon as an assignment to it succeeds, and the
whole record is rebuilt on each config load, so a user who fixes their
file stops being told it is broken.

Resolves the caller's OWN namespace first, like `get-option`.

### `register-option`

```wit
register-option: func(name: string, ty: option-type, default: string, doc: string) -> bool
```

Declare a plugin option into the editor's `ConfigRegistry`. `default` is
the initial value as a string (parsed via the chosen `option-type`); `doc`
is the `:describe-option` summary. Returns `false` (registering nothing) if
`default` doesn't parse for `ty` OR `name` collides with an existing option
— a plugin must not silently shadow another option. Idempotent to retry
after a rejected default.

**Auto-namespaced.** `name` is prefixed with the plugin's id — a plugin
with id `auto-pair` registering `style` contributes `auto-pair.style`. Use
SHORT names; the host owns the namespace so plugins can't collide (and a
user sets it as `:set auto-pair.style=…`). `get`/`set-option` resolve the
same way (short name → own namespace).

**Example — Register a typed option (namespaced by the host as `comment.leader-space`)** · [`plugins/comment/src/lib.rs`](../../../../plugins/comment/src/lib.rs)

```rust
config::register_option(
    "leader-space",
    OptionType::Boolean,
    "true",
    "insert a space between the comment leader and the code (`// x`, not `//x`)",
);
```

### `register-structured-option`

```wit
register-structured-option: func(name: string, schema: config-schema, default: config-value, doc: string) -> bool
```

Declare an option whose value has structure. The schema-taking peer of
`register-option`, with the same namespacing and the same collision
rules.

`default` is validated against `schema` before anything is registered:
a plugin whose own default does not fit its own declaration registers
NOTHING and gets `false`, rather than an option that exists and cannot
hold a legal value.

**Example — Register a list-of-records option with a schema and a structured default** · [`plugins/project/src/lib.rs`](../../../../plugins/project/src/lib.rs)

```rust
let rows = switch::defaults();
let _ = lattice::plugin_host::config::register_structured_option(
    switch::OPTION,
    &switch::schema(),
    &switch::to_value(&rows),
    "Rows of the project-switch menu. Each names an ex-command that \
     takes a project root as its first argument — which is the whole \
     contract for adding your own.",
);
```

### `set-option`

```wit
set-option: func(name: string, value: string) -> bool
```

Set (override) an EXISTING option's value (CI.7) — the init.rs config
front-end symmetric with `lattice.toml` and `:set`. Backed by the same
`parse_and_set_command` path `:set name=value` uses: the value string is
type-coerced and validated, and a successful set publishes
`OptionChanged` so subscribers react uniformly. Returns `false` (setting
nothing) if the option is unregistered, the value is invalid for its type,
or no registry is wired — never a trap. An `init.rs` `on-plugin-loaded`
handler uses this to configure a plugin's options the moment it loads
(config-and-init.md §5). Like `get-option`, resolves the caller's OWN
namespace first (`style` → `<id>.style`), else the raw name — so a config
can set another plugin's option by its full `auto-pair.style` name.

**Example — Toggle this plugin's `enabled` option from an ex-command** · [`plugins/treesitter-context/src/lib.rs`](../../../../plugins/treesitter-context/src/lib.rs)

```rust
// Flip the loader-registered enablement switch. This one needs no
// tree — it only reads and writes an option — which is exactly why
// it survives where `:context-up` could not.
CB_EX_CONTEXT_TOGGLE => {
    let _ = ctx;
    let on = get_option("enabled").map(|v| v == "true").unwrap_or(true);
    set_option("enabled", if on { "false" } else { "true" });
    Ok(vec![Effect::None])
}
```

### `set-option-in-buffer`

```wit
set-option-in-buffer: func(buffer: u64, name: string, value: string) -> bool
```

Set an option for ONE buffer — the `:setlocal` front-end, and the call a
mode-lifecycle handler needs.

`set-option` above writes the GLOBAL layer (it is the `:set` path), so a
handler that wants *wrap in org buffers* cannot use it: it would wrap
everything, and nothing would unwrap on leaving org. This writes the
buffer-local override layer instead, which is exactly the scope the
question has.

The canonical use is a `major-entered` / `minor-activated` subscription
filtered to one mode — `add-hook 'org-mode-hook` in this editor's
vocabulary:

```ignore
subscribe(&EventFilter {
    kinds: Some(vec![EventKind::MajorEntered]),
    major_modes: Some(vec!["org-mode".into()]),
    ..
}, ON_ORG);
// in the handler:
set_option_in_buffer(ev.buffer, "autowrap", "all");
```

Works uniformly for built-in, core-plugin and external-plugin modes:
the lifecycle events are published by the mode dispatcher, which does
not know which of those declared the mode.

Parsed by the same path `:setlocal name=value` uses, so a guest can
express nothing `:setlocal` could not and a bad value is refused with
the same message. Returns `false` — setting nothing — on an unknown
option, an invalid value, or an unknown buffer; never a trap, the
`set-option` contract.

**Applied on the next host tick, not synchronously.** The buffer-local
layer lives on the Editor rather than in the config registry, so this
publishes a host-internal request the Editor drains. A handler cannot
observe its own write by reading the option back in the same call.

### `set-option-value`

```wit
set-option-value: func(name: string, value: config-value) -> bool
```

Set an option from a tree. Validated against the option's declared
schema, so a bad field is refused with a PATH
(`templates[2].target.file: expected string, got integer`) rather than
by whatever message the plugin would have written. `false` on an unknown
option, a value that does not fit, or no registry — never a trap, the
`set-option` contract.

**Example — Set a structured (list-of-records) option by building its value arena** · [`crates/lattice-plugin-host/tests/fixtures/config-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/config-guest/src/lib.rs)

```rust
// Set one through the typed seam, then read it back the same
// way. Recording what came BACK — not what went in — is the
// point: a seam that accepted the tree and stored a mangled one
// would pass any assertion made on the write alone.
let _ = config::set_option_value(
    "templates",
    &config::ConfigValue {
        nodes: vec![
            config::ValueNode::String("t".to_string()),           // 0
            config::ValueNode::String("~/org/refile.org".to_string()), // 1
            config::ValueNode::Record(vec![("file".to_string(), 1)]),  // 2
            config::ValueNode::Record(vec![
                ("key".to_string(), 0),
                ("target".to_string(), 2),
            ]), // 3
            config::ValueNode::List(vec![3]),                     // 4
        ],
        root: 4,
    },
);
```

## Types (7)

### enum `option-type`

```wit
enum option-type {
    boolean,
    integer,
    string,
}
```

The value type of a plugin option. Maps 1:1 to a native `OptionType`
impl: `boolean`→`bool`, `integer`→`i64`, `string`→`String`. The option's
value is set / read as a `string` and parsed/formatted through that type
(so `:set name=value` and `get-option` share one string contract).

### record `schema-field`

```wit
record schema-field {
    name: string,
    schema: u32,
    required: bool,
    doc: string,
}
```

One field of a `schema-node.record`. `schema` is an INDEX into the
owning `config-schema.nodes`, which is how nesting survives an ABI with
no recursion.

`doc` is per field, not only per option, because that is what
`:describe-option` and `:customize` render beside it — an option-level
doc string describing six fields is the wall of prose this replaces.

**Fields**

- `name`: `string`
- `schema`: `u32`
- `required`: `bool` — A missing required field is a validation error naming its path; a
  missing optional one is simply absent from the value.
- `doc`: `string`

### variant `schema-node`

```wit
variant schema-node {
    scalar(option-type),
    enum-of(list<string>),
    list-of(u32),
    record(list<schema-field>),
}
```

One node of a schema arena. Mirrors `lattice_config::ConfigSchema`, with
child links as indices.

`enum-of` is not sugar for a string: it is the difference between
`:customize` offering a picker and offering a text field.

**Cases**

- `scalar`: [`option-type`](#enum-option-type)
- `enum-of`: `list<string>`
- `list-of`: `u32` — The element shape, by index.
- `record`: `list<schema-field>`

### record `config-schema`

```wit
record config-schema {
    nodes: list<schema-node>,
    root: u32,
}
```

The declared shape of an option's value, as an arena.

`root` is explicit rather than "node 0 by convention": a convention is
an invariant nothing checks, and this one has to be range-checked at the
boundary regardless.

### variant `value-node`

```wit
variant value-node {
    bool(bool),
    int(s64),
    string(string),
    list(list<u32>),
    record(list<tuple<string, u32>>),
}
```

One node of a value arena. Mirrors `lattice_config::ConfigValue`.

A record's fields are an association list because WIT has no map; the
host converts to an ordered map on arrival, so two values differing only
in field order are the same value — which they must be, since one config
home writes TOML (unordered) and the other writes a struct.

### record `config-value`

```wit
record config-value {
    nodes: list<value-node>,
    root: u32,
}
```

A value shaped by a `config-schema`, as an arena.

### record `config-diagnostic`

```wit
record config-diagnostic {
    message: string,
    source: string,
}
```

OC.11c: one failed assignment to an option.

**Fields**

- `message`: `string` — The message the loader or the registry produced, verbatim. For a
  composite it carries the schema PATH —
  `[2].target.file: expected string, got integer` — which is the
  whole reason this is worth surfacing over a bare "it failed".
- `source`: `string` — The config file the assignment came from, or empty for a runtime
  `:set`. That distinction is "go fix your config" versus "the thing
  you just typed did not take", and a guest reporting one as the
  other sends the user to the wrong place.

