# Mode Architecture (developer reference)

Authoritative design for lattice's major / minor mode system. The
plan section at the end lists the slices that take us from the
current state (mode work scattered across `BufferKind` matches in
the UI layer, ad-hoc dispatch in `lattice-lsp`, no formal mode
registry) to a single composable mode model that all customization
flows through.

This document is a *companion* to DESIGN.md, not a replacement.
DESIGN.md §5.8 names major / minor modes as the primary
customization model; this doc spells out the trait, the resolution
algorithm, the taxonomy criteria, and the migration sequence the
spec leaves implicit. After review, DESIGN.md gets a link here from
§5.6 / §5.8 / §5.9 / §5.10 / §5.12.

## 1. Vision

> *"Composable, content-type-aware customization. Modal state
> (Normal/Insert/Visual/etc.) is orthogonal."* -- DESIGN.md §2.1

> *"A buffer has exactly one major mode. The major mode declares:
> ... rendering profile, style mappings, default minor modes,
> commands."* -- DESIGN.md §5.8.1

> *"A buffer can have any number [of minor modes] active.
> Composable, additive features."* -- DESIGN.md §5.8.2

The architecture commitments this implies:

- **Modes are the locus of customization.** Keymaps, options,
  events, decorations, lifecycle hooks, statusline contributions,
  and renderer dispatch all flow through mode-resolved values.
  Not BufferKind, not feature-by-feature `if` ladders, not global
  state with per-feature gates.
- **Composability is first-class.** `git-blame-mode` +
  `whitespace-show-mode` + `lsp-completion-mode` simultaneously
  active is a non-event. Conflicts are declared, not implicit.
- **Modes are an interface, not a distribution unit.** Built-in
  modes ship as compiled Rust against the `Mode` trait. Bundled
  plugin modes ship as WASM components against the same trait via
  an adapter. Third-party plugin modes ditto. *The host treats
  all three identically downstream of registration.*
- **Options are typed identities, not strings.** Every built-in
  option is a unique Rust type. `config.get::<Tabstop>()` is the
  canonical access. Strings only appear at boundaries (cmdline
  `:set`, TOML, plugin manifests). Cross-crate uniqueness is
  guaranteed by Rust's type system, not by string-name discipline.
- **Same store, two front-ends -- one for individual options,
  one for groups.** `:set <option>=<value>` is the
  command-line parser, single-option, immediate. `:customize
  <mode-or-group>` is the form-buffer front-end against the
  *same* registry, organized by either a mode (a focused view
  of one mode's options) or a `OptionGroup` (an explicit
  cross-mode collection -- `lsp`, `picker`, `editor`, ...). Both
  work in TUI and GUI: the customize buffer is just a buffer
  with a `customize-mode` major (DESIGN.md §5.9.10's
  "everything-is-a-buffer applies to interactive prompts");
  GPUI and ratatui render the same form-row decorations, with
  widget sophistication varying by surface. v1 ships both
  `:set` and `:customize`; only the TOML write-through path
  is deferred to v1.x.
- **Modes and groups are different axes.** Modes are units of
  runtime behavior (option overrides, keymaps, events,
  decorations, lifecycle). `OptionGroup`s are units of
  user-facing organization (what's listed together under
  `:customize`). A mode's options typically belong to a group;
  a group typically aggregates options from multiple modes.
  Both are first-class registered types, both compile-time
  unique across crates, neither derived from the other.
- **Performance.** Mode-resolved option reads are O(1) on the
  hot path (cached, invalidated on mode toggle / option write).
  Mode-keymap layering uses the existing layered registry from
  `keymap-architecture.md` §5-6. Lifecycle events ride the
  existing typed event bus (DESIGN.md §5.10).
- **Introspection from day one.** `:describe-mode <name>`,
  `:list-modes`, `:describe-option-resolution <name>` show what's
  active and where each resolved value came from. (DESIGN.md
  §5.11 commits; this doc extends.)

## 2. Correction to §5.8.3 (three implementation paths)

§5.8.3 reads: *"Major and minor modes are implemented as WASM
plugins. No privileged built-in path. Built-in modes ship as
bundled plugins."*

This is the maximalist position. It's wrong on two counts:

1. **Per-call WASM overhead** (§8 budget: <500ns p99) compounds
   for hot-path mode dispatch -- keymap lookup, option resolution,
   lifecycle dispatch happen many times per keystroke. Even
   amortised, asking core LSP / help / file-tree to round-trip
   through `wasmtime` for things the host already knows is
   wasteful.
2. **It conflates *distribution* with *implementation*.** A mode
   is a contract. Three legitimate ways to fulfill it:
   - **Built-in.** Compiled into a lattice crate. Direct trait
	 impl. Zero WASM overhead. Examples: `rust-mode` (parser +
	 LSP + indent), `lsp-mode` (umbrella subsystem),
	 `help-mode` (read-only buffer with link nav).
   - **Bundled plugin.** Ships as a WASM component with the
	 editor distribution. Default-on. Adapter on the host side
	 bridges WIT calls to the `Mode` trait. Same registration
	 surface as built-ins; users can disable.
   - **Third-party plugin.** Same WIT surface as bundled
	 plugins. User opt-in via package manager (post-1.0).

The three paths share **one trait** and **one registry**. The
host doesn't branch on origin during dispatch.

This is also how Emacs actually works: `cc-mode` ships built-in
(in C); third-party modes are elisp-loaded. The trait is
`define-derived-mode` / `define-minor-mode`; the runtime is
either built-in or interpreted. Same surface.

§5.8.3 should be revised accordingly. The "no privileged
built-in path" sentence becomes "the trait does not privilege
built-in implementations -- a third-party plugin can override
or replace any built-in mode."

## 3. What is and isn't a mode

The design pressure: avoid over-modeling. Not every piece of
runtime state deserves a mode entry; modes carry config weight.

### 3.1 Major mode

One per buffer. Identifies the *content's* identity:

- Tree-sitter grammar, indent / locals / injection queries.
- Default LSP server set.
- Default keymap layer (chord additions / overrides).
- Comment syntax, indent style, formatter, default fold method.
- Default minor modes auto-activated on entry.
- Mode-scoped option overrides (e.g. `markdown-mode` sets
  `wrap=true`).
- Lifecycle hooks (`on_activate` / `on_deactivate`).

Stable for the buffer's lifetime. Changes mean re-parsing,
re-attaching servers, re-running activation hooks.

### 3.2 Minor mode

Any number active. Composable. Toggleable.

Contributes:

- Option overrides (additive, layered with explicit precedence
  -- see §6).
- Keymap layer (pushed onto the layered registry at this mode's
  priority slot).
- Typed event subscriptions (filters + handlers).
- Decoration providers (gutter / inline / overlay /
  statusline segments).
- Commands.
- Lifecycle hooks.

Activation: auto-activate per major mode declaration, user
toggle (`:enable <name>` / `:disable <name>`), or programmatic
from another mode / plugin.

### 3.3 Not a mode (don't promote)

- **Modal state** (Normal / Insert / Visual / Op-pending /
  Command / Search) -- already orthogonal axis. CLAUDE.md.
- **Transient overlays with their own keymap layer**
  (completion popup, snippet expansion, picker, chord-capture).
  These are stack-pushed keymap *layers*; they contribute no
  persistent options or events. Already handled by the layered
  keymap registry (`keymap-architecture.md` §6).
- **Observable runtime state** (macro recording, hlsearch
  active, jump-list state). Surfaced via the modeline /
  introspection API. Not toggleable as a feature.
- **Single-flag user preferences without behavior bundle**
  (e.g. `set ruler`). These are typed options, not modes. A
  mode bundles options + keymap + events + decorations; if all
  you have is one option, you have an option, not a mode.

The discriminator: *would a user reasonably want to toggle this
as a unit?* If yes, it's a mode. If they'd toggle one option
without expecting other effects, it's an option.

### 3.4 Every mode is user-toggleable and user-reloadable

Both kinds of mode are runtime-toggleable: `:enable <name>`,
`:disable <name>`, `:toggle <name>`, plus an auto-generated
ex-command `:<mode-name>` per registered mode (matching emacs
muscle memory). Major modes additionally support reload: calling
`:rust-mode` while the buffer is already in `rust-mode`
deactivates and re-activates, re-running setup. Minor mode
deactivation is *complete* by construction -- every option
override, keymap entry, event subscription, and decoration
provider the mode contributed is removed (§9.6).

## 4. Inventory: current lattice features → mode mapping

Maps the surface implemented today onto the proposed model. Used
as the migration checklist in §10.

### 4.1 Major modes

| Mode                      | Replaces today's                                            | Owner crate                | Notes                                                                            |
|---------------------------|-------------------------------------------------------------|----------------------------|----------------------------------------------------------------------------------|
| `text-mode`               | `BufferKind::Document` + `Lang::Plain`                      | `lattice-mode` (foundation)| Catch-all default. No parser, default keymap.                                    |
| `rust-mode`               | `Lang::Rust`                                                | `lattice-grammar`          | tree-sitter-rust + indent + comment + auto `lsp-mode`.                           |
| `python-mode`             | `Lang::Python`                                              | `lattice-grammar`          | Per-language.                                                                    |
| `javascript-mode`         | `Lang::JavaScript`                                          | `lattice-grammar`          | Per-language.                                                                    |
| `markdown-mode`           | `Lang::Markdown`                                            | `lattice-grammar`          | Block + inline split. Reused by hover popup content.                             |
| `help-mode`               | `BufferKind::Help` (when used for `:help` / describe-*)     | `lattice-help` (new) or `lattice-core` | Read-only, link nav (`<CR>`), `:apropos` search line.               |
| `lsp-log-mode`            | `:lsp-log` buffers                                          | `lattice-lsp`              | Read-only, follow-tail toggle.                                                   |
| `lsp-trace-log-mode`      | `:lsp-trace-log` buffers                                    | `lattice-lsp`              | Read-only, JSON-RPC-aware syntax highlighting.                                   |
| `lsp-server-log-mode`     | `:lsp-server-log` buffers                                   | `lattice-lsp`              | Read-only, server-stderr feed.                                                   |
| `file-tree-mode`          | `BufferKind::FileTree`                                      | `lattice-core`             | Tree nav keymap, expand/collapse, open-on-`<CR>`.                                |
| `oil-mode`                | `BufferKind::Oil`                                           | `lattice-core`             | Editable directory; `:write` applies rename / delete.                            |
| `command-line-mode`       | `:` minibuffer (DESIGN.md §5.9.10)                          | `lattice-core`             | Rich minibuffer's command-prompt major.                                          |
| `search-line-mode`        | `/` and `?` minibuffer                                      | `lattice-core`             | Same family as `command-line-mode`.                                              |
| `diagnostics-mode`        | `:diagnostics` buffer                                       | `lattice-lsp`              | Read-only, jump-on-`<CR>`.                                                       |
| `buffer-list-mode`        | `:ls` output (when promoted to a buffer)                    | `lattice-core`             | Read-only, switch-on-`<CR>`.                                                     |
| `messages-mode`           | (future `:messages` buffer)                                 | `lattice-core`             | Once we add a message-history buffer.                                            |
| `customize-mode`          | (new -- the `:customize <group>` form buffer; §6.7)         | `lattice-mode` or `lattice-ui-tui` | Form-row navigation, per-row edit, apply-via-`:set`. Buffer-backed so it's TUI / GUI agnostic. |

### 4.2 Minor modes

#### 4.2.1 LSP minor modes (the deepest cluster)

