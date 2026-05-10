# Options

Options are lattice's **typed configuration knobs**. Every
toggle, threshold, and behavior selector — `number`, `tabstop`,
`foldmethod`, `lsp.log.display` — is a registered option with a
type, a default, a doc string, and an owning group. Options are
the surface the user touches when they want to change how the
editor behaves; modes, plugins, and the renderer all read the
same options through one resolver.

> **Two surfaces, one model.** This doc covers the *concepts*:
> what an option is, how to set one, where the value comes from,
> how `:set` and TOML interact. For the **live list of every
> registered option** with current values and doc strings, run
> `:options` — the reference is generated at call time from the
> registry, so it stays current as new options ship. For a
> per-option deep-dive (default, current, valid values, full
> doc) run `:describe-option <name>`.

---

## Quick reference

| Command                          | Meaning                                                                        |
|----------------------------------|--------------------------------------------------------------------------------|
| `:set NAME`                      | Boolean: set to `true`                                                         |
| `:set noNAME`                    | Boolean: set to `false`                                                        |
| `:set NAME=VALUE`                | Set to `VALUE` (parsed against the option's type)                              |
| `:set NAME?`                     | Show the option's current value (echo only, no buffer)                         |
| `:options`                       | Live reference: every option, grouped by group, with type / current / doc      |
| `:describe-option NAME`          | Full metadata for one option (default + current + valid values + full doc)     |
| `:apropos PATTERN`               | Search options + commands + events for matching names / docs                   |

`<Tab>` after `:set ` enumerates option names; for enum-typed
options, `<Tab>` after `:set NAME=` enumerates valid values.

---

## Concepts

### Typed options

Every option carries a Rust type at compile time (`bool`, `i64`,
`String`, or a domain enum like `FoldMethod` /
`BufferDisplayPreference`). The type drives:

- **Parsing.** `:set tabstop=8` succeeds; `:set tabstop=eight`
  produces `E521: number required after =`.
- **Validation.** Range / shape checks live next to the
  declaration. `:set tabstop=999` echoes the validator's
  message (`tabstop must be 1..=32`).
- **Tab completion.** Enum-typed options enumerate their valid
  values; `:set foldmethod=<Tab>` cycles through `manual`,
  `indent`, `markdown`, `syntax`.

The internal hot path reads options by **type**, not by name —
the renderer's `option_cache.tabstop` lookup is a struct field
load, not a map probe. Names appear only at the boundary
(`:set`, TOML keys, the customize buffer view).

### Layered resolution

Each option's *effective* value at any point is the resolved
result of several layers, with later layers overriding earlier
ones:

1. **Built-in default** — the value the option was registered
   with (`Tabstop = 8`, `Wrap = false`).
2. **TOML overrides** — keys from your `~/.config/lattice/init.toml`
   (and project-level `.lattice/init.toml`) loaded at startup.
3. **Runtime `:set`** — explicit settings during the session.
4. **Mode contributions** — modes can contribute options via
   `Mode::options()`. Example: `help-mode` contributes
   `ReadOnly = true` and `Wrap = true`; activating it on a
   buffer flips both for that buffer regardless of the global
   default. Major modes contribute first, then minor modes
   layer on top.
5. **Per-buffer locals** — explicit per-buffer overrides
   (the future `:setlocal NAME=VALUE` form).

The resolved value is cached per buffer; every change to any
layer triggers a rebuild for the affected buffers. The renderer
sees only the resolved values — it doesn't know which layer the
value came from.

### Groups

Options are organized into **groups** for navigation and
`:customize`. The built-in groups are:

- **`editor`** — bare-named editor options (`tabstop`, `number`,
  `wrap`, `foldmethod`, ...). Reserved namespace; plugins
  declare their options under their own prefix.
- **`display`** — visual-presentation options across modes:
  per-feature buffer-display preferences (`lsp.log.display`,
  `hover.display`, ...), gutter contents, current-line
  highlight.
- **`editing`** — search / indent / auto-pair behavior.
- **`completion`** — insert-completion knobs (sources,
  priorities, commit chars, ghost text).
- **`lsp`** — every option owned by an LSP mode
  (`lsp-completion-mode`, `lsp-diagnostics-mode`, ...).
  `:customize lsp` will show the union sectioned by owning mode.
- **`picker`** — file finder / command palette / symbol search.
- **`filetree`** / **`oil`** / **`help`** — mode-specific
  options for those buffer kinds.
- **`appearance`** — theming, sprite icons, font rendering.

### Customizable vs internal

Options carry a `customizable` flag. The default is `true` —
the option appears in `:set` autocompletion, in `:options`, and
in the `:customize` group view. A handful are
`customizable = false`: engine-internal state that's mode-driven
or cascade-driven, not user-typed. Example: `read-only` is
contributed by `help-mode` / `file-tree-mode` / the LSP log
modes — users who want a particular buffer read-only enable the
appropriate mode rather than typing `:set read-only=true`.
`:options` hides `customizable = false` entries; they still
respond to `:describe-option <name>` (so you can see the
mechanism), but the live reference focuses on what users
actually configure.

---

## Where to set what

### Static settings: TOML

Put values that should persist across sessions in
`~/.config/lattice/init.toml`. Project-level overrides live in
`.lattice/init.toml` at the repo root and override user-level
on a per-project basis. The TOML keys mirror the option names
verbatim:

```toml
# ~/.config/lattice/init.toml
number = true
tabstop = 4
foldmethod = "syntax"
"lsp.log.display" = "split-h"
"hover.display" = "popup-cursor"
```

Dotted names need quoting in TOML (`"lsp.log.display"`).
Unknown keys produce a startup warning rather than an error —
the editor still launches.

### Runtime tweaks: `:set`

Use `:set` for mid-session changes. The change applies for the
rest of the session and is *not* persisted to TOML.

```vim
:set wrap
:set tabstop=4
:set hover.display=active-pane
```

### Programmable logic: `init.rs`

Programmable configuration (custom keymaps, autocmds, hooks,
custom commands) lives in your `init.rs` module — a Rust source
file compiled to WASM and loaded by the plugin host with a boot
capability set. Static settings stay declarative (TOML); logic
stays code (Rust → WASM). This is the *one* extension substrate;
there is no Lua / vimscript / elisp.

> **Status:** the plugin host and `init.rs` loader are the
> Phase 4+ surface. Today, all configuration runs through TOML
> + `:set`. Track progress in
> [`../dev/operations/implementation.md`](../dev/operations/implementation.md).

---

## Tab completion

`:set <Tab>` enumerates every customisable option name plus the
`noNAME` form for boolean options. After `:set NAME=`, `<Tab>`
enumerates valid values when the option's type knows them
(enums emit their variants; booleans emit `true` / `false`;
strings and integers offer no completion).

`:describe-option <Tab>` enumerates *every* registered option,
customisable or not — useful when you want to read the doc for
an internal option without setting it.

---

## Edge cases

### Cascades

Some option changes trigger follow-on changes. Setting
`relativenumber = true` implies `number = true` (vim's
behaviour). The cascade hook runs after each set and is
idempotent — setting `relativenumber` repeatedly doesn't
re-fire downstream effects.

### `:set NAME?` doesn't change anything

The query form (`:set tabstop?`) echoes the current value but
runs no event publishing, no cascade, no validator. It's safe
on every option, including read-only ones.

### Failures echo and leave state untouched

A failed parse, a failed validator, or a failed type-mismatch
on `:set noNAME` (called against a non-bool) all echo the
specific error and leave the option's value unchanged. State
remains consistent.

### Mode contributions can't be hand-overridden

If `help-mode` is active on a buffer and contributes
`Wrap = true`, typing `:set nowrap` against that buffer wins
in vim's sense (later layer overrides earlier), but the moment
you re-enter the buffer or activate any mode, the
mode-contributed value re-applies. The future `:setlocal`
form lands a per-buffer-local layer that wins over modes; for
v1, prefer toggling the mode (`:help-mode` to deactivate
help-mode for the buffer) over fighting its contributions.

---

## See also

- `:help modes` — what major / minor modes are, how
  `:<mode-name>` toggles work, mode option contributions.
- `:help completion` — the insert-completion options
  (`completion.source.*.priority`, `completion.ghost_text`).
- `:help lsp-mode` — the LSP gate option semantics.
- `:options` — **live** reference of every registered option.
- `:describe-option <name>` — full metadata for one option.
- `:apropos <pattern>` — search options + commands + events.
- [`../dev/architecture/design.md`](../dev/architecture/design.md) §5.12 — the configuration
  system spec (typed options, layered resolver, customize
  buffer view).
