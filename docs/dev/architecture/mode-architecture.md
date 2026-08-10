# Mode Architecture (developer reference)

Authoritative design for lattice's major / minor mode system. The
plan section at the end lists the slices that take us from the
current state (mode work scattered across `BufferKind` matches in
the UI layer, ad-hoc dispatch in `lattice-lsp`, no formal mode
registry) to a single composable mode model that all customization
flows through.

This document is a *companion* to design.md, not a replacement.
design.md §5.8 names major / minor modes as the primary
customization model; this doc spells out the trait, the resolution
algorithm, the taxonomy criteria, and the migration sequence the
spec leaves implicit. After review, design.md gets a link here from
§5.6 / §5.8 / §5.9 / §5.10 / §5.12.

## 1. Vision

> *"Composable, content-type-aware customization. Modal state
> (Normal/Insert/Visual/etc.) is orthogonal."* -- design.md §2.1

> *"A buffer has exactly one major mode. The major mode declares:
> ... rendering profile, style mappings, default minor modes,
> commands."* -- design.md §5.8.1

> *"A buffer can have any number [of minor modes] active.
> Composable, additive features."* -- design.md §5.8.2

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
  with a `customize-mode` major (design.md §5.9.10's
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
  existing typed event bus (design.md §5.10).
- **Introspection from day one.** `:describe-mode <name>`,
  `:list-modes`, `:describe-option-resolution <name>` show what's
  active and where each resolved value came from. (design.md
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

**Plugin modes get the identical surface.** The `:<mode-name>`
toggle ex-command is built by one shared
`lattice_grammar::registry::mode_toggle_ex_command_spec`, used by
BOTH boot (native modes, `register_mode_toggle_commands`) and the
plugin modes-seam drain (`lattice-plugin-loader::drain_mode`). A
plugin-registered mode is therefore togglable by `:<mode-name>`,
resolves in `:describe-command`/`:apropos`/`:list-commands`, and
appears in `:`-completion, exactly like a native mode. Two seams
make that hold at runtime: (a) plugin commands drain into the
runtime-mutable command `ArcSwap` the dispatcher + introspection
read, and its `generation()` counter (bumped on every register /
unregister) keys the `:`-completion cache so a load/unload
invalidates it without a manual flush; (b) the plugin drain
registers the toggle under `SourceLayer::Plugin(id)`, so unload's
provenance-driven `CommandRegistry::unregister_plugin` reverses it
(unconditionally — no per-seam "did I register a command?" flag to
forget). `:list-modes`/`:describe-mode` read the live mode
`ArcSwap`, so plugin modes list there too.

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
| `command-line-mode`       | `:` minibuffer (design.md §5.9.10)                          | `lattice-core`             | Rich minibuffer's command-prompt major.                                          |
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
| `lsp-progress-mode`        | `$/progress` accumulator + modeline `[title: msg NN%]` segment                     | `lsp-mode`. Server need not opt in.            |
| `lsp-document-highlight-mode` | `textDocument/documentHighlight` overlay (same-symbol references)              | `lsp-mode` + server has `documentHighlightProvider`. |
| `lsp-selection-range-mode` | `textDocument/selectionRange` smart-expansion operator                             | `lsp-mode` + server has `selectionRangeProvider`. |
| `lsp-folding-mode`         | `textDocument/foldingRange` feeding `FoldMethod::Lsp`                              | `lsp-mode` + server has `foldingRangeProvider`. |
| `lsp-inlay-hint-mode`      | `textDocument/inlayHint` virtual-text overlay                                      | `lsp-mode` + server has `inlayHintProvider`.   |
| `lsp-semantic-tokens-mode` | `textDocument/semanticTokens/full` per-kind fg overlay (overrides tree-sitter)     | `lsp-mode` + server has `semanticTokensProvider`. |
| `lsp-lens-mode`            | Code lens (post-1.0)                                                               | Opt-in.                                        |
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
| `auto-pair-mode`             | Bracket / quote pairing — the `auto-pair` core plugin (WASM), ships on by default. |
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
	/// RAII cleanup token returned by `on_activate`. The
	/// dispatcher stores one Guard per `(BufferId, ModeId)`
	/// pair; deactivation drops the Guard, firing its `Drop`
	/// impl. Marker modes with no cleanup write `type Guard = ();`.
	/// See §7.1 for the cleanup contract.
	type Guard: Send + 'static;

	fn id(&self) -> ModeId;                  // canonical name (interned)
	fn kind(&self) -> ModeKind;              // Major | Minor

	/// Option overrides this mode contributes. Type-keyed (see
	/// §6.4): each override is paired with the option's type at
	/// compile time. Pure declarative -- the registry, not the
	/// mode, applies these to the layer stack.
	fn options(&self) -> OptionOverrideSet;

	/// Keymap chord -> command additions / overrides. Layered
	/// into the existing keymap registry
	/// (`keymap-architecture.md` §5-6) at this mode's priority
	/// slot.
	fn keymap(&self) -> &Keymap;

	/// Default auto-activation policy (minor modes). The host's
	/// minor-activation resolver reads this on `MajorEntered` to
	/// decide whether to auto-activate (§7.4). Default: `Manual`.
	/// (Reactive, while-active subscriptions are NOT declared here —
	/// they are acquired in `on_activate` and held in the Guard;
	/// `Mode::subscriptions()` was removed in MO.4.c.)
	fn activation_policy(&self) -> ActivationPolicy;

	/// Decoration providers (gutter / inline / overlay /
	/// statusline). Polled by the renderer.
	fn decorations(&self) -> &[DecorationProvider];

	/// Capabilities the mode requires from the host.
	fn required_capabilities(&self) -> CapabilitySet;

	/// Conflicts.
	fn conflicts_with(&self) -> &[ModeId];

	/// Implies.
	fn implies(&self) -> &[ModeId];

	/// Lifecycle hook. Called when the mode activates on a
	/// buffer. Returns a typed `Self::Guard` that the
	/// dispatcher stores; dropping it on deactivation runs the
	/// mode's cleanup via the Guard's `Drop` impl. Modes with
	/// no cleanup return `()`.
	///
	/// `ctx` is consumed by value. `ModeContext` is owned and
	/// `Send + 'static`, which lets the dispatcher (post
	/// M-async.2c) spawn this future on the shared runtime.
	///
	/// **There is no `on_deactivate`.** Cleanup is via the
	/// Guard's `Drop` impl, not an explicit teardown method.
	/// See §7.1 for the design rationale.
	fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard>;
}

pub enum ModeKind { Major, Minor }

pub struct ModeId(InternedStr);  // fast == and HashMap-keyed lookups

pub struct CapabilitySet { /* bitfield: BufferUri | Lsp | TreeSitter | Fold | ... */ }

/// Owned, `Send + 'static`. Carries Arc handles to shared
/// registries the mode may need during activation. **Notably
/// absent: `BufferLocals`.** Mode-private state lives in the
/// typed `Guard` (carried by the dispatcher between activate
/// and deactivate). App-managed rich locals stay App-only and
/// are never exposed to lifecycle hooks. See §7.1.2.
pub struct ModeContext {
	buffer_id: BufferId,
	current_mode: ModeId,
	config: Arc<ConfigRegistry>,
	events: Arc<EventBus>,
	services: Arc<ServiceRegistry>,
}

/// Object-safe view of `Mode` used internally by the registry
/// (the typed `Guard` is box-erased to `Box<dyn Any + Send>`).
/// Users implement `Mode`; the blanket `impl<M: Mode> DynMode
/// for M` handles erasure. `pub` only so external callers can
/// invoke declarative methods on the `Arc<dyn DynMode>` they
/// get back from `ModeRegistry::get`.
pub trait DynMode: Send + Sync + 'static { /* ... */ }

/// `LifecycleFuture<'a, T = ()>` = `Pin<Box<dyn Future<Output =
/// Result<T, ModeActivationError>> + Send + 'a>>`. `T` is the
/// mode's `Guard` (defaults to `()` for marker modes).
pub type LifecycleFuture<'a, T = ()> = /* see crate */;
```

### 5.2 Modes own their lifecycle work

The trait splits cleanly into two halves:

- **Declarative methods** (`options`, `keymap`, `subscriptions`,
  `decorations`, `required_capabilities`, `conflicts_with`,
  `implies`) return read-only data. The *registry*, not the
  mode, applies these to the layer stack on activation and
  removes them on deactivation. A mode can never "leak"
  contributions past its lifetime by construction.
- **Lifecycle hook** (`on_activate`) is where modes do their
  imperative work — swapping coupled options, spawning a server
  connection, opening a file watcher, subscribing to events.
  The hook receives an owned `ModeContext` (buffer id, current
  mode id, Arc handles to the shared `ConfigRegistry`,
  `EventBus`, `ServiceRegistry`) and returns a typed
  `Self::Guard` that the dispatcher stores until deactivation.
  Cleanup is via the Guard's `Drop` impl — no `on_deactivate`
  method exists. See §7.1 for the design rationale.

  Constraint: the hook may NOT mutate *another* mode's state.
  Each mode's runtime state lives in its own typed `Guard`,
  not in a shared buffer-locals map — cross-mode mutation is
  impossible by construction (the Guard is private to the
  dispatcher between activate and deactivate). Cross-mode
  option clobbering is uncoupled by the layer-stack mechanism
  — each mode contributes options through its `options()`
  declarative output, and the registry merges; hooks should
  only mutate options when the mode and the option are
  inherently coupled (e.g. `lsp-folding-mode` ⇔ `foldmethod=lsp`),
  and the prior value is captured in the Guard for `Drop` to
  restore.

**Why this matters: renderer-agnostic activation.** A TUI
host (`lattice-ui-tui::App`) and a future GPUI host should
both call `registry.activate_minor(...)` and get identical
side effects. The mode is responsible for its own work; the
App is just the orchestrator that holds the handles
(`ConfigRegistry`, `BufferLocals`, event bus once Phase 2
lands, service handles once Phase 3 lands) and feeds them
to the registry. New activation paths (plugin API,
programmatic trigger, cascade) get the mode's behaviour for
free because everything funnels through `Mode::on_activate`.

**Reload semantics** (§9.6) stay clean by the same
construction: `:lsp-diagnostics-mode` (toggle off) deactivates
the mode, which removes every override the mode contributed
*and* drops the mode's `Guard` — `Drop` runs whatever cleanup
the Guard implements (unsubscribe from events, restore prior
option values, abort a tokio task, etc.). The cleanup is
compile-time enforced; modes cannot forget to undo their
setup.

#### Migration phases

The `ModeContext` extension is staged so each phase touches
a smaller blast radius:

- **Phase 1** (4.4.f) — `ConfigRegistry` exposed via
  `ctx.config()`. `LspFoldingMode` migrated to own its
  `foldmethod=lsp` swap.
- **Phase 1.5** — declarative `Mode::mirrors_option()`
  hint replaces six hardcoded per-display-mode special
  cases in the App's option-changed cascade with one
  generic loop.
- **Phase 2** — typed event bus exposed via
  `ctx.events()`. `LspMode` publishes
  `LspBufferAttached` / `LspBufferDetached` from its
  hand-written hooks (was App-side
  `on_lsp_mode_activated` / `_deactivated`).
- **Phase 3** — typed service registry exposed via
  `ctx.service::<LspSupervisorHandle>()`. Sub-mode cascade
  is now driven by `Mode::implies()` (the registry walks
  the slice on activate AND, via the Phase 3 extension, on
  deactivate). `LspMode` was rewritten to a stateful
  struct with the 13 sub-mode ids in `implies()`. App-side
  `on_lsp_mode_activated`, `on_lsp_mode_deactivated`,
  `activate_lsp_sub_modes_for`, and
  `deactivate_lsp_sub_modes_for` deleted.
- **Phase 3 follow-up — `LspBufferDetached` subscriber.**
  Wire-level `didClose` + `buffer_uris` cleanup
  (previously the last LSP-specific orchestration in
  `deactivate_mode_by_id`) now lives behind a typed-event
  subscription. Boot subscribes to
  `LspBufferDetached`; the App's per-tick drain
  (`drain_lsp_detach_events`) calls `lsp_close_buffer` per
  event. `deactivate_mode_by_id` no longer knows anything
  about `lsp-mode`.

`OptionOverrideSet` is a type-checked bag built via macro (§6.4
shows the syntax). Each entry pairs an option type with a value
of that option's type; the compiler rejects any mismatch.

### 5.3 Modes own their action handlers

The keymap is half the surface. The other half is the **action
handler body** — the closure that runs when a chord or
ex-command fires. A mode contributing keymap entries to its
own layer while leaving the handler bodies in
`lattice-host::dispatch::do_<provider>_action` is a HALF
MIGRATION and violates `feedback_mode_owns_its_surface`. The
acid test: a new provider crate landing should require ZERO
`Editor::` method additions in `lattice-host` and ZERO new
variants in the host's `Action` enum.

#### 5.3.1 `ActionHandlerRegistry` substrate

```rust
pub struct ActionHandlerRegistry { /* ... */ }

pub type ActionHandler = Arc<dyn Fn(&ActionContext) -> Option<Effect>
    + Send + Sync + 'static>;

pub struct ActionContext<'a> {
    pub buffer_id: BufferId,
    pub cursor: Position,
    pub services: &'a ServiceRegistry,
    pub events: &'a EventBus,
    // ...additional read-only host state the handler may need
}