| Mode                       | What it owns                                                                       | Auto-activate                                  |
|----------------------------|------------------------------------------------------------------------------------|------------------------------------------------|
| `lsp-mode`                 | Umbrella: server attachment, didOpen / didChange dispatch, capabilities gate       | When a server attaches to the buffer.          |
| `lsp-completion-mode`      | LSP as completion source; insert-mode popup contributor                            | `lsp-mode` + server has `completionProvider`.  |
| `lsp-diagnostics-mode`     | Receive `publishDiagnostics`; paint inline + gutter; `:diag-next` / `-prev`        | `lsp-mode`.                                    |
| `lsp-hover-mode`           | `K` binding → hover popup (markdown content in floating geometry)                  | `lsp-mode` + server has `hoverProvider`.       |
| `lsp-signature-mode`       | Auto signature help on `(`, `,`                                                    | `lsp-mode` + server has `signatureHelpProvider`.|
| `lsp-format-mode`          | `:lsp-format`, `:lsp-format-range`, format-on-save hook                            | `lsp-mode` + server has `documentFormattingProvider`. |
| `lsp-rename-mode`          | `:lsp-rename` + workspaceEdit apply                                                | `lsp-mode` + server has `renameProvider`.      |
| `lsp-symbols-mode`         | `:lsp-symbols`, `:lsp-workspace-symbol`                                            | `lsp-mode` + corresponding capabilities.       |
| `lsp-code-action-mode`     | `:lsp-code-action`                                                                 | `lsp-mode` + server has `codeActionProvider`.  |
| `lsp-nav-mode`             | go-to definition / declaration / type-def / implementation / references            | `lsp-mode` + corresponding capabilities.       |
| `lsp-lens-mode`            | Code lens (post-1.0)                                                               | Opt-in.                                        |
| `lsp-inlay-hint-mode`      | Inlay hints (post-1.0)                                                             | Opt-in.                                        |
| `lsp-semantic-tokens-mode` | Semantic tokens (post-1.0)                                                         | Opt-in.                                        |

`lsp-mode` is the gate: disabling it deactivates every LSP
sub-mode. Sub-modes are independently disable-able while
`lsp-mode` is on. *"Disable LSP completion but keep diagnostics
on this buffer"* becomes `:disable lsp-completion-mode`.

#### 4.2.2 Display / editing minor modes

| Mode                         | What it owns                                                                |
|------------------------------|-----------------------------------------------------------------------------|
| `line-numbers-mode`          | Gutter contribution. Replaces `:set number` as the user-facing toggle.      |
| `relative-line-numbers-mode` | Gutter contribution. Implies `line-numbers-mode`. Replaces `:set rnu`.      |
| `whitespace-show-mode`       | Decoration provider for trailing/leading/tab whitespace. (Was `:set list`.) |
| `current-line-highlight-mode`| Decoration provider for the cursor's line. (Was `:set cursorline`.)         |
| `read-only-mode`             | Forbids edits. Orthogonal to major mode -- any buffer can be read-only.     |
| `wrap-mode`                  | Universal wrap toggle. Applies to every buffer kind. Replaces ad-hoc        |
|                              | `Wrap { trim: false }` calls in the renderer.                               |
| `auto-pair-mode`             | (future) Bracket pairing.                                                   |
| `git-blame-mode`             | (future) Inline blame author per line.                                      |
| `git-gutter-mode`            | (future) Gutter symbols for added / modified / deleted lines.               |
| `flymake-mode`               | (future) On-the-fly diagnostics not via LSP.                                |
| `rainbow-delimiters-mode`    | (future) Coloured matching brackets.                                        |

Several of these (notably `line-numbers-mode`,
`whitespace-show-mode`) are typed options today. Promotion
strategy in §10 / M.7: the option stays as the underlying
storage; the minor mode is a thin wrapper that sets / unsets
the option. `:enable line-numbers-mode` and `:set number` are
two surfaces on the same underlying state.

### 4.3 Existing surfaces that *don't* become modes

- **Modal state** (Normal / Insert / Visual / Op-pending /
  Command / Search): orthogonal axis, stays.
- **Completion popup**: keymap layer (already in the layered
  registry); not a mode.
- **Snippet expansion**: keymap layer; not a mode.
- **Picker**: keymap layer + transient state; not a mode.
- **Chord capture**: keymap layer; not a mode.
- **Macro recording / replay**: state, surfaced in modeline.
- **Hlsearch active**: state derived from `app.last_search`.
- **Jump list / position history**: data, not a mode.

## 5. The `Mode` trait

### 5.1 Trait surface

```rust
pub trait Mode: Send + Sync + 'static {
	fn id(&self) -> ModeId;                  // canonical name (interned)
	fn kind(&self) -> ModeKind;              // Major | Minor

	/// Option overrides this mode contributes. Type-keyed (see
	/// §6.4): each override is paired with the option's type at
	/// compile time, so a typo or wrong-type contribution is a
	/// compile error, not a runtime registration failure.
	/// Pure declarative -- the registry, not the mode, applies
	/// these to the layer stack.
	fn options(&self) -> OptionOverrideSet;

	/// Keymap chord -> command additions / overrides. Layered
	/// into the existing keymap registry
	/// (`keymap-architecture.md` §5-6) at this mode's priority
	/// slot.
	fn keymap(&self) -> &Keymap;

	/// Typed event subscriptions. Filters + handlers. Activated
	/// alongside the mode; deactivated on exit.
	fn subscriptions(&self) -> &[Subscription];

	/// Decoration providers (gutter / inline / overlay /
	/// statusline). Polled by the renderer.
	fn decorations(&self) -> &[DecorationProvider];

	/// Capabilities the mode requires from the host. Validated
	/// at activation: a mode that needs `BufferUri` cannot
	/// activate on a buffer without one. Missing capability ⇒
	/// typed error, never silent skip.
	fn required_capabilities(&self) -> CapabilitySet;

	/// Conflicts. Activating this mode auto-deactivates
	/// conflicting minor modes; activation fails if a
	/// conflicting major is already active.
	fn conflicts_with(&self) -> &[ModeId];

	/// Implies. Activating this mode auto-activates these
	/// (used by `relative-line-numbers-mode` ⇒
	/// `line-numbers-mode`).
	fn implies(&self) -> &[ModeId];

	/// Lifecycle. Called once per (buffer, activation) cycle.
	/// `ModeContext` exposes event publishing + buffer reads,
	/// NOT mutable config writes -- modes contribute via
	/// declarative `options()`, never by side-effecting the
	/// registry. Errors propagated as typed
	/// `ModeActivationError` -- do not panic.
	fn on_activate(&self, ctx: &ModeContext) -> Result<(), ModeActivationError> {
		Ok(())
	}
	fn on_deactivate(&self, ctx: &ModeContext) -> Result<(), ModeActivationError> {
		Ok(())
	}
}

pub enum ModeKind { Major, Minor }

pub struct ModeId(InternedStr);  // fast == and HashMap-keyed lookups

pub struct CapabilitySet { /* bitfield: BufferUri | Lsp | TreeSitter | Fold | ... */ }

/// Read-only by design (see §5.2). Modes contribute via
/// declarative trait methods; the lifecycle hook is for side
/// effects (spawn server, open watcher), not for direct config
/// or keymap mutation.
pub struct ModeContext<'a> {
	pub buffer: &'a Buffer,
	pub events: &'a EventBus,
	// no &mut Config -- options come from `options()`.
	// no &mut Keymap -- keymap comes from `keymap()`.
	// no direct LSP / actor access -- modes go through events.
}
```

### 5.2 The declarative-only rule

The trait splits cleanly into two halves:

- **Declarative methods** (`options`, `keymap`, `subscriptions`,
  `decorations`, `required_capabilities`, `conflicts_with`,
  `implies`) return read-only data. The *registry*, not the
  mode, applies these to the layer stack on activation and
  removes them on deactivation. A mode can never "leak"
  contributions past its lifetime by construction.
- **Lifecycle hooks** (`on_activate`, `on_deactivate`) are for
  side effects only -- spawning a server connection, opening
  a file watcher, allocating a buffer-side cache. They receive
  a *read-only* `ModeContext`. They cannot mutate the config
  registry, the keymap registry, or another mode's state.

Why this matters: it makes the user-facing toggle / reload
contract (§9.6) clean by construction. `:disable
lsp-diagnostics-mode` removes every override the mode
contributed, because the registry owns them and the mode has no
way to install state outside the registry's view. Going around
the override system is impossible by API design, not by
convention.

`OptionOverrideSet` is a type-checked bag built via macro (§6.4
shows the syntax). Each entry pairs an option type with a value
of that option's type; the compiler rejects any mismatch.

## 6. Option resolution

### 6.1 Layers (highest to lowest priority)

1. **Modal-state override** -- if any. Rare; reserved for
   options that genuinely depend on Normal vs Insert, e.g.
   `cursor-shape`. Most options don't have this layer.
2. **Buffer-local explicit set** -- the user's `:setlocal foo=bar`
   on this buffer.
3. **Active minor modes** -- in activation order, with explicit
   `OverridePriority` to break ties when two minors set the
   same option. Default priority is "normal"; higher / lower
   priority slots exist for special cases.
4. **Major mode** -- the buffer's major-mode declared
   overrides.
5. **Global** -- `:set foo=bar` (the typed registry's current
   value).
6. **Built-in default** -- the option type's `DEFAULT` const.