impl ActionHandlerRegistry {
    pub fn register(&self, action_id: ActionId, handler: ActionHandler);
    pub fn unregister(&self, action_id: ActionId);
    pub fn lookup(&self, action_id: ActionId) -> Option<&ActionHandler>;
}
```

The registry lives in `lattice-mode` next to `ModeRegistry`
and `KeymapRegistry`. **M.10.1.b (2026-06-03) discovery path:**
boot registers a single `ActionHandlerRegistryHandle`
(`Arc<ActionHandlerRegistry>`) in the `ServiceRegistry`; modes
pull it from `on_activate` via
`ctx.service::<ActionHandlerRegistryHandle>()`. This matches
the established pattern for shared subsystem handles
(`MultibufferRegistryHandle`, `ProjectSearchServiceHandle`,
`LspSupervisorHandle`, ...) and avoids threading a sixth Arc
parameter through the 40+ `activate_major`/`activate_minor`
call sites in `lattice-host`. Per
`feedback_servicesregistry_arc_typeid`, the handle's
TypeId-keyed registration and lookup use the same
`ActionHandlerRegistryHandle` alias.

The host's chord-resolved-action dispatcher consults the
registry. When the action's `ActionId` resolves to a
registered closure, the host invokes the closure with an
`ActionContext` and applies the returned `Effect` (if any)
through the existing apply-effect pipeline. No host-side
`match action { ... }` arm is needed for mode-contributed
actions.

#### 5.3.2 Lifecycle

Handler registration is part of `on_activate`. The mode's
`Guard` carries an `ActionHandlerRegistration` token; the
token's `Drop` impl calls `unregister(action_id)` so chord
dispatch returns `None` once the mode deactivates. Reload
semantics are uniform with everything else — toggle the
mode off, handlers vanish via Guard drop; toggle on,
handlers re-register from `on_activate`.

#### 5.3.3 What still lives in the host

- The generic chord dispatcher (`Editor::dispatch_chord`)
  that walks the keymap layer stack and resolves a chord to
  an `ActionId`.
- The generic Effect applier — `Effect::OpenAt { path,
  position }`, `Effect::Edits(...)`, `Effect::CursorMove(...)`
  all flow through the existing host pipeline regardless of
  which closure produced them.
- The host's `Action` enum carries ONLY host-intrinsic
  actions (motion, edit, cursor, modal-state transitions).
  Provider-specific actions (`<CR>` jump-to-source, `gr`
  refresh, `:multibuffer-expand`) are NOT enum variants —
  they are `ActionId`s registered via
  `ActionIds::register(...)` and bound to handler closures
  by the owning mode at activation.

#### 5.3.4 What the substrate publishes vs. what the mode owns

Per the **substrate-vs-helper rule** in `CLAUDE.md`:

- **`Document` trait method** — for data the *renderer or
  generic dispatch loop* reads uniformly across buffer kinds.
  K.4.6 `display_line_numbers` and K.4.11
  `dispatch_with_cancel` are correct trait methods because
  the consumer is generic.
- **Substrate helper function** in the substrate's owning
  crate — for data only the mode's handler reads. The
  multibuffer crate exports
  `translate_composed_to_source(handle, cursor)`,
  `multibuffer_expand_excerpts_at(handle, cursor, delta)`,
  etc. The mode imports the helper and uses it inside its
  handler closure.

The acid test for whether mode-relevant behavior belongs on
the trait or in a helper: **who consumes it?** If only a
specific mode's handler reads it, it's a helper. If the
renderer or the generic chord-dispatch loop reads it
uniformly, it's a trait method.

This carving is what closes the half-migration loophole. The
chord is bound by the mode; the handler is registered by the
mode; the substrate data the handler reads is a helper, not a
trait method that implies host involvement; the host's role
is purely generic dispatch.

### 5.4 Modes own the production of their buffers

A mode that backs a synthetic buffer (`*compilation*`, `*lsp*`,
`*messages*`, `*ai:…*`, future `*scratch*` / REPL / DAP logs)
owns **creating** that buffer, not just filling it. Creation
means "find-or-create the named `Document` **and activate the
major on it**" — and activating a mode mutates the mode
registry / active-modes / options cache, so it needs
`&mut Editor`. That rules out doing it through the `&self`
`BufferStore` handle (which is find / read / write-handle
only). The reliable, mode-owned creation seam is:

```rust
// lattice-mode::ModeActivator (impl'd by Editor)
fn ensure_named_document(&mut self, name: &str, major: ModeId, flags: BufferFlags) -> BufferId;
```

Idempotent (a second call reuses the buffer, no re-activation);
the `Editor` impl forwards to `ensure_named_synthetic_document`,
which mints the buffer and runs `major`'s `on_activate` — so the
drain / subscription the mode sets up there is live before the
producer's first record.

Two front-ends to this one primitive, chosen by call context:

- **Imperative** — a provider's trigger function, called with
  `&mut dyn ModeActivator`, invokes `ensure_named_document`
  directly (e.g. compilation's `start_compilation` creates
  `*compilation*` *and* starts the run in one `&mut Editor`
  operation). This is `ModeActivator`'s reason for existing:
  extension crates that create buffers + activate modes host-side
  without depending on `lattice-host`.
- **Declarative** — a *pure* ex-command `apply` closure (no
  `&mut Editor` / services in scope; it can only return an
  `Effect`) returns `Effect::OpenSyntheticBuffer { name, mode_id }`,
  which the host applies through the same primitive (e.g. AI-log,
  ACP popups from a tick-callback).

`BufferStore` deliberately carries **no** create method — an
earlier `BufferStore::ensure_named_document` claimed find-or-create
but its `&self` impl could only find and panicked on a miss; it
was removed. `BufferStore` is find (`find_by_name`), write-handle
(`handle_for`), and generic insertion (`insert_document_buffer`).
See `design.md` §5.10.5 and `multibuffer-views.md` §3.7.

### 5.5 Modes declare their refresh action; the chord is shared

`gr` means **refresh this view** in every synthetic buffer. That is a
property of synthetic views as a class, not of any one of them — so by
the "shared behaviour is a minor mode, never a copied keymap" standing
rule the chord belongs on one shared minor, not re-declared per mode.

It was re-declared per mode. As of 2026-08-10 **five** independent
copies existed — `magit-core-mode` (`action:magit-refresh`),
`compilation-mode` (`action:compilation-recompile`), `providers::search`
(`action:search-refresh`), `plugins-mode` (`action:plugins-refresh`) and
`notifications-mode` (`action:notification-refresh`) — while the two
synthetic views that landed most recently, `*problems*` and narrow,
**had no `gr` at all**. Nobody noticed, because a gap in a copied set
does not announce itself. That is the failure this section closes.

The first draft of this section said *three* copies. That count came
from reading the code, and it was wrong by two — the last pair surfaced
only when RV.2 grepped the whole tree. The failure mode applies to the
audit as readily as to the code, which is why RV.2 ends in a source-grep
test (`lattice-host/tests/gr_is_declared_once.rs`) rather than trusting
the next hand-written inventory to be complete.

#### The seam

```rust
// on the `Mode` trait, defaulting to None
fn refresh_action(&self) -> Option<&'static str> { None }
```

The mode declares **which of its own actions** refreshes its view, by
the same `"action:…"` name `action_handlers()` registers under — the
`Option<&'static str>` shape `mirrors_option()` already uses. It does
not declare a body. The body stays exactly where §5.3 puts it.

Dispatch is generic:

1. `refreshable-view-mode` (a minor in `lattice-mode`) contributes one
   keymap entry: `gr` → `action:view-refresh`, pushed at
   `KeymapLayer::MinorMode` like every other mode keymap.
2. **The host** resolves it — `Editor::resolve_refresh_action` walks the
   focused buffer's active modes, **minors most-recently-activated
   first, then the major**, and takes the first declaration.
   Most-specific-wins: a provider minor on a multibuffer beats the
   generic `MultibufferMode`.
3. Chord dispatch redirects to the resolved `CommandId` before the
   `ActionHandlerRegistry` lookup, so the owning crate's existing
   handler runs unchanged.
4. The walk does not stop early: a second declaration is logged at
   `debug!` as shadowed. Two modes claiming the refresh is a wiring bug,
   and silently picking one is how it would stay invisible.

**Why the host resolves rather than the mode.** An `ActionHandler` gets
only `&ActionContext` — buffer, cursor, services, events. The active-mode
set lives on `Editor`, not in the `ServiceRegistry`, so a mode-side
handler *cannot* do this walk. This is the same split
[`invocation_runner`](#) already uses (`Editor::resolve_invocation_runner`
is the sibling function, same walk, different table): the mode declares,
the host walks and dispatches. `action:view-refresh` is a *generic* host
action carrying no per-view logic — it only redirects.

Activation is automatic: the mode-activation cascade activates
`refreshable-view-mode` when any activated mode returns `Some`. A mode
author writes **one line** and gets the chord. (`implies()` is the
fallback if evaluating that predicate inside the cascade proves awkward
— but it costs a second line to remember, and a forgotten `implies()`
kills the chord as silently as the copied keymap did.)

#### Why declare a target rather than do the work

Two alternatives were considered and rejected.

- **`ViewRefreshService` keyed by `BufferId`, holding closures.**
  Rejected: needs per-buffer registration, `DocumentClosed` cleanup, and
  closure storage — and it recreates the silent gap one level down, since
  a provider that forgets to register gets a dead key with nothing to
  catch it. It also puts a body somewhere no other mode body lives.
- **`Mode::refresh(&self, ctx)` doing the work in the trait method.**
  Rejected: modes would then have two ways to express an action body —
  an `action_handlers()` closure or this — and `magit-refresh` already
  exists as the former, so the trait method would duplicate or orphan it.

Declaring a target is an indirection of exactly one level, which is what
`invocation_runner()` and `mirrors_option()` already do on this trait.
The consumer is generic host machinery reading uniformly across every
mode, which is the condition the substrate-vs-mode rule sets for a trait
method.

#### The absence becomes visible

Default `None` means the generic handler echoes **"nothing to refresh
here"** instead of swallowing the key. A synthetic view without a
refresh now says so. `*problems*` stops failing silently the moment the
shared minor lands, before anyone implements its refresh body.

Sequencing: [`../operations/slice-plans/refreshable-views.md`](../operations/slice-plans/refreshable-views.md).

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

Bench lands in `../operations/benchmarks.md` as part of M.2.1.

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
commitment (§5.9 of design.md, §3 of this doc) means the same
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