First non-empty layer wins. Layers are *not* additive for
scalars (a minor mode's `wrap=true` overrides the major mode's
`wrap=false`, doesn't merge). For collection-shaped options
(e.g. statusline segments, decoration providers, completion
sources) the layers *do* concatenate, with priority controlling
intra-layer order.

### 6.2 Conflict policy within a layer

When two active minor modes set the same scalar option:

- If both declare `OverridePriority::Normal`: **last-activated
  wins**, AND a typed `ModeOptionConflict { option, modes }`
  event is published (visible in `:messages` and the
  introspection buffer). Conflicts are not silent.
- If one is `Normal` and the other is `High` / `Low`: the
  explicit priority wins.
- If both are `High` (or both `Low`): same as both `Normal` --
  last-activated wins, conflict event published.

The intent: *forcing* a winner via priority is for modes that
genuinely need it (e.g. `read-only-mode` overrides any
`writable=true` contribution). Most modes shouldn't touch
priority.

### 6.3 Resolution mechanism + caching

`ResolvedOptions` is a per-buffer struct holding the resolved
value for every option, keyed by `TypeId` (NOT by string name).
**`ResolvedOptions` and the resolver itself live in
`lattice-config`, not `lattice-mode`.** Resolution is a
content-agnostic layered-lookup over the option store: it
walks an iterator of override layers (whatever produced them)
and picks the first non-empty value per option. Modes are one
source of override layers; buffer-local sets are another;
modal-state hooks are a third. The mechanism doesn't care
where layers came from -- it's a registry operation, not a
mode operation. (See §9.3 for the dependency-direction
rationale.)

Recomputed on:

- Mode toggle (major change, minor enable / disable).
- Option write (global `:set` ⇒ every open buffer's cache;
  buffer-local set ⇒ that buffer only).
- Modal-state transition (only for the small subset of options
  with a modal layer).

Hot-path renderer reads via `view.option::<Tabstop>() -> &u64`
are O(1) `TypeId` lookups against the cached struct.
**No layer walk on the keystroke path.**

#### 6.3.1 Eager invalidation, whole-cache recompute (v1)

The v1 invalidation policy is **eager + whole-cache**: any
trigger recomputes the affected buffer's full `ResolvedOptions`
struct. Reads are then pure O(1) lookups against the cache.

The alternative (lazy mark-as-stale + per-option recompute on
read) is more efficient for sparse access patterns, but the
renderer reads most options every frame, so the savings are
marginal. Whole-cache recompute is one code path, branch-free
on the hot path, and bounded in cost: ~30 options × ~10
layers × ~10ns/op ≈ 3µs per recompute on a buffer with 10
active minor modes. Within the §6.3 perf gate.

The known worst case is a global `:set` with many open buffers:
50 buffers × 3µs = 150µs total, well under 1ms (invisible to
the user). At ~500 open buffers it climbs to ~1.5ms which is
visible; if benches surface that as a real workload we'd add
per-option (instead of whole-cache) invalidation as an
optimization. Not blocking v1.

#### 6.3.2 Performance gate (CI bench)

- Option-resolution read p99 < 50ns.
- Recompute on mode toggle p99 < 10µs for a buffer with 10
  active minor modes.

Bench lands in `BENCHMARKS.md` as part of M.2.1.

### 6.4 Option identity: types are keys, strings are metadata

Every built-in option is a unique Rust type implementing the
`Option` trait:

```rust
pub trait Option: 'static {
	type Value: ConfigValue;       // bool / i64 / f64 / String / typed enum / list
	const DEFAULT: Self::Value;
	const DOC: &'static str;
	const CUSTOMIZABLE: bool = true;  // false ⇒ hidden from :set autocomplete + :customize
}
```

Declared via one of two macros (the API split is the
compile-time mechanism that reserves the bare namespace --
see §6.8):

- `editor_options!` (foundation-crate-only; declares
  bare-named options) -- *not* exported from the public API.
  Internal to `lattice-config` / `lattice-mode`.
- `mode_options!(namespace = ..., group = ...)` -- public.
  Always requires a namespace; bare names physically
  impossible from this macro.

Each declaration block binds to a registered `OptionGroup`
(§6.7.1.1):

```rust
// Inside lattice-config (foundation crate; uses the private
// macro):
editor_options! {
	group = EditorGroup;

	/// Width of a tab stop in columns.
	pub Tabstop: u64 = 8;

	/// Show line numbers in the gutter.
	pub Number: bool = false;
}

// Inside lattice-grammar's rust_mode module (uses the public
// macro):
mode_options! {
	namespace = "rust-mode";
	group = EditingGroup;

	/// Indent unit: tabs or spaces.
	pub IndentStyle: enum { Spaces, Tabs } = Spaces;
	pub IndentWidth: u64 = 4;
}
```

The macros generate, for each option, a unit struct (e.g.
`pub struct Tabstop;`) implementing `Option`. Two options with
the same identifier within one declaration block (or one
crate) ⇒ duplicate `struct` definition ⇒ compile error. The
display name is derived from the identifier by macro
(`Tabstop` ⇒ `"tabstop"` for editor options, or
`"rust-mode.indent-width"` after namespace prepending). No
manual `NAME` overrides; the macro is the single source of
both type and string names, so they cannot drift.

Hot-path internal access -- type-driven, monomorphic, zero
runtime string handling:

```rust
let width = config.get::<rust_mode::IndentWidth>();
config.set::<rust_mode::IndentWidth>(2);
```

Mode contributions -- type-checked at compile time:

```rust
fn options(&self) -> OptionOverrideSet {
	overrides! {
		rust_mode::IndentWidth = 4,
		rust_mode::IndentStyle = IndentStyle::Spaces,
	}
}
```

A typo (`IndnetWidth`) or wrong type (`= "four"`) is a
compile error in the mode's own crate.

#### 6.4.1 Tiered uniqueness guarantee

| Origin                              | Guaranteed                                                                                | When                                                                       |
|-------------------------------------|-------------------------------------------------------------------------------------------|----------------------------------------------------------------------------|
| Same crate (built-in)               | No two options share Rust type identifier                                                 | Compile time -- duplicate `struct` definition error.                       |
| Different built-in crates           | No two options share their cross-crate Rust path                                          | Compile / link time -- Rust's type system enforces.                        |
| Different built-in crates -- displayed name collision | No two options share `display_name` (the boundary string) | Program startup, pre-`main`, via `linkme` distributed slice aggregation; panic on duplicate. |
| Built-in vs WIT plugin              | Plugin namespace prefix (`<plugin-id>.`) cannot collide with bare names or mode names     | Plugin load time -- registration rejects shadowing.                        |
| WIT plugin internal                 | No duplicate option names within one plugin                                               | Plugin's own compile time -- the macro generates types in the plugin's source crate; duplicates fail there. |
| Cross-WIT-plugin collisions         | Plugin IDs are globally unique                                                            | Plugin load time -- host-side validation against the plugin registry.      |

This is the strongest set of checks Rust delivers in practice.
True compile-time uniqueness across crates of *string display
names* (as opposed to types) requires a workspace-level
proc-macro reading a manifest file -- fragile, rejected. The
`linkme` startup check is the practical equivalent: every
`cargo run` / `cargo test` smokes it out, and CI catches it on
every push.

#### 6.4.2 Strings only at boundaries

Strings appear only where the editor talks to humans or
non-Rust code:

| Surface             | What                                                          |
|---------------------|---------------------------------------------------------------|
| `:set foo=bar`      | User input. Parsed; the name is looked up in a `&str → TypeId` table populated at registration. |
| `lattice.toml`      | Configuration file. Same `&str → TypeId` lookup as `:set`.    |
| `:describe-option`  | Introspection. Pulls metadata (DOC, default, current value, source layer) from the registry. |
| `:apropos`          | Search. Walks the name table, fuzzy matches.                  |
| WIT plugin manifest | Plugins declare options as data; host registers under the plugin ID's namespace. |

Internal code -- the mode trait, the renderer, decoration
providers, the LSP layer, anything in the workspace -- never
touches strings. `config.get::<T>()` and the `overrides!` macro
are the surface.

### 6.5 Plugin-extended options

Plugins (built-in feature crates and WIT plugins alike) can
register typed options. These participate in the same
resolution layers (§6.1), the same surfaces (`:set`,
`lattice.toml`, `:describe-option`, `:customize`), and the
same cascade events as core options.

#### 6.5.1 Option ownership: not always tied to a mode

Modes own their *override layer* declaratively via
`Mode::options()`. Modes can also *register* new options that
they (or others) read. These are different actions:

- **Registration** -- declaring an option type *exists*. Done
  once per option, at startup (built-in) or plugin load (WIT).
- **Override** -- contributing a value at one of the resolution
  layers (§6.1). Done by modes via `Mode::options()`.

A plugin can register options without owning a mode (a pure
decoration-provider plugin, for instance). A mode can override
options it didn't register (`whitespace-show-mode` overriding
the editor-core `List` option). Keep the two concepts
separate.

Built-in option registration is a `register_options!()`
declaration in the crate's init path (`linkme`-aggregated). WIT
plugin option registration is a static manifest pulled at
plugin load (§6.6).

Plugins can also register new `OptionGroup`s (in the
`<plugin-id>.<group-name>` namespace) or join existing built-in
groups by referencing a built-in group type. Group membership
is unrestricted -- a plugin can declare options into any
registered group including `Editor`. What's reserved is the
*bare namespace*: plugin options are mechanically prefixed by
plugin ID at registration (§6.7.1.1), so a plugin physically
cannot declare a bare `tabstop`. Group is purely organizational;
the behavioral surface reads options by `TypeId`, never by
group.

#### 6.5.2 Namespace policy

Three classes of names live in `lattice.toml` and the typed
registry:

| Display-name pattern     | Owner                                | Examples                                    |
|--------------------------|--------------------------------------|---------------------------------------------|
| Bare names (no prefix)   | Editor                               | `tabstop`, `number`, `wrap`                 |
| `<mode-name>.<key>`      | Built-in mode                        | `rust-mode.indent-width`, `lsp-mode.log-level`, `lsp-completion-mode.idle-delay-ms` |
| `<plugin-id>.<key>`      | WIT plugin                           | `git-blame.delay-ms`, `rainbow-delimiters.colors` |

Plus one structural class that lives in the same TOML file but
is *not* in the typed registry:

| Pattern                  | Owner                                                                          |
|--------------------------|--------------------------------------------------------------------------------|
| `lsp.<server-id>.<key>`  | LSP server config -- forwarded to the server via `workspace/configuration`.    |

The structural class is the existing `lsp_config_tree`
mechanism. It does NOT participate in the typed registry --
it's pure passthrough -- and consequently does not collide
with any of the three typed-registry classes structurally
(server config is always at depth ≥ 3 with the literal `lsp.`
prefix; mode names like `lsp-mode` use a hyphen, not a dot, so
`lsp-mode.<key>` and `lsp.<server>.<key>` cannot intersect).

The macro enforces the prefix at registration:
`mode_options!(namespace = "rust-mode")` auto-prepends
`rust-mode.` to every declared option's `display_name`.
Plugins go through a host-side registration shim that
auto-prepends the plugin ID. Manual override of the prefix is
rejected at compile time (built-in) / load time (plugin).

Plugin IDs are reserved at registration: a plugin called
`tabstop` cannot register because the bare name `tabstop` is
core. The host maintains a reserved-prefix list (the bare
core names) and the plugin registry's set of in-use IDs.

#### 6.5.3 Customizable vs internal

Each `Option::CUSTOMIZABLE` flag controls *user-facing*
visibility:

- `CUSTOMIZABLE = true` (default): option appears in `:set`
  autocomplete, in `:apropos`, in the `:customize` UI buffer
  (post-v1).
- `CUSTOMIZABLE = false`: plugin-internal state. Read / written
  through the registry like any option, but hidden from the
  user-facing surfaces. Equivalent of emacs's `defvar` (vs
  `defcustom`).

Plugin-internal counters, cache sizes, and the like declare
`false`. Anything a user might reasonably tweak declares
`true`.

#### 6.5.4 Validators

Plugin manifests can declare validators on registration:

- `range_i64 { min, max }` / `range_f64 { min, max }`
- `length_bound { min, max }` (strings, lists)
- `enum_set` (already implied by `enum`-typed options)
- `regex(pattern)` -- compiled and cached host-side at
  registration

Built-ins use the same validator type. A `:set` or TOML write
that fails validation produces a typed error echoed to the
user; the previous value is preserved.

For validation logic that can't be expressed declaratively (a
plugin needs "valid cron expression" or "this string must be
an existing file path"), the plugin subscribes to its own
`Event::OptionChanged` and rejects post-hoc by re-setting the
previous value + emitting an error event. Awkward, but
necessary -- callbacks into WASM on every `:set` invocation
violate the §8 budget.

### 6.6 WIT shape for plugin options

The WIT API is structured around the §8 perf budget: per-call
WIT < 500ns p99, host-call overhead amortised. That dictates
*static-declaration registration, event-driven reads, no
host-callable validators*.

```wit
// Pulled by the host at plugin load. Cached. Never re-read.
record option-decl {
	name: string,                  // raw; host prepends plugin-id namespace
	type: option-type,
	default: option-value,
	doc: string,
	customizable: bool,
	validator: option<option-validator>,
}

variant option-type {
	%bool, i64, f64, %string,
	list-of(option-type),
	enum-of(list<string>),
}

variant option-value {
	bool-val(bool), i64-val(s64), f64-val(f64), string-val(string),
	list-val(list<option-value>), enum-val(string),
}

variant option-validator {
	range-i64(range-i64-spec),     // record { min: option<s64>, max: option<s64> }
	range-f64(range-f64-spec),
	length-bound(length-bound-spec),
	enum-set(list<string>),
	regex(string),
}

interface plugin-options {
	/// Plugin's static option manifest. Pulled once at load.
	declared-options: func() -> list<option-decl>;
}
```

#### 6.6.1 Reads on hot paths -- event-driven cache

The host does NOT provide a `get-option` import for hot-path
reads. Plugins that need an option value on the keystroke path
subscribe to `Event::OptionChanged { name }` for the keys they
care about and maintain a local cache. The host emits this
event automatically on any successful set (whatever the
source). Cache is initialised at plugin activation by reading
the current resolved value once.

For cold-path reads (option's value at the moment a plugin
command is invoked), a `get-option` import is provided -- one
WIT round-trip is fine for one-off reads.

#### 6.6.2 Writes

Writes (`set-option`) cross WIT but are rare (driven by user
`:set` or programmatic plugin actions, not per-frame). Host
validates against the declared validator; rejection returns a
typed error.

#### 6.6.3 Type subset

What the WIT can express: `bool`, `i64`, `f64`, `string`,
`list<T>`, `option<T>`, `enum-of(strings)`. No structs, no
trait objects, no closures. The 90% case fits comfortably; the
remaining 10% can decompose into multiple options.

### 6.7 `:set` and `:customize`: two surfaces, two scopes

Every option type carries enough metadata for both:

- `DOC` -- prose description (shown by `:describe-option`,
  rendered as inline help in the customize form).
- `Value` type -- determines the input widget (`bool` ⇒
  toggle, `enum` ⇒ dropdown, `i64` ⇒ stepper or text input,
  `string` with regex validator ⇒ validated text input).
- `DEFAULT` -- "reset to default" affordance.
- `CUSTOMIZABLE` -- hides plugin-internal state from both
  `:set`'s autocomplete and `:customize`'s group views.
- Validator -- inline error rendering on invalid input.

#### 6.7.1 Scope distinction

`:set` and `:customize` overlap in *power* (both call into the
same registry to mutate the same store) but differ in *scope*:

| Surface              | Scope                                                            | Why use it                                                                                          |
|----------------------|------------------------------------------------------------------|-----------------------------------------------------------------------------------------------------|
| `:set <opt>=<val>`   | One option                                                       | Quick one-off tweak from the cmdline. Already know the option name. Want immediate cmdline feedback. |
| `:customize <name>`  | A focused mode view OR an explicit cross-mode group              | Exploring or configuring a *related set* of options.                                                |

`<name>` resolves to one of two things:

- **A registered mode** (always ends in `-mode`) -- focused
  view. `:customize lsp-completion-mode` shows only that
  mode's options.
- **A registered `OptionGroup`** (never ends in `-mode`) --
  cross-mode collection. `:customize lsp` shows every option
  whose `Group` is `LspGroup`, sectioned by owning mode for
  readability. `:customize editor` shows every option in the
  bare-named editor group.

There is no ambiguity: mode names always end in `-mode`,
group names cannot end in `-mode`. The convention is enforced
at registration.

`:customize` with no args opens a picker
(`customize-group-picker-mode` buffer) listing every
registered group plus every mode with at least one
customizable option.

**Origin-agnostic.** The mechanism is identical for built-in
modes and WIT plugin modes. `:customize lsp` works the same
way whether the LSP modes ship in `lattice-lsp` or as a
third-party plugin -- the option-and-group registries are the
single source of truth, distribution is a build-system
concern.

Server-side LSP config (`lsp.<server-id>.<key>`) is *not*
included in `:customize lsp` -- those are structural
passthrough (§6.5.2), not typed-registry options. The user
hand-edits them in `lattice.toml` for now; a future slice may
surface them in a parallel "server settings" buffer that
shares the form-row primitive.

#### 6.7.1.1 The `OptionGroup` registry

`OptionGroup` is a registered entity, parallel to `Option`:

```rust
pub trait OptionGroup: 'static {
	const NAME: &'static str;       // user-facing identifier; cannot end in `-mode`
	const DOC: &'static str;        // shown in :describe-group, group picker
}

groups! {
	/// Bare-named editor options (`tabstop`, `number`, `wrap`, ...).
	pub Editor;

	/// Display-related options across modes (line numbers, wrap,
	/// whitespace visualisation, current-line highlight).
	pub Display;

	/// Editing-related options (search, indent, auto-pair, ...).
	pub Editing;

	/// Every option owned by an LSP mode (`lsp-mode`,
	/// `lsp-completion-mode`, `lsp-diagnostics-mode`, ...).
	pub Lsp;

	/// Completion across providers (LSP completion, snippets,
	/// buffer-words, paths).
	pub Completion;

	pub Picker;
	pub Filetree;
	pub Oil;
	pub Help;
	pub Appearance;        // theme, colours, sprite icons
}
```

Same compile-time / link-time uniqueness story as options
(§6.4.1): types are keys, `linkme` aggregates across crates,
duplicate display names panic at startup. The `groups!` macro
also emits a compile-time assertion that the derived name does
not end in `-mode` (`const _: () = assert!(...)` against a
`const fn` byte-walk) -- enforces the modes-vs-groups
disambiguation rule (§6.7.1) at build time, not runtime. The
parallel `mode_decl!` macro asserts the *opposite* (mode names
*must* end in `-mode`), also at compile time.

Options bind to a group at declaration via the `mode_options!`
or `options!` macro:

```rust
mode_options! {
	namespace = "lsp-completion-mode";
	group = LspGroup;            // default for all options below

	pub IdleDelayMs: u64 = 100;
	pub MaxResults: u64 = 50;

	#[group(CompletionGroup)]   // override for this option
	pub TriggerOnDot: bool = true;
}
```

A mode block declares a default group; per-option `#[group(...)]`
overrides for cross-cutting cases. Both reference registered
group types, so typos fail at compile time.

Plugins register `OptionGroup`s via the same WIT static-manifest
mechanism that registers options (§6.6); plugins can also join
existing built-in groups (a `git-blame-mode` plugin's options
can declare `group = LspGroup` if they're LSP-related, or
`group = GitBlameGroup` for plugin-private organization).

Group membership is **not** access-controlled. Any registered
plugin can declare options into any registered group, including
`Editor`. What *is* reserved is the bare namespace itself: a
plugin's WIT manifest goes through host-side registration that
mechanically prepends `<plugin-id>.` to every option name, so
plugin options are always namespaced (`git-blame.tabstop`,
never bare `tabstop`). A plugin author who chooses to declare
`git-blame.advanced-tabstop` into the `Editor` group makes a
questionable organizational choice -- the option is still
clearly labelled by its namespace, just listed in the wrong
section -- but no integrity invariant is violated. The
behavioral surface (renderer, dispatch, LSP layer) reads
options by `TypeId`, never by group; group membership only
affects `:customize` organization.

Group *display names* are unique by construction: linkme
aggregation panics at startup on duplicate names. A plugin
cannot declare a new group called `Editor` (or any other name
already in use), so name-shadowing isn't possible.

`:describe-group <name>` shows the group's doc, the modes
contributing to it, and (when applicable) the options listed
under it. M.8 introspection adds this command alongside
`:describe-mode`.

#### 6.7.1.2 Hierarchy is post-v1

Emacs supports nested groups (`programming` ⊃ `lsp` ⊃
`lsp-completion`). Lattice's v1 groups are flat. The "all LSP
options" use case is handled by the flat `Lsp` group; finer
breakdowns are handled by selecting a specific mode
(`:customize lsp-completion-mode`).

A future slice can add a `parent: Option<GroupId>` to
`OptionGroup` and a `:customize <parent>` walk that includes
descendants. Schema-compatible: today's groups become roots in
the future tree. No migration needed when hierarchy lands.

### 6.8 Constraint enforcement: where each check lands

Wherever Rust's type system or const evaluation lets us push a
check to compile time, we should. The remaining runtime checks
are the ones that can't be made compile-time without
fundamentally changing the architecture (e.g. requiring a
workspace-wide build-script manifest).

| Constraint                                                   | Built-in Rust path                                                                                                            | WIT plugin path                                                                                                              |
|--------------------------------------------------------------|-------------------------------------------------------------------------------------------------------------------------------|------------------------------------------------------------------------------------------------------------------------------|
| Option type identity is unique                               | **Compile time** (Rust types are unique by their fully-qualified path)                                                        | N/A (plugins use string names; see below)                                                                                    |
| Same-crate option / group display-name collision             | **Compile time** (`options!` / `groups!` macros generate type identifiers from the option's identifier; duplicates ⇒ duplicate `struct` ⇒ compile error) | **Plugin compile time** (same mechanism if the plugin uses a wit-bindgen-style macro to declare options)                     |
| Cross-crate display-name collision                           | Link/startup (`linkme` distributed slice; duplicate display names panic before `main` runs)                                   | Plugin load time (host registry rejects)                                                                                     |
| Mode name ends in `-mode`                                    | **Compile time** (`const fn` byte-walk in `mode_decl!` macro emits `const _: () = assert!(...)`; failure is a compile error) | Plugin load time                                                                                                             |
| Group name does *not* end in `-mode`                         | **Compile time** (same `const fn` pattern in the `groups!` macro)                                                             | Plugin load time                                                                                                             |
| Bare namespace reservation (no plugin can declare bare names) | **Compile time** (`editor_options!` macro is *not* exported from the public API; only `mode_options!(namespace = ...)` is, and it requires a namespace argument. Declaring a bare name from outside the foundation crates fails as "unresolved macro") | **Plugin load time** (host mechanically prepends `<plugin-id>.` to every option name in the manifest; bare names impossible by construction) |
| Group reference resolves                                     | **Compile time** (`group = LspGroup` is a type reference; unknown type ⇒ compile error)                                       | Plugin load time (string lookup against the host's group registry)                                                           |
| Plugin ID uniqueness                                         | N/A                                                                                                                           | Plugin install/load time                                                                                                     |

Two notes on the runtime rows:

- **Cross-crate display-name uniqueness.** Strictly compile-time
  enforcement across crates would require a workspace-level
  build script reading a central manifest of all option names.
  That works only for built-in crates (not plugins) and is
  fragile (manifest can drift from code). The `linkme` startup
  check is the practical equivalent: every `cargo run` /
  `cargo test` exercises it, every CI run catches a duplicate
  before release.
- **WIT plugin checks** can be pushed earlier with
  wit-bindgen-style tooling that runs the same const-assert
  / type-collision checks at the plugin's own compile time
  -- the plugin author gets a build error in their crate
  rather than a "plugin failed to load" error from the host.
  Worth pursuing as a v1.x developer-experience improvement;
  not a v1 requirement, since the host-side check is
  authoritative either way.

The principle: **for built-in modes, every constraint that
matters lands at compile time except cross-crate display-name
uniqueness.** Plugins absorb a few extra checks at plugin-load
time as the unavoidable cost of being external code.

#### 6.7.2 Buffer-backed form: TUI parity is non-negotiable

`:customize` opens a buffer in `customize-mode`. It is not a
GPUI-only feature. The buffer's content is the form rendered
as a sequence of rows; the *rendering* of those rows differs
between TUI and GUI surfaces, but the buffer model, the
keymap, the navigation, the apply / cancel / reset commands,
and the underlying option store are identical:

- **TUI rendering (ratatui).** Each row is a line: label,
  current value (with type-aware formatting), source layer
  ("from `lsp-mode` (default)", "from `:set` (global)", "from
  `~/.config/lattice/lattice.toml`"), doc snippet. Editing
  uses inline edit -- a popup over the row, or in-place
  rich-minibuffer edit, or a dedicated edit pane depending on
  the value type. Validation errors render as decorations on
  the row.
- **GUI rendering (GPUI).** Same buffer, same rows, same
  metadata. Richer widgets where they help -- color picker
  for theme options, slider for ranges, multi-select for
  list-of-enum -- but never *required*: every option type
  has a TUI-compatible edit affordance, and the GUI just
  upgrades the rendering.

This works because the customize buffer is just another
buffer (`customize-mode` major). The "everything is a buffer"
commitment (§5.9 of DESIGN.md, §3 of this doc) means the same
rendering pipeline that paints documents, help, file-tree
buffers also paints customize. No GUI-only path, no
separate-form-view component.

#### 6.7.3 Apply semantics

Submitting an edit calls into the same `:set` machinery
(through the registry's typed-write path). Each edit is a
discrete `:set` call -- not batched -- so the option-changed
cascade fires per option, decorations refresh, errors echo
inline.

Without TOML write-through (deferred), all changes are
session-only. The form is useful for exploration: tweak
values, see effects, pick the values you want, then either
keep them for the session or commit them to your `lattice.toml`
by hand. The post-v1 write-through slice changes the trailing
step (a "Save" command writes through `toml_edit`,
preserving the user's existing file structure and comments)
without changing the buffer model or the apply path.

Implication: shipping the form view in v1 with apply-only
semantics doesn't lock anything in. The metadata exists from
M.1 onwards; M.9 (§10) adds the form rendering; the eventual
write-through slice extends apply semantics with persistence
without rework.

## 7. Lifecycle events

Mode lifecycle rides the existing typed event bus (DESIGN.md
§5.10). Plugins / other modes subscribe with the same filter
machinery as any other typed event:

```rust
pub enum ModeEvent {
	MajorEntered { buffer: BufferId, mode: ModeId },
	MajorExiting { buffer: BufferId, mode: ModeId },
	MinorActivated { buffer: BufferId, mode: ModeId },
	MinorDeactivated { buffer: BufferId, mode: ModeId },
	OptionConflict { buffer: BufferId, option: &'static str, modes: SmallVec<[ModeId; 2]> },
}
```

`MajorEntered` is the load-bearing one for setup work --
parser attach, server lookup, default minor activation. The
trait's `on_activate` runs first; *then* the event publishes,
so subscribers see a buffer in a consistent state.

`MajorExiting` runs *before* state teardown -- subscribers can
inspect what's about to be torn down. The trait's
`on_deactivate` runs after the event drains.

### 7.1 Deactivation is synchronous; teardown can be async

`on_deactivate` is a synchronous call: it returns to the user
immediately so toggle commands don't block. But torn-down state
may include resources whose actual release is async -- closing
an LSP server connection, draining a watch channel, etc.

The contract:

1. **Logical deactivation** (synchronous, immediate): the
   registry pops the mode's overrides, deregisters its
   subscriptions, runs `on_deactivate`. Hot paths see the
   mode as gone instantly.
2. **Physical teardown** (asynchronous, background): if
   `on_deactivate` started cleanup work (e.g. spawned a future
   to close a server), that work continues post-event. The
   mode's `MinorDeactivated` / `MajorExiting` event has
   already fired; subscribers are responsible for handling
   "the resource the mode managed may still be in mid-shutdown".

Concrete consequence for LSP: deactivating `lsp-mode` returns
to the user immediately (`:disable lsp-mode` echoes "off"
straight away). A `publishDiagnostics` already in flight from
the server may land *after* deactivation; the diagnostics
subscriber must check whether the mode is still active and
drop the message if not. The check is cheap (one ResolvedOptions
read) and the failure mode is a stale diagnostic, not a crash
or a corrupted view.

## 8. Crate placement

Three shapes evaluated:

| Shape | Pros                                                                                           | Cons                                                                                  |
|-------|------------------------------------------------------------------------------------------------|---------------------------------------------------------------------------------------|
| (A) New `lattice-mode` crate with trait + registry + lifecycle (option resolver lives in `lattice-config`; see §9.3) | Mode infra testable / benchmarkable in isolation. Doesn't bloat `lattice-core`. Foundation for both built-in and WIT plugin paths. | One more crate.                                                                       |
| (B) Mode infra inside `lattice-core`                                                            | One fewer crate.                                                                                | Conflates "engine" with "mode machinery." Mode-registry tests can't run without spinning up a full core context. |
| (C) Per-mode crate (`lattice-mode-lsp`, `lattice-mode-help`, ...)                               | Maximum modularity.                                                                             | Overkill -- modes naturally belong with the feature crate they parametrize.           |

**Recommendation: (A).** New crate `lattice-mode`. Plus an
**M.2 expansion of `lattice-config`** to own the option /
group / resolver primitives -- modes contribute via
`Mode::options()`, but the resolver and `ResolvedOptions` live
in `lattice-config` (resolution is a registry operation, not a
mode operation; see §6.3 / §9.3). Resulting layout:

```
crates/lattice-mode/                    # M.1 + M.2.1
├── lib.rs              # re-exports
├── mode.rs             # Mode trait, ModeKind, ModeId
├── registry.rs         # ModeRegistry; major / minor lookup; activation API
├── active.rs           # ActiveModes (per-buffer); push / pop minors
├── capability.rs       # CapabilitySet bitfield
├── error.rs            # ModeActivationError
├── event.rs            # ModeEvent variants
├── context.rs          # ModeContext (read-only handle passed to lifecycle hooks)
├── contributions.rs    # mode_options! macro (delegates to lattice-config),
│                       # OptionOverrideSet re-export, decoration / subscription stubs
├── adapter.rs          # ModeAdapter trait for WASM-plugin modes (M.10)
└── tests/              # standalone trait + registry + resolution tests

crates/lattice-config/                  # M.2.0 expansion
├── ... existing modules ...
├── option.rs           # Option trait, options! / editor_options! macros
├── group.rs            # OptionGroup trait, groups! macro
├── registry.rs         # ConfigRegistry (extended: linkme aggregation, type-id ↔ erased)
├── overrides.rs        # OptionOverride, OptionOverrideSet, OverridePriority
├── resolver.rs         # Resolver: walks layered overrides, produces ResolvedOptions
└── resolved.rs         # ResolvedOptions cached snapshot
```

Dependencies (as of M.2):

- `lattice-config` is foundational. Defines option /
  group / resolver primitives. No upstream dep on `lattice-mode`.
- `lattice-mode` depends on `lattice-config` (re-exports
  `OptionOverrideSet` for `Mode::options()`; the
  `mode_options!` macro delegates to `lattice-config`'s
  `options!`).
- `lattice-core` depends on both: `Document` carries
  `ActiveModes` (from `lattice-mode`) + `ResolvedOptions`
  (from `lattice-config`) + `buffer_local_overrides`
  (`OptionOverrideSet`); orchestrates `recompute_options()`
  by stitching layered input from active modes, buffer
  locals, and modal state.
- `lattice-lsp` depends on `lattice-mode` + `lattice-config`;
  declares `lsp-mode` + sub-minors and registers their
  options.
- `lattice-grammar` depends on `lattice-mode` +
  `lattice-config`; declares language major modes.
- `lattice-ui-tui` depends on `lattice-mode` +
  `lattice-config`; renderer reads `ResolvedOptions`, drops
  `BufferKind` matches.
- `lattice-ui-gpui` (future): same shape.

Per-mode files live with the feature crate. `rust-mode.rs` in
`lattice-grammar/src/modes/`. `lsp_mode.rs` and the sub-minors
in `lattice-lsp/src/modes/`. `help_mode.rs` etc. in their owner
crates.

## 9. Integration with existing systems

### 9.1 Keymap registry

The layered keymap registry in `keymap-architecture.md` §5-6
already supports per-major-mode and per-minor-mode layers
(priority slots 2 and 3 respectively). Mode activation /
deactivation pushes / pops the corresponding layer. **No keymap
work in the foundation slice (M.1)** -- the keymap layer is
ready; we just feed it from the mode registry.

### 9.2 Event bus

DESIGN.md §5.10 already commits to a typed event bus with
filter-based subscriptions. Mode lifecycle events (§7) are
just additional payload variants. Mode-declared subscriptions
register with the bus on activation, deregister on
deactivation.

### 9.3 Configuration registry

DESIGN.md §5.12 + the existing `lattice-config::ConfigRegistry`
hold typed options as the global-layer store. **M.2 expands
`lattice-config` to own not just the store but the resolver
mechanism + cached `ResolvedOptions`** -- modes contribute
override layers via `Mode::options()` (which returns a
`lattice-config`-defined `OptionOverrideSet`), but the
*resolution* of those layers is a registry operation, not a
mode operation. Why this dependency direction:

- Modes are *one* source of override layers. Buffer-local
  `:setlocal` is another. Modal-state hooks are a third. The
  resolver doesn't care where layers came from; it walks them
  in priority order and picks the first non-empty value per
  option. That's a registry concern.
- Putting the resolver in `lattice-config` lets non-mode code
  (a future feature flag, a profiling overlay, ...) layer
  overrides on top of the registry without depending on
  `lattice-mode`. The mode system is the largest contributor
  but not the only conceivable one.
- `lattice-mode` becomes a thin contributor: defines
  `Mode::options() -> OptionOverrideSet` and the
  `mode_options!` macro that delegates to
  `lattice-config`'s `options!`. No resolver code; no
  `ResolvedOptions` ownership.

The registry's existing `Event::OptionChanged` cascade
(`app/options.rs` `drain_option_changes`) extends to invalidate
the `ResolvedOptions` cache on writes. Mode-toggle invalidation
is driven by callers when they invoke `ModeRegistry::activate_*`
(see §9.4 -- `Document::recompute_options` after activation
returns).

The existing `OptionHandle<T>` API is the seed of the
type-keyed surface in §6.4. M.2.0 promotes the type to be the
*primary* identity (not a perf optimization on top of a
string-keyed registry):

- `linkme::distributed_slice` aggregates every `options!`
  declaration across all `lattice-*` crates at link time.
- The registry is built from the aggregated slice at startup
  (`pre-main` initialiser). Two specs declaring the same
  display name → panic before any user-visible work.
- Internal access keys on `TypeId` (or, equivalently, the
  monomorphised `OptionHandle<T>`). The `&str → TypeId` map is
  built alongside for boundary lookups (`:set`, TOML).
- Plugin-registered options use a parallel
  `Box<dyn ErasedPluginOption>` storage keyed by the prefixed
  display name (since plugins lack host types). Both type-keyed
  and erased options live in the same registry from the
  consumer's POV; the dual storage is internal.
- The imperative `Option::new()` constructor is removed as a
  public API -- the macros are the single way to declare
  built-in options. An internal `register_erased(...)` path
  survives `pub(crate)` for the WIT plugin adapter (since
  plugins don't have host Rust types).

There is exactly one config file: **`lattice.toml`**. Per the
loader (`lattice-config/loader.rs`), read order is
`~/.config/lattice/lattice.toml` (user) followed by
`<workspace_root>/.lattice/config.toml` (project), with project
overriding user at scalar leaves. There is no separate
`lsp.toml`, no `options.toml`, no per-plugin config file.
Everything lives in the single `lattice.toml` namespace
governed by §6.5.2.

### 9.4 Buffer / Document model

In the code, the per-buffer-state container is
`lattice_core::Document`; `Buffer` is the thin rope wrapper.
This doc uses "buffer" colloquially.

Today: `BufferKind` enum with four variants
(`Document | Help | FileTree | Oil`) lives in `lattice-ui-tui`.
The earlier draft committed to "M.3 retires it"; **M.3.1
revises that commitment** -- the enum stays as a *storage-shape
discriminator* while major-mode IDs handle the *behavior*
discriminator role. The two roles were conflated; M.3 separates
them rather than retiring one.

**Two roles, one decision per role:**

- **Storage-shape discriminator.** `BufferData::Document(_)` /
  `Help(_)` / `FileTree(_)` / `Oil(_)` carries fundamentally
  different runtime structs (actor handle + tree-sitter cache
  vs rendered prose + links vs tree-of-files vs editable
  directory listing). This information cannot disappear; the
  variants are typed payloads, not just tags. The enum (likely
  renamed to `BufferStorage`) continues to serve this role.
- **Behavior discriminator.** "Is this read-only?", "which
  default keymap?", "which renderer paints it?", "what
  options does this kind contribute?" — all answered through
  the active major mode and resolved options. Per
  `mode-architecture.md` §6.1 the resolver overlays mode
  contributions on top of the registry's defaults; per §3.4
  modes are user-toggleable, so capability flags like
  `ReadOnly` are real options that any mode can flip rather
  than enum-baked properties.

**`ReadOnly` as the canonical example (M.3.1).** `ReadOnly:
bool = false` is a registered editor option with
`CUSTOMIZABLE = false` (mode-driven, not user-typed). Major
modes for read-only buffer kinds (`HelpMode`, `FileTreeMode`,
`LspLogMode`, `LspTraceLogMode`, `LspServerLogMode`) declare
`Mode::options()` returning `overrides! { ReadOnly = true }`.
At buffer creation, `App::activate_major_for_buffer_kind`
resolves the right major (via `resolve_major_mode(kind, lang)`),
calls `ModeRegistry::activate_major`, and triggers
`recompute_options_for_buffer`. The user-facing
`BufferKind::is_read_only()` becomes
`app.resolved_option::<ReadOnly>(buffer_id)` — same answer,
sourced through one mechanism.

This pattern generalises: `wrap`, `line-numbers`,
`current-line-highlight`, etc. all become mode-contributable
options as the relevant minor modes (`wrap-mode`,
`line-numbers-mode`, ...) land in M.7.

#### Buffer-local mode-internal data — Shape A direction (M.3.2 target)

A separate question is where mode-specific *runtime data*
lives — the `SyntaxHandle` for `rust-mode`, the
`Vec<FileTreeEntry>` for `file-tree-mode`, the `Vec<Link>` for
`help-mode`, oil's snapshot for diffing. Today these are
fields on the `BufferData` variants. Long-term they belong on
a typed-map of **buffer-locals** owned by the modes that
populate them — a typed Rust analogue of emacs's
`buffer-local-variables`.

Architecture sketch (M.3.2 lands this):

```rust
pub struct BufferEntry {
	pub id: BufferId,
	pub flags: BufferFlags,
	pub storage: BufferStorage,    // (rope, cursor, universal state)
	pub locals: BufferLocals,      // typed-map of mode-owned data
}

pub trait BufferLocal: Any + Send + Sync + 'static {
	const NAME: &'static str;        // "file-tree.entries"
	const DOC: &'static str;
	const OWNER_MODE: &'static str;  // mode id that owns this local
	fn describe(&self) -> String;    // for :describe-buffer
}
```

Modes contribute locals in `on_activate`, remove in
`on_deactivate`. The `OWNER_MODE` const enforces "only the
owning mode can mutate this local" at the registry surface.
`:describe-buffer` walks the map and groups entries by their
owner mode, giving inspection of every piece of state a buffer
carries.

**Why deferred:** the migration from "per-variant fields" to
"buffer-locals" touches every site that accesses kind-specific
data (`entry.file_tree().entries` etc.). Substantive but
mechanical; warrants its own slice (M.3.2.a infrastructure +
M.3.2.b/c per-kind migrations) rather than mixing with the
ReadOnly demonstration.

**Where this leaves `BufferStorage`:** after M.3.2 it carries
only the *universal payload* (typically just rope + cursor
fields the storage type needs to expose). At that point we
revisit whether the enum still earns its keep or whether
every buffer collapses to one struct with all kind-specific
data living in `BufferLocals`. That decision waits until M.3.2
is complete and we can see what's left.

**Where mode-system state actually lives** (M.2.1 implementation
note that supersedes earlier doc text):

- `Document.modes: ActiveModes` (M.1) -- present on the
  document, but `Document` lives behind the runtime actor's
  snapshot path so reads aren't synchronously available to the
  App. M.4 promotes `ActiveModes` to `DocumentSnapshot` so the
  fields converge on Document.
- `App.active_modes: HashMap<BufferId, ActiveModes>` (M.2.1)
  -- the canonical map the App reads. Populated by
  `ModeRegistry::activate_*` calls, keyed by the App's
  `buffers::BufferId`.
- `App.buffer_local_overrides: HashMap<BufferId, OptionOverrideSet>`
  (M.2.1) -- buffer-local explicit `:setlocal` overrides.
- `App.resolved_options: HashMap<BufferId, ResolvedOptions>`
  (M.2.1) -- the cached snapshot the renderer reads.

The cache lives on App rather than `Document` because
`lattice-core` cannot depend on `lattice-config`
(`lattice-config` already depends on `lattice-core` for
`FoldMethod`). Putting `ResolvedOptions` on Document would
require either inverting that dep or moving `FoldMethod` to a
lower layer. M.4 -- when the renderer reads through
`DocumentSnapshot` -- is the natural point to revisit; until
then the App is the orchestrator.

`App::recompute_options_for_buffer(buffer)` is the
orchestrator. After any layer change (mode toggle,
buffer-local set, modal-state transition for modal-keyed
options), the caller invokes it. Pseudocode:

```rust
fn recompute_options_for_buffer(&mut self, buffer: BufferId) {
	let mut resolved = ResolvedOptions::new();
	self.config.bootstrap_resolved_with_current_values(&mut resolved);

	let modes = self.active_modes.get(&buffer).cloned().unwrap_or_default();
	let mut mode_contributions = Vec::new();
	if let Some(major) = modes.major().and_then(|id| self.mode_registry.get(id)) {
		mode_contributions.push(major.options());
	}
	for &id in modes.minors() {
		if let Some(m) = self.mode_registry.get(id) {
			mode_contributions.push(m.options());
		}
	}

	let buffer_local = self.buffer_local_overrides
		.get(&buffer).cloned().unwrap_or_default();
	let modal_layer = OptionOverrideSet::new();  // M.7

	let mut layered: Vec<&OptionOverrideSet> = vec![&modal_layer, &buffer_local];
	for set in mode_contributions.iter().rev() {
		layered.push(set);  // last-activated minor highest in walk
	}

	Resolver::new().resolve_into(layered, &mut resolved);
	self.resolved_options.insert(buffer, resolved);
}
```

Reads via `App::resolved_option::<D>(buffer)` are O(1)
TypeId lookups against the cached snapshot, with a fallback to
`config.get_typed::<D>()` for the transient pre-recompute
window.

Capability checks (`is_read_only`, `accepts_writes`) become
queries on the resolved mode set:
`app.active_modes[&buffer].has_minor(MODE_READ_ONLY)`. Cleaner
and -- crucially -- extensible (any mode can flip a
capability).

### 9.5 Renderer

The renderer reads `ResolvedOptions` for every per-frame
decision: wrap, line numbers, gutter width, foldcolumn,
statusline contributors, decoration providers. No `BufferKind`
match in the render path. Hover popup is a floating-geometry
view of a `markdown-mode` buffer with a `hover-mode` minor
contributing `wrap=true, line-numbers=false, anchor=cursor`.

### 9.6 Toggle and reload semantics

Modes are user-toggleable and reloadable. The mechanisms:

#### 9.6.1 Auto-generated ex-commands

Every registered mode auto-generates an ex-command with its
canonical name:

| Action                       | What runs                                                                              |
|------------------------------|----------------------------------------------------------------------------------------|
| `:rust-mode` on a buffer not in `rust-mode`                | Deactivate current major (and its default minors); activate `rust-mode`; auto-activate its default minors. |
| `:rust-mode` on a buffer already in `rust-mode`            | Reload: deactivate, then re-activate. Idempotent setup contract makes this safe.       |
| `:lsp-diagnostics-mode` (no args) on a buffer where it's inactive | Activate, validating capabilities + conflicts.                              |
| `:lsp-diagnostics-mode` on a buffer where it's already active     | Toggle off: deactivate, pop overrides.                                       |

Plus generic forms for explicit semantics:

- `:major-mode <name>` -- switch the active major.
- `:enable <minor-name>` -- turn on (no-op if already on).
- `:disable <minor-name>` -- turn off (no-op if already off).
- `:toggle <minor-name>` -- flip.

The auto-generated ex-commands match emacs muscle memory
(`M-x rust-mode`); the explicit verbs match vim sensibilities
(`:enable` / `:disable`). Both work on every registered mode.

#### 9.6.2 Idempotent setup contract

`on_activate` may be called more than once in a buffer's
lifetime. Every call is preceded by `on_deactivate` if the
mode was previously active. Heavy work in `on_activate` is
fine -- that's what reload is *for* -- but it must be safe to
repeat. Implementations check existing state before allocating;
they don't assume "first activation."

#### 9.6.3 Clean teardown by construction

Because mode contributions are declarative (§5.2) and the
registry owns the layer stack, `:disable lsp-diagnostics-mode`
removes every option override, every keymap entry, every
event subscription, every decoration provider the mode
contributed -- all at once, via the registry, before
`on_deactivate` even runs. The mode cannot leak contributions
because it never installed them directly.

The remaining surface for leakage is side effects done in
`on_activate` (server connection, file watcher). Those are
the mode's own responsibility to clean up in `on_deactivate`.
The synchronous-deactivation / async-teardown contract (§7.1)
applies.

#### 9.6.4 What the user sees

`:list-modes` shows the active major + active minors for the
current buffer, plus all *registered* modes (active or not),
keyed by name. `:describe-mode <name>` shows the mode's
declared options, keymap entries, event subscriptions, and
decoration providers; if the mode is active, also shows which
of its options are currently winning the resolution stack.
`:describe-option-resolution <name>` shows the layer stack for
one option: which mode (or which surface -- buffer-local,
global, default) provided each layer's value, and which layer
won.

This is the introspection commitment from §1; M.8 implements
it.

## 10. Migration plan

| #         | Slice                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          | Crate(s)                                                                    | Done when                                                                                                                                                                                                                     |
|-----------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|-----------------------------------------------------------------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| M.0       | This doc (mode-architecture.md) reviewed + accepted. DESIGN.md §5.6 / §5.8 / §5.9 / §5.10 / §5.12 augmented with links + the §5.8.3 correction. No code.                                                                                                                                                                                                                                                                                                                                                                                                                       | `docs/`                                                                     | User reviews + signs off; DESIGN.md links land.                                                                                                                                                                               |
| M.1       | New `lattice-mode` crate. `Mode` trait, `ModeRegistry`, `ActiveModes` on `Document` (the actual lattice-core per-buffer-state container; `Buffer` is the rope wrapper), lifecycle event variants. No actual modes registered. Tests for registration, conflict, capability checks.                                                                                                                                                                                                                                                                                             | new `lattice-mode`, `lattice-core`                                          | `cargo test -p lattice-mode` green; `Document` carries `ActiveModes` (empty by default).                                                                                                                                      |
| M.2.0     | **`lattice-config` types-as-keys + resolver primitives.** `Option` trait, `options!` / `editor_options!` macros, `OptionGroup` trait + `groups!` macro, `linkme` aggregation, `OptionOverride` / `OptionOverrideSet`, `Resolver`, `ResolvedOptions`. Migrate every existing built-in option from `Option::new()` to the macro form. Remove the public `Option::new()` constructor (keep `register_erased` `pub(crate)` for the M.10 plugin adapter).                                                                                                                           | `lattice-config` (callers in every crate that registers options)            | Macro API is the only public surface; existing options work identically; `resolve(layered)` returns `ResolvedOptions` correctly for any iterator of layers; cross-crate display-name uniqueness panics at startup.            |
| M.2.1     | **Mode-driven layers + App orchestration.** `Mode::options() -> OptionOverrideSet` real shape (`OptionOverride` / `OptionOverrideSet` / `OverridePriority` moved to `lattice-mode` to break the cycle); `lattice-config::overrides!` proc macro for compile-time-typed override construction; App carries `active_modes` / `buffer_local_overrides` / `resolved_options` keyed by `BufferId`; `App::recompute_options_for_buffer(...)` stitches layered input; `App::resolved_option::<D>(buffer)` for type-keyed reads. Bench in BENCHMARKS.md for resolution + invalidation. | `lattice-mode`, `lattice-config`, `lattice-config-macros`, `lattice-ui-tui` | `resolved_get_typed` p99 < 50ns (measured ~13.5ns); `resolve_into_10_layers` p99 < 10µs (measured ~851ns). Mode toggles refresh the cache; reads are O(1).                                                                    |
| M.3.0     | Declare every built-in major mode (`text-mode`, `rust-mode`, `python-mode`, `javascript-mode`, `markdown-mode`, `help-mode`, `file-tree-mode`, `oil-mode`, `lsp-log-mode`, `lsp-trace-log-mode`, `lsp-server-log-mode`). Self-register at App boot via per-crate `register_*_modes` helpers. Pure declarations -- empty `options()` etc.                                                                                                                                                                                                                                       | `lattice-mode`, `lattice-syntax`, `lattice-lsp`, `lattice-ui-tui`           | Every mode is reachable via `mode_registry.is_registered(...)`; per-mode unit tests (id uniqueness, kind, registry population) green.                                                                                         |
| M.3.1     | `ReadOnly` core option (Editor group, `customizable = false`); read-only majors (`HelpMode`, `FileTreeMode`, `LspLogMode`, `LspTraceLogMode`, `LspServerLogMode`) contribute `overrides! { ReadOnly = true }` via `Mode::options()`. App activates the resolved major at buffer creation (`activate_major_for_buffer_kind`) and triggers `recompute_options_for_buffer`. `BufferKind::is_read_only` callers shift to `app.resolved_option::<ReadOnly>(buffer_id)`.                                                                                                             | `lattice-config`, `lattice-mode`, `lattice-lsp`, `lattice-ui-tui`           | Help / FileTree / LSP-log buffers resolve `ReadOnly = true`; Document / Oil resolve `false`. End-to-end mode-driven option pipeline validated on a real piece of buffer state.                                                |
| M.3.2.a   | **Buffer-locals foundation.** New `BufferLocal` trait + `BufferLocals` typed-map in `lattice-mode`. `ModeContext` rewritten to carry `&mut BufferLocals` + `current_mode` for the OWNER_MODE check. `Mode::on_activate`/`on_deactivate` signatures change to `&mut ModeContext<'_>`. Registry's activation methods thread `&mut BufferLocals` through. App carries `buffer_locals: HashMap<BufferId, BufferLocals>`. New `WrongOwnerMode` error variant. No per-kind data migration in this slice.                                                                             | `lattice-mode`, `lattice-ui-tui`                                            | A test mode's `on_activate` populates a `BufferLocal`, `on_deactivate` removes it, `:describe-buffer`-style descriptor iteration returns expected entries.                                                                    |
| M.3.2.b.1 | **Mirror help-buffer data into mode-owned locals.** Define `HelpLinks` / `HelpAnchors` / `HelpHighlights` newtypes implementing `BufferLocal` with `OWNER_MODE = "help-mode"`. Promote `BufferLocals::insert` to `pub` for App-level construction-time seeding (mirror of emacs's `setq-local` -- any code can seed; ownership is metadata). `App::open_help_in_pane` mirrors the parsed data into `buffer_locals[id]` alongside the existing fields.                                                                                                                          | `lattice-mode`, `lattice-ui-tui`                                            | Help buffers post-creation have `buffer_locals[id]` populated; readers still consume the struct fields.                                                                                                                       |
| M.3.2.b.2 | **Flip renderer to read through buffer-locals.** `render.rs`'s 5 reader sites for `help.highlights` / `help.links` go through a new `help_render_data(app, id, fallback)` helper that prefers `buffer_locals` and falls back to the struct fields for the bootstrap window. The struct fields stay; field removal waits for M.3.2.c.                                                                                                                                                                                                                                           | `lattice-ui-tui`                                                            | Tests prove the renderer reads through locals (modify the locals after creation, observe the new value); `help.links` / `.highlights` no longer appear in render.rs production code.                                          |
| M.3.2.c.1 | **HelpBuffer production-reader migration.** App-side reader sites for `HelpLinks` / `HelpAnchors` (`do_help_follow_link`, in-anchor-link's anchor-line lookup) read through `buffer_locals` keyed on `pane.buffer_id` (the registered id, not `help.id`'s construction-time id; see comment in `open_help_in_pane`). Renderer's `help_render_data` helper updated to use the registered id. Fallback to struct fields retained for synthetic-test paths and the bootstrap window.                                                                                              | `lattice-ui-tui`                                                            | Production code paths read help-mode-owned data through `buffer_locals`; a regression test inserts a synthetic `HelpLinks` and asserts `FollowLink` dispatches on the locals-side value.                                      |
| M.3.2.c.2 | **FileTreeBuffer migration.** Move `root` / `entries` / `nerd_fonts` to `file-tree-mode`-owned `BufferLocal` newtypes; mirror at `open_file_tree_in_pane`-style sites; flip readers via the same locals-first-with-fallback pattern from M.3.2.b/c.1.                                                                                                                                                                                                                                                                                                                          | `lattice-mode`, `lattice-ui-tui`                                            | Production code paths read file-tree data through `buffer_locals`.                                                                                                                                                            |
| M.3.2.c.3 | **OilBuffer migration.** Move `dir` / snapshot to `oil-mode`-owned locals; mirror at the oil-buffer creation sites.                                                                                                                                                                                                                                                                                                                                                                                                                                                            | `lattice-mode`, `lattice-ui-tui`                                            | Production code paths read oil data through `buffer_locals`.                                                                                                                                                                  |
| M.3.2.c.4 | **DocumentEntry migration.** Largest sub-slice -- migrate `syntax: Option<SyntaxHandle>`, `last_parsed_text_version`, `last_synced_syntax_version`, `Vec<Fold>` to language-mode-owned locals (e.g. `RustMode` / `MarkdownMode` / `TextMode` declare ownership). Reader surface is broad (renderer's syntax walk, fold rendering, every site that calls `App::syntax_for_buffer` etc.).                                                                                                                                                                                        | `lattice-mode`, `lattice-syntax`, `lattice-ui-tui`                          | Production code paths read document mode-data through `buffer_locals`.                                                                                                                                                        |
| M.3.2.c.5 | **Field retirement + BufferStorage decision.** Drop the now-redundant struct fields from `HelpBuffer`, `FileTreeBuffer`, `OilBuffer`, `DocumentEntry`. Constructors no longer populate them; tests construct `BufferLocals` directly. Free functions / App methods replace `link_at`, `scroll_to_anchor`, etc. Promote `range_contains_position` to a free function in `crate::help`. Evaluate whether `BufferStorage` retires entirely (everything mode-data lives in locals; only universal payload is per-variant) or stays as the storage-shape discriminator.             | `lattice-mode`, `lattice-ui-tui`, `lattice-syntax`                          | No `help.links` / `tree.entries` / etc. struct-field reads anywhere in the codebase; `:describe-buffer` enumerates every local for any buffer. Per-kind structs are minimal (cursor / scroll / id / rope only) or eliminated. |
| M.4       | Renderer consumes `ResolvedOptions`. Drop `BufferKind` branches in `draw_panes`. Hover popup unification: floating-geometry view of a `markdown-mode` buffer with a `hover-mode` minor.                                                                                                                                                                                                                                                                                                                                                                                        | `lattice-ui-tui`                                                            | Single render path. K-hover gets markdown highlighting. No `match buffer.kind` in renderer.                                                                                                                                   |
| M.5       | **`lsp-mode` umbrella**. Refactor: every LSP feature checks the mode gate before doing work. Auto-activate when server attaches; user can `:disable lsp-mode`. Tests: disable ⇒ no LSP work.                                                                                                                                                                                                                                                                                                                                                                                   | `lattice-lsp`                                                               | `:disable lsp-mode` silences all LSP traffic for the buffer; `:enable` resumes.                                                                                                                                               |
| M.6       | **LSP sub-modes**. `lsp-completion-mode`, `lsp-diagnostics-mode`, `lsp-hover-mode`, `lsp-signature-mode`, `lsp-format-mode`, `lsp-rename-mode`, `lsp-symbols-mode`, `lsp-code-action-mode`, `lsp-nav-mode`. Each independently toggleable.                                                                                                                                                                                                                                                                                                                                     | `lattice-lsp`                                                               | Each sub-mode independently disable-able; tests cover gating per-feature.                                                                                                                                                     |
| M.6.5     | **Namespace cleanup**: rename the existing `lsp.log-level` typed option (shipped pre-M as the LSP-config-loaded check) to `lsp-mode.log-level` so it sits in the mode-owned namespace. The `lsp.*` namespace is then exclusively the structural `workspace/configuration` passthrough (§6.5.2). One-line code change + TOML migration note.                                                                                                                                                                                                                                    | `lattice-ui-tui`                                                            | Old key emits a deprecation echo for one minor version, then is removed.                                                                                                                                                      |
| M.7       | **Display minor modes** wrapping existing typed options: `line-numbers-mode`, `relative-line-numbers-mode`, `whitespace-show-mode`, `current-line-highlight-mode`, `read-only-mode`, `wrap-mode`. `:set number` and `:enable line-numbers-mode` converge on the same state.                                                                                                                                                                                                                                                                                                    | `lattice-mode`, `lattice-ui-tui`                                            | Both surfaces work; toggling either updates the other.                                                                                                                                                                        |
| M.8       | **Introspection**. `:describe-mode <name>`, `:list-modes`, `:describe-option-resolution <name>` (showing the layer each resolved value came from).                                                                                                                                                                                                                                                                                                                                                                                                                             | `lattice-mode`, `lattice-ui-tui`                                            | All three commands populated and tested.                                                                                                                                                                                      |
| M.9       | **`:customize <name>` form view (TUI)**. New `customize-mode` major and `OptionGroup` registry. Built-in groups pre-registered (`Editor`, `Display`, `Editing`, `Lsp`, `Completion`, `Picker`, `Filetree`, `Oil`, `Help`, `Appearance`). Resolution: `<name>` ending in `-mode` ⇒ that mode's options; otherwise ⇒ that group's members, sectioned by owning mode. Per-row navigation + edit + apply through `:set` machinery. Apply is session-only; TOML write-through deferred (§12). Group picker for `:customize` with no args.                                           | `lattice-mode`, `lattice-ui-tui`                                            | `:customize lsp-completion-mode` (mode), `:customize lsp` (group), `:customize editor` (bare names) all work; edits apply; `:customize` with no args opens the picker.                                                        |
| M.10      | **WIT plugin path**. WIT API for declaring modes from WASM components. Adapter implements `Mode` trait by bridging WIT calls. Validate built-in / plugin parity from the consumer's POV.                                                                                                                                                                                                                                                                                                                                                                                       | `lattice-mode`, `wit/`                                                      | A trivial third-party `markdown-pretty-mode` plugin loads, registers, activates.                                                                                                                                              |

Each slice ships docs + tests + (perf-relevant) benches +
graceful error handling per CLAUDE.md.

#### Slice landings (running ledger)

- M.3.0 / M.3.1 / M.3.2.a / M.3.2.b.{1,2} / M.3.2.c.{1,2,3} -- ✅ landed
  (see git log; readers flipped through `buffer_locals` first-with-fallback
  pattern; per-kind data mirrored at construction sites).
- M.3.2.c.4 -- ✅ landed. Four `BufferLocal` newtypes
  (`DocumentSyntax`, `DocumentLastParsedTextVersion`,
  `DocumentLastSyncedSyntaxVersion`, `DocumentFolds`) under
  `text-mode` ownership in `crate::modes`. Reader accessors
  on App: `document_syntax_for(id)`, `document_folds_for(id)`,
  `document_last_parsed_text_version_for(id)`,
  `document_last_synced_syntax_version_for(id)`. Active-buffer
  hot-path readers (`refresh_highlights`, `recompute_syntax_folds`,
  `maybe_reparse_syntax`, completion's tree-sitter source, the
  inactive-pane reparse) flow through these accessors. The active
  branch returns the App field directly; inactive returns from
  `buffer_locals`. Round-trip bug fix: `last_synced_syntax_version`
  now persists across switch-away-and-back (was silently rolling
  back to 0). 5 new tests cover seeding + accessor behaviour.
- M.3.2.c.5 -- 🟡 partial-with-known-limit.
  - **DocumentEntry mode-fields fully retired**: `syntax`,
    `last_parsed_text_version`, `last_synced_syntax_version`, `folds`
    are gone from `DocumentEntry`; the entry now holds only `id` +
    `handle`. Activation transitions stash / restore mode-state
    through `buffer_locals` directly via
    `seed_empty_document_locals(id)` (initial seed at construction)
    and `snapshot_active_document` (de-activation stash).
  - **HelpBuffer**: production read paths flipped to `buffer_locals`
    exclusively. New `HelpContent` bundle (slim `HelpBuffer` +
    parsed `HelpMetadata`) returned by every factory; `App::open_popup`
    seeds metadata into `buffer_locals[help.id]`. Struct fields
    (`links`/`anchors`/`highlights`) stay as vestigial test-fixture
    state. `Deref<Target = HelpBuffer>` on `HelpContent` keeps tests
    that access `content.cursor`/`content.line_count()` etc. working.
  - **FileTreeBuffer**: renderer + `do_open_file_tree_under_cursor`
    read `entries` exclusively through `buffer_locals`. Vestigial
    fields stay; the `toggle_at` mutator continues to write the
    struct field, with a re-mirror to locals after each call.
  - **OilBuffer**: `do_oil_follow`, `do_write` (oil branch), and
    `do_list_buffers` read `dir` exclusively through `buffer_locals`.
    Vestigial fields stay; `navigate_into` mutates the struct field
    and re-mirrors to locals.
  - **BufferStorage decision: keep the enum.** Document is
    structurally different from Help / FileTree / Oil (its content
    lives in an actor accessed via `DocumentHandle`; the others
    embed a rope inline + carry kind-specific methods like
    `FileTreeBuffer::toggle_at` / `OilBuffer::navigate_into` /
    `OilBuffer::apply` whose semantics are meaningfully distinct).
    Collapsing into one `Buffer` struct would either smear the
    Document-vs-rope distinction or inline `Option<DocumentHandle>`
    on every kind -- either way encoding the dispatch the enum
    already encodes cleanly.
  - **Deferred**: full struct-field removal across the three
    non-Document kinds. Would require rewriting ~70 test
    construction sites to seed `BufferLocals` directly instead of
    inspecting the struct fields. Production code is already on
    locals-only; the deferral is purely test-fixture migration.
- M.4 -- 🟡 partial.
  - **Per-kind pane dispatch consolidated.** `draw_panes` and
    `draw_pane_status_line` no longer `match buffer.kind`; the
    branches live behind `draw_pane_content` and
    `App::pane_status_label`. Mode-driven dispatch (each major
    mode contributes its own draw fn) replaces the helper-side
    matches in a follow-up.
  - **`option_cache` flows through `ResolvedOptions`.**
    `rebuild_option_cache` reads via
    `resolved_option::<D>(active_id)` for every option; mode
    contributions on the active buffer (e.g. `ReadOnly` from
    help-mode) propagate to the renderer's hot-path accessors.
    Cascade re-resolves the active buffer; activation refreshes
    the cache.
  - **Per-pane option resolution.** `App::show_line_numbers_for`
    + `App::relative_line_numbers_for` resolve per-buffer.
    `FrameView::for_buffer` is the per-pane view used by inactive-
    pane render paths. Two visible buffers with differing mode
    stacks render their gutters independently.
  - **Hover popup unification (Option B).** The popup UI
    component is buffer-agnostic in intent: any buffer can render
    in a popup; today's popup content happens to be a help buffer.
    Help buffers run `markdown-mode` major + `help-mode` minor
    (the `ReadOnly` + link/anchor/`<CR>`-follow contribution).
    Hover popups run `markdown-mode` major + `hover-mode` minor
    (auto-dismiss-on-doc-cursor-motion). The auto-dismiss
    discriminator now consults `active_modes` for the popup buffer
    rather than the structural `prev_pane_for_help.is_none()`
    check. Display preference (popup / split / tab / minibuffer)
    is orthogonal to which mode the buffer carries -- a buffer
    can be moved between display strategies without changing its
    mode.
  - **Popup slot rename**. `App.help_buffer` -> `App.popup_buffer`.
    The slot is now named to reflect its popup-generic intent;
    the field type stays `Option<HelpBuffer>` for one more slice.
    Recorded as the documented contract: this is the popup's
    content, not a help-only field.
  - **Mode-keyed pane render dispatch.** The helper-side
    `match buffer.kind` in `draw_pane_content` /
    `pane_status_label` flipped to a `ModeId`-keyed
    [`PaneRenderRegistry`] lookup (in `lattice-ui-tui::pane_render`).
    Each registered provider pairs a `PaneRenderFn` (content draw)
    with a `PaneStatusFn` (status label). Lookup walks active
    *minors* in reverse activation order before falling back to
    the active *major*, so a help-mode minor on a `markdown-mode`
    major buffer wins over the major's default (document) path.
    No provider matches → renderer falls through to the document
    default. Boot registers `HelpMode` / `FileTreeMode` /
    `OilMode` providers; document buffers register no provider
    (they take the fallback). Plugins (post-1.0) extend the same
    registry. The dispatch lives in the renderer crate (not
    `lattice-mode`) because the function signatures take ratatui
    types -- a future GPUI / web renderer gets its own registry,
    keyed by the same `ModeId`s.
  - **Popup buffers participate in the unified registry.**
    `open_popup` and `do_open_hover` now register the popup's
    `HelpBuffer` in `app.buffers` with
    `BufferFlags { listed: false, hidden: true }` (skipped by
    `:bn` / `:bp` / `:ls`; informational `hidden`), matching the
    pattern `open_help_in_pane` already uses for `:lsp-log` etc.
    `dismiss_popup` removes the entry plus its `active_modes` /
    `buffer_locals` / `resolved_options`; back-to-back popups no
    longer leak stale state. The State-A hover auto-dismiss path
    routes through `dismiss_popup` for a single cleanup edge.
    `App.popup_buffer`'s type stays `Option<HelpBuffer>` -- the
    slot is the hot-path mirror, the registry is canonical.
  - **Deferred**: (a) flipping `App.popup_buffer`'s type from
    `Option<HelpBuffer>` to `Option<BufferId>` so every reader
    resolves through `app.buffers.help(id)` -- mechanical but
    touches ~125 sites including many tests that observe
    `HelpBuffer` fields directly. (b) The in-pane vs popup
    display preference for help buffers (today centred-popup is
    hard-wired in App methods).
- Crate audit (lattice-ui-tui shrink) -- 🟡 partial.
  In support of M.4 and the broader "everything-is-a-buffer"
  commitment, content models that aren't tui-shaped were lifted
  out of `lattice-ui-tui` so any future renderer (GPUI, web)
  can depend on them without pulling ratatui:
  - **lattice-help** ✅. `HelpBuffer` / `HelpContent` /
    `HelpMetadata` / link parser / topic registry. The seven
    LSP-aware factories (`HelpContent::diagnostics` /
    `lsp_*`) and their two helpers (`summarise_capabilities`,
    `format_log_record`) moved to `lattice-lsp::help_views` as
    free functions returning `lattice_help::HelpContent` --
    they read LSP runtime types
    (`DiagnosticsLayer` / `LspSupervisor` / `LspLogger` /
    `Capabilities` / `LogRecord`) and shouldn't pull lsp into
    a content-model crate. `lattice-ui-tui::{help, help_topics}`
    are now re-export shims.
  - **lattice-oil** ✅. Clean extraction; oil's rope holds bare
    names and the renderer adds icons as spans, so no icon dep
    to factor out.
  - **lattice-file-tree** ✅, with prerequisite icon split:
    `lattice_core::ui::icons` now owns the path → glyph + colour
    table, returning `(&'static str, IconColor)` where
    `IconColor` is a renderer-neutral enum (`Rgb(u32)` plus
    seven named variants). `lattice-ui-tui::icons` reduces to a
    thin adapter that maps `IconColor` → ratatui `Color` /
    `Style`. `lattice-file-tree` embeds glyphs in the rope via
    `glyph_for_entry`; colour is applied by the renderer at draw
    time.
  - **lattice-app** -- ⏸ deferred. `App` (~31k LoC across 24
    submodules in `crates/lattice-ui-tui/src/app/`) is tightly
    coupled to ratatui types in render paths, the picker, the
    cmdline overlay, and the popup geometry. Extracting
    cleanly requires either (a) lifting `theme` / `render` /
    picker-rendering / cmdline-rendering into `lattice-app`
    too -- which doesn't shrink the renderer crate, just
    renames it -- or (b) making `App` renderer-agnostic by
    threading a renderer trait through every draw path, which
    is the M.6+ "additional renderers" milestone, not an M.4
    follow-up. Documented as a non-goal for the current
    architectural slice.

### 10.1 Why LSP is the right canary (M.5 first among "real" mode work)

After foundation slices M.0-M.4 land, M.5 (the `lsp-mode`
umbrella) is the right first feature subsystem to migrate:

- **It exercises every part of the mode system.** Capabilities
  (server attached?), conflicts (none), implies (sub-modes),
  options (per-server settings as mode-scoped overrides),
  events (didChange, publishDiagnostics), decorations
  (diagnostics gutter, hover popup), commands (`:lsp-*`).
- **Its features are already independently dispatched.**
  `do_lsp_hover_request`, `do_lsp_completion_request`,
  `do_lsp_signature_help_request`, etc. are already separate
  methods (`app/lsp.rs`). Wrapping each in
  `if !mode_active { return; }` is mechanical; the mode system
  gets exercised end-to-end without inventing new feature
  surface.
- **Real ergonomic payoff.** "Disable LSP completion but keep
  diagnostics" and "disable LSP entirely on this generated
  file" become first-class, not roleplayed.
- **The diagnostic-doesn't-show bug we're chasing right now**
  is dogfood. Once `lsp-diagnostics-mode` is a toggle, "is the
  mode active?" is the first question, with a visible answer
  via `:list-modes`.

The risk of starting earlier: **circular slice dependency.**
M.5 depends on M.1 (foundation) + M.2 (resolution) + M.3
(major modes registered, so per-major-mode default minor
activation works). M.5 cannot land before M.4 either, because
the renderer needs to consume mode-resolved options before LSP
sub-modes can contribute their decoration providers.

## 11. Open questions (decide before M.0 lands)

1. **Conflict policy specifics.** "Last-activated wins" with
   conflict-event publish (§6.2) is the proposal. Alternative:
   activation *fails* on unannotated conflict. Stricter; harder
   to live with. *Lean current proposal.*
2. **Buffer-local options vs minor-mode-contributed options.**
   Is `:setlocal foo=bar` a separate layer (§6.1 layer 2), or
   does it always implicitly create a buffer-scoped minor mode?
   *Lean separate layer -- they're conceptually different.*
3. **Hover popup's mode pair.** Buffer is `markdown-mode`
   (content identity). Popup-shape behavior comes from a
   `hover-mode` minor (anchor-at-cursor, wrap, no line numbers).
   Reusable on any markdown buffer shown in floating geometry.
   Confirm.
4. **`BufferKind` retirement.** M.3 wants to remove the enum.
   Some places use it as a coarse-grained gate (`is_read_only`,
   `accepts_writes`); those become capability queries on the
   resolved mode set. Confirm: ok to retire?
5. **Built-in modes and reload.** Built-in modes are compiled
   in. User can `:disable` them but not unload. Plugin modes
   can be unloaded. Document this asymmetry, or make them
   uniform via a "loaded but disabled" state for built-ins
   too? *Lean asymmetric -- built-in and plugin are different
   distribution units; pretending otherwise is a leak.*
6. **Mode definition for the rich minibuffer.** §5.9.10 commits
   to `command-line-mode` / `search-line-mode` etc. as majors
   on per-prompt buffers. Does the modal state ever cross
   between the prompt buffer and the underlying buffer?
   *Lean no -- the prompt buffer has its own modal state machine
   while focused.*
7. **Declarative-validator coverage gap.** Some plugin
   validators can't be expressed in the WIT subset (e.g. "this
   string must be a valid cron expression," "this path must
   exist on disk"). Fallback is post-hoc rejection via
   `Event::OptionChanged` subscription, which is awkward
   enough that some plugins will validate at *use* time
   instead of *set* time. Is that acceptable? *Lean yes for v1
   -- callbacks-into-WASM-on-every-set is worse.*
8. **Plugin-options write-through to `lattice.toml`.** A user
   `:set git-blame.delay-ms=200` is session-only today (TOML is
   read-at-startup). For plugin options to feel like real
   customization, eventual `:customize` write-through (post-v1
   UI) needs to handle plugin options the same as core. Not
   blocking M.0, but flag it: the metadata for write-through is
   the same as for `:set`, so when `:customize` lands, plugin
   options inherit persistence "for free."
9. **`ModeAdapter` trait shape.** WASM plugin modes (M.10) need
   a host-side adapter that implements `Mode` by calling into
   the plugin. The static-fact methods (`options`, `keymap`,
   `subscriptions`, `decorations`, `required_capabilities`,
   `conflicts_with`, `implies`) are pulled at plugin load and
   cached -- no per-call WIT round-trip. Lifecycle hooks
   (`on_activate` / `on_deactivate`) and decoration-provider
   polling DO cross WIT. Is that the right boundary, or do we
   want decoration polling cached too with a "dirty" event?
   *Open. Decide at M.10, not now.*

Resolved by this revision (no longer open):

- *`ModeId` interning* -- yes, intern everything. One type
  (`InternedStr`) for uniform fast `==` and HashMap keys.
- *Compile-time uniqueness* -- types-as-keys (§6.4) gives
  cross-crate uniqueness for free; `linkme` aggregation gives
  startup-time display-name uniqueness; plugin namespacing is
  by construction.
- *`:customize` shape* -- group-oriented (not single-option;
  that's what `:set` is for), buffer-backed (`customize-mode`
  major), TUI-first (ratatui rendering of form rows; GPUI is
  an upgrade, not a requirement). Ships in v1 with apply-only
  semantics; TOML write-through deferred. (§6.7, M.9.)
- *Group identity* -- explicit `OptionGroup` declarations,
  parallel to `Option`. Prefix-stem matching dropped in favour
  of explicit groups (§6.7.1.1). Built-in groups pre-registered;
  plugins can join any built-in group or declare their own.
  The bare *namespace* is reserved (plugin options are
  mechanically prefixed by plugin ID at registration); group
  membership is not access-controlled because group is purely
  organizational and has no behavioral effect.
- *Group hierarchy* -- flat groups in v1; `parent: Option<GroupId>`
  is a post-v1 extension. Today's groups become roots in the
  future tree; no migration. (§6.7.1.2.)
- *Sync vs async deactivation* -- synchronous deactivation,
  async teardown allowed (§7.1).

## 12. Non-goals for v1

- **Hot-reloading mode definitions** (built-in or plugin). Mode
  changes require a restart for v1. (Plugin host already
  doesn't hot-reload plugins.)
- **Mode inheritance** (`define-derived-mode` in emacs). Maybe
  post-1.0; for now, modes are flat. A new mode that wants
  most-of-X copies what it needs.
- **Per-window mode overrides.** Modes are buffer-scoped. A
  buffer shown in two panes has the same mode set in both.
  (Vim's `setlocal` for window-only options is a separate
  layer; not a mode concern.)
- **Mode-aware undo / history.** Mode changes don't enter the
  undo stack. The undo stack tracks content edits.
- **`:customize` write-through to `lattice.toml`.** v1 ships
  the form view (M.9) with apply-only semantics: edits land in
  the registry session-only. The persistence path -- "Save"
  writing through `toml_edit` to preserve the user's existing
  TOML structure and comments -- is a v1.x slice. v1 users
  who want permanent settings hand-edit `lattice.toml` (or
  put `:set` calls in their init module).

Note: `:customize` itself -- the form-buffer UI in TUI -- is
*not* listed here as a non-goal. It ships in v1 (§10 / M.9).
Only the persistence path is deferred. GUI-specific
upgrades to the form widgets (color pickers, sliders) are
also post-v1, but the TUI form is fully functional on its
own and is not blocked on those.