Mode lifecycle rides the existing typed event bus (design.md
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
trait's `on_activate` runs first (awaited to completion);
*then* the event publishes, so subscribers see a buffer in a
consistent state.

`MajorExiting` runs *before* state teardown -- subscribers can
inspect what's about to be torn down. The trait's
`on_deactivate` runs after the event drains.

### 7.1 Async lifecycle — Drop-based cleanup via typed Guard

**Trait shape (revised 2026-05-14).** The `Mode` trait has a
single lifecycle hook (`on_activate`) that returns a typed
`Self::Guard` RAII cleanup token. There is **no
`on_deactivate`** method. The dispatcher stores the Guard for
the duration of the mode's activation on the buffer; on
deactivation, the dispatcher drops the Guard, firing its
`Drop` impl which runs the mode's cleanup. This is Rust-idiomatic
RAII applied to the mode lifecycle, validated by zed's
`Subscription` / `Task` patterns.

```rust
pub trait Mode: Send + Sync + 'static {
    /// RAII cleanup token returned by `on_activate`. The
    /// dispatcher stores one Guard per `(BufferId, ModeId)`
    /// pair; deactivation drops the Guard, firing its `Drop`
    /// impl. Marker modes with no cleanup write `type Guard = ();`.
    type Guard: Send + 'static;

    fn id(&self) -> ModeId;
    fn kind(&self) -> ModeKind;
    // ... declarative methods (options, keymap, subscriptions,
    //     decorations, capabilities, conflicts_with, implies,
    //     mirrors_option, completion_sources) ...

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard>;
}
```

**Object safety: the `DynMode` adapter.** `Mode` is not directly
object-safe because `Self::Guard` makes method signatures
generic. The registry stores `Arc<dyn DynMode>` (a public,
object-safe view); a blanket `impl<M: Mode> DynMode for M`
handles type erasure — the typed `Guard` returned by
`Mode::on_activate` is boxed as `Box<dyn Any + Send>` and
stashed in the dispatcher's `GuardStore`. Users implement
`Mode`; `DynMode` is implementation detail exposed only for
declarative-method access via `ModeRegistry::get`.

**Cleanup contract for `Guard::drop`:**
- Must not panic. Drop runs during deactivation paths and
  during buffer/registry teardown; a panic there is a
  corruption hazard (double-panic aborts the process).
- Must not block. Drop runs on whatever thread is calling
  `deactivate_*` (usually the App thread). Heavy work or
  `block_on` belongs in a spawned task (`lattice_runtime::
  spawn_task`), not in `Drop`.
- Async cleanup (e.g. awaiting `textDocument/didClose`) is
  spawn-and-detach: the `Drop` body spawns a task that runs
  the cleanup. The dispatcher cannot observe completion —
  that's the trade-off for compiler-enforced cleanup. If
  cleanup-await observability ever becomes a real requirement,
  a separate `AsyncDeactivate` trait can layer alongside
  `Mode` for the few modes that need it.

**Why Drop and not `on_deactivate`:**

| Property | Drop-based | Explicit `on_deactivate` |
|---|---|---|
| Cleanup forgotten? | Impossible — `Drop` is automatic. | Possible if author misses it. |
| Error-during-`on_activate` cleanup | Automatic via `?` (Guard dropped on early return). | Manual — every mode must handle partial-setup-then-error. |
| Buffer destruction without explicit deactivate | Automatic via `GuardStore::drop_buffer`. | Manual cleanup walk required. |
| Compiler-enforced? | Yes (Rust's drop checker). | No (convention). |
| Async cleanup observability | No (spawn-and-detach). | Yes (`Result` from `.await`). |
| Field on the trait | One method. | Two methods. |

The compile-time enforcement wins for lattice's actual cleanup
patterns (all sync nanosecond ops: `events.unsubscribe(...)`,
`config.set(...)`, `handle.abort()`). The async-observability
loss is theoretical — no in-tree mode needs it today.

**Reference: editors with similar patterns.** Zed uses the
same RAII model throughout — `Subscription` returned by
`cx.subscribe(...)`, `Task<T>` returned by
`background_executor().spawn(...)`. Helix uses Rust ownership
directly (no `Mode` trait, but `Document` owns its `Syntax`,
`Client` cleans up on drop, etc.). VSCode uses an explicit
disposables list (manual; not compiler-enforced; supports
async cleanup). Lattice's Drop-based design lands in zed's
cluster.

### 7.1.1 Concrete Guard examples

**Sync cleanup — unsubscribe an event handler:**

```rust
pub struct LspServerLogGuard {
    inner: Option<LspServerLogState>,
}
struct LspServerLogState {
    events: Arc<EventBus>,
    sub_id: SubscriptionId,
}
impl Drop for LspServerLogGuard {
    fn drop(&mut self) {
        if let Some(state) = self.inner.take() {
            state.events.unsubscribe(state.sub_id);
        }
    }
}

impl Mode for LspServerLogMode {
    type Guard = LspServerLogGuard;
    fn on_activate(&self, ctx: ModeContext)
        -> LifecycleFuture<'_, LspServerLogGuard>
    {
        Box::pin(async move {
            // ... setup, may early-return Ok(LspServerLogGuard { inner: None }) ...
            let sub_id = ctx.events().subscribe_typed(tx);
            Ok(LspServerLogGuard {
                inner: Some(LspServerLogState {
                    events: ctx.events_handle(),
                    sub_id,
                }),
            })
        })
    }
}
```

**Sync cleanup — restore a prior option value:**

```rust
pub struct FoldmethodRestoreGuard {
    config: Arc<ConfigRegistry>,
    prior: FoldMethod,
}
impl Drop for FoldmethodRestoreGuard {
    fn drop(&mut self) {
        folding_sync::on_deactivate(&self.config, self.prior);
    }
}

impl Mode for LspFoldingMode {
    type Guard = Option<FoldmethodRestoreGuard>;
    fn on_activate(&self, ctx: ModeContext)
        -> LifecycleFuture<'_, Option<FoldmethodRestoreGuard>>
    {
        Box::pin(async move {
            let prior = folding_sync::on_activate(ctx.config());
            Ok(prior.map(|p| FoldmethodRestoreGuard {
                config: ctx.config_handle(),
                prior: p,
            }))
        })
    }
}
```

**Composed cleanup — multiple resources:**

```rust
pub struct MyModeGuard {
    _sub_guard: SubscriptionGuard,   // drops → unsubscribe
    _task_abort: TaskAbortGuard,     // drops → JoinHandle::abort
}
// No Drop impl needed; fields drop in declaration order,
// each running its own Drop.
```

**No cleanup — marker mode:**

```rust
impl Mode for TextMode {
    type Guard = ();
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_> {
        Box::pin(async move { Ok(()) })
    }
}
```

### 7.1.2 `ModeContext` shape

`ModeContext` is owned, `Send + 'static`, cheap to construct
(clones a handful of `Arc`s):

```rust
pub struct ModeContext {
    buffer_id: BufferId,
    current_mode: ModeId,
    config: Arc<ConfigRegistry>,
    events: Arc<EventBus>,
    services: Arc<ServiceRegistry>,
}
```

**Notably absent: `BufferLocals`.** Modes don't read or write
buffer-locals through ctx. Mode-private state lives in the
typed `Guard` (carried between activate and deactivate by the
dispatcher); App-managed rich locals (`FileTreeEntries`,
`HelpLinks`, `OilDir`, etc.) stay in the App's own
`HashMap<BufferId, BufferLocals>` and are never exposed to
mode lifecycle hooks. This is the cleaner architecture — the
shared map was always single-writer-per-key in practice; the
typed Guard makes that explicit and compiler-enforced.

The `service::<T>()` lookup is read-only access to subsystem
handles (BufferStoreHandle, LspSupervisorHandle, LspLogger).
`events_handle()` and `config_handle()` are cheap `Arc::clone`
helpers used when a Guard needs to hold one of these across
its Drop call.

### 7.2 Activation / deactivation flow

**Activation (synchronous prefix):**

1. Validate the mode (registered, kind matches, capabilities
   satisfied, no conflicts, all implied modes registered).
2. Construct `ModeContext` (cheap — Arc clones).
3. Drive `mode.on_activate(ctx)` to completion via `poll_now`
   (M-async.2b.foundation) or spawn on runtime
   (M-async.2c, future).
4. Stash the returned typed Guard in `GuardStore` keyed by
   `(BufferId, ModeId)`. The Guard is box-erased to
   `Box<dyn Any + Send>` for uniform storage.
5. Mutate `active_modes`, emit `MajorEntered` /
   `MinorActivated` events.
6. Cascade implied modes.

**Deactivation:**

1. Look up the active mode; emit `MajorExiting` /
   `MinorDeactivated` synchronously so subscribers see a
   clean active-mode set.
2. Remove from `active_modes`.
3. `GuardStore::guards.remove((BufferId, ModeId))` — the
   returned `Box<dyn Any>` is dropped, firing the typed
   Guard's `Drop` impl which performs cleanup (sync, brief).
4. Cascade-deactivate implied modes (each drops its own
   Guard).

**Buffer destruction without explicit deactivate:** the App
calls `GuardStore::drop_buffer(buffer_id)`, which retains
only entries for other buffers; the removed entries drop,
firing every active mode's `Drop`. No "did we forget to
deactivate?" surface — Rust's drop checker guarantees the
cleanup runs.

### 7.3 Re-entrancy and ordering invariants

The dispatcher guarantees:

- A mode's `on_activate` future for buffer `B` runs to
  completion before its Guard lands in the `GuardStore`. If
  the future is immediately ready (today's marker modes + sync
  LSP modes), it completes on the App thread inside the
  try-sync-then-spawn driver -- no spawn boundary, so rapid
  `activate → deactivate` is race-free by construction.
- A mode whose `on_activate` truly `.await`s (yields Pending
  on first poll) trips the driver into spawning the
  remainder. A deactivate arriving while the spawn is in
  flight is reconciled via the **epoch counter** (§7.3.1).
- Cross-mode activation (e.g. `LspMode` implies
  `LspDiagnosticsMode`) sequences via the cascade plan: the
  driver walks the plan DFS, awaiting each step's future
  before the next. Implied children always see the parent's
  post-`on_activate` state.
- Cascade abort: when a step's `on_activate` returns `Err`,
  the driver publishes `ModeActivationFailed` for the failing
  step plus a synthetic `cascade aborted by <trigger>`
  failure for every remaining unrun step. The App's per-tick
  drain (`drain_mode_lifecycle_events`) calls
  `deactivate_mode_by_id` for each, cleaning `active_modes`
  + `mode_guards` for the whole subtree.

These guarantees are enforced inside the dispatcher, not by
hook discipline. Modes don't need to defensively check
ordering.

#### 7.3.1 Epoch counter for activate / deactivate
serialization

When a mode's `on_activate` future yields `Pending` on first
poll, the driver spawns the remainder onto the runtime. The
spawn's eventual `try_insert` of the Guard back into the
`GuardStore` would race a synchronous deactivate that
arrived in the meantime, leaving a leaked Guard in a
logically-inactive store slot.

The `GuardStore` carries an `epochs: HashMap<(BufferId,
ModeId), u64>` map. The protocol:

1. **Sync prefix (`activate_*`)** calls
   `guards.bump_epoch(buffer, mode)` when queueing each
   `CascadeStep`; the new epoch is stored on the step.
2. **Spawn task**, after `step.entry.on_activate_dyn(ctx)
   .await` resolves with `Ok(guard)`, calls
   `guards.try_insert(buffer, mode, step.epoch, guard)`.
   - If current epoch == `step.epoch`: insert succeeds; the
     driver publishes `MajorEntered` / `MinorActivated`.
   - If mismatch: `try_insert` returns `Err(stale_guard)`.
     The driver drops the Box in place; the original
     Guard's `Drop` fires for out-of-band cleanup
     (publishes `LspBufferDetached`, restores prior
     `foldmethod`, etc.). No success event is published.
3. **Sync deactivate (`deactivate_*`)** calls
   `guards.remove(buffer, mode)`, which **internally bumps
   the epoch first** (invalidating any in-flight spawn) then
   removes the (possibly-present) Guard. Either way, the
   deactivate publishes `MinorDeactivated` /
   `MajorExiting` synchronously.

The protocol's correctness relies on:

- **Drop-based cleanup contract** (§7.1): the Guard's `Drop`
  must be sufficient for cleanup regardless of when it
  fires (synchronously from deactivate, or out-of-band from
  the spawn detecting a stale epoch).
- **Standard-future re-registration**: a future polled with
  one waker then re-polled with another (tokio's) must
  re-register with the new waker; standard Rust futures
  honour this.
- **u64 wrap tolerance**: 2^64 activate / deactivate cycles
  per `(buffer, mode)` between a spawn's queue and its
  completion would be needed to alias an epoch. Not a
  realistic concern.

The protocol does **not** require a `tokio::sync::Mutex` per
pair. The epoch counter resolves the race lock-free; the
spawned task's `try_insert` is a single `std::sync::Mutex`
acquisition (already needed for the store map).

### 7.4 Activation triggers — what *fires* activation (no mode-set scan)

§7.2 covers the activation *flow* once the dispatcher has decided to
activate a mode. This section pins *what makes the decision*, and the
load-bearing constraint behind it: **activation is never an O(modes)
scan.** As the mode set grows (every language, every feature minor, every
plugin), any scheme that polls every mode per buffer-lifecycle event ("do
you apply here?") couples the host to all modes and, on the buffer-switch
path, risks a dropped frame (paramount #1). Activation is driven by
*subscriptions* — bounded by who subscribed to the firing event, not by the
mode count. This is the §7 intro made concrete: lifecycle rides the event
bus with the same filter machinery as any typed event.

Two triggers, matching the two mode kinds (the emacs parallel — `auto-mode-alist`
for major, hooks for minor):

**Major mode — an ordered resolver on `DocumentOpened`.** A buffer has
exactly one major mode (§3.1); a broadcast filter can't express
first-match-wins, so major-mode selection is a thin ordered resolver
subscribed to `Event::DocumentOpened`:

	1. Run registered major-mode matchers in priority order; first wins.
	2. Built-in language modes reuse `lattice_syntax::Lang::detect_from_path`;
	   plugin majors supply a `path_glob` / content predicate -- the *same*
	   glob util `EventFilter.path_glob` uses, one matcher not two.
	3. Drive the winner's activation (§7.2); that emits `MajorEntered`.

O(major-modes), on open only, declarative, async — the `auto-mode-alist`
shape, and the one place a scan legitimately survives. Nothing per-keystroke.

**Minor mode — a single host resolver over `MajorEntered` (decision B,
2026-06-12).** Each minor declares a default `ActivationPolicy`
(`Mode::activation_policy()`, §5.1) — `Manual` (the default: opt-in only),
`Global`, or `Majors([..])` (an allowlist of major-mode ids). The host runs
**one** resolver subscribed to `Event::MajorEntered`; on each entry it queries
`ModeRegistry::auto_activatable_minors(major)` — which walks the registered
minors and returns those whose (config-folded) policy admits the entered major
— and calls `activate_minor` (§7.2) for each. A paired `MajorExiting` /
`DocumentClosed` handler deactivates. This is O(registered minors) on a *rare*
event (buffer open / major switch), never per-keystroke.

`Global` is scoped to **real document buffers** (`BufferKind::Document`):
"global" means every code/text buffer, not the synthetic UI buffers (file
tree, help, `*messages*`, terminal, multibuffer). The resolver passes the
buffer's kind to `auto_activatable_minors(major, kind)`, and
`ActivationPolicy::admits` gates `Global` on `kind == Document`. A mode that
genuinely wants to live inside a synthetic buffer names that buffer's major
explicitly via `Majors([..])`, which is kind-independent — an explicit opt-in
is honored anywhere. (MA.2; decided with the user 2026-06-12.)

Why a single resolver rather than one bus subscription per minor (the shape an
earlier draft of this section described): both are O(minors) per `MajorEntered`,
but the per-mode subscription still needs a generic host driver to reach
`activate_minor` (there is no `CommandInvocation` for "activate minor X"), so it
buys no extra mode-ownership over the resolver — only N bus subscriptions and
drain plumbing instead of one. The substrate-vs-mode rule (CLAUDE.md) puts an
activation policy that *uniform host machinery reads across all minors* on a
trait-method-as-data + generic resolver (the `dispatch_with_cancel` shape), not
a per-mode subscription. The mode still owns its declared allowlist; the host
owns the single resolver + the config fold.

This is **not** a resurrection of the `Mode::subscriptions()` method removed in
MO.4.c. That method was a *reactive, while-active* subscription mechanism
(replaced by `on_activate` + a Guard-held RAII `Subscription`); the activation
policy here is a *registration-time, while-inactive* trigger — a genuinely
different lifecycle phase. The two coexist: a mode declares `activation_policy()`
to be *turned on*, and subscribes inside `on_activate` to *react while on*.

For the resolver's filter to work, the major lifecycle events must be
filterable by major-mode id. MA.1 lands the **four observable lifecycle
transitions** — `MajorEntered` / `MajorExiting` (carrying
`{ buffer, major: String }`) and `MinorActivated` / `MinorDeactivated`
(carrying `{ buffer, minor: String }`) — as `lattice_protocol::Event` enum
variants (design.md §5.10.1's public catalog), migrating them off the typed
`ModeEvent` bus so hooks and EF.1's `EventFilter` apply uniformly. Only the
**internal** `ModeActivationFailed` / `OptionConflict` cascade-and-rollback
signals stay on the typed `ModeEvent` bus — they carry richer payloads (a
reason string, a conflicting-mode set) and have a single internal consumer,
not open subscription.

**`EventFilter` — implement the reserved fields, don't fork a matcher.**
`lattice_runtime::EventFilter` honors `kinds` only today; `path_glob`,
`major_modes`, and `predicate` are *reserved* in its own module doc
("declared… stay TODO until callers need them"). Mode activation is that
caller. Implement them — AND-combined, checked on the already-`kinds`-bucketed
candidates so the publish scan never widens — rather than a parallel matcher
type that would duplicate the filter the bus already dispatches on.
`predicate` is the escape hatch; modes prefer the declarative fields so the
bus can reason about them.

**Resolver wired once, at host boot.** The host subscribes the single
minor-activation resolver to `Event::MajorEntered` once, at boot. After that
the bus does indexed O(subscribers-of-kind) dispatch to it; the resolver's
per-entry `auto_activatable_minors(major)` walk is O(registered minors) on a
rare event. Nothing re-scans the mode set per keystroke. (Mode *registration*
no longer wires any bus subscription — `Mode::subscriptions()` was removed in
MO.4.c; reactive subscriptions are `on_activate` + Guard, §5.1.)

**Allowlist is explicit; a mode may default to none.** Activation criteria
are never inferred from the runtime registry ("activate where snippets
exist"). A mode declares a *default* `ActivationPolicy` via
`activation_policy()`, which may be `Manual` / an empty `Majors([])` — then it
activates nowhere until the user opts in. Leaving the onus on the user is a
legitimate mode choice (some modes won't ship a sensible default and shouldn't
guess). TOML carries the policy
(`<mode>.activation = global | <allowlist> | off`); the host folds the option
over the declared default before `auto_activatable_minors` consults it, and
re-folds on an `OptionChanged` subscription so `:set` is live. `init.rs`
(WASM, bus access) is the programmable override — force-global, disable, or a
custom predicate via the resolver's `EventFilter`.

**Async / off-UI-thread (paramount #1).** The resolver handler runs async
(modes are tokio tasks, §7.1 / design §5.7); activation work never runs
inline on a publish that originates on the render path. The bus drops its
mutex before dispatch (audit M1); the handler must not block it.

**Worked example — snippet.** `SnippetCompletionMode::activation_policy()`
returns its config-driven, possibly-empty language allowlist
(`ActivationPolicy::Majors([..])`); the host resolver activates it when a
buffer enters one of those majors. Today's buffer-*kind* auto-activation
(language-blind, via `auto_activated_minors_for_buffer_kind`) is replaced by
this language-aware policy. `SnippetActiveMode` (the `<Tab>` placeholder-nav
mode) is already correctly session-transient (activated by
`sync_keymap_overlays` while `active_snippet.is_some()`) and is unchanged.

**Rejected: `Mode::wants_buffer(ctx) -> bool` polled over all modes.**
O(modes) per lifecycle event, host coupled to every mode, a sync scan on the
buffer-switch path risks a frame block. Rejected on paramount #1 + heuristic
#1; the resolver model is the merit win. This *was* the obvious option; the
substrate's reserved `EventFilter` + a registry query is the third option that
fits the goals and removes the duplication.

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

design.md §5.10 already commits to a typed event bus with
filter-based subscriptions. Mode lifecycle events (§7) are
just additional payload variants. Mode-declared subscriptions
register with the bus on activation, deregister on
deactivation.

### 9.3 Configuration registry

design.md §5.12 + the existing `lattice-config::ConfigRegistry`
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
| M.0       | This doc (mode-architecture.md) reviewed + accepted. design.md §5.6 / §5.8 / §5.9 / §5.10 / §5.12 augmented with links + the §5.8.3 correction. No code.                                                                                                                                                                                                                                                                                                                                                                                                                       | `docs/`                                                                     | User reviews + signs off; design.md links land.                                                                                                                                                                               |
| M.1       | New `lattice-mode` crate. `Mode` trait, `ModeRegistry`, `ActiveModes` on `Document` (the actual lattice-core per-buffer-state container; `Buffer` is the rope wrapper), lifecycle event variants. No actual modes registered. Tests for registration, conflict, capability checks.                                                                                                                                                                                                                                                                                             | new `lattice-mode`, `lattice-core`                                          | `cargo test -p lattice-mode` green; `Document` carries `ActiveModes` (empty by default).                                                                                                                                      |
| M.2.0     | **`lattice-config` types-as-keys + resolver primitives.** `Option` trait, `options!` / `editor_options!` macros, `OptionGroup` trait + `groups!` macro, `linkme` aggregation, `OptionOverride` / `OptionOverrideSet`, `Resolver`, `ResolvedOptions`. Migrate every existing built-in option from `Option::new()` to the macro form. Remove the public `Option::new()` constructor (keep `register_erased` `pub(crate)` for the M.10 plugin adapter).                                                                                                                           | `lattice-config` (callers in every crate that registers options)            | Macro API is the only public surface; existing options work identically; `resolve(layered)` returns `ResolvedOptions` correctly for any iterator of layers; cross-crate display-name uniqueness panics at startup.            |
| M.2.1     | **Mode-driven layers + App orchestration.** `Mode::options() -> OptionOverrideSet` real shape (`OptionOverride` / `OptionOverrideSet` / `OverridePriority` moved to `lattice-mode` to break the cycle); `lattice-config::overrides!` proc macro for compile-time-typed override construction; App carries `active_modes` / `buffer_local_overrides` / `resolved_options` keyed by `BufferId`; `App::recompute_options_for_buffer(...)` stitches layered input; `App::resolved_option::<D>(buffer)` for type-keyed reads. Bench in ../operations/benchmarks.md for resolution + invalidation. | `lattice-mode`, `lattice-config`, `lattice-config-macros`, `lattice-ui-tui` | `resolved_get_typed` p99 < 50ns (measured ~13.5ns); `resolve_into_10_layers` p99 < 10µs (measured ~851ns). Mode toggles refresh the cache; reads are O(1).                                                                    |
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
| M.5       | **`lsp-mode` umbrella ✅ landed across M.5.0–M.5.7.** Per-buffer minor mode gating every LSP feature: request entry points (M.5.4), document sync (M.5.5), diagnostic rendering (M.5.6). Auto-activates on `MajorEntered` for languages with a configured server (M.5.2); deactivates send `textDocument/didClose` (M.5.3). User toggles via `:lsp-mode` (M.5.1's auto-generated `:<mode-name>` command). Architectural side effects: typed-event surface in `lattice-protocol::event_registry` + `EventBus::publish_typed`/`subscribe_typed` (M.5.3.a); LSP events own themselves in `lattice-lsp::events` (M.5.3.b); `:describe-events` walks the registry (M.5.3.c); `BufferDisplay`/`BufferDisplayCategory` for per-feature display preferences (M.4 follow-up). | `lattice-lsp`, `lattice-protocol`, `lattice-runtime`, `lattice-ui-tui`      | `:lsp-mode` toggle silences all LSP traffic for the buffer; re-toggle resumes.                                                                                                                                                |
| M.6       | **LSP sub-modes**. `lsp-completion-mode`, `lsp-diagnostics-mode`, `lsp-hover-mode`, `lsp-signature-mode`, `lsp-format-mode`, `lsp-rename-mode`, `lsp-symbols-mode`, `lsp-code-action-mode`, `lsp-nav-mode`, `lsp-progress-mode`, `lsp-document-highlight-mode`, `lsp-selection-range-mode`, `lsp-folding-mode`, `lsp-inlay-hint-mode`, `lsp-semantic-tokens-mode`. Each independently toggleable.                                                                                                                                                                                                                                                                                                                                     | `lattice-lsp`                                                               | Each sub-mode independently disable-able; tests cover gating per-feature.                                                                                                                                                     |
| M.6.5     | **Namespace cleanup**: rename the existing `lsp.log-level` typed option (shipped pre-M as the LSP-config-loaded check) to `lsp-mode.log-level` so it sits in the mode-owned namespace. The `lsp.*` namespace is then exclusively the structural `workspace/configuration` passthrough (§6.5.2). One-line code change + TOML migration note.                                                                                                                                                                                                                                    | `lattice-ui-tui`                                                            | Old key emits a deprecation echo for one minor version, then is removed.                                                                                                                                                      |
| M.7       | **Display minor modes** wrapping existing typed options: `line-numbers-mode`, `relative-line-numbers-mode`, `whitespace-show-mode`, `current-line-highlight-mode`, `read-only-mode`, `wrap-mode`. `:set number` and `:enable line-numbers-mode` converge on the same state.                                                                                                                                                                                                                                                                                                    | `lattice-mode`, `lattice-ui-tui`                                            | Both surfaces work; toggling either updates the other.                                                                                                                                                                        |
| M.8       | **Introspection**. `:describe-mode <name>`, `:list-modes`, `:describe-option-resolution <name>` (showing the layer each resolved value came from).                                                                                                                                                                                                                                                                                                                                                                                                                             | `lattice-mode`, `lattice-ui-tui`                                            | All three commands populated and tested.                                                                                                                                                                                      |
| M.9       | **`:customize <name>` form view (TUI)**. New `customize-mode` major and `OptionGroup` registry. Built-in groups pre-registered (`Editor`, `Display`, `Editing`, `Lsp`, `Completion`, `Picker`, `Filetree`, `Oil`, `Help`, `Appearance`). Resolution: `<name>` ending in `-mode` ⇒ that mode's options; otherwise ⇒ that group's members, sectioned by owning mode. Per-row navigation + edit + apply through `:set` machinery. Apply is session-only; TOML write-through deferred (§12). Group picker for `:customize` with no args.                                           | `lattice-mode`, `lattice-ui-tui`                                            | `:customize lsp-completion-mode` (mode), `:customize lsp` (group), `:customize editor` (bare names) all work; edits apply; `:customize` with no args opens the picker.                                                        |
| M.10      | **WIT plugin path**. ⏸ Deferred to the broader Plugin Architecture phase. WIT API for declaring modes from WASM components, adapter that implements `Mode` trait by bridging WIT calls, parity validation across built-in / plugin distribution. Co-evolves with the WIT capability surface, Component Model toolchain wiring, and fuel/sandboxing model for the host -- those decisions belong to the plugin-architecture work, not the mode-architecture migration. The mode-side groundwork is in place: `Mode` is a trait (not a struct), `ModeId` is name-keyed (not type-keyed), and the registry / lifecycle / option-resolution / introspection surfaces are origin-agnostic by construction. When the WIT path lands, the adapter slot goes into existing scaffolding.                                                                                                                                                                                                                                                                                                                                                                                       | `lattice-mode`, `wit/`                                                      | Plugin-architecture phase.                                                                                                                                                                                                    |

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
- M.3.2.c.5 -- ✅ landed.
  - **DocumentEntry mode-fields fully retired**: `syntax`,
    `last_parsed_text_version`, `last_synced_syntax_version`, `folds`
    are gone from `DocumentEntry`; the entry now holds only `id` +
    `handle`. Activation transitions stash / restore mode-state
    through `buffer_locals` directly via
    `seed_empty_document_locals(id)` (initial seed at construction)
    and `snapshot_active_document` (de-activation stash).
  - **HelpBuffer**: struct fields `links`/`anchors`/`highlights`
    retired. `HelpContent` bundles a slim `HelpBuffer` (id, title,
    rope, cursor, scroll) with a sibling `HelpMetadata` carrying
    the parsed `[label](url)` links, named anchors, and per-line
    markdown highlight spans. `App::open_popup` seeds the metadata
    into `buffer_locals[help.id]` via `seed_help_metadata_locals`.
    Vestigial `HelpBuffer::link_at` / `scroll_to_anchor` /
    `with_markdown_syntax` methods removed; `HelpContent` carries
    `scroll_to_anchor` (reads `self.metadata.anchors`) and
    `with_markdown_syntax` (writes `self.metadata.highlights`)
    so test ergonomics survive. App-side accessors
    `popup_help_links()` / `popup_help_anchors()` /
    `popup_help_highlights()` give tests a chokepoint that mirrors
    the renderer's `buffer_locals`-keyed read path.
  - **FileTreeBuffer**: free functions + `App` chokepoints
    (`set_file_tree_root` / `set_file_tree_entries` /
    `set_file_tree_nerd_fonts`) drive all writes. `root`,
    `entries`, `nerd_fonts` live exclusively in the
    `FileTreeRoot` / `FileTreeEntries` / `FileTreeNerdFonts`
    locals. `FileTreeMode` moved to `lattice-file-tree`.
  - **OilBuffer**: free functions + `App::set_oil_dir`,
    `oil_dir_for`, `oil_with_dir`, `do_oil_navigate_up`
    chokepoints. `dir` lives exclusively in the `OilDir`
    local. `OilMode` moved to `lattice-oil`.
  - **BufferStorage decision: keep the enum.** Document is
    structurally different from Help / FileTree / Oil (its content
    lives in an actor accessed via `DocumentHandle`; the others
    embed a rope inline + carry kind-specific methods whose
    semantics are meaningfully distinct). Collapsing into one
    `Buffer` struct would either smear the Document-vs-rope
    distinction or inline `Option<DocumentHandle>` on every kind --
    either way encoding the dispatch the enum already encodes
    cleanly.
- M.4 -- ✅ landed.
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
    `open_popup` and `do_open_hover` register the popup's
    `HelpBuffer` in `app.buffers` with
    `BufferFlags { listed: false, hidden: true }` (skipped by
    `:bn` / `:bp` / `:ls`; informational `hidden`), matching the
    pattern `open_help_in_pane` already uses for `:lsp-log` etc.
    `dismiss_popup` removes the entry plus its `active_modes` /
    `buffer_locals` / `resolved_options`; back-to-back popups no
    longer leak stale state. The State-A hover auto-dismiss path
    routes through `dismiss_popup` for a single cleanup edge.
  - **`App.popup_buffer` flips to `Option<BufferId>`.** The
    HelpBuffer itself lives in `app.buffers`; the slot is just a
    reference into the registry. Two accessors,
    `App::popup_help` / `App::popup_help_mut`, resolve the slot
    through the registry on demand; the field's name and
    contract stay (popup-content reference, not help-only).
    Bulk-migrated ~50 reader sites and 8 writer sites; the
    `:lsp-log` rebuild path drops its hot-path-mirror sync (the
    registry copy was the only canonical record). The popup is
    now structurally a normal buffer with one extra reference.
  - **Generic per-feature display preferences.** New
    `lattice_core::ui::display` decouples *what* (the buffer)
    from *where* (the display) for every dedicated-buffer
    producer.
    - `BufferDisplay`: `Popup(PopupPlacement) | ActivePane |
      Split(SplitOrientation)`.
    - `BufferDisplayCategory`: per-feature *intent* tag
      (`LspStatus`, `LspLog`, `HelpTopic`, `HelpDescribe`,
      `HelpApropos`, `HelpList`, `Hover`, `Signature`,
      `PickerResult`). Multi-choice surfaces (`:diagnostics`,
      `:references`, `:symbol`, `:Files`, `:buffers`,
      `:lsp-server-log`) are picker-shaped by design and route
      their accept destination through `PickerResult`; their
      picker UI itself isn't a category.
    - `App::display_buffer(content, category)` is the single
      dispatch seam; `App::prepare_pane_for_picker_result()` is
      its picker-accept counterpart. Defaults are coded in via
      `default_display(category)` and match the prior
      hard-coded behaviour exactly. User-facing
      `:set <category>.display = popup|active-pane|split-h|
      split-v` overrides arrive as a follow-up via the typed-
      option system.
    - Retired the unused `App.help_display_mode` field and
      `lattice_help::HelpDisplayMode` enum -- subsumed by
      `BufferDisplay` (one enum across LSP, help, hover,
      signature, picker, ... rather than help-only).
    - Hover keeps its custom path until a minor-mode-override
      hook lands (it activates `hover-mode` rather than the
      default `help-mode` minor); the `Hover` / `Signature`
      categories are reserved for that migration.
- M.4 follow-ups -- ✅ all landed.
  - **Hover via `display_buffer` + `FloatingPopup` variant.**
    `BufferDisplay::FloatingPopup(placement)` codifies State A
    semantics (popup floats; doc keeps focus; `markdown-mode`
    major + `hover-mode` minor). `App::open_floating_popup` is
    the dispatch target. `default_display(Hover)` and
    `default_display(Signature)` route here. `do_open_hover`
    is now a one-line `display_buffer(content, Hover)` call.
  - **`:set <category>.display = ...` user overrides.**
    `BufferDisplayPreference` enum in
    `lattice_core::ui::display` (flat: `Default | PopupCentered
    | PopupCursor | FloatingCursor | ActivePane | SplitHorizontal
    | SplitVertical`); `OptionType` impl in
    `lattice_config::domain`. Nine typed options under the
    `Display` group (`lsp.status.display`, `lsp.log.display`,
    `help.topic.display`, `help.describe.display`,
    `help.apropos.display`, `help.list.display`,
    `hover.display`, `signature.display`,
    `picker.result.display`). Each defaults to `Default` --
    `App::resolve_display(category)` reads the option, calls
    `pref.resolve(category)`, falls through to
    `default_display(category)` on `Default`. No behavioural
    change unless the user opts in.
  - **Bug 4: wrap behaviour for non-document buffers.**
    `HelpMode` and `HoverMode` contribute `Wrap = true` via
    `Mode::options()`; the resolver layers it over the editor
    default (`Wrap = false`) and the renderer's existing
    `option_cache.wrap_lines` pipeline picks it up. Long
    markdown bodies (help docs, hover descriptions) wrap
    inside their pane / popup width instead of overflowing
    horizontally. File-tree, oil, and lsp-log modes
    intentionally don't contribute -- line-oriented content,
    wrapping confuses the layout.
- M.5.0 -- ✅ landed. `LspMode` minor declared in
  `lattice-lsp::modes` (kind = Minor; no capability
  requirements -- standalone-server use cases want activation
  on un-named buffers). Registered at boot via
  `register_lsp_log_modes`. App accessor
  `App::lsp_mode_enabled_for(buffer_id) -> bool` reads the
  active-modes set; default-false everywhere until M.5.2's
  auto-activation hook lands. Lifecycle hooks are no-ops in
  this slice; M.5.3 wires the real attach/detach +
  didOpen/didClose.
- M.5.1 -- ✅ landed. §9.6.1 auto-generated `:<mode-name>`
  toggle commands. Boot-time
  `register_mode_toggle_commands(cmd_registry, mode_registry)`
  walks every registered mode and registers a per-mode
  ex-command whose apply-fn returns
  `Effect::ToggleMode { mode_name }`. The dispatcher routes
  that to `App::toggle_mode_by_name`; minors flip
  active↔inactive, majors activate (registry treats
  re-activation of the current major as a reload, otherwise
  swaps in). New `ModeRegistry::iter_meta()` exposes
  `(ModeId, ModeKind)` for catalogue iteration (also used by
  M.8's `:list-modes`). Programmatic API:
  `App::activate_mode_by_id` / `deactivate_mode_by_id` /
  `toggle_mode_by_name` for hooks / config callers. **Major
  swaps don't touch active minors** -- state lives in
  per-mode typed `BufferLocals`, so we get emacs's "globalised
  minor" UX without `kill-all-local-variables` semantics.
  User-facing `:help modes` topic added.
- M.5.2 -- ✅ landed. `lsp-mode` auto-activation hook on
  major-mode entry. New `LspSupervisor::has_server_for_path`
  / `LspSupervisorHandle::has_server_for_path` answers "is
  there a configured server for this path?" via the existing
  `file_patterns` registry. App-side
  `maybe_auto_activate_lsp_mode(buffer_id)` runs after every
  major activation (both the buffer-creation path and the
  `:<mode-name>` toggle path) -- looks up the buffer's path,
  asks the supervisor, activates `lsp-mode` if a match
  exists and `lsp-mode` isn't already active. Modelled as a
  synchronous post-activation hook for now; converting to an
  event-bus subscription on `MajorEntered` is a follow-up
  once the broader subscriber API lands. **Asymmetric by
  design**: no auto-deactivate on `MajorExited` -- minors
  survive major swaps; user runs `:lsp-mode` to flip off
  explicitly. Per-major opt-out (`:set rust-mode.lsp = false`)
  is deferred to a future slice once typed-option per-mode
  conventions are settled.
- M.5.3 -- ✅ landed. `lsp-mode` activation / deactivation
  lifecycle. Two new editor events
  (`Event::LspBufferAttached` / `Event::LspBufferDetached` in
  `lattice-protocol`) carry the higher-level "buffer is now
  / no longer LSP-tracked" semantic. App-side
  `on_lsp_mode_activated` / `on_lsp_mode_deactivated` fire
  from `activate_mode_by_id` / `deactivate_mode_by_id` when
  the target mode is `LspMode`. Deactivation routes through
  the existing `lsp_close_buffer` path: sends
  `textDocument/didClose` per attached server (the LSP
  wire-level "stop tracking this URI" message; matches
  nvim's `vim.lsp.buf_detach_client` and emacs `lsp-mode` /
  eglot), clears `App::buffer_uris[id]`, the server
  connection persists if other buffers are still attached.
  Activation today emits the event signal only; wire-level
  `didOpen` still fires from the `attach_driver` listening
  to `DocumentOpened` (file-open path). Unifying attach
  through lsp-mode (re-attach on `:lsp-mode` re-activate
  after a deactivate) is M.5.5 territory.
- M.5.3.a -- ✅ landed. Typed-event surface + EventBus typed
  API. Replaces "every event in `lattice-protocol::Event`"
  with "feature crates declare their events at home;
  protocol owns the declaration API." Mirrors the
  `lattice-mode` ownership model.
  - `lattice-protocol::event_registry`: `Event` trait
    (Any + Debug + Send + Sync + 'static), `EventTypeId`
    (TypeId + name), `EventDescriptor` (name, doc, source
    crate), `EVENT_DESCRIPTORS` `linkme` distributed slice,
    and `register_event!` macro that impls the trait and
    pushes the descriptor in one declaration.
  - `lattice-runtime::EventBus::publish_typed::<T>` /
    `subscribe_typed::<T>` -- type-erased internal
    `Arc<dyn Any + Send + Sync>` bus with TypeId-keyed
    buckets and downcast forwarder closures. Closed-channel
    pruning + lock-discipline mirror the legacy enum path.
  - The legacy `Event` enum and enum-based bus stay; new
    events (M.5.3.b LSP migration first) use the typed
    path. Migrating built-in variants is a future
    cleanup slice. `:describe-events` (M.5.3.c) walks
    `EVENT_DESCRIPTORS`.
- M.5.3.b -- ✅ landed. LSP events out of `lattice-protocol`'s
  central enum and into `lattice-lsp::events` as concrete
  types using the typed-event surface from M.5.3.a:
  `LspBufferAttached { id, path }`,
  `LspBufferDetached { id, path }`,
  `LspLogPushed { server_id: Arc<str>, level, source, message }`.
  Each registers via `register_event!` so
  `EVENT_DESCRIPTORS` enumerates them for the M.5.3.c
  `:describe-events` view. Publishers route through
  `EventBus::publish_typed` (`LspLogger`'s publisher
  closure, App's mode activate/deactivate hooks).
  Subscribers use `EventBus::subscribe_typed::<T>` (App
  boot's lsp-log live-tail wiring). `LogEventPublisher`'s
  type changes from `Arc<dyn Fn(Event)>` to
  `Arc<dyn Fn(LspLogPushed)>`. The three variants are
  removed from both `Event` and `EventKind`; remaining
  built-in events stay until a follow-up cleanup migrates
  them.
- M.5.6 -- ✅ landed. Diagnostic rendering gate on
  `lsp-mode`. Three render sites consult
  `lsp_mode_enabled_for(active_buffer)` and skip when the
  gate is closed: `severity_for_line` (gutter glyph),
  `diagnostics_on_line` (inline underline overlay),
  `active_lsp_segment` (modeline `[lsp:<server>]` badge).
  The `DiagnosticsLayer` keeps storing data the
  supervisor's per-server fan-in pumps in; the renderer
  pretends none exists for the gated buffer. LSP
  completion candidate generation was already gated in
  M.5.4 (insert-mode + explicit `:complete` both
  short-circuit), so this slice needed no further
  completion-side code.
- M.5.7 -- ✅ landed. End-to-end integration coverage +
  user-facing documentation. The new
  `lsp_mode_round_trip_end_to_end` test exercises every
  M.5 gate across one buffer's lifetime: file-open
  auto-activation (M.5.2), toggle-off detach event (M.5.3)
  + request-gate echo (M.5.4) + publish-site sync gate
  (M.5.5), toggle-on resumption. `docs/../../user/lsp-mode.md`
  documents the gate behaviour for end users (toggle,
  auto-activation, what's gated when off, programmatic
  API, editor-bus events) and is reachable via
  `:help lsp-mode`. **M.5 is complete.**
- M.5.5 -- ✅ landed. Document sync gate on `lsp-mode`.
  `lattice-lsp::events::LspDocumentChanged` (typed event,
  `register_event!`-registered) replaces the per-actor
  fan-in's prior `Event::DocumentChanged` subscription. App's
  `publish_document_changed` always publishes the generic
  `DocumentChanged` (non-LSP subscribers stay informed) and
  additionally publishes the typed `LspDocumentChanged` only
  when `lsp_mode_enabled_for(active_buffer)` is true. Result:
  with the gate off, no didChange flows to any attached
  server; with the gate on, fan-in routes per-edit work the
  same way it did before.
  - `EventBus::unsubscribe` extended to walk the `typed_subs`
    bucket (M.5.3.a's typed-event surface). Supervisor
    shutdown that unsubscribes the fan-in tears down typed
    subscriptions correctly.
  - didSave wire plumbing isn't in place today (capabilities
    advertise it but no fan-in pumps `Event::DocumentSaved`
    to servers); when that lands, the same pattern applies
    via a typed `LspDocumentSaved`.
- M.5.4 -- ✅ landed. LSP request entry points gate on
  `lsp_mode_enabled_for(active_buffer)`. Thirteen `do_lsp_*`
  methods updated (hover, completion, on-type-format,
  rename, code-action, format, document-symbol,
  workspace-symbol, signature-help, nav including the
  shared definition / declaration / typeDef / impl path,
  references). The gate fires *after* any prior-token
  cancellation so stale in-flight work tears down even
  when the gate is now closed. New
  `App::check_lsp_mode_gate()` helper centralises the
  echo: ex-command and chord-keymap entry points emit one
  `Info`-level "lsp-mode disabled (`:lsp-mode` to enable)"
  echo so the gate is discoverable; insert-mode triggers
  (completion / on-type-format / signature) gate silently
  via the raw `lsp_mode_enabled_for` check (typing path
  doesn't echo on every keystroke). Pre-existing tests
  that probed post-gate behaviour (URI-bail, tag-origin
  capture, pre-cancel-prior-token) updated to activate
  lsp-mode first; one new test verifies the gate echo
  semantics.
- M.5.3.c -- ✅ landed. `:describe-events` ex-command
  introspection. `Effect::DescribeEvents` /
  `Effect::DescribeEvent { name }` register as
  `ex:describe-events` / `ex:describe-event` with
  user-facing aliases. `App::do_describe_events` walks
  `EVENT_DESCRIPTORS`, groups by source crate (BTreeMap for
  stable presentation), sorts entries by name within each
  group, renders through `display_buffer` under
  `BufferDisplayCategory::HelpList`. Per-event view
  (`:describe-event <name>`) routes through
  `BufferDisplayCategory::HelpDescribe`; unknown names
  surface a one-line error echo without opening a popup.
  Each row links via `[name](event:NAME)` -- the
  `event:` URL scheme is reserved for a future follow-up
  that hooks `<CR>`-on-link to dispatch
  `:describe-event NAME`. Tests verify the catalogue lists
  all three M.5.3.b LSP events grouped under
  `lattice-lsp`.
- M.7 -- ✅ landed across M.7.0 / M.7.1 (with M.7.2 deferred).
  Display minor modes wrap typed display options; `:set`
  and `:<mode-name>` converge.
  - **M.7.0 -- declarations.** `display_minor_mode!` macro in
    `lattice-mode::modes::display` declares
    `LineNumbersMode`, `RelativeLineNumbersMode`, `WrapMode`,
    `ReadOnlyMode`. Each contributes its corresponding
    typed-option's "on" value via `Mode::options()`.
    Activation flips the mode-contribution layer in the
    resolver (priority `mode > typed-option`); deactivation
    removes it. `register_foundation_modes` extends to
    register all four.
    `relative-line-numbers-mode` contributes both
    `RelativeNumber = true` AND `Number = true` directly,
    matching vim's `:set rnu ⇒ :set nu` cascade so users who
    never touch `:line-numbers-mode` still get a visible
    gutter. `read-only-mode` is the user-typed pathway for
    the `customizable = false` `ReadOnly` option (the only
    user surface; `:set read-only` is rejected).
  - **M.7.1 -- bidirectional convergence.** New
    `mirror_typed_option_to_display_mode::<D>(mode_id)`
    helper called from `apply_option_cascade` after
    `:set number` / `:set relativenumber` / `:set wrap`. The
    helper reads the typed-option layer's value and
    activates / deactivates the matching mode on the active
    buffer. Mode → typed-option direction is intentionally
    not mirrored: `:line-numbers-mode` flips the
    mode-contribution layer only (modes are buffer-scoped;
    the typed option is global -- flipping the mode
    shouldn't silently rewrite the user's global default).
    `read-only-mode` doesn't get a cascade hook --
    `customizable = false` means `:set read-only` is rejected,
    so there's no second source of truth.
  - **M.7.2 -- whitespace + current-line-highlight.**
    Two typed options under the Editor group: `Whitespace`
    (alias `list`, vim convention; default false) and
    `CursorLine` (canonical `current-line-highlight`;
    aliases `cul` / `cursorline`; default false). Two minor
    modes via `display_minor_mode!`: `whitespace-show-mode`
    contributes `Whitespace = true`,
    `current-line-highlight-mode` contributes
    `CursorLine = true`. Cascade convergence wired through
    `mirror_typed_option_to_display_mode::<D>` -- `:set list`
    ↔ `:whitespace-show-mode`, `:set cursorline` ↔
    `:current-line-highlight-mode`.
  - **M.7.3 -- renderer painting.** Three sub-slices:
    - **M.7.3.a foundation.** Five typed glyph options
      under the Display group
      (`display.whitespace.tab` / `.trailing` / `.leading` /
      `.space` / `.eol`), replacing vim's encoded
      `listchars=...` blob with a destructured surface.
      Defaults follow emacs `whitespace-mode`: tab + trailing
      + leading on; space + EOL opt-in. OptionCache extends
      with seven new fields (`show_whitespace`,
      `current_line_highlight`, plus 5 `Option<char>` glyphs).
      Theme adds `whitespace_style`,
      `whitespace_trailing_style`, `cursor_line_bg`
      (`Color::Indexed(236)` -- 256-color-safe portable dark
      gray).
    - **M.7.3.b whitespace pre-pass.** The renderer's
      whitespace decoration: walks rendered spans
      char-by-char, classifies each whitespace byte by
      position (trailing > tab > leading > space precedence),
      substitutes the configured glyph with the appropriate
      style. Splits spans at substitution boundaries so
      syntax-highlighted spans pass through unchanged on
      their non-whitespace content. `apply_whitespace_decoration`
      helper testable in isolation; wired at both render call
      sites (active + inactive panes) gated on
      `app.option_cache.show_whitespace`.
    - **M.7.3.c current-line highlight.** Style-merge
      approach: walk the cursor row's body spans, OR
      `theme.cursor_line_bg` into each span's style where
      the existing bg is unset. Selection wins per-cell.
      Pads to `buffer_w` so the highlight extends to the
      pane's right edge. Active pane only (vim-like; inactive
      panes have conceptual cursors, not visual ones). Gutter
      + severity cell intentionally not highlighted; future
      polish can extend.

  Workspace 1383 → 1418 (+35 across M.7.0 / M.7.1 / M.7.2 /
  M.7.3.a / M.7.3.b / M.7.3.c). The full M.7 family is now
  green end-to-end.
- M.8 -- ✅ landed. Three introspection ex-commands matching
  the existing `:describe-option` / `:describe-event`
  shape:
  - **`:list-modes`** renders every registered mode under
    `## majors` / `## minors` headers; current major + each
    active minor get a `*` marker. Mode counterpart of
    `:options`.
  - **`:describe-mode <name>`** shows id, kind, contributed
    option overrides (TypeId → display name resolved via
    OPTION_DECLS), required capabilities, current
    activation state on the active buffer. Counterpart of
    `:describe-option`.
  - **`:describe-option-resolution <name>`** walks the
    §6.1 layer model in priority order (modal-state →
    buffer-local → minors in reverse activation order →
    major → typed-option → default), marking each
    contributing layer with a ⭐. The "why is this option
    that value" introspection — answers shadowing
    surprises like "I `:set nonumber` but the gutter still
    shows" by surfacing that a minor mode contributes
    `Number = true`.

  Surfaces: three new `Effect` variants
  (`ListModes` / `DescribeMode` / `DescribeOptionResolution`),
  three ex-command registrations + aliases, three
  `App::do_*` methods routing through `display_buffer`
  under `HelpList` / `HelpDescribe` categories. The
  `:describe-mode <name>`'s contributed-options view drops
  per-value formatting for v1 -- the override's
  `Arc<dyn Any>` value can't be cleanly downcast without
  knowing the option's Value type at runtime; v2 can wire
  this through the `OptionDecl` metadata once a typed
  formatter table is needed elsewhere.

  Workspace 1394 → 1401 (+7 across the three commands).

- M.9 -- ✅ landed across M.9.0 / M.9.1 / M.9.2.
  `:customize [name]` ships as a buffer-backed form view with
  three resolution paths and inline edit. The user-facing
  surface that ties together everything M.0–M.8 declared.
  - **M.9.0 -- listing form.** `:customize` (no args) opens
    the picker (every registered group + every customizable
    mode); `:customize <group>` lists every customizable
    option in the group; `:customize <mode-name>` lists the
    options the mode contributes via `Mode::options()`.
    Resolution uses the `ends_with_mode_suffix` const fn to
    distinguish modes from groups -- matches the §6.7.1
    invariant. Mode views surface a `[mode-shadow]` indicator
    when the mode is active on the current buffer (the user
    sees that a `:set` write would be overridden by the
    contribution layer until the mode deactivates).
    Routes through `display_buffer` under
    `BufferDisplayCategory::HelpList`.
  - **M.9.1 -- picker links.** New
    `HelpLinkTarget::Customize(name)` variant +
    `customize:NAME` link scheme. The picker's
    `[name](customize:name)` rows follow on `<CR>` to the
    focused customize buffer for that group / mode. Pushes
    the prior cursor onto the position history so `<C-o>`
    walks back from the focused view to the picker.
  - **M.9.2 -- inline edit.** New
    `HelpLinkTarget::CustomizeEdit(name)` variant +
    `customize-edit:NAME` link scheme (parsed before the
    shorter `customize:` so the prefix-overlap is unambiguous).
    `append_customize_row` wraps each option's name in
    `[NAME](customize-edit:NAME)`; `<CR>` on the row
    prefills the cmdline with `:set NAME=current_value` and
    switches to Command mode. The actual write goes through
    the existing `:set` parser -- validation + cascade +
    `OptionChanged` event-bus publishing all run unchanged.
    The visible body text is preserved across M.9.0 → M.9.2
    because `extract_links_and_clean` strips the markdown
    link syntax during render.

  Deferred polish: auto-refresh the customize popup after
  an `OptionChanged` write (today the popup goes stale
  until the user re-runs `:customize`). The
  `customize-mode` major (the spec's named mode for the
  form buffer) stays implicit -- the customize buffer
  composes `markdown-mode` + the link-scheme handlers, no
  dedicated major needed for v1's surfaces.

  Workspace 1418 → 1427 (+9 across all three sub-slices in
  ui-tui, +2 in lattice-help).

- M.6 -- ✅ landed across M.6.0 / M.6.1 / M.6.2 / M.6.3 /
  M.6.4. Nine LSP sub-mode minors give per-feature
  toggles inside the `lsp-mode` umbrella; each is
  independently disable-able while the umbrella stays
  active. Decomposition:
  - **M.6.0 -- declarations.** `lsp_sub_mode!` macro in
    `lattice-lsp::modes` declares `LspCompletionMode`,
    `LspDiagnosticsMode`, `LspHoverMode`, `LspSignatureMode`,
    `LspFormatMode`, `LspRenameMode`, `LspSymbolsMode`,
    `LspCodeActionMode`, `LspNavMode`. Pure markers (kind
    Minor, no options, no capability requirements, no-op
    lifecycle hooks). `register_lsp_log_modes` (kept name
    for compatibility) extends to register all nine.
    App-side accessors `lsp_<feature>_mode_enabled_for(id)`
    -- one per sub-mode -- back the gate sites; a
    `minor_mode_enabled_for` helper backs all nine.
  - **M.6.1 -- cascade.** `on_lsp_mode_activated` invokes
    `activate_lsp_sub_modes_for(id)` which flips every
    sub-mode on in one .remove/.insert pass (option recompute
    paid once, not nine times); already-active sub-modes are
    skipped silently. `on_lsp_mode_deactivated` mirrors with
    `deactivate_lsp_sub_modes_for`. **Deliberate trade-off:**
    sub-modes auto-activate regardless of which capabilities
    the attached server advertises. They are user-controllable
    *disable switches*, not duplicate capability gates -- the
    wire layer already filters per-server
    (`handle.capabilities().supports_hover()` etc. before
    issuing). Auto-activating regardless of capability avoids
    the async race between umbrella activation (immediate)
    and the `initialize` response (hundreds of ms), and gives
    the right user-facing error when a server doesn't support
    a feature ("server doesn't advertise hover" rather than
    "lsp-hover-mode disabled"). Re-toggling `:lsp-mode` is the
    "reset to defaults" gesture (cascade-off then cascade-on
    flips every sub-mode back on, including ones the user had
    disabled individually).
  - **M.6.2 -- per-feature gates.** Each `do_lsp_*_request`
    gates on its specific sub-mode via the new
    `check_lsp_sub_mode_gate(mode_id, label)` helper, which
    runs the umbrella check first (single
    message-source-of-truth: enable lsp-mode first, then the
    sub-mode). Insert-mode auto-triggers (`do_lsp_insert_completion_request`,
    `do_lsp_signature_help_request`, `do_lsp_on_type_formatting_request`)
    skip the echo path and check the bool directly -- a typed
    character that doesn't fire isn't a moment to surface
    mode state. Twelve methods touched.
  - **M.6.3 -- diagnostic surfaces.** `render::severity_for_line`
    + `render::diagnostics_on_line` (gutter glyph + inline
    underline) move from the umbrella gate to
    `lsp_diagnostics_mode_enabled_for`. `do_next_diagnostic`
    / `do_prev_diagnostic` gate on `lsp-diagnostics-mode`
    (echo). Existing M.5.6 tests stay green via cascade-off
    propagating to the sub-mode. Modeline `[lsp:server]`
    intentionally stays on the umbrella -- it reflects "is
    this buffer LSP-tracked?" not "are diagnostics on?".
    Test fixture `seed_diags_at_lines` auto-activates lsp-mode
    (matches `seed_diagnostic` helper).
  - **M.6.4 -- coverage + docs.** End-to-end test
    `m6_end_to_end_independent_sub_modes_per_feature`
    exercises cascade-on + independent disable + selective
    feature behaviour + re-enable. `docs/../../user/lsp-mode.md`
    extended with the sub-mode table, contract details
    (umbrella echo wins, sub-modes aren't capability gates,
    re-toggle = reset), and the programmatic accessor
    catalogue. `lsp.*` namespace cleanup (M.6.5 in the table)
    deferred to a separate one-line slice.

  Workspace: 1369 → 1383 (+14 across the four sub-slices --
  3 declarations, 4 cascade, 4 gate, 2 diagnostic, 1 e2e).
- M.6.5 -- ✅ landed. `lsp.log-level` →
  `lsp-mode.log-level` namespace cleanup.
  `apply_persistent_lsp_editor_options` reads canonical
  first; legacy key still works for one minor version with a
  deprecation Warn echo. Canonical wins silently when both
  are set. The `lsp.*` TOML namespace is now exclusively the
  structural `workspace/configuration` passthrough
  (per-server subtables like `[lsp.rust-analyzer]`);
  editor-side options owned by the `lsp-mode` minor live
  under `lsp-mode.*`. Workspace 1390 → 1392 (+2 new tests).
- CSM.1 -- ✅ landed (insert-completion.md §12 first slice).
  Mode-driven completion-source trait surface in
  `lattice-completion::source`:
  `CompletionSourceContribution` + `CompletionSourceKind`
  (Sync / Async) + `SyncCompletionSource` /
  `AsyncCompletionSource` traits + `CandidateSink` +
  `InsertContextSnapshot`. `Mode::completion_sources()`
  default method added to the trait. No production code
  references these types yet (CSM.4 -- CSM.8 migrate sources
  one at a time). `lattice-mode` gains a direct
  `lattice-completion` dep; edge already existed transitively
  through `lattice-config`.
- CSM.2 -- ✅ landed.
  `CompletionMode` minor added to
  `lattice-mode::modes::completion`, registered in
  `register_foundation_modes` alongside `HelpMode` /
  `HoverMode`. Placement is in the foundation crate (not
  `lattice-completion`) to avoid a dep cycle:
  `lattice-mode` already depends on `lattice-completion` for
  the contribution type; reversing direction would require
  the `Mode` trait to live in completion.
  `sync_keymap_overlays` reconciles mode-active state with
  `App.insert_completion.is_some()` on every command tail --
  the same chokepoint that already manages the popup keymap
  layer; no need to thread changes through every popup
  open / close site. Added `App::completion_popup_active()`
  + `App::completion_mode_active_for(buffer_id)` as the
  architectural source of truth for "popup is open."
  Migrated production gate reads in `render.rs`, `runtime.rs`
  (TranslateContext setup), and `test_helpers.rs` to the new
  reader; left state-content reads in `app/edit.rs` and
  `app/completion.rs` against the field (they run mid-apply,
  before the sync tail). 7 new tests (3 in lattice-mode, 4
  in lattice-ui-tui). Workspace 2551 → 2558.
- CSM.3 -- ✅ landed.
  `ActiveCompletionSources(Vec<CompletionSourceContribution>)`
  buffer-local newtype in `lattice-mode::modes::completion`
  (owner_mode = `"completion-mode"`). New
  `App::recompute_active_completion_sources_for(buffer_id)`
  walks `active_modes[buffer_id]`, calls
  `mode.completion_sources()` on each, stores the merged
  list into the cache. Wired into every mode-transition
  site -- `activate_buffer_kind_modes`,
  `activate_mode_by_id`, `deactivate_mode_by_id`, and
  `sync_completion_mode_activation` -- so the cache stays
  in lockstep with `active_modes`. Cost is amortised at
  mode-transition rate, not keystroke rate.
  `populate_insert_completion_sync` reads the cache and
  invokes each contributed Sync source's `produce()` before
  the v1 hardcoded calls; cache is empty in practice today
  (no mode contributes yet -- CSM.4 lights up the first
  one), so the hardcoded path keeps the popup populated.
  Per-language `EffectiveCompletionConfig.source_enabled`
  still gates each contribution.
  Two existing tests updated to filter their descriptor
  assertions on the owner mode (CSM.3's `completion-mode`-
  owned local coexists with `help-mode` / `file-tree-mode`
  locals on the same buffer). 5 new tests (2 in
  lattice-mode for the buffer-local trait shape, 3 in
  lattice-ui-tui for the recompute + reader + fallback
  path). Workspace 2558 → 2563.
- CSM.K1 -- ✅ landed (insert-completion.md §12 two-mode split).
  Renamed CSM.2's `completion-mode` → `completion-popup-mode`
  (the transient popup-live marker; what the keymap-overlay
  sync tracks). New `completion-mode` born as a marker minor
  that auto-activates on writable buffer kinds (Document
  only today) via
  `auto_activated_minors_for_buffer_kind(BufferKind)`. The
  popup-trigger gate in `do_completion_trigger` checks
  `completion-mode` activation; read-only buffers (Help,
  FileTree, Oil) silently no-op on `<C-Space>`.
  `<C-x><C-o>` Insert-mode binding (vim omni-completion
  alias) retired -- `<C-Space>` is the sole popup trigger.
  Per-source filter chords (`<C-o>` / `<C-f>` / `<C-l>` /
  `<C-k>` / `<C-s>` / `<C-r>` / `<C-b>` / `<C-t>`) live
  inside `completion-popup-mode` from CSM.K2 onward.
  New `popup_filter_chord: Option<char>` field on
  `CompletionSourceContribution`; defaults to `None`,
  populated by source migrations CSM.4 onward (each
  contributor advertises its single-char chord). Renamed
  `App::completion_mode_active_for` is now genuinely about
  the persistent gate; new
  `App::completion_popup_mode_active_for` reads the
  transient. `App::completion_popup_active()` shorthand
  unchanged in name but now delegates to the popup-mode
  reader.
  Tests: 5 new in lattice-ui-tui (auto-active on Document,
  inactive on Help, gate blocks trigger when inactive,
  the two modes track independently); `<C-x><C-o>` test
  inverted to assert no-binding. Workspace 2563 → 2568.
- CSM.4 -- ✅ landed (first source migration).
  `BufferWordsMode` minor in `lattice-mode::modes::completion`
  (placement note: ideally `lattice-completion::modes` but
  dep-cycle through CSM.1's `Mode::completion_sources()`
  return type forces the thin `Mode` adapter to live here;
  the underlying `BufferWordsSource` stays in
  `lattice-completion::insert`). Contributes one
  `CompletionSourceContribution` with `id =
  "gen:buffer-words"`, `default_priority = 100`,
  `popup_filter_chord = Some('b')`,
  `kind = Sync(Arc::new(BufferWordsSource::new()))`.
  `BufferWordsSource` gains an `impl SyncCompletionSource`
  alongside its legacy `InsertSource` impl (parity during
  the migration; the latter retires when every source has
  moved). Auto-activates on Document via
  `auto_activated_minors_for_buffer_kind`. The hardcoded
  buffer-words call in `populate_insert_completion_sync` is
  removed -- the cache-driven path (CSM.3) is the sole
  population route now.
  Three CSM.3 tests updated to reflect the cache being
  non-empty by default (boot now seeds `gen:buffer-words`);
  the test that previously asserted "fallback to hardcoded
  path" renamed to "populates via mode-contributed source."
  Workspace 2568 stable (3 tests updated in-place; no net
  count change).
- CSM.5 -- ✅ landed (second source migration -- snippet).
  `SnippetCompletionMode` + `SnippetCompletionSource` in
  `lattice-snippet::modes` (feature crate placement -- the
  "feature crate owns its mode" rule holds here because
  `lattice-snippet` is a leaf in the dep graph; adding
  `lattice-mode` + `lattice-completion` + `lattice-config`
  upstream deps is cycle-free).
  `App.snippet_registry` migrates from bare `SnippetRegistry`
  to `Arc<arc_swap::ArcSwap<SnippetRegistry>>` so the mode
  + source keep their captured handle valid across
  `:reload-snippets`. Read sites switch to `.load()`;
  `:reload-snippets` swaps via `.store()`. `SnippetRegistry`
  gains `Clone` (needed for test-time mutate-and-store).
  `InsertContext` + `InsertContextSnapshot` gain a
  `language: &'a str` / `String` field (CSM.4 buffer-words
  didn't need it; CSM.5 snippet does for the
  `matching_prefix(language, query)` lookup). Host populates
  from `active_language_id()` at popup-open time.
  Contribution shape:

      CompletionSourceContribution {
          id: "gen:snippet",
          default_priority: 150,
          auto_trigger: true,
          trigger_chars: [],
          popup_filter_chord: None,  // snippets read best
                                     // alongside prose -- no
                                     // dedicated chord per §12
          kind: Sync(Arc::new(SnippetCompletionSource { ... })),
      }

  Snippet candidates carry the snippet's stable `name` as
  the `Extension::payload` bytes (UTF-8). `App::snippet_meta_for`
  retired the sidecar-index lookup; it now decodes the name
  from the payload and resolves the body via
  `SnippetRegistry::by_name`. The
  `App.insert_completion_snippet_meta` sidecar is no longer
  populated (left as an empty Vec for one slice; deletion
  follows in a cleanup pass).
  Auto-activates on Document via
  `auto_activated_minors_for_buffer_kind`. Hardcoded snippet
  branch in `populate_insert_completion_sync` retired.
  Tests: one CSM.4 cache-content test updated to assert
  both buffer-words + snippet contributions at boot. The
  snippet-trigger test reworked to query `snippet_meta_for`
  via the candidate's payload instead of poking the sidecar.
  Workspace 2568 → 2571.
- CSM.6 -- ✅ landed (third source migration -- tree-sitter).
  `TreeSitterCompletionMode` + `TreeSitterSymbolSource` in
  `lattice-syntax::modes` (feature crate placement -- the
  per-language majors already live here, so the dep graph
  was already set up). `lattice-syntax` gains a direct
  `lattice-completion` dep (the transitive edge through
  `lattice-mode` already existed; direct dep makes the import
  path explicit).
  Source is stateless -- iterates the pre-computed
  `ctx.tree_sitter_symbols` slice without re-traversing the
  syntax tree. The host walks `collect_symbols()` once per
  populate / refilter (same work as today's hardcoded path)
  and threads the result through the new `InsertContext`
  fields `tree_sitter_symbols: &'a [String]` +
  `InsertContextSnapshot::tree_sitter_symbols: Vec<String>`.
  Contribution shape:

      CompletionSourceContribution {
          id: "gen:tree-sitter-symbol",
          default_priority: 80,        // < buffer-words (100)
                                       // per §3.4 ranking
          auto_trigger: true,
          trigger_chars: [],
          popup_filter_chord: Some('t'),
          kind: Sync(Arc::new(TreeSitterSymbolSource)),
      }

  `<C-t>` inside `completion-popup-mode` (CSM.K2) will narrow
  the popup to tree-sitter symbols only.
  `register_language_modes` extends to register the new mode
  alongside the per-language majors (already called from the
  App's boot path). Auto-activates on Document via
  `auto_activated_minors_for_buffer_kind`. Hardcoded
  tree-sitter branch in `populate_insert_completion_sync`
  retired entirely.
  Tests: existing tree-sitter completion tests pass through
  the new path. CSM.4/5 boot-cache test extended to assert
  the tree-sitter contribution + its `popup_filter_chord =
  Some('t')`. Workspace 2571 stable (tests updated in-place;
  no net count change yet -- per-source unit tests in
  lattice-syntax follow with the source's lib-test).
- CSM.7 -- ✅ landed (fourth source migration -- path).
  `PathCompletionSource` in `lattice-completion::path` (new
  module). `PathCompletionMode` adapter in
  `lattice-mode::modes::completion` (same dep-cycle pragma
  as `BufferWordsMode` -- the underlying source lives in
  `lattice-completion`; the thin `Mode` adapter sits in
  `lattice-mode`).
  `InsertContext` + `InsertContextSnapshot` gain two fields:
  `path_context: bool` (set from
  `App.completion_in_path_context` -- host's tree-sitter
  scope detection) and `buffer_dir: Option<&Path>` (pre-
  resolved base directory: active doc's parent or
  `std::env::current_dir()` fallback). Source self-suppresses
  when `path_context == false` -- read-only of the flag, no
  side effects.
  Contribution shape:

      CompletionSourceContribution {
          id: "gen:path",
          default_priority: 90,
          auto_trigger: true,
          trigger_chars: ['/'],
          popup_filter_chord: Some('f'),
          kind: Sync(Arc::new(PathCompletionSource)),
      }

  `<C-f>` inside `completion-popup-mode` (CSM.K2) will
  narrow the popup to filesystem entries only.
  Cache-reader loop in `populate_insert_completion_sync`
  gains a path-context guard: when `ctx.path_context ==
  true`, non-`gen:path` contributions are skipped. Other
  sources stay stateless; the loop owns the suppression.
  `populate_path_completion` retired (~140 lines).
  `App.path_completion_cache` + `PathCompletionCache` struct
  retired -- the host-side cache was a perf optimization for
  the hardcoded path; the new source has no cache for v1.
  Profile and re-add as source-internal `Mutex<Option<...>>`
  if keystroke budget regresses.
  Tests: dotfile-filter test that exercised the host's
  removed path branch now drives through the new source
  (same fixture; the source filters dotfiles + the
  `IGNORE_NAMES` set). CSM.4/5/6 boot-cache test extends
  to assert path contribution + `popup_filter_chord =
  Some('f')`. Workspace 2571 → 2574.
- CSM.8a -- ✅ landed (fifth source migration -- LSP
  surface; first async source).
  `LspCompletionSource` in `lattice-lsp::completion` (new
  module). `LspCompletionMode` upgrades from M.6.0's
  marker `lsp_sub_mode!` macro emission to a hand-written
  stateful struct holding `LspSupervisorHandle`; the mode's
  `completion_sources()` returns one
  `CompletionSourceContribution { id:
  "gen:lsp-completion", default_priority: 200,
  popup_filter_chord: Some('o'), kind: Async(...) }`.
  `register_lsp_completion_mode(registry, lsp_handle)` is
  the new boot helper called from `App::new` after
  `register_lsp_log_modes`. The supervisor handle is
  shared with the host's existing LSP machinery.
  Activation rides on the existing M.6.1 cascade
  (`activate_lsp_sub_modes_for`) -- when `lsp-mode`
  attaches to a buffer, the cascade flips
  `lsp-completion-mode` on as part of the sub-mode set;
  the cascade's tail now calls
  `recompute_active_completion_sources_for` so the cache
  picks up `gen:lsp-completion` immediately. Symmetric
  on detach.
  **Slice scope -- placeholder `produce_async`.** The
  source's future resolves immediately with no candidates.
  Production LSP fan-out continues in
  `lattice-ui-tui::app::lsp::do_lsp_insert_completion_request`
  (~200 lines, plus the
  `App.insert_completion_lsp_meta` sidecar +
  `lsp_completion_meta_for` decoder). CSM.8b will move
  the fan-out into `produce_async`, retire the sidecar,
  and decode LSP metadata from candidate payloads via
  serde-encoded `lsp_types::CompletionItem`. CSM.8a
  lands the surface so `<C-o>` in the CSM.K2 filter
  chord palette wires through unchanged.
  Existing tests for `LspCompletionMode` as a marker
  relocated -- the `lsp_sub_mode!` macro-emission tests
  for the unit struct no longer apply; the marker check
  was the only invariant they pinned. New test asserts
  the cache reflects the cascade: empty before
  `toggle_mode_by_name("lsp-mode")`, contains
  `gen:lsp-completion` with `popup_filter_chord =
  Some('o')` + `kind: async` after. Workspace 2574 →
  2575.
- CSM.K2 -- ✅ landed (filter-in-popup wiring).
  New `source_filter: Option<SourceId>` on
  `InsertCompletionState` (initialised `None` in
  `open()`). `refilter_insert_completion` consults it
  before the matcher: candidates whose
  `RawCandidate.source` doesn't match are dropped from
  the rendered list. The full `state.raw` survives so
  `<C-Space>` (clear) can restore the mixed list
  without re-triggering. Two new `AppEffect` variants
  (`CompletionFilterToSource(String)`,
  `CompletionFilterClear`) flow through the standard
  apply chain into new app methods
  `do_completion_filter_to_source` /
  `do_completion_filter_clear`. The grammar action
  surface gains `action:completion-filter-to-source`
  (Args::String payload, dispatched via a new
  `captured_string_action` helper mirroring the
  `captured_char_action` pattern) plus
  `action:completion-filter-clear` (no-args). A new
  `bind_invocation_with_string` keymap helper folds a
  constant `Args::String(source-id)` into the bound
  invocation so a single action covers every source --
  five static chord bindings inside
  `completion_popup_layer_bindings`:
  `<C-b>` → `gen:buffer-words`,
  `<C-o>` → `gen:lsp-completion`,
  `<C-f>` → `gen:path`,
  `<C-t>` → `gen:tree-sitter-symbol`,
  `<C-s>` → `gen:snippet`.
  `<C-Space>` inside the popup layer rebound from
  `completion-trigger` → `completion-filter-clear`
  (re-trigger continues to live one layer down on the
  base Insert keymap, naturally surfacing when the
  popup isn't active). Docs-scroll bindings moved off
  `<C-f>`/`<C-b>` (those are now filter chords) to
  `PageDown`/`PageUp` -- page-wise semantics without
  the chord-namespace collision.
  Renderer: `draw_insert_completion_popup` reserves
  the bottom row of the popup as a filter-chord hint
  via the new `insert_completion_footer_line` helper.
  Unfiltered hint is a pruned chord menu showing only
  the chords whose source has candidates in
  `state.raw` (compact: `<C-b> buf  <C-o> lsp  ...`).
  Filtered hint is `source: <label>  <C-Space> all`
  so the user can see which source is active and how
  to clear. Both lines render dimmed.
  Tests: 2 in `app::completion`
  (`completion_filter_to_source_narrows_rendered_list`
  asserts state.rendered drops the non-matching source
  while state.raw is preserved;
  `completion_filter_clear_restores_full_list`
  asserts the round-trip); 4 in `keymap_insert`
  (`<C-b>`, `<C-o>` filter-chord dispatch with
  Args::String payload, `<C-Space>` clear,
  `PageDown` docs-scroll); 3 in `input` retasked
  (`<C-f>`/`<C-b>` filter dispatch via translate,
  `<C-Space>` clear). Workspace 2575 → 2581
  (+6 net new; 3 in input were retargeted from the
  retired docs-scroll/re-trigger assertions).
- CSM.8b -- ✅ landed (full LSP async migration:
  candidate IS the metadata; sidecar retired).
  Five sub-slices ship together:
  - **CSM.8b.1** -- `LspCompletionMeta` +
    `LSP_COMPLETION_KIND_ID` relocated from
    `lattice-ui-tui::app` to `lattice-lsp::completion`
    (re-exported by the host as
    `app::LspCompletionMeta` / `LSP_COMPLETION_KIND_ID`
    so existing imports keep working). The struct
    gains `Serialize + Deserialize` derives;
    `server_id` changes from `Arc<str>` to `String`
    for serde-friendliness. New `encode_meta` /
    `decode_meta` helpers round-trip via JSON
    (`serde_json`). Two new round-trip tests in
    `lattice-lsp::completion`.
  - **CSM.8b.2** -- `LspCompletionSource::produce_async`
    body populated. Multi-server fan-out + dedup on
    `(label, kind)` + `MAX_LSP_ITEMS=500` cap +
    cancellation honored at await points; each item
    becomes one `RawCandidate` whose
    `CandidateData::Extension::payload` is
    `encode_meta(&LspCompletionMeta)`. Trigger kind
    derived from `ctx.trigger`. URI + UTF-16 position
    read from new `InsertContextSnapshot` fields
    (`uri: Option<String>`,
    `lsp_position: Option<(u32, u32)>`); the LSP
    source parses the URI back via
    `lsp_types::Uri::from_str` and builds a
    `Position` from the line/character pair. New
    `CandidateSink::mark_incomplete()` trait method
    (default no-op) carries the `isIncomplete: true`
    signal from source to host.
  - **CSM.8b.3** -- aggregator drives the source. New
    `BatchingSink` in `app::lsp` buffers
    `produce_async` pushes into a single
    `InsertCompletionLspOutcome::Items {
    candidates: Vec<RawCandidate>, is_incomplete }`
    message on the existing channel; the spawn
    wrapper awaits the future, drains the sink,
    sends one message. The drain consumes the new
    shape; legacy `do_lsp_insert_completion_request`
    body collapses from ~200 lines to a thin
    "find LSP source in cache → snapshot → spawn"
    function. Cancellation, mode gate, path-context
    suppression, and per-language allowlist all
    stay host-side (they're host responsibilities,
    not source concerns).
  - **CSM.8b.4** -- `App::lsp_completion_meta_for`
    decodes from the candidate's own payload via
    `lattice_lsp::completion::decode_meta`; returns
    owned `Option<LspCompletionMeta>` instead of
    `Option<&LspCompletionMeta>`. All ~10 callers
    keep working (most already cloned the borrowed
    result). The resolve fire site computes
    `meta_index` by counting LSP rows in
    `state.raw` up to (and not including) the
    focused candidate -- payload-bytes match
    rather than the legacy 4-byte index parse.
  - **CSM.8b.5** -- `App.insert_completion_lsp_meta`
    field deleted. Resolve drain now finds the
    target LSP row by indexing into `state.raw`'s
    filtered LSP slice, decodes its payload,
    applies the resolved fields, re-encodes via
    `encode_meta`, writes back to
    `state.raw[idx].data` in place. Docs popup
    body refresh after resolve compares the
    focused candidate's `original_item` (which LSP
    servers preserve across resolve) against the
    just-resolved meta so a moved selection
    doesn't get its body overwritten. Every push /
    clear of the sidecar across `app.rs`,
    `app/completion.rs`, `app/lsp.rs`,
    `app/boot.rs`, `app/test_helpers.rs`,
    `render.rs`, and three test fixtures removed.
  Each candidate is now self-describing: the
  payload IS the source of truth, parallel writes
  are gone, the candidate / metadata can't drift,
  and future plugin completion sources slot into
  the same shape without ever needing a host-side
  sidecar of their own. Decoding cost in the
  per-frame docs / glyph / commit-char hot paths
  is microseconds (serde_json over a ~1-2 KB blob);
  well inside the frame budget.
  Tests: 2 new in `lattice-lsp::completion` (encode
  / decode round-trip; garbage payload returns
  `None`). Multiple existing test fixtures
  rewritten to use the new
  `encode_meta(&meta)`-backed payload shape and
  the helper `lsp_meta_candidate(meta)` in
  `app::lsp::tests`. Workspace 2581 → 2583
  (+2 net new; sidecar-write / sidecar-read
  assertions in 6 existing tests rewritten in
  place to read decoded payload).
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

## 13. Mode-owned keymaps + contribution debt (2026-06-01)

### The convention (going forward)

**A mode owns its full surface — keymaps, lifecycle
subscriptions, status-line contributions, decoration providers,
completion sources, option overrides, capability requirements.
None of these should live at a universal layer if they're
feature-gated by the mode's activation.**

Concretely, for keymaps:

- **A feature-specific chord lives at `KeymapLayer::MinorMode(mode_id)`
  (or `MajorMode(mode_id)`)**, NOT at `KeymapLayer::Builtin`.
  The mode-id-scoped layer means K.1.c's per-keystroke filter
  only fires the chord when the mode is in `ActiveModes` for
  the active buffer.

- **The mode's owning crate declares its keymap via the
  `Mode::keymap()` trait method.** Returns a
  [`lattice_mode::Keymap`] populated through the chain form
  (`Keymap::new().bind_chord(...)`) or the table form
  (`Keymap::from_entries(&[keymap_entry!{...}])`); see
  [`keymap-architecture.md` §11.2](keymap-architecture.md#112-the-real-keymap-contribution-type)
  for the API surface and
  [`../notes/mode-keymap-authoring.md`](../notes/mode-keymap-authoring.md)
  for the recipe. The K.2.4 host translation pass walks the
  registry at boot, calls `Mode::keymap()` on each mode, and
  pushes the contribution as a `MinorMode(mode.id())` layer.
  No per-mode helper, no host-side boot glue.

- **No global Builtin entry for feature-gated bindings.** If
  the binding wouldn't work in every buffer (because the
  feature isn't always active), it doesn't belong at Builtin.

### The principle generalises beyond keymaps

The same logic applies to every Mode trait slot. The trait
already provides `Mode::keymap()`, `Mode::options()`,
`Mode::decorations()`, `Mode::activation_policy()` (MA.1),
`Mode::completion_sources()`, `Mode::required_capabilities()`,
`Mode::conflicts_with()` / `Mode::implies()`, and
`Mode::on_activate()`. (Reactive subscriptions are not a trait
slot — they live in `on_activate` + the Guard since MO.4.c.)

**Current state (2026-06-01): modes are under-utilised — they
declare a few slots but the host still owns lots of
mode-specific surface that should live in the mode itself.**
This is technical debt that compounds as new modes (M.6+
providers, post-v1 plugins) follow the existing inconsistent
pattern.

### Outstanding debt (sequencing in slice plan)

Sequencing + per-phase scope + status for the migration
(LSP / Oil / Snippet / broader mode-ownership pass) lives in
[`docs/dev/operations/slice-plans/mode-ownership-cleanup.md`](../archive/mode-ownership-cleanup.md).
That file owns *when + in what order + status*; this section
stays scoped to the *what + why* (the principle + the going-
forward convention).

The slice plan records the audited cluster sizes (LSP 7
chords, Oil 1, Snippet 4 with a runtime `is_snippet_active`
check standing in for what should be a keymap-layer scope)
and the phasing constraint: each phase lands AFTER the
M-series multibuffer work, to avoid concurrent touches to
the same per-area code.

### Patterns already correct (the convention in action)

- `multibuffer-mode`'s `]e` / `[e` / `]E` / `[E` motions —
  declared via `MultibufferMode::keymap()` returning
  `Keymap::from_entries(&MULTIBUFFER_KEYMAP)` in
  `lattice-multibuffer::mode`. Each entry references its
  canonical motion name (`multibuffer.next-excerpt-start`,
  etc.); the host translation pass resolves names at boot.
  K.2.5 (commit `7719e27`).
- `project-search-multibuffer-mode`'s `<CR>` / `gr` chords —
  declared via `ProjectSearchMultibufferMode::keymap()`
  returning `Keymap::from_entries(&PROJECT_SEARCH_KEYMAP)` in
  `lattice-multibuffer::providers::search`. K.2.5
  (commit `7719e27`).
- `diff-mode`'s `do` / `dp` chords still use the older
  `crate::diff::mode::diff_mode_layer_bindings` helper at
  `MinorMode(diff-mode)` layer (host-side, not yet
  migrated). Migration is tracked under
  [`../operations/slice-plans/mode-ownership-cleanup.md`](../archive/mode-ownership-cleanup.md)
  as the MO.x diff-mode-keymap-migration slice.

### Convention for new mode work (going forward)

- New modes implement `Mode::keymap()` and return a populated
  `lattice_mode::Keymap`. Pick the chain form (`bind_chord`)
  for 1–5 bindings or dynamically-named commands; the table
  form (`from_entries` + `keymap_entry!`) for 5+ bindings
  against canonical command names. The two forms cohabit on
  the same `Keymap` if a mode needs both. The
  [`mode-keymap-authoring.md`](../notes/mode-keymap-authoring.md)
  guide walks through the recipe end-to-end.
- The K.2.4 translation pass (boot-time + dynamic
  `ModeRegistry::register`) picks up the contribution and
  pushes it as a `KeymapLayer::MinorMode(<mode>::mode_id())`
  layer. K.1.c per-keystroke filtering scopes the chord to
  mode-active buffers automatically — same shape major and
  minor modes both use.
- For larger keymaps (LSP-scale, 10+ chords), split per
  logical sub-mode (a future `lsp-folding-mode` would have
  its own `Mode::keymap()` distinct from the main
  `lsp-mode`'s). Each sub-mode's contribution lands at its
  own `MinorMode(mode_id)` layer; K.1.c overlay order is
  activation-order, so the sub-mode activation sequence
  determines precedence on ties.
- For dynamic bindings whose `CommandInvocation` is
  constructed at mode-construction time (not a canonical
  registered name), the chain form (`bind_chord`) is the
  right surface. The mode stashes the typed invocation on
  `self` and references it in the `keymap()` impl.
- Plugin-loaded modes (Phase 7+, WASM Component Model) land
  through the same path — the plugin host calls
  `Mode::keymap()` over the WIT boundary; the trait surface
  is plugin-language-agnostic.

### Broader "modes are under-utilised" follow-up

Beyond keymaps, expect a wider refactor pass once the
multibuffer M-series wraps. Audit candidates that should move
from host-owned to mode-owned:

- **Decoration providers** — gutter signs (diagnostics,
  diff hunks, LSP code-lens chips, breakpoints) — most are
  declared host-side; should be `Mode::decorations()`
  contributions.
- **Status-line items** — LSP "rust-analyzer ready", diff
  "+5 -3", recording macro indicator — same shape: each
  mode's `status_line_items()` (new trait method?) contributes
  its own segments.
- **Lifecycle subscriptions** — the host wires several typed-event
  subscribers (LSP progress, diagnostic refresh, semantic
  tokens refresh, code-lens refresh, document highlight)
  that are LSP-mode-specific. These should live in
  `LspMode::on_activate`'s returned Guard.
- **Per-buffer state** — buffer-locals already follow this
  pattern (`feedback_mode_owns_its_buffers`); the same
  principle extends to other per-buffer collections that
  host currently owns.

The under-utilisation is real; the cleanup is bounded. The
refactor lands as a series of focused per-area slices,
sequenced after M-series multibuffer work to avoid touching
the same files concurrently.
