# Design Document: A Modal, GPU-Accelerated, Plugin-First Editor

> **Status:** Draft v0.4
> **Codename:** `lattice` (placeholder -- rename freely)
> **Author:** TBD
> **Last updated:** 2026-04-30

---

## Changes from v0.3

This revision pivots editing semantics, plugin model, and UI layout to align the design with the project's four paramount goals: performance, extensibility, extensible vim modal editing, and asynchronicity.

- **Strict vim grammar.** Modal editing follows vim semantics exactly: `[count] ["register] (operator [count] (motion | text-object) | motion | text-object | action)`. Kakoune-flavored selection-first composition is dropped. Multi-cursor is no longer the default model -- the data layer accommodates it as a clean post-1.0 extension; v1 invariants assume a single selection with vim's visual extents.
- **The vim grammar is the public command API.** Every operator, motion, text object, register, range, and count is a first-class typed command. Keymaps are configuration that bind chord sequences to command invocations -- the default vim keymap is itself a config file. Extending the grammar (new motions, text objects, operators) is first-class, not an afterthought. Tree-sitter-driven motions and text objects are a clean future plugin, requiring no host changes.
- **Unified command / grammar dispatch (deviation from vim).** Vim's `:` ex-command world and its functional / plugin world are merged into one `CommandRegistry` with one dispatcher. Operators, motions, text objects, ex-commands, plugin contributions, and command-palette entries all share the same `CommandInvocation` shape and flow through the same `execute`. The `:`-line is a parser front-end that maps vim ex-syntax to typed invocations. Vim user UX is preserved exactly; the simplification lives below the parser. Wins: single dispatch site, plugins-as-peers-of-builtins, lower per-call overhead, cleaner mental model.

The remaining v0.4 changes are a set of vim/emacs unifications that bridge the two editor cultures:

- **Hooks and autocmds unified as typed event subscriptions** (§5.10). One event registry with typed payloads; vim `:autocmd` syntax becomes sugar for `subscribe(event, filter, invocation)`; emacs's hook idiom maps to the same primitive.
- **Self-documenting help system** (§5.11). Every command, option, event, mode, and keybinding carries metadata. `:describe-key`, `:describe-command`, `:describe-event`, `:describe-option`, `:describe-mode` open buffer-backed help views. Emacs-class introspection from v1.
- **Unified position history** (§5.1). One ring per buffer + one global ring. Items are pushed by tagged sources: `AutoJump` (vim's auto-push behavior), `ExplicitMark` (emacs's set-mark), `PluginPush` (LSP hops, etc.). Different keybindings walk different source-filtered views of the same data.
- **Visual mode IS the active region.** When Visual is active, the current selection is the default `range` arg for any range-accepting command. Vim's "operate on visual selection" and emacs's "operate on region" become the same mechanism.
- **Macros are recorded `CommandInvocation` sequences, not keystrokes.** Replay survives keymap changes, plays back faster, and is editable as data (a buffer-backed view of a recorded macro is editable before save).
- **Typed options + customize-as-buffer-view** (§5.12). Every option is a typed registered value (`name, type, default, doc, group, validator`). `:set` is a parser front-end producing `set-option` invocations. A customize buffer view shows options grouped by topic, type-aware, writes back to user TOML.
- **Rich minibuffer.** The `:` line, `/` search line, and every interactive prompt are full buffers with a major mode (`command-line`, `search-line`, `git-commit-line`, `repl-input`, ...). Full vim grammar applies. Tree-sitter highlights the command syntax. Live error indicators, parameter hints, type validation, completion popups, and substitution preview render as inline decorations on the editor and minibuffer buffers. See §5.9.10.

A small set of further unifications -- interactive arg specs, `:g`/`:v` as normal commands, history pickers, `*messages*` / `*scratch*` / `*compilation*` as buffers, `:redir` as an effect-capture wrapper -- are documented in **Appendix B**.

- **Iconography and sprites in v1** (§5.6.7). A sprite atlas (separate from the glyph atlas; same GPU pipeline) supports file-type icons in the file tree, severity icons in gutters and diagnostics lists, language logos in tab strips, status-line indicators, picker leading icons, and notification badges. Plugins register `SpriteSet`s; users override individual sprites in TOML. Distinct from Path 4 (full inline media blocks), which remains post-1.0.
- **WebAssembly Component Model plugin host from day one.** WIT interfaces are the canonical plugin API. Plugins are cross-language (Rust, Zig, Go, AssemblyScript, anything that compiles to component-model WASM), capability-gated, crash-isolated, and fuel-limited. Performance is the gating constraint, not WASM-vs-native: AOT compilation, lazy instantiation, resource handles for zero-copy buffer access, native host APIs for tree-sitter / ripgrep / regex / I/O, and a strict host-call overhead budget (p99 < 500ns typed call, < 5μs round-trip for grammar-extension calls). Built-in motions, text objects, and operators stay native in `lattice-grammar` -- WASM is for *extensions*, not for replacing the hot path.
- **Everything is a buffer.** No fixed sidebars or bottom panels. File tree, outline, symbol list, diagnostics, search results, terminal, REPL -- all are buffers, distinguished only by mutability flags, content provider, and major mode. Users place them in panes via the same split/window operations used for code buffers. The pane tree, tabs, popups, pickers, notifications, mode line, header line, and command/echo area remain.
- **Modal mode stays orthogonal to major/minor modes.** Major mode = content-type identity (rust, markdown). Modal mode = a buffer-level state machine (Normal, Insert, Visual, Op-pending, Command, Search). The two axes do not collapse into each other.

---

## Changes from v0.2

This revision adds UI specification and a performance baseline:

- **New Section 5.9: UI Components** -- formalizes the window/pane tree, popups, pickers, panels, sidebars, notifications, status lines, and the command/echo area. This was a meaningful gap in v0.2.
- **Status segment registry** specified -- defined contribution model for the mode line, header line, and gutter.
- **Picker abstraction** specified as a first-class primitive used by file finder, command palette, symbol search, and plugin-defined pickers.
- **Section 9 (Plugin API)** expanded with explicit UI contribution primitives.
- **Section 6 (Core Protocol)** expanded with UI commands and events.
- **New Appendix A: Performance Comparison** -- concrete comparison with Neovim and Emacs across all relevant dimensions, with methodology and risk assessment. Intended as a reference baseline during implementation.

---

## Changes from v0.1

(Preserved for context.)

- Rendering subsystem restructured around a `Renderer` trait abstraction with multiple specialized implementations.
- Rich-buffer rendering pipeline added as a first-class capability.
- Major modes and minor modes added as the primary customization model.
- Future web-rendering capability acknowledged but deferred.
- Performance commitments restated with concrete numbers per rendering path.

---

## 1. Vision

A text editor that combines the **modal editing power of Vim** with the **extensibility model of Emacs**, built on a **modern, asynchronous, multi-threaded core** that never blocks the UI. The editor is GPU-accelerated, has first-class support for tree-sitter and LSP, and exposes a plugin API where extensions are sandboxed WebAssembly modules -- meaning a slow or buggy plugin cannot freeze the editor, leak memory, or crash the host.

The guiding principle: **the user's keystrokes are sacred.** Every architectural decision is in service of the rule that input latency stays under one frame (<= 8ms at 120Hz) regardless of what the editor is doing in the background -- indexing a million-line repo, running an LSP request, evaluating a misbehaving plugin, or re-shaping rich text in a markdown buffer.

A secondary principle, equally non-negotiable: **the architecture must compose.** A buffer of plain code, a buffer of richly-styled markdown, and a future buffer of rendered web content are all the same kind of thing as far as the core is concerned. Specialization happens at the rendering layer, not the data layer.

---

## 2. Goals and Non-Goals

### 2.1 Goals

The four paramount goals -- in priority order when they conflict -- are **performance**, **extensibility**, **extensible vim modal editing**, and **asynchronicity**. Everything else serves these.

- **Strict vim modal editing.** The full vim grammar -- counts, registers, operators, motions, text objects, ex-ranges, dot-repeat, marks, macros -- is preserved exactly. Specific edge cases may be deferred for v1, but the language semantics are not altered.
- **The vim grammar is the public command API.** Operators, motions, text objects, registers, ranges, and counts are typed commands. Keymaps are configuration that bind chord sequences to command invocations -- the default vim keymap is itself a config file, not hardcoded behavior. Any user or plugin can invoke any command, compose commands, or build entirely new editing flows.
- **Unified command / grammar dispatch.** Vim's split between ex-commands and functional APIs is merged into one `CommandRegistry` and one dispatcher. The `:` line is a parser front-end producing typed `CommandInvocation`s; everything below the parser is one normal call path. Plugins and built-ins are peers of the dispatcher, not separated worlds.
- **Vim/emacs unifications.** Hooks and autocmds unify as typed event subscriptions (§5.10). Self-documenting help via metadata on every registered primitive (§5.11). Jump list and mark ring unify as position history with tagged sources (§5.1). Visual mode IS the active region. Macros are recorded `CommandInvocation` sequences. Options are typed with a customize buffer view (§5.12). The minibuffer is a rich editing surface with full vim grammar, tree-sitter syntax highlighting, and live error / parameter-hint decorations (§5.9.10).
- **Extensible grammar.** Registering new motions, text objects, and operators is first-class -- not bolted on later. Tree-sitter-driven motions ("next function," "inner argument," "outer class") are a clean future plugin requiring no host changes.
- **Sub-frame input latency** (target: keystroke to glyph on screen <= 8ms at 120Hz, <= 16ms at 60Hz) on a mid-range laptop, for buffers up to 100MB. **Performance parity with Vim/Neovim for code editing is the bar.**
- **Truly non-blocking architecture.** No plugin, LSP request, file I/O, syntax parse, or text-shaping operation can stall the UI thread. Ever. Multi-threaded by construction.
- **First-class tree-sitter integration**: incremental parsing, syntax highlighting, structural motions, structural selection, language injections.
- **First-class LSP integration**: diagnostics, completion, hover, go-to-definition, rename, code actions.
- **GPU-accelerated rendering** with sub-pixel-precise text, smooth scrolling, layered rendering paths optimized per content type.
- **Rich-buffer rendering**: variable fonts, sizes, and weights within a single buffer, with editing latency indistinguishable from code editing.
- **Major mode / minor mode customization**: composable, content-type-aware customization. (Modal state -- Normal/Insert/Visual/etc. -- is orthogonal; see §5.2.)
- **Everything-is-a-buffer UI.** File tree, outline, diagnostics, search results, terminal, REPL -- all are buffers placed by the user into panes. The editor enforces no fixed sidebar or bottom-panel layout.
- **Modern transient UI surface**: tabs, splits, popups (completion, hover, signature help), pickers (file, symbol, command palette), mode line, header line, notifications, command line / echo area.
- **WebAssembly Component Model plugin host from day one.** Plugins are sandboxed WASM components: cross-language (anything that compiles to component-model WASM), capability-gated, crash-isolated, fuel-limited. Performance-disciplined: AOT compilation, lazy instantiation, resource handles for zero-copy buffer access, native host APIs for hot work (tree-sitter, ripgrep, regex, I/O). Strict per-call overhead budgets enforced in CI. Built-in motions / text objects / operators stay native; WASM is for *extensions*. Multi-threaded by construction -- each plugin instance has its own `wasmtime::Store` and runs as a tokio task; many plugins execute in parallel across cores.
- **Cross-platform**: macOS, Linux, Windows. Wayland and X11 both work day one on Linux.
- **Headless mode**: the core can run without a UI.

### 2.2 Non-Goals (for v1.0)

- **Native web-page rendering.** Architecture accommodates a future `WebRenderer` but it's not in scope.
- **Collaborative real-time editing** (CRDT/OT). Data model won't preclude it.
- **Built-in terminal emulator.** Plugin material (and a buffer, like everything else).
- **Email, IRC, calendar, file manager.** Out of scope.
- **Backwards compatibility with Vim or Emacs configs/plugins.**
- **Web/mobile targets.**
- **Tree-sitter alternatives.**
- **Inline arbitrary HTML/CSS layout in buffers.** Floating elements, multi-column flows, CSS-grid in buffer body are out.
- **Kakoune-style selection-first composition.** Vim-grammar semantics only.
- **Multi-cursor as the primary editing model.** The selection data type is a set so multi-cursor is a clean post-1.0 extension; v1 invariants assume a single selection with vim's visual extents.
- **Pluggable editing paradigms in v1.** No emacs/readline-style alternative to vim modal editing in v1. The command API is paradigm-agnostic so an alternative can be added post-1.0 without redesign.
- **Fixed dock layout.** No left/right sidebar or bottom-panel as a first-class concept. Panels-as-buffers compose via the pane tree.
- **In-process scripting language / sub-keystroke REPL.** No Lua, no embedded Scheme, no `M-x ielm`. WASM (Rust today; any component-model language tomorrow) is the single extension substrate. Live evaluation is the `*scratch:rust*` plugin-authoring workflow described in §10, with 1-3 s compile latency, not a sub-keystroke evaluator. A community-shipped plugin can offer the latter as an extension; the host does not.
- **Backwards-compatible config syntax** beyond TOML. Lua / vimscript / elisp config files are not supported; config is TOML for static data and Rust→WASM (`init.rs`, §5.12) for code. Extensions are WASM. There is one extension substrate.
- **A function-call / palette / scripting syntax on the `:` line.** The `:` line is vim's ex-syntax DSL, full stop -- a parser front-end that produces typed `CommandInvocation`s for the unified dispatcher (§5.2.1). Code paths (plugins, `init.rs`, the Rust functional API) construct `CommandInvocation` values directly via the WIT host, which is the canonical typed surface for non-typing input. Two input surfaces (vim DSL for users, typed `CommandInvocation` for code), one dispatcher. We deliberately do *not* attempt to fold typing-into-`:` and code-construction into a single shape; the cost (a sugar layer that re-implements vim DSL inside a function-call parser, plus the cognitive overhead of "is this a function or a vim shorthand?") outweighs the gain (a unification we already have at `CommandInvocation`).

---

## 3. Architectural Overview

The editor is structured as **three strictly separated layers** communicating exclusively via typed message passing over async channels.

```
+------------------------------------------------------------------+
|                   UI Layer (GPU-rendered)                        |
|                                                                  |
|  Window/tab management | Pane tree | Compositor | Input loop     |
|  Popup manager | Picker manager | Notification queue             |
|  Status bar | Buffer-backed views (placed in panes)              |
|                                                                  |
|  +------------------------------------------------------------+  |
|  |              Renderer trait (abstraction)                  |  |
|  |   +----------------+ +------------------+ +-----------+    |  |
|  |   | EditorRenderer | | DocumentRenderer | |  Future:  |    |  |
|  |   |  (panes)       | |  (everything     | |    Web    |    |  |
|  |   |                | |   else)          | |  Renderer |    |  |
|  |   +----------------+ +------------------+ +-----------+    |  |
|  +------------------------------------------------------------+  |
+------------------------------+-----------------------------------+
					   Commands | Events
							   v ^
+------------------------------------------------------------------+
|                          Core Layer                              |
|                                                                  |
|  Buffers (rope) | Documents | Selections | Undo Tree |           |
|  Mode/Command Dispatcher | Tree-sitter | LSP Clients |           |
|  Major/Minor Mode Registry | Plugin Host | File I/O |            |
|  Workspace | Search | Status Segment Registry                    |
+------------------------------+-----------------------------------+
					   Commands | Events (same protocol as UI)
							   v ^
+------------------------------------------------------------------+
|                  Plugin Layer (WASM sandboxes)                   |
+------------------------------------------------------------------+
```

**Critical architectural property:** *the UI is just the most privileged client of the core.* Major and minor modes are themselves implemented as plugins.

**Threading model:**

- **UI thread** (one): owns window, GPU surface, input event loop, layout, rendering. Renders at vsync. Does no blocking work.
- **Core executor** (tokio multi-thread): owns buffers, command dispatcher, LSP clients, plugin instances.
- **Render-prep workers** (rayon pool): parallel work -- syntax highlights, search/replace, indexing, text shaping for rich buffers.
- **Blocking I/O thread pool** (`spawn_blocking`): file ops on slow filesystems, large files, tree-sitter parsing.

### 3.1 Core vs plugin: the fast path / orchestration split

CLAUDE.md goal #1 (performance) and goal #2 (extensibility) compete on every subsystem decision. The discipline that resolves the conflict:

> **Fast path stays in core. Configuration / orchestration / authoring goes to plugins. The trait surface between them is the extensibility seam.**

The WASM Component Model boundary has real cost (typed-call p99 budget < 500ns; round-trip < 5μs per §5.5). Anything that fires per-keystroke or holds keystroke-hot-path state pays that cost on every input event, and even with batching it accumulates against the 8ms-at-120Hz / 16ms-at-60Hz keystroke-to-glyph budget. We earn extensibility *around* those subsystems via traits, not *inside* them via WASM dispatch.

Concretely:

| Subsystem | Core (Rust crate) | Plugin (WASM Component) |
|---|---|---|
| **LSP** | wire layer, actor, document sync, diagnostics broadcast bus, supervisor primitives, position-encoding shim. Per-keystroke hot path (didChange, hover paint). | server installer / config UI / lifecycle preferences (`lighthouse`, Phase 8b); per-buffer source contributions (AI completion, project linters). |
| **Snippets** | parser, body type, render walker, active-snippet state machine, placeholder navigation. Per-keystroke hot path during placeholder edits. | snippet pack discovery + management UI; project-snippet sync; authoring tools. |
| **Picker** | widget, keymap, popup geometry, matcher / ranker default impls. Used by ~10 surfaces; each fires off the keystroke path. | picker *sources* (fuzzy file finder, project-grep, command palette content) via the existing trait surface. |
| **Insert-mode completion** | state machine, aggregator, popup widget, completion-popup minor mode, matcher / ranker traits. | sources beyond the bundled set (LSP + snippets + buffer-words + path + tree-sitter): AI / Copilot, project-specific generators. |
| **Modal engine** | grammar, dispatcher, builtins (motions / operators / text objects), modal state machine. | new motions / text objects / operators via the grammar-extension trait surface. |
| **Renderer** | layered architecture (TUI, GPU, document), pane tree, popup primitives, sprite atlas. | renderer overlays (e.g. inline-error decorations, custom gutter content); status-line segments. |
| **Buffer / Document** | rope, undo, edit dispatch, snapshot model. | content providers for non-file buffers (REPL, terminal, scratch:rust, magit-clone, diff viewer). |

**What this isn't:** a closed shop. The trait surface is rich (CandidateGenerator / CandidateMatcher / CandidateRanker / CandidateAnnotator / SourceGenerator / Hook / EventSubscriber / SpriteSet / StatusSegment / FoldProvider / etc.). Plugins extend lattice through these traits — that's where extensibility lives. What the trait surface does *not* expose is the keystroke-frequency state machines themselves: those stay in core and the trait surface taps into them.

**What goes plugin:** anything that's "an opinionated tool built on top of the editor" rather than "the editor's keystroke response." A magit clone, a fuzzy file finder, a git-blame inline overlay, a markdown-preview pane, a test-runner integration — all clearly plugin material. Phase 8b lists these explicitly.

**Implication for build order:** core grows first (Phases 4–6), establishing the trait surfaces plugins will consume; the plugin host (Phase 7) then ships against a concrete, exercised set of seams rather than speculative ones.

---

## 4. Technology Stack

| Concern               | Choice                                                             | Rationale                                                           |
|-----------------------|--------------------------------------------------------------------|---------------------------------------------------------------------|
| Language              | **Rust (stable)**                                                  | Memory and data-race safety; mature async; strong editor ecosystem. |
| Async runtime         | **tokio** (multi-thread)                                           | Default; integrates everywhere.                                     |
| Buffer                | **`ropey`**                                                        | Battle-tested rope; O(log n) edits; cheap clones.                   |
| Parser                | **`tree-sitter`**                                                  | Incremental, error-recovering, ubiquitous.                          |
| LSP types             | **`lsp-types`**                                                    | Generated bindings; we write our own client.                        |
| GPU rendering         | **GPUI** (preferred) or **`wgpu`** (fallback)                      | GPUI purpose-built; wgpu the fallback.                              |
| Layout (UI furniture) | **`taffy`**                                                        | Standalone flexbox/block layout.                                    |
| Text shaping          | **`cosmic-text`** or **`parley`**                                  | Full Unicode when needed; bypassed on monospace fast path.          |
| Plugin runtime        | **`wasmtime`** + Component Model + WASI                            | Sandboxing, fuel limits, async host.                                |
| Serialization         | **`serde`** + MessagePack (`rmp-serde`); WIT for plugin interfaces | Zero-cost in-process; Component Model for plugins.                  |
| Config                | **TOML**                                                           | Single config tier. Anything beyond config is a WASM plugin (§10).  |
| CLI                   | **`clap`**                                                         | Standard.                                                           |
| Logging               | **`tracing`** + `tracing-subscriber`                               | Structured logs, span timing.                                       |
| Build                 | **`cargo`** workspace                                              | Crate boundaries enforce architecture.                              |
| Testing               | **`cargo test`**, `insta`, `criterion`                             | Snapshot + benchmarks.                                              |

---

## 5. Component Designs

### 5.1 Buffer and Document Model

The atomic unit of editable text is a **`Buffer`**, internally backed by `ropey::Rope`. Buffers are wrapped in a **`Document`** with metadata.

```rust
struct Document {
	id: DocumentId,
	buffer: Buffer,
	language: Option<LanguageId>,
	syntax: Option<SyntaxState>,
	diagnostics: Vec<Diagnostic>,
	selections: SelectionSet,
	history: UndoTree,
	version: u64,
	encoding: Encoding,
	line_ending: LineEnding,
	dirty: bool,
	major_mode: MajorModeId,
	minor_modes: Vec<MinorModeId>,
	rendering_profile: RenderingProfile,
}
```

**Key properties:**
- Cheap snapshotting via O(1) rope clone.
- Versioning for stale-result rejection.
- Branching undo tree, persisted to sidecar.
- Selections support a primary cursor with charwise / linewise / blockwise visual extents (vim model). The data type is a set so multi-cursor is a clean post-1.0 extension; v1 invariants assume one selection.
- Major mode determines parser, LSP server, keymaps, style mappings, rendering profile.
- Modal state (Normal / Insert / Visual / Op-pending / Command / Search) lives on the Document as a separate field; see §5.2. Major mode and modal mode are orthogonal axes.

#### 5.1.1 Position history (jump list + mark ring unified)

Both vim's jump list and emacs's mark ring track "where the cursor was" -- the same underlying data, with different push policies. We unify them into one **position history** per buffer plus one global history, with each entry tagged by the source that pushed it:

```rust
struct PositionHistory {
	entries: VecDeque<PositionEntry>,    // bounded ring
	cursor: usize,                       // current index for next/prev navigation
}

struct PositionEntry {
	document: DocumentId,
	position: Position,
	source: PositionSource,
	timestamp: Instant,
}

enum PositionSource {
	AutoJump,        // pushed by "big motions" (vim jump list semantics)
	ExplicitMark,    // pushed by user (emacs set-mark semantics)
	PluginPush,      // pushed by plugins (LSP go-to-definition, fuzzy-finder hop, etc.)
	NamedMark(char), // vim's a-zA-Z marks
}
```

**Different keybindings walk different views.** `<C-o>` / `<C-i>` (vim) iterate over `AutoJump` and `PluginPush` entries; `g;` / `g,` (vim mark history) iterate over `NamedMark` entries; an emacs-style `pop-to-mark` walks `ExplicitMark` entries. All bindings consume the same data structure through filtered iterators -- no duplicate state.

Plugins push entries through a typed API (`history.push(entry)`); LSP go-to-definition, fuzzy-finder selections, and search jumps all participate in the same history with `PositionSource::PluginPush`.

### 5.2 Modal Editing Engine

A buffer-level state machine in front of the buffer. **Vim semantics, with one deliberate deviation: the command line and the functional API are unified.** Orthogonal to major / minor modes -- modal state is its own axis.

**Modal states:**

- **Normal** -- default; keys parse as commands.
- **Insert** -- text input; only a small set of editing keys are special.
- **Visual** -- charwise (`v`), linewise (`V`), blockwise (`<C-v>`) extension of selection.
- **Operator-Pending** -- entered automatically after an operator; awaits a motion or text object.
- **Command** -- `:` ex-command line.
- **Search** -- `/` and `?` incremental search.
- **Replace** -- `R` overstrike.

**Command grammar (vim):**

```
[count] ["register] ( operator [count] (motion | text-object)
					| motion
					| text-object
					| action )
```

`dw`, `c2iw`, `>ap`, `"ay$`, `3dd`, `gUap`, `:.+1,$d` all parse cleanly. Operator + (motion | text-object) enters and exits Operator-Pending automatically. Counts compose (`2d3w` = `d6w`). Registers prefix any operator. Ex-command ranges (`:1,5`, `:%`, `:'<,'>`, `:.,+10`, pattern ranges) are part of the grammar.

#### 5.2.1 Unified command / grammar dispatch (deviation from vim)

In vim, ex-commands (`:write`, `:%s/foo/bar/g`) and functional APIs (`function()`, `call()`, autoload) are two dissociated worlds bridged by `:call` and `:execute`. **We unify them.** Every named primitive -- built-in operators, motions, text objects, ex-commands, plugin contributions, command-palette entries -- lives in one `CommandRegistry` with a typed signature. There is one dispatcher.

```rust
pub struct CommandRegistry { /* ... */ }

pub struct CommandInvocation {
	pub command: CommandId,
	pub count: Option<Count>,
	pub register: Option<Register>,
	pub range: Option<Range>,
	pub args: Args,                  // typed per the command's signature
}

/// Submit an invocation. Returns immediately with a typed handle; the
/// invocation runs on the document actor that owns the target buffer.
/// This is the seam every command flows through -- vim chord, `:` line,
/// keymap, plugin, palette -- so it MUST NOT block the caller.
pub fn execute(inv: CommandInvocation) -> Result<Pending, CommandError>;

pub struct Pending {
	pub id: InvocationId,
	pub effect: oneshot::Receiver<Result<Effect, CommandError>>,
}
```

**Why async, not sync.** Commands may run synchronously on the document actor (`dw`, simple motions, single-buffer edits) or asynchronously (`:write` to a slow disk, `:!cmd`, plugin operators that fetch over LSP). The dispatcher cannot tell which without inspecting the registered body, and even sync-looking commands can fan out into hooks that must run before commit. Returning a `Pending` is uniform: callers that want to wait can `await` the receiver, callers that don't (the input loop, macro replay) hand it to a per-buffer queue.

**Veto-class hooks are bounded-sync.** Hooks subscribed to pre-mutation events (`BeforeBufferWrite`, `BeforeApplyEdit`) run inline on the actor under a hard latency budget (target: 1 ms p99 per hook, total 5 ms p99); exceeding it is a logged warning, not a panic, and a repeat offender gets demoted to advisory-only. Observation-class hooks (`BufferChanged`, `ModeChanged`) are dispatched fire-and-forget after commit. This split is what keeps the UI's keystroke-to-glyph budget intact even with active plugins.

**Atomicity scope.** A single `CommandInvocation` is atomic *within one document*: edits, selection updates, and the resulting `Effect` commit together or not at all. Cross-document and cross-pane invocations (`:bufdo`, `:windo`, plugin-orchestrated multi-buffer refactors) are a sequence of per-document atoms with explicit failure modes, not a global transaction -- the cost of distributed transactions over actor mailboxes is not worth the use cases.

**Backpressure.** Each document actor has a bounded mailbox. When full, the dispatcher returns `CommandError::Busy` rather than blocking; the caller decides (input loop drops to a "buffer is busy" indicator; scripts retry with backoff). The event bus uses the same discipline: subscriber queues are bounded, and a slow subscriber gets dropped events with a counter, not backpressure on the publisher.

The vim user experience is preserved exactly:

- `dw` -- the state machine assembles `CommandInvocation { command: ops::delete, args: Target::Motion(motions::word_forward), count: 1, ... }` and calls `execute`.
- `:1,5d` -- the `:`-line **parser front-end** maps the ex-syntax string to a `CommandInvocation` with `range = Some(Lines(1, 5))` and dispatches through the same `execute`.
- `:%s/foo/bar/g` -- parser produces `CommandInvocation { command: edits::substitute, range: Some(Whole), args: (Pattern("foo"), Replacement("bar"), Flags::Global), ... }`.
- A plugin invoking the same command from WASM passes a `CommandInvocation` over the WIT boundary; same dispatcher, same signature.

**What unification gives us:**

1. **Single dispatch site.** One place to instrument, fuel-meter, log, route to undo, record for dot-repeat, benchmark.
2. **Plugins and built-ins are peers.** The only difference between a built-in operator and a plugin-registered one is whether the body is native or WIT-bound WASM. Same signature, same invocation path.
3. **Performance.** No double-hop through ex-runtime -> wrapped function. The `:` parser does syntax -> typed args; everything after is one normal call.
4. **Discoverability.** Every command is reachable from `:`, from a keymap chord, from the command palette, and from scripts. One registry, one truth.
5. **No vimscript residue.** `:call`, `:execute`, `:function`, `:return` distinctions die. Plugins call the dispatcher directly.

**What lives in the parser, not the dispatcher.** Vim's syntactic oddities -- `:set wrap!`, `:set lcs=tab:>·`, `:s/foo/bar/g` flag parsing, range pattern syntax -- are handled by the `:`-line parser, which produces typed args. Each weird-looking command resolves to a normal `(command_id, args)` pair. The dispatcher knows nothing about the input syntax.

**Two input surfaces, one substrate.** The vim DSL on `:` is the canonical surface for *user typing*; constructing a `CommandInvocation` directly via the WIT host is the canonical surface for *code* (plugins, `init.rs`, the Rust functional API, future scripting-shaped extensions). They meet at `CommandInvocation` -- byte-identical from `execute(...)`'s perspective regardless of origin. The DSL stays vim-shaped because vim users have decades of muscle memory and many idioms (`:%s/.../.../g`, `:1,5d`, `:wq!`) don't map cleanly to function-call syntax without re-implementing the DSL as a sugar layer; the typed surface stays typed because plugins want signatures, not strings. We deliberately do not unify the *input surface* -- only the *dispatch substrate*. (The corresponding non-goal is in §2.2.)

**Every command is reachable from `:` via a small kind-prefix form.** Ex-commands keep the bare alias surface (`:wq`, `:write foo.txt`, `:set number` -- vim-shaped DSL, unchanged). Motions, operators, text-objects, and any plugin contribution registered as one of those kinds are reachable from `:` through a kind-prefix word + the registered name's tail (the part after the canonical `motion:` / `operator:` / `text-object:` namespace prefix):

- `:motion goto-first-line` runs the same `gg` motion the chord grammar reaches.
- `:operator delete word-forward` runs the operator over the named motion (or text-object) target.
- `:text-object inner-word` errors helpfully -- text-objects are operator targets, so they need an operator.

The kind word disambiguates the namespace cleanly without forcing colons to repeat (`:motion:goto-first-line` reads as two `:`s on the cmdline -- visual noise). The three kind words (`motion`, `operator`, `text-object`) are reserved on the `:` line; no ex-command may shadow them. Targets after the operator name are themselves looked up first as motions, then as text-objects; the bare tail (without prefix) is sufficient because the dispatch context fixes the namespace.

This kind-prefix reachability is what closes the long-standing parser gap that otherwise contradicted "every command is reachable from `:`" while motions / operators / text-objects were silently rejected. The `:` form is meant for palette / discovery / scripting use; the chord grammar (`5dw`, `daw`, `gg`) remains the natural surface for power-typing.

**Keymaps bind chord sequences to `CommandInvocation`s.** The default vim keymap is itself a config file: `"dw"` binds to a `delete` invocation with a `WordForward` target. Users and plugins override or compose without recompiling.

#### 5.2.2 The grammar as typed values

Within the unified dispatcher, the grammar's primitive concepts are still typed values -- not strings or untyped enums. They are command IDs into the registry, plus typed argument values:

```rust
pub struct OperatorId(CommandId);
pub struct MotionId(CommandId);
pub struct TextObjectId(CommandId);
pub struct ExCommandId(CommandId);

pub enum Target {
	Motion(MotionId, Args),
	TextObject(TextObjectId, Args),
	Range(Range),
}

pub enum Register {
	Unnamed, Named(char), System, BlackHole,
	Expression, ReadOnly(char), Numbered(u8),
}

pub enum Range {
	Span { start: RangeBound, end: RangeBound },   // :1,5 / :'<,'> / :.,+10
	CurrentLine,                                   // .
	Whole,                                         // %
	Selection,                                     // current Visual / active region
	Custom(RangeId, Args),                         // plugin-registered range
}

pub enum RangeBound {
	Line(u32), Mark(char), CurrentLine, LastLine,
	Pattern(Regex), Offset(Box<RangeBound>, i32),
}

pub struct Count(pub u32);
```

The built-in command catalog includes every standard vim operator, motion, and text object as registered commands:

```rust
struct Builtins {
	// Operators
	pub delete: OperatorId,
	pub change: OperatorId,
	pub yank: OperatorId,
	pub indent_left: OperatorId,
	pub indent_right: OperatorId,
	pub format: OperatorId,
	pub upper: OperatorId,
	pub lower: OperatorId,
	pub toggle_case: OperatorId,
	pub filter: OperatorId,

	// Motions
	pub char_forward: MotionId,
	pub word_forward: MotionId,
	pub word_backward: MotionId,
	pub line_start: MotionId,
	pub line_end: MotionId,
	pub first_non_blank: MotionId,
	pub paragraph_forward: MotionId,
	pub find_char: MotionId,            // takes char arg
	pub mark: MotionId,                 // takes char arg
	pub search: MotionId,               // takes (pattern, direction) args
	// ...

	// Text objects
	pub inner_word: TextObjectId,
	pub around_word: TextObjectId,
	pub inner_paren: TextObjectId,
	// ...

	// Ex commands (unified peers of operators/motions; same dispatcher)
	pub write: ExCommandId,
	pub quit: ExCommandId,
	pub substitute: ExCommandId,
	pub set_option: ExCommandId,
	pub source: ExCommandId,
	// ...
}
```

#### 5.2.3 Keymap resolution

Walks layered keymaps in priority order:

1. Built-in vim default keymap (a config file)
2. Major-mode keymap
3. Active minor-mode keymaps (in activation order)
4. User config overrides
5. Per-buffer ad-hoc bindings

Each keymap entry is `(chord_sequence) -> CommandInvocation`.

Authoritative architecture reference: [`docs/keymap-architecture.md`](keymap-architecture.md). Covers the trie data structure, layer merging, performance commitments, plugin / user-config registration paths, and the migration plan from today's hand-rolled `input.rs` dispatcher.

#### 5.2.4 Extensibility -- first-class

Plugins extend the grammar by registering new commands. The same registration API serves operators, motions, text objects, and ex-commands; all become first-class citizens of the dispatcher:

```rust
registry.register_motion(MotionSpec {
	id: "tree-sitter:next-function".into(),
	args_schema: ArgsSchema::None,
	evaluator: Box::new(|ctx, count, args| {
		// query tree-sitter, return a new position
	}),
	jump: true,
	exclusive: false,
});

registry.register_text_object(TextObjectSpec {
	id: "git-hunk".into(),
	args_schema: ArgsSchema::None,
	evaluator: Box::new(|ctx, args| { /* compute Range covering the hunk */ }),
});

registry.register_operator(OperatorSpec {
	id: "sort-lines".into(),
	args_schema: ArgsSchema::None,
	apply: Box::new(|ctx, range, register, args| { /* return Edit */ }),
	repeatable: true,    // dot-repeat eligible
});

registry.register_ex_command(ExCommandSpec {
	id: "git:blame".into(),
	parse_args: Box::new(|raw| { /* string -> typed Args */ }),
	apply: Box::new(|ctx, invocation| { /* return Effect */ }),
});
```

Every registration returns an id usable in keymaps, in `:` invocations, in scripts, and in plugin-to-plugin calls. **Tree-sitter-driven motions and text objects are post-v1, but the extension point is first-class today** -- a `tree-sitter-motions` plugin registers motions whose evaluators query the tree-sitter tree, with no host changes required.

The keymap-side companion of this extension point -- how plugins and user config bind chords to the ids returned here, layer priority across builtin / major-mode / minor-mode / user / per-buffer, and the trie merge cost on overlay push / pop -- is documented in [`docs/keymap-architecture.md`](keymap-architecture.md) §5 (extensibility) and §6 (layered registry).

**Macros, marks, registers, and dot-repeat** are mechanical because every change flows through `execute(invocation)`. The `last change` for `.` is the most recent recorded `CommandInvocation`. **Macros record `CommandInvocation` sequences -- not raw keystrokes.** Replay survives keymap changes, plays back faster (no parse pass), and is editable as data: opening the macro register in a buffer-backed view (`*macro:q*`) yields a one-invocation-per-line buffer the user can hand-edit and re-store.

**Visual mode IS the active region.** When Visual is active, the current selection is automatically supplied as the `range` argument to any range-accepting command. Vim users see "operate on visual selection"; users coming from emacs see "operate on region." Both reduce to: the dispatcher receives `range = Some(Range::Selection)` when no explicit range is given and Visual is active. This is the `Range::Selection` variant added to the range type for exactly this purpose.

**Multi-cursor (post-1.0).** The selection set already permits it. Adding multi-cursor later requires per-feature semantic spec (which operators broadcast, how registers behave, how dot-repeat interacts) but no fundamental rework of the grammar, the dispatcher, or the command API.

#### 5.2.5 Latency classes (the keystroke contract)

Every command's `CommandSpec` declares a **latency class** that pins how the runtime schedules its work and what budget the CI test harness enforces:

```rust
pub enum LatencyClass {
	Reflex,      // sync Effect must commit within keystroke budget (<2ms p99)
	Display,     // sync Effect within "feel responsive" budget (~10ms p99)
	Background,  // no user-perceived sync budget; throughput-only
}
```

**Reflex.** Single-stroke editing primitives: cursor motion, char insert, mode entry, dot-repeat, simple delete, scroll. Their evaluator must commit a sync `Effect` within the keystroke budget. The input loop awaits the `Pending::effect` receiver on the keystroke path; if it's not ready by the deadline, the dispatcher cancels (see below) and the keystroke completes without a commit.

**Display.** UI affordances that must *appear* immediately even when the data behind them is incomplete: open completion popup, open picker, open hover, post status segment, render last-cached search highlights. Their sync prelude commits a "shell" `Effect` (popup with placeholder, picker with no items yet, status segment with a spinner); the actual content arrives later via events.

**Background.** No user-perceived sync work. File-watcher tick, indexer pass, plugin housekeeping, LSP `didChange` debounce. Fire-and-forget; effects flow into snapshots whenever they're ready.

##### Events over invocation

When a Display or Background command needs work it cannot complete sync-fast, **it MUST publish an event rather than synchronously invoke a follow-up command.** Subscribers (other commands, plugins, UI surfaces, providers) consume the event and produce their own events as additional work completes. This composes:

- Multiple subscribers can react to the same event in parallel; no single command holds a chain of awaiting work.
- Adding a new participant means subscribing to an existing event, not rewriting the originating command.
- Late arrivals join the next snapshot's commit cycle without disturbing earlier ones.
- Sorting, ranking, deduplication, filtering are themselves subscribers -- they consume raw events and emit refined events. No central coordinator orders the pipeline.

Concrete shape for completion:

1. User types `.`. The Reflex command "insert char" commits its sync `Effect` -- the period is in the buffer.
2. A minor mode subscribed to `BufferEdit` recognises a completion-trigger char and **publishes** `Event::CompletionRequested { document, position, version }`.
3. The Display command "open completion popup" runs its sync prelude -- popup appears with a spinner. Kept short so the popup itself is sync-fast.
4. The LSP client subscribes to `CompletionRequested`; on receipt it issues `textDocument/completion` asynchronously and **publishes** `Event::CompletionCandidatesArrived { document, version, items, source }` when the response lands.
5. Plugin completion providers (snippet sources, dictionary sources, AI providers) subscribe to the same `CompletionRequested` event and emit their own `CompletionCandidatesArrived` events as they finish -- possibly out of order.
6. A ranking subscriber consumes raw `CompletionCandidatesArrived`, applies sorting / scoring / deduplication, and emits `Event::CompletionRanked { document, version, ordered_items }`.
7. The completion popup view subscribes to `CompletionRanked`; each arrival updates the popup content. Each update is a sync `Effect` committed to the next snapshot. The user sees the spinner replaced by candidates the moment they arrive, and the list refining as more sources finish.

The dispatcher and the event bus (§5.10) are the only coordination primitives. Plugin authors compose new behavior by subscribing -- never by direct cross-command calls.

##### Cancellation contract for Reflex

Every Reflex evaluator runs against a `CancellationToken`. The dispatcher sets a deadline timer based on the command's class budget; on expiry, the token is flipped. The evaluator must observe the flip and return promptly:

- Target: < 100us from token flip to evaluator return.
- Concrete pattern: poll `token.is_cancelled()` once per loop iteration over a buffer scan, and after every host call from a WASM evaluator. The CI test harness verifies budget compliance under cancellation by injecting flips at adversarial times.

The same token is flipped by **user-initiated cancellation** -- pressing Esc during a long motion, or interrupting a regex motion hitting a pathological backtrack. The runtime treats both sources uniformly: a flipped token means "stop now." The user's Esc and the deadline timer are equivalent at the evaluator's level; the surface behavior (a flash "search interrupted" echo vs. silent abort) is differentiated at the input loop, not at the evaluator.

**On cancellation, no `Effect` is committed.** The document actor sees `CommandError::Cancelled` and skips the snapshot publish step. The atomicity property of §5.2.1 holds: edits, selection updates, and decoration changes are committed *together* or not at all. A cancelled Reflex leaves the document at the version the keystroke arrived at; the user perceives the keystroke as having had no effect -- the correct framing, since they cancelled it.

Display and Background commands cancel via the same token mechanism, but their deadline is class-appropriate (~10ms / no deadline), and their async tails carry independent cancellation tokens that are flipped when a newer same-event request supersedes them (a newer `CompletionRequested` cancels the in-flight LSP request from the prior one).

##### CI enforcement

Every registered command is benchmarked under criterion against a representative buffer corpus. The harness asserts:

- Reflex commands meet their < 2ms p99 budget on normal-size buffers (1k-10k lines) and degrade gracefully (cancel, not blow) on adversarial inputs (100MB log, regex with backtracking).
- Display commands' sync prelude is < 10ms p99.
- Reflex evaluators correctly observe injected token flips within 100us p99.
- Background commands have throughput targets but no latency assertions.

A command that fails its class's budget is a CI regression on the same gate that catches §8.2 commitments.

### 5.3 Syntax: Tree-Sitter Integration

Tree-sitter is responsible for **all** structural code understanding.

**Update flow:** edit -> `tree.edit()` (sync, microseconds) -> reparse on `spawn_blocking` worker -> atomic tree swap -> renderer queries new tree on next frame. Renderer never blocks waiting for fresh tree; one-frame-stale highlights are acceptable.

**Highlight queries** evaluated lazily on visible viewport + overscan, on rayon pool, cached per (tree-version, line-range).

**Structural motions:** `]f`/`[f`, `]c`/`[c`, `af`/`if`, `ae`/`ie`. `locals.scm` for scope-aware rename.

**Injections** are first-class: markdown code blocks, JSX in JS, regexes in strings.

### 5.4 LSP Subsystem

We write our own client. `tower-lsp` is server-side; `async-lsp` brings tower middleware that doesn't fit our actor model. `lsp-types` (LSP 3.17) provides the wire types; the rest is hand-rolled. The companion docs ([`lsp-architecture.md`](lsp-architecture.md) for module-level commentary, [`help/lsp.md`](help/lsp.md) for the user surface, [`lsp-features.md`](lsp-features.md) for per-method tracking) elaborate beyond the design-relevant detail captured here.

#### 5.4.1 Crate layout

`lattice-lsp` is a self-contained crate; `lattice-ui-tui` consumes its public API. The module split mirrors the data-flow stages:

- `framing` -- LSP `Content-Length` header parser. Pure; stream-agnostic. Default 64 MiB per-message ceiling guards against runaway servers.
- `jsonrpc` -- JSON-RPC 2.0 typed `Request` / `Response` / `Notification` with `RequestId` correlation (`Number` / `String` / `Null` accepted on the read path; `Number` emitted). Standard error codes (`-32700..=-32600`) plus LSP extensions (`-32099..=-32000`).
- `codec` -- tokio `AsyncBufRead` / `AsyncWrite` codec. One `read_message` / `write_message` per LSP message; reuses scratch buffers so steady-state alloc count is one per round-trip (the body `Vec` from serde_json).
- `transport` -- `tokio::process::Command` child-process spawn with `kill_on_drop`; captures stdin / stdout / stderr; `split()` yields the codec halves + retained `Child` so the actor's read / write loops own independent tasks.
- `pending` -- `Pending<T>` (`oneshot::Receiver` wrapper parameterised over `LspError`). Mirrors `lattice_runtime::Pending` semantics: async-await, blocking_recv, or sync drop.
- `error` -- `LspError` enum (`Transport` / `Codec` / `Framing` / `Server` / `ActorGone` / `ResponseDropped` / `Cancelled` / `HandshakeFailed` / `ResponseDecode` / `NotInitialized`) with `is_fatal` / `is_retryable` classifiers.
- `capabilities` -- client capability advertisement (utf-8 preferred + utf-16 fallback, stale-request support, applyEdit, configuration, workspaceFolders, textDocument synchronisation + publishDiagnostics; per-feature buckets opened per phase) and negotiated `Capabilities` snapshot.
- `config` -- `ServerConfig` (binary, args, env, root markers, init options, file patterns, language id) + curated `builtin_servers()` registry (rust-analyzer, pyright, gopls, typescript-language-server, clangd, lua-language-server) + `resolve_workspace_root` walking up for marker files.
- `actor` -- per-server tokio task; `ServerHandle` is the editor-facing analogue of `lattice_runtime::DocumentHandle` (clone-cheap, Arc-internal, sync `request<P, R>` / `notify<P>` / `cancel(id)` / `shutdown()`).
- `position` -- utf-8 ↔ utf-16 ↔ utf-32 column conversion (`byte_to_lsp_character` dispatches per negotiated encoding; utf-8 short-circuits to 1ns).
- `sync` -- `DocSync`: per-server `HashMap<Uri, DocState>`. `open` / `record_edit` / `flush` / `flush_all` / `close`. Honours `TextDocumentSyncKind::{Incremental, Full, None}`. Maintains a `String` mirror per URI so utf-16 column conversion has access to BEFORE-state line text.
- `diagnostics` -- `DiagnosticEvent` typed payload (`server_id: Arc<str>`, `uri`, `version`, `Arc<[Diagnostic]>`); `DiagnosticsBus` over `tokio::sync::broadcast`.
- `diagnostics_layer` -- `DiagnosticsLayer` per-URI state container keyed by `(uri, server_id)` so multi-server scenarios don't overwrite. Apply-side version gating (`event.version < prev.version` drops); empty-list = clear semantics. Lookup APIs: `diagnostics_for(uri)` / `diagnostics_on_line(uri, line)` / `line_severity(uri, line)` / `severity_counts()` / `snapshot()`. `pump_diagnostics(layer, rx)` is the supervisor's spawn-point.
- `logging` -- `LogLevel` ordered Trace < Debug < Info < Warn < Error; `LogSource` ∈ {Client, Stderr, LspMessage, LspShowMessage, Trace}; `LogRing` bounded `VecDeque<LogRecord>`; `LspLogger` Arc-shared facade with global ring + per-server rings + per-server min-level overrides + per-server trace toggle. Every emission also fires `tracing::*` at the matching level so `RUST_LOG=lattice_lsp=debug` users see the same stream.
- `supervisor` -- `LspSupervisor`: registry of `ServerConfig`s + per-`(workspace, server-id)` actor map + per-actor `DocSync` + per-`Uri` attachment list + shared logger + shared `DiagnosticsLayer`. `open_buffer` / `close_buffer` / `record_edit` / `flush` / `servers_for` / `attach_handle` / `shutdown` (last one runs the LSP shutdown sequence per actor).

#### 5.4.2 Three-task topology per server

```text
+---------+   cmd via mpsc    +-------+    Message via mpsc    +-----------+
|App      |------------------>|Actor  |----------------------->|Write Loop |
+---------+                   |       |                        +-----------+
                              |       |                              |
                              |       |                          LspWriter
                              |       |                              |
                              |       |                         server stdin
                              |       |
                              |       |    Message via mpsc    +-----------+
                              |       |<-----------------------|Read Loop  |
                              +-------+                        +-----------+
                                  ^                                  ^
                                  |                              LspReader
                            Pending map:                              |
                            RequestId ->                          server stdout
                            oneshot::Sender
```

A separate `stderr_drain` task reads each line of `ChildStderr` and emits a `Warn` / `Stderr` record through the logger. Four tasks total per server, but the `actor` task itself does no I/O on its hot path — it owns coordination state and delegates reads / writes via channels. Three reasons for the split:

- **Burst tolerance.** A single-task design collapses when an indexer publishes hundreds of `$/progress` notifications while the editor is also sending `didChange` per keystroke. Splitting reads / writes onto separate tasks lets the OS schedule them across cores; the actor stays cheap.
- **Bounded contention.** The pending-requests `HashMap` is touched only on the actor task — no locks. The writer is `Mutex`-guarded only because the handshake's initialize request needs to fire before the loop owns it.
- **Crash containment.** A panicked `read_loop` doesn't take the `write_loop` with it; the actor observes the `mpsc::Receiver::recv()` returning `None` and runs the cleanup path (drain pending with `LspError::ActorGone`, fire shutdown sequence).

#### 5.4.3 Per-language-per-workspace lifecycle

The supervisor keys actors by `(workspace_root, server_id)`. Implications:

- **Two `.rs` files in the same Cargo workspace share one rust-analyzer actor.** Indexing happens once. Both buffers' `didOpen` go through the same `DocSync`; the actor's pending table fans responses back to the right caller via JSON-RPC id.
- **Two `.rs` files in different Cargo workspaces get two actors.** Different indexed views; no cross-workspace navigation in v1.
- **Two distinct languages in the same workspace get two actors.** Different `server_id`s.

Spawn is lazy: the first `didOpen` for a `(workspace, server_id)` triggers `actor::spawn`. The handshake (`initialize` → `initialized`) completes before `open_buffer`'s reply lands, so once the supervisor's mailbox dispatches the next request the actor is fully attached. Spawn failures (binary not on `PATH`, handshake error, server response malformed) are logged via the supervisor's logger and the relevant attachment is skipped without sinking the buffer-open.

**Attach is event-driven (paramount goal #4: asynchronicity).** Both the initial document and every subsequent `:e <path>` follow exactly one path: the publisher (`App::new` for the initial document, `App::do_edit` for follow-up opens) sets `BufferId → Uri` eagerly (the URI is a deterministic `uri_from_path`), publishes [`Event::DocumentOpened { id, path, version, text }`](§5.10.1), and **returns immediately**. The LSP attach driver (`lattice_lsp::attach_driver::spawn`, wired in `App::build_lsp_subsystem`) runs on the LSP runtime, owns one mpsc subscriber for `EventKind::DocumentOpened`, and serially submits each path-bearing event to the supervisor's mailbox via `LspSupervisorHandle::open_buffer`. The UI thread never parks on the LSP `initialize` round-trip — the editor's first frame draws as soon as `App::new` returns; rust-analyzer / pyright / gopls / etc. attach in the background and diagnostics flow whenever the server finishes initialising.

A `BufferId ↔ Uri` map lives on the App; `lattice-lsp` is below the UI layer in the crate graph and can't see `BufferId`. The App threads URIs into the supervisor's API.

#### 5.4.4 Document synchronisation

`DocSync::record_edit(uri, edit)` translates a lattice `Edit { range, kind: Replace { text } }` into an LSP `TextDocumentContentChangeEvent`:

1. Look up the `(uri, server_id)`'s mirror; read the lines at `edit.range.start.line` and `edit.range.end.line` (BEFORE-state).
2. Convert `Position::byte` to LSP `character` via `position::byte_to_lsp_character` against the negotiated encoding.
3. Build the change event with the converted range + the new text.
4. Apply the edit to the mirror.
5. Bump the per-doc version counter.
6. Push to the per-doc pending queue.

`flush(uri)` honours the negotiated `TextDocumentSyncKind`:

- **Incremental** -- send the queued events as one `didChange`.
- **Full** -- drop queued events; send the entire mirror as one no-range change.
- **None** -- clear the queue; emit nothing.

The mirror is a `String` rather than a `Rope` because the LSP layer only ever splices one contiguous region per edit; per-line indexing is rare and bounded by line count. Mirror cost is one `String` per attached buffer per server — a few MB at most for a typical session.

`flush` is debounced by the App's idle timer (~50ms after the last keystroke). One `didChange` per debounce window coalesces typist-pace bursts into a single wire message. `close` flushes pending then sends `didClose` and drops the mirror.

#### 5.4.5 Cancellation model

Every request returns `Pending<T>` (oneshot-backed; matches §5.2.1's envelope). Two cancellation scopes:

- **Actor-internal.** `ServerHandle::cancel(jsonrpc_id)` resolves the matching `Pending` with `LspError::Cancelled` and fires `$/cancelRequest` so the server can free its scheduling slot. Advertised via `general.staleRequestSupport.cancel`.
- **Editor-driven supersession.** When a newer same-flavour request supersedes a stale one (cursor moves before completion returns, etc.), the editor calls `cancel` on the stale id. The dispatch site for each per-feature command owns the supersession bookkeeping.

Cancellation is **cooperative**: a misbehaving server can ignore `$/cancelRequest` and run to completion. The actor still resolves the local `Pending` immediately; the late response, if any, is logged and discarded.

#### 5.4.6 Diagnostics pipeline

```text
server          ---publishDiagnostics--->         actor
                                                    |
                                          DiagnosticEvent ::=
                                            (server_id, uri, version,
                                             Arc<[Diagnostic]>)
                                                    |
                                            DiagnosticsBus
                                          (tokio::sync::broadcast,
                                           cap 256)
                                                    |
            +---------------------------------------+----------------------------+
            |                          |                            |            |
       pump_diagnostics       gutter glyph             :diagnostics        future
        task per server      provider (4.1.d.iii)       buffer view       plugins
            |                                        (4.1.d.iv)
       DiagnosticsLayer
       keyed by (uri, server_id)
        + version gating
        + multi-server merge
            |
       diagnostics_for(uri)        diagnostics_on_line(uri, line)
       line_severity(uri, line)    snapshot() / iter_uris() / count()
```

The bus is the wire-side primitive; the layer is the editor-side state container. Multiple subscribers fan out on each event; a lagging consumer drops oldest first (correct: latest publish supersedes anyway).

The renderer threads through the layer per-frame:

- The gutter prepends a 1-cell severity column per line. The most-severe diagnostic on the line picks a glyph + colour (`■` red Error, `▲` yellow Warning, `●` blue Information, `·` dim Hint).
- The body has an underline overlay applied to each diagnostic range using ratatui's `Modifier::UNDERLINED` + `Style::underline_color`. Composes with visual / hlsearch / current_match overlays without conflict.

The `:diagnostics` buffer is a help-style synthesised view: one row per diagnostic with a `[severity] [path:line:col message](file:path:line)` markdown link the existing help-link path knows how to follow.

`]d` / `[d` per-buffer navigation queries `diagnostics_for(uri)` sorted by `(line, col)` and walks; `:cnext` / `:cprev` (vim-quickfix aliases) currently scope per-buffer but are intended to walk the `:diagnostics` buffer's flat list (workspace-wide) once the cross-file jump lands.

#### 5.4.7 Logging pipeline

Layered to mirror emacs's `*lsp-log*` / `*<server> stderr*` convention on lattice's everything-is-a-buffer surface:

- **`*lsp*`** -- subsystem-wide events (supervisor: spawn / handshake / crash / restart) plus cross-server messages.
- **`*lsp:<server-id>*`** -- per-server: stderr lines (Warn/Stderr), `window/logMessage` and `window/showMessage` notifications mapped by the server's `type` field, `publishDiagnostics` summaries (Debug), lifecycle events, decode failures.
- **`*lsp:<server-id>:trace*`** -- full JSON-RPC wire trace. Off by default; toggle per-server. `←` (inbound) / `→` (outbound) markers + 240-char body excerpt.

Producer: `LspLogger.log(server_id, level, source, message)` runs:

1. **Trace gate.** `level == Trace` + per-server trace toggle off → return immediately. Toggle on → bypass the level filter (deliberate opt-in).
2. **Level filter.** Drop iff `level < effective_min(server_id)`.
3. **`tracing::*` fan-out.** Always fires; survives without a subscriber.
4. **Ring push.** Append to global ring (server_id None) or per-server ring.

Per-record cost ≈ 91ns; trace-off short-circuit ≈ 9ns. Both Background-class.

`:lsp-log [server]` opens the relevant buffer; `:lsp-trace <server>` toggles + opens the trace buffer; `:lsp-status` shows running actors with capability summary; `:lsp-log-level [server] <level>` adjusts gating; `:lsp-log-clear [server]` drops the ring.

Why two pipelines (rings + tracing): rings serve buffer views and survive without an external subscriber; `tracing` fan-out lets power users drive `RUST_LOG`-style filtering, JSON log shipping, OpenTelemetry, etc.

#### 5.4.8 Multi-buffer / multi-server topology

Two scenarios deserve explicit treatment.

**Multiple buffers, separate servers per language.** Each piece of LSP state is keyed by URI or `(URI, server_id)`. Cross-buffer isolation is structural — there's no "active buffer" mutable register that features rewrite. `]d` / `[d` are per-buffer; `:diagnostics` is workspace-wide as a buffer-backed list.

**Multiple servers attached to one buffer.** The supervisor keeps `attachments: HashMap<Uri, Vec<(workspace, server_id)>>`. Two servers attach when both `ServerConfig`s' `file_patterns` match. Per-feature merge (lands per phase as features arrive):

- **Diagnostics**: layer keyed by `(uri, server_id)`; readers merge across servers. Already shipped.
- **Hover** (4.2): concat with `[server-id]` labels.
- **Goto-def family** (4.2): race to first non-empty; priority breaks ties.
- **References / symbols** (4.2): union with dedupe.
- **Completion** (4.2): each server is a `gen:lsp:<server-id>` generator into the existing `lattice-completion` pipeline.
- **Code actions** (4.3): union with picker entries prefixed `[server-id]`.
- **Rename** (4.3): merge `WorkspaceEdit`s; conflict → priority-resolved.
- **Formatting** (4.3): single winner (priority).
- **Semantic tokens** (4.4): single server (multi-server merge deferred).

Server priority is a single integer per `ServerConfig`; default 100. Used by the few "single winner" features.

#### 5.4.9 Crash recovery

The `read_loop` detects pipe close and ends. The actor task observes the inbound `mpsc::Receiver::recv()` returning `None`, drains pending requests with `LspError::ActorGone`, signals the supervisor. The supervisor restarts with exponential backoff (100ms → 5s; max 5 retries) and re-issues `didOpen` for every URI it was tracking under that `(workspace, server_id)`. The diagnostics layer's per-server entries clear via `clear_server(&id)`; other servers attached to the same buffer keep working.

Restart-with-backoff lives in the supervisor (App-side knowledge of which buffers were attached); the actor only signals "I'm gone." 4.1.h shipped the actor-side detection; 4.4 lands the supervisor-side restart loop.

#### 5.4.10 Performance characteristics

LSP requests are §5.2.5 *Background*-class — no sync-prelude budget. The wire layer is benched at the framing / encode / decode / position-conversion / logging level (`crates/lattice-lsp/benches/lsp.rs`); all sit in nanoseconds-to-microseconds. Selected numbers (Background targets):

| Operation | Time |
|---|---|
| Content-Length parse | ~77ns |
| `didChange` encode | ~208ns |
| `publishDiagnostics` decode | ~1.58µs |
| utf-16 column conversion (CJK line) | ~23ns |
| `LspLogger::log` per record | ~91ns |
| `LspLogger::log` trace-off short-circuit | ~9ns |

The §8.2 commitment is "LSP plumbing never shows up next to editor work in a flame graph." `BENCHMARKS.md` carries the full bench table.

#### 5.4.11 Roadmap

Features land in this order: diagnostics → completion + resolve → hover → definition / declaration / typeDefinition / implementation / references → documentSymbol / workspace.symbol → codeAction → rename → formatting (full + range + on-type) → signatureHelp → semanticTokens → inlayHint → foldingRange → documentHighlight → callHierarchy + typeHierarchy → codeLens → documentLink → inlineValue → inlineCompletion. Per-method status in [`lsp-features.md`](lsp-features.md).

#### 5.4.12 Non-goals (v1)

- **Notebook documents** -- post-1.0; needs the rich-buffer rendering work in §5.6 + a notebook-aware buffer kind.
- **Multi-root workspace folders beyond the initial root** -- post-1.0; v1 is single-root per actor, multiple actors handle multiple roots.
- **Server-side LSP** -- lattice talks to servers, doesn't host one. The grammar API (§5.2) is the canonical extensibility surface for in-process commands.

### 5.5 Plugin Subsystem

**Two principles:** plugins are actors; the API grows from real plugins.

**Runtime: `wasmtime` + Component Model + WASI.** WIT interfaces are the canonical plugin API; plugin authors target any language with component-model toolchain support (Rust, Zig, Go, AssemblyScript, etc.). Sandbox guarantees: memory isolation, fuel limits, capability-based filesystem / network / subprocess access, crash isolation. A trapping plugin does not take down the editor.

**Plugin types:**

| Type | Purpose | Examples |
|---|---|---|
| **Major mode plugin** | Defines a content type's identity | `rust-mode`, `markdown-mode` |
| **Minor mode plugin** | Composable feature toggle | `git-blame-mode`, `auto-pair-mode` |
| **Grammar extension** | New motions, text objects, operators, ex-commands | `tree-sitter-motions`, `git-hunk-objects` |
| **Buffer-backed view** | Content provider for non-file buffers (file tree, outline, etc.) | `file-tree`, `diagnostics-list` |
| **Feature plugin** | Standalone command/UI contribution | `fuzzy-finder`, `format-on-save` |

A single plugin component can register any combination of these.

#### 5.5.1 Concurrency model

Plugins compose with the §5.7 multi-threaded async architecture:

- **Each plugin instance owns its own `wasmtime::Store`.** Stores are independent; there is no global plugin lock.
- **Each Store runs as one or more tokio tasks** on the multi-thread runtime. Two plugins doing CPU work execute on two cores in parallel.
- **The async ABI is the canonical pattern.** Host functions are async; when a plugin calls one, the WASM stack suspends and the OS thread is released to run other tokio tasks. Wakeup resumes the plugin.
- **The UI thread never invokes WASM directly.** Plugin work is scheduled on the core executor; results flow back as events.
- **A single Store is `!Send` while executing**, but yields at every host call. Between yields the OS thread runs other tasks. This is normal tokio multi-thread executor behavior; no thread is pinned.
- **Fuel exhaustion traps cleanly.** A runaway plugin's task is killed; other plugins, the document actor, the UI, the LSP clients all keep running.
- **Intra-plugin native threading is out of scope.** `wasi-threads` is experimental; the Component Model is moving toward the async ABI for concurrency. Plugin authors compose async tasks via host primitives instead of spawning OS threads. Heavy compute belongs in host APIs (tree-sitter, ripgrep), called from plugins.

The architectural rule **"no plugin can stall the UI thread, ever"** holds by construction: there is no synchronous path from UI input to plugin code.

#### 5.5.2 Performance discipline

WASM call overhead is real but bounded. Ground rules:

1. **AOT compilation.** Wasmtime + Cranelift compiles plugin modules ahead of time at install. Per-instantiation cost is the cost of allocating linear memory and resolving imports, not codegen.
2. **Module cache on disk.** Re-installs and editor upgrades reuse compiled artifacts.
3. **Lazy instantiation.** A plugin that is never invoked is never instantiated. 50 installed plugins do not contribute 50 instantiation costs to startup.
4. **Resource handles, not copies.** Buffers, documents, ranges, edits cross the WASM boundary as opaque handles. Plugin code does not receive a copy of the rope; it calls back into native APIs to read slices.
5. **Native host APIs for hot work.** Tree-sitter parsing and queries, ripgrep, regex (`fancy-regex`), file I/O, HTTP — all native, exposed to plugins as host functions.
6. **Built-ins stay native.** Default vim motions, text objects, operators, registers, ranges, and the dispatcher live in `lattice-grammar` (native Rust). The default vim keymap never crosses the WASM boundary. WASM exists for *user/plugin extensions*.
7. **Plugins emit data, not draw calls.** Status segments return `SegmentContent`; gutter providers return `GutterContent`; decorations are styled ranges. There is no per-frame WASM call.
8. **Coarse APIs.** Where a plugin would otherwise want a per-byte stream, the API takes a range and returns a slice / iterator backed by host memory.

**Per-call overhead budgets (CI-enforced):**

| Call class | p50 | p99 |
|---|---|---|
| Typed host function call | < 100ns | < 500ns |
| Grammar-extension round-trip (motion/text-object/operator) | < 1μs | < 5μs |
| Status / gutter segment update | < 10μs | < 50μs |
| Picker filter pass per item | < 500ns | < 2μs |
| Major-mode event handler | < 50μs | < 250μs |

**Cold-start budget.** Editor startup contribution from 50 lazily-loaded plugins: < 30ms total (instantiation amortized as plugins are first invoked). First-paint plugin work (status segments visible at startup): < 5ms total.

**What this means concretely.** A motion evaluator implemented in WASM that queries a host tree-sitter API costs ~5μs round-trip; called once per relevant keypress, this is 0.25% of the < 2ms keystroke budget. The bottleneck is the parser, not the WASM boundary.

#### 5.5.6 Bundled plugins

Lattice ships with a curated set of **bundled plugins** -- WASM Component Model packages compiled into the editor binary (or shipped in a known directory next to it) so they're available without a separate install step. They are the same shape as user-installed plugins; they just have a higher trust default and zero install friction.

The strategy: features that *aren't architecturally core* but *are essential to ship feature-complete out of the box* live here. Core stays narrow (buffers, modal grammar, command registry, renderer trait, runtime, plugin host); editor-quality wins (LSP server management, project-wide search, version-control UIs, snippets, surround / comment / auto-pair editing helpers) ship as bundled plugins. This dogfoods the plugin host on real workloads, gives third-party plugin authors high-quality reference implementations to study, and keeps the plugin API surface honest.

**Trust distinction.** Bundled plugins inherit the editor's trust level -- their capabilities are pre-granted at build time, no per-install consent prompt. User-installed plugins (via the bundled plugin manager) go through capability prompts on first install. Plugin manifests declare requested capabilities (`fs:write:install_dir`, `net:http`, `proc:spawn`, ...); the runtime gates accordingly.

**Bootstrap.** The plugin manager itself is bundled; you can't install it via itself. Bundled plugins live in `core-plugins/<name>.wasm` next to the binary, or compiled-in via `include_bytes!` for single-binary distributions. On first launch the host instantiates them with their pre-granted capabilities; the plugin manager then handles user-installed plugins from `${XDG_DATA_HOME}/lattice/plugins/`.

**Bundled-plugin candidates** (Phase 8 -- post-Phase-7 plugin host; concrete inventory in `docs/IMPLEMENTATION.md`):

- **LSP server manager** -- install / update / uninstall LSPs into a managed `${XDG_DATA_HOME}/lattice/lsp/<name>/<version>/` tree; bundled registry of common servers; SHA-pinned downloads. Lighthouse implementation -- the first non-trivial bundled plugin we build, validating that the WIT surface is sized correctly.
- **Plugin manager** -- install / update / uninstall third-party plugins; capability-prompt UX.
- **Project / workspace fuzzy-finder** (Telescope / fzf-lua equivalent).
- **Project-wide grep** (ripgrep wrapper, results-as-buffer).
- **Git client** (magit-style — git ops as buffers).
- **Snippet engine** (LSP-spec snippets + custom).
- **Editing helpers**: comment toggle, surround, auto-pairs, multi-cursor.
- **Diff viewer / merge tool**.
- **Outline / symbols sidebar** (consumes LSP `documentSymbol`).
- **Format-on-save controller**.
- **Test runner integration**.
- **Markdown preview**.

**WIT prerequisites that this design imposes on Phase 7's plugin host** (the first three are blockers for the LSP server manager specifically):

1. `LspSupervisor` mutation through WIT -- plugins register `ServerConfig`s pointing at paths under their managed install dir. `ServerConfig` becomes a stable WIT type.
2. **Filesystem capability** scoped per-plugin -- `${XDG_DATA_HOME}/lattice/plugins/<plugin-id>/data/` mounted via `wasi:filesystem`; writes outside it require an explicit broader capability.
3. **Network capability** -- `wasi:http` (preview2), gated; consent prompt on first install.
4. **Subprocess capability** -- contentious, since "spawn arbitrary process" approximates "trust this plugin completely". v1: bundled plugins only get `proc:spawn`; user-installed plugins ship pre-built binary recipes (no source-build paths). Sandboxed subprocess primitives are post-1.0.
5. **Long-running task surface** -- `start_task → push_output → finalize` so plugin-driven installs / scans stream stdout into a buffer-backed view without blocking the renderer.
6. **Ex-command registration** through WIT (already in §5.2.1's plan; called out here as a load-bearing dependency).
7. **§5.12 typed-options registration** through WIT -- plugins register `lsp-manager.install_root`, `lsp-manager.github_token`, etc. into the same `ConfigRegistry` core options live in.

### 5.6 Rendering -- The Layered Architecture

#### 5.6.1 The Renderer trait

```rust
trait Renderer: Send {
	type Content;
	type Event;
	type Config;

	fn set_content(&mut self, content: Self::Content);
	fn handle_input(&mut self, event: InputEvent) -> Vec<Self::Event>;
	fn layout(&mut self, constraints: LayoutConstraints) -> LayoutResult;
	fn paint(&mut self, frame: &mut Frame, viewport: Rect);
	fn accessibility_tree(&self) -> AccessibilityNode;
	fn invalidate(&mut self, region: InvalidationRegion);
}
```

| Renderer | Purpose | Status |
|---|---|---|
| `EditorRenderer` | Editable buffers (code and rich text) -- GPU primary | v1.0 |
| `DocumentRenderer` | Read-only flowed content (popups, status lines, pickers, previews) -- GPU primary | v1.0 |
| `TuiRenderer` | Terminal renderer for headless / SSH / low-bandwidth use | v1.0 (subset) |
| `CanvasRenderer` | Plugin-driven custom UIs | v1.0 (limited API) |
| `WebRenderer` | Full web-page rendering | Deferred placeholder |

The **GPU UI is the primary v1 surface** -- variable fonts, sub-pixel-precise text, smooth scrolling, popups, pickers, the rich minibuffer. The **terminal UI is a first-class peer** intended for headless / SSH / low-bandwidth use, not a bootstrap dev fixture. It shares the input pipeline, command dispatcher, modal engine, tree-sitter, LSP, and plugin layer with the GPU UI -- only the renderer differs.

The TUI accepts the limits its substrate imposes:
- Monospace cells only; variable fonts, mixed sizes, and sub-pixel positioning are out.
- Color via 24-bit ANSI when supported, 8-color fallback otherwise.
- Sprites (§5.6.7) degrade to text glyphs (nerd-font icons or ASCII placeholders); the icon registry returns a sprite *and* an optional fallback grapheme.
- Path 3 (per-line shaped) rich-buffer rendering renders as plain monospace in the terminal.
- Path 4 (inline blocks, post-1.0) is a no-op in the terminal; affected buffers fall back to placeholder text.

Beyond those, the TUI is held to the same input-latency invariants as the GPU UI: no plugin or background task may stall its event loop; rendering is damage-tracked, not full-redraw-per-frame.

#### 5.6.2 EditorRenderer -- Layered fast paths

| Path | Latency | Applies to |
|---|---|---|
| **Pure monospace, no decorations** | <1ms full repaint, <100us damage | Plain code editing, default |
| **Monospace with inline decorations** | 1-2ms full repaint | Code with LSP (squiggles, inlay hints, gutter) |
| **Per-line shaped** (mixed sizes/fonts) | 3-5ms full repaint, sub-ms damage | Markdown, org-mode |
| **Inline blocks** (images, LaTeX, charts) | 5-15ms first paint, cheap repaint | Specialized buffers |

Each buffer's major mode declares its rendering profile; **a buffer never silently upgrades to a slower path**.

##### Path 1: Pure monospace

Pipeline: visible-line-range determination -> cached highlight query -> walk rope by line, compute `glyph X = column * advance` (no shaping) -> GPU atlas lookup -> emit instance quads -> emit selection backgrounds -> emit cursor -> submit one or two draw calls.

**Optimizations:** monospace fast path skips HarfBuzz shaping entirely; glyph atlas (alacritty/kitty/ghostty/Zed pattern); damage tracking via rope edit ranges; instanced rendering.

Capable of 240Hz on modern laptop GPUs. Bottleneck is OS input latency.

##### Path 2: Monospace with decorations

Same as path 1 plus extra passes for diagnostic squiggles, inlay hints (virtual inline text), gutter widgets, line highlights. Decorations submitted as data (range + style), composited on the renderer's schedule.

##### Path 3: Per-line shaped (rich buffers)

Per-line layout cache: each line shaped once via `cosmic-text`/`parley`, cached. Re-shape only when line content, style context, or font config changes.

```rust
struct LineLayout {
	line_index: u64,
	content_hash: u64,
	style_context_hash: u64,
	shaped_glyphs: Vec<ShapedGlyph>,
	line_height: f32,
	ascent: f32,
	descent: f32,
	cursor_positions: Vec<f32>,  // byte-offset to x-pixel
}
```

**Cumulative-height index** (Fenwick tree) for variable-height lines: O(log n) scroll lookups.

**Edit pipeline:** mutation -> identify affected lines (typically 1-2) -> mark cache dirty -> shape on rayon worker (50-200us/line) -> update Fenwick index -> renderer picks up new layout next frame; uses stale layout for one frame if shape isn't done.

**Style mappings** are data, not code:

```toml
[style-mappings]
heading_1   = { font = "ui_serif", size = 24, weight = "bold" }
emphasis    = { italic = true }
code_block  = { font = "code_mono", preserve_monospace = true }
```

`preserve_monospace = true` keeps code blocks within markdown on the fast path.

##### Path 4: Inline blocks (post-1.0)

For full-size embedded media: rendered LaTeX equations, embedded charts, image previews larger than line-height, code-output cells. Implemented as inline decorations with non-zero block size that participate in the cumulative-height index. Plugins produce blocks; the renderer composites cached textures.

(Note: small in-line iconography -- file-type icons, severity icons, mode-line glyphs -- is a separate v1 capability handled by the sprite atlas in §5.6.7, not by Path 4.)

#### 5.6.3 DocumentRenderer -- UI furniture

Built on `taffy` + `cosmic-text`. Renders into the same shared GPU atlas.

Pipeline: styled tree -> taffy layout (1-10ms) -> cosmic-text shaping (1-5ms) -> atlas rasterization -> emit quads.

**Total: 5-20ms first paint of typical popup content. Subsequent frames re-emit cached geometry -- sub-ms.**

Off the input path. Cannot affect editor responsiveness.

#### 5.6.4 Cursor positioning and logical/visual translation

Core, plugins, and command dispatcher deal exclusively in **logical positions** (line, byte). Renderer translates to visual positions when drawing. Plugins never see pixels.

#### 5.6.5 Compositing model

Window is a single GPU surface. Renderers render into regions. All renderers share one GPU atlas, one shader pipeline, one frame submission. A frame containing an editor pane plus a popup plus a buffer-backed view in another pane costs roughly the same as a single editor pane alone.

#### 5.6.6 Future WebRenderer placeholder

Deferred indefinitely. Architecturally the trait accommodates Servo embedding, system webview (`wry`/WebView2/WKWebView), or CEF. Decision punted.

#### 5.6.7 Iconography and sprites (v1)

Small graphical elements -- file-type icons in the file tree, severity icons in the diagnostics list and gutter, language logos in tab strips, status-line indicators (LSP healthy / sick, git branch), picker leading icons, notification level badges -- are first-class in v1. They are *not* Path 4: they are line-height-sized, atlas-backed, and rendered through the same GPU pipeline as glyphs.

##### Sprite atlas

A separate-from-glyphs **sprite atlas** holds rasterized small graphics. Sources are SVG (preferred, multi-resolution) or PNG (for fixed pixel art). At load time, plugins register `SpriteSet`s; the host rasterizes each sprite at the device's display scale (with extra resolutions for HiDPI / scale changes) and packs them into the atlas.

```rust
pub struct SpriteSet {
	pub id: SpriteSetId,
	pub plugin: PluginId,
	pub sprites: Vec<SpriteSpec>,
}

pub struct SpriteSpec {
	pub id: SpriteId,                 // namespaced: "git-gutter:added"
	pub source: SpriteSource,         // Svg(bytes) | Png(bytes) | Builtin(name)
	pub default_tint: Option<Color>,  // None = use source colors
	pub themable: bool,               // tint follows current theme on render
}

pub enum SpriteSource {
	Svg(Vec<u8>),
	Png(Vec<u8>),
	Builtin(BuiltinIcon),             // a curated set ships with the editor
}
```

##### Where sprites appear

Sprites are referenced from the same primitives that already exist; no new rendering path:

- **Decorations.** A new variant `Decoration::InlineSprite { sprite: SpriteId, tint: Option<Color> }` places a sprite at a byte offset in any buffer. The file tree's per-line file-type icon is exactly this -- a per-line decoration on the file-tree buffer.
- **Gutter segments.** `GutterContent::PerLine` items can hold sprites instead of (or alongside) text glyphs. Diagnostic markers, breakpoint indicators, and git diff markers become sprites with optional tints.
- **Status segments.** `SegmentContent::Sprite { sprite, tint }` and the existing composite type let mode-line / header-line indicators show icons.
- **Picker items.** `ItemRenderer` can return rich content with a leading sprite (file-type icon in the file picker, symbol-kind icon in the symbol picker).
- **Tab strip.** Each tab's title may carry a leading sprite (the active document's language icon).
- **Notification level badge.** Info / Warn / Error each have a default sprite; users / themes override.

##### Bundled icon set + plugin sprites

A curated **`builtin-icons`** sprite set ships with v1, covering: 60+ language file-type icons, common file kinds (folder, symlink, hidden, executable), LSP symbol kinds, severity levels (error/warn/info/hint), VCS markers (added/modified/deleted/conflict), generic UI primitives (close, expand, collapse, search, settings).

Plugins ship their own sprites. The `git-gutter` plugin registers `git-gutter:added`, `git-gutter:modified`, `git-gutter:deleted` etc. Users override individual sprites in their TOML config:

```toml
[icons]
"file-types.rust"     = "/path/to/custom-rust.svg"
"git-gutter.modified" = { source = "/path/to/glyph.svg", tint = "#cccccc" }
```

##### Performance

- **Atlas allocation.** Sprite atlas separate from glyph atlas; both share the GPU pipeline. Default cap 32MB; LRU eviction.
- **Resolution variants.** Each registered sprite is rasterized at 1x, 2x, and 3x device scale up front (for HiDPI). Re-rasterized on display-scale change (rare). Cost: <50us per sprite per resolution; amortized at startup or first-use.
- **Decoration overhead.** A sprite decoration is one extra atlas lookup + one quad emit. No measurable impact on the monospace fast path -- the file tree with 200 visible icon decorations renders in the same frame budget as a 200-line code buffer.
- **Theme tinting.** Tints are per-draw-call uniforms; no atlas re-rasterization needed for theme switches.

##### Why this is not Path 4

Sprites fit in line height. They participate in the existing decoration + gutter + status + picker pipelines. They share the GPU pipeline with glyphs. They do not change line layout. Path 4 (inline blocks) is for *non-line-height* media that affects the cumulative-height index and requires per-block layout work -- a strictly more complex problem deferred to post-1.0.

#### 5.6.8 Render-snapshot coherence (the core / renderer contract)

Every frame the renderer reads a coherent view of `(buffer text, syntax tree, decorations, selections, layout cache)`. These pieces live on different actors -- text and selections on the document actor, syntax trees on `spawn_blocking` workers, decorations from any source publishing into the document's decoration layer, layout cache on rayon workers (shaped buffers only). The renderer cannot acquire a lock the document actor holds, and it cannot afford a synchronous "give me a snapshot" round-trip into the actor: that round-trip would put the keystroke-to-glyph budget at the mercy of the actor's mailbox depth.

The contract between core and renderer is therefore **publish-versioned, copy-on-write snapshots**: the document actor publishes immutable snapshots; the renderer reads the latest published snapshot with one atomic load per visible document at frame start, and uses *that snapshot* for the entire frame.

```rust
pub struct DocumentSnapshot {
	pub document_id: DocumentId,
	pub version: u64,                              // monotonic per document
	pub text: Arc<RopeSnapshot>,                   // O(1) clone of ropey
	pub selections: Arc<SelectionSet>,             // transformed against AppliedEdit
	pub syntax: Option<Arc<SyntaxSnapshot>>,       // None for plain-text
	pub decorations: Arc<DecorationLayer>,         // immutable; one layer per snapshot
	pub layout: Option<Arc<LayoutCacheSnapshot>>,  // shaped buffers only
}

/// One atomic-load-published-pointer per document. arc-swap is the
/// canonical primitive; `Cache::load()` is wait-free and ~2ns.
pub struct PublishedSnapshot(arc_swap::ArcSwap<DocumentSnapshot>);
```

##### Publish discipline (actor side)

The document actor holds the writable state. On every committed `Effect`, it constructs a new `DocumentSnapshot`:

1. Fields not affected by the commit are `Arc::clone`d (one word each).
2. Fields affected by the commit are rebuilt from the new state -- but most rebuilds are cheap: `RopeSnapshot` and `SelectionSet` are `Arc`-cloned plus a small mutation; `DecorationLayer` is rebuilt with the structural-sharing trick (a persistent map / `im::OrdMap`) so unchanged decorations are not copied.
3. The actor does an atomic `store_release` on the published pointer.

Snapshot construction p99 budget: **< 10 us** for buffers up to 100MB. No syscalls, no allocations beyond the changed fragments.

Late arrivals from other actors -- tree-sitter completing a parse on a `spawn_blocking` worker, a plugin publishing decorations, rayon finishing a line shape -- submit their result to the document actor as an event. The actor folds the result into the *next* snapshot it publishes. Workers never publish snapshots themselves.

##### Renderer discipline (read side)

At frame start, each renderer instance does *one* `arc_swap::Cache::load` per visible document. The returned `Arc<DocumentSnapshot>` lives for the duration of the frame. All subsequent reads -- line text, span styles, decoration ranges, selection extents, shaped glyphs -- go through that snapshot. There are no additional loads, no actor round-trips, no per-line locks, no lifetime ambiguity.

At end-of-frame the `Arc` drops; if the actor has since published newer snapshots and no other reader holds the old one, it's reclaimed. Lock-free reclamation is `arc-swap`'s job.

##### Coherence guarantees within a snapshot

- **Text + selections + decorations are always coherent with each other**, because they all commit through the actor's single publish step. A selection at byte 100 corresponds exactly to that byte in this snapshot's rope, even if the actor has since committed an edit that would shift it.
- **Syntax may lag the text by one snapshot**: if the actor publishes snapshot N before the parse for N has landed, snapshot N carries the tree from version N-1. The renderer treats this as "highlights are one snapshot stale," consistent with §5.6.2's existing claim that one-frame-stale highlights are accepted.
- **Layout cache may lag similarly** for shaped buffers; the renderer falls back to the prior `LayoutCacheSnapshot` for one frame when shaping isn't done.

##### Cross-pane: same document, different snapshots

Two panes rendering the same document at the same vsync may capture *different* snapshots if their frame work straddles a publish. This is intentional. Forcing same-version across panes would require a global frame fence that holds back the leading pane's render until the trailing one is ready -- the wrong tradeoff against latency. Visually, pane B may render one snapshot behind pane A; this is below human perception at >= 60Hz.

##### Multi-pane selection transformation under remote edits

When the actor commits an edit, the resulting `AppliedEdit` is **applied to all open selections on that document** (including selections owned by panes other than the one that issued the edit) before the next snapshot is published. The transformed selections become part of the new snapshot's `Arc<SelectionSet>`. Panes whose next frame uses that snapshot see selections at correct positions; panes using an older snapshot continue to see selections at the older positions -- which are still internally coherent with that older snapshot's text. There is no "selection points at byte 100 but the text shifted" torn state, ever.

This is the property emacs's marker objects provide and that vim doesn't need (vim never edits-and-renders concurrently). It pins §15:12 (multi-window state synchronization).

##### Why `arc-swap` specifically

`arc-swap` is an RCU-flavored primitive: lock-free read, atomic publish, refcount-based reclamation on last drop. We name it explicitly so the implementation is not free to swap to a flavor with different visibility rules (full-fence atomics across an unrelated mutex, per-thread epoch tables) that would change the renderer's correctness model. The required semantics are:

- **Renderer reads a published snapshot with `load-acquire` ordering** -- the read sees all writes the publisher ordered before its `store-release`.
- **Actor publishes with `store-release`** -- prior writes to the snapshot's interior are visible to any reader observing the new pointer.
- **Reclamation is by refcount drop, not by epoch fence** -- the renderer's `Arc<DocumentSnapshot>` keeps the snapshot alive for as long as it needs it, regardless of how many newer snapshots the actor publishes in the meantime.

##### Memory cost

A `DocumentSnapshot` is approximately: six `Arc` words (~48 bytes on 64-bit) plus the changed-fragment costs of the underlying immutable structures. Per-document overhead with one snapshot in flight + one being constructed: ~200 bytes regardless of file size, because `Arc` clones do not copy underlying B-trees. Even a 100MB buffer is one shared rope tree.

Snapshot retention: the actor keeps **one** published snapshot live; older ones are dropped when no reader holds them. Renderers naturally release at end-of-frame. A frozen renderer (e.g. a debugger has stopped the UI thread) would pin one old snapshot, but this is bounded -- the actor keeps publishing newer ones; old snapshots are released when the renderer thaws.

##### Performance contract

| Operation | Target (p99) |
|---|---|
| Snapshot publish (actor side) | < 10us |
| Snapshot load (renderer side, `arc_swap::Cache::load`) | < 5ns |
| Whole-frame: locks held by renderer | 0 |
| Whole-frame: actor round-trips by renderer | 0 |

The renderer's frame budget (§8.2) treats snapshot acquisition as a fixed cost in the single-digit-nanoseconds range, freeing the rest of the budget for actual rendering work.

This is the load-bearing async invariant of the editor. Every other piece of the architecture -- actor mailboxes, dispatcher async returns, plugin async ABI, event bus -- assumes the renderer has a frozen, coherent view to work against. That assumption is what this section pins.

### 5.7 Async Runtime and Threading

| Component | Runs on |
|---|---|
| UI event loop | Dedicated UI thread |
| Core dispatcher | tokio multi-thread |
| Buffer mutations | Core executor (single-task-per-document) |
| Tree-sitter parses | `spawn_blocking` |
| Text shaping (rich buffers) | rayon pool |
| LSP I/O | tokio tasks |
| Plugin instances | tokio tasks (wasmtime async) |
| Search/index | rayon pool |
| File I/O (small) | tokio file ops |
| File I/O (huge) | `spawn_blocking` |

**The actor pattern for documents:** each open document owned by one tokio task; mutations via mpsc; reads via O(1) rope snapshot. No locks on the hot path.

### 5.8 Major Modes and Minor Modes

#### 5.8.1 Major modes

A buffer has exactly one major mode. The major mode declares: file/shebang/content patterns, tree-sitter grammar, indent/locals/injection queries, LSP servers, keymap, comment syntax, indent style, formatter, rendering profile, style mappings, default minor modes, commands.

**Built-in major modes:** rust, python, javascript, typescript, go, c, cpp, java, ruby, markdown, asciidoc, json, yaml, toml, xml, html, text. Org-mode post-v1.0.

**Mode resolution:** explicit override -> file pattern -> shebang -> content detector -> fallback (`text`).

#### 5.8.2 Minor modes

A buffer can have any number active. Composable, additive features.

A minor mode declares: auto-activation rules, keymap additions, event subscriptions, decoration provider flag, statusline/gutter segments, commands.

**Examples:** `auto-pair`, `rainbow-delimiters`, `git-blame-line`, `git-gutter`, `whitespace-show`, `relative-line-numbers`, `flymake`, `markdown-live-preview`, `outline`.

Activation: auto-activate per major mode, user toggle (`:enable`/`:disable`), programmatic from plugins.

#### 5.8.3 Implementation as plugins

Major and minor modes are implemented as WASM plugins. No privileged built-in path. Built-in modes ship as bundled plugins (§9.7).

### 5.9 UI Components

The UI layer's structure determines how the user actually experiences the editor. The components below are all UI-layer concerns living in `lattice-ui-gpui`; the core knows nothing about windows, panes, popups, or pickers.

**Foundational principle: everything is a buffer.** File tree, outline, symbol list, diagnostics list, search results, terminal, REPL -- all are buffers, distinguished only by mutability flags, content provider, and major mode. Users place them in panes via the same split / window operations as code buffers. The editor enforces no fixed sidebar or bottom-panel layout; the user composes their workspace from panes containing buffers of their choice.

What is *not* a buffer:

- **Popups** (completion, hover, signature help, code action) -- anchored transient overlays.
- **Pickers** -- modal fuzzy-search overlays (file, symbol, command palette).
- **Notifications** -- corner-anchored transients.
- **Mode line / header line** -- per-pane status surfaces, contributed via segment registry.
- **Command line / echo area** -- per-window single-line input/echo.

These are transient or per-pane attachments, not docked layout.

#### 5.9.1 Window structure

```
+----------------------------------------------------------------+
| [Title bar -- OS-native or custom]                             |
+----------------------------------------------------------------+
| [Tab bar -- optional]                                          |
+----------------------------------------------------------------+
| [Header line -- per-pane, optional]                            |
+----------------------------------------------------------------+
|                                                                |
|                                                                |
|   Pane tree (recursive splits; each leaf holds one buffer)     |
|                                                                |
|   File tree, outline, diagnostics, search results, terminal,   |
|   REPL -- ALL buffers. The user splits panes and chooses       |
|   which buffer occupies each pane. There is no left sidebar,   |
|   right sidebar, or bottom panel as a first-class concept.     |
|                                                                |
+----------------------------------------------------------------+
| [Mode line -- per-pane]                                        |
+----------------------------------------------------------------+
| [Command line / echo area -- per-window]                       |
+----------------------------------------------------------------+

Floating overlays (z-ordered above all):
  * Completion popup
  * Hover popup
  * Signature help popup
  * Diagnostic popup
  * Code action menu
  * Pickers (file, symbol, command palette)
  * Notifications (corner-anchored)
```

#### 5.9.2 The pane tree

A window contains a recursive tree of panes. Each leaf is an editor view; each branch is a horizontal or vertical split.

```rust
enum PaneNode {
	Leaf(Pane),
	Split {
		orientation: SplitOrientation,
		children: Vec<PaneNode>,
		sizes: Vec<f32>,  // proportions, sum to 1.0
	},
}

struct Pane {
	id: PaneId,
	document: Option<DocumentId>,
	scroll_position: ScrollPosition,
	selection_focus: SelectionFocus,
	header_line_config: HeaderLineConfig,
	mode_line_config: ModeLineConfig,
	show_gutter: bool,
	show_minimap: bool,
}
```

**Pane operations** (UI-layer commands):
- `SplitPane { pane: PaneId, orientation, new_document: Option<DocumentId> }`
- `ClosePane { pane: PaneId }`
- `FocusPane { pane: PaneId }`
- `ResizePane { pane: PaneId, delta: f32 }`
- `SwapPanes { a: PaneId, b: PaneId }`
- `MovePane { pane: PaneId, target: PaneId, position: SplitPosition }`

The same document can appear in multiple panes (and multiple windows). Each pane holds its own scroll/selection state; the buffer is shared.

#### 5.9.3 Tabs

Tabs are a per-window grouping of documents, distinct from panes. A tab can hold multiple panes (a saved split layout). Tabs are optional -- users who prefer pure split-only navigation can disable the tab bar.

```rust
struct Tab {
	id: TabId,
	title: String,             // user-editable; defaults from primary document
	pane_tree: PaneNode,
	active_pane: PaneId,
}

struct Window {
	id: WindowId,
	tabs: Vec<Tab>,
	active_tab: TabId,
	layout: WindowLayout,      // saved layout state (split sizes, view placement)
}
```

**Tab operations:** create, close, rename, reorder, move-to-window, duplicate.

#### 5.9.4 Status lines: mode line and header line

Two horizontal status surfaces per pane, both rendered by `DocumentRenderer`.

**Mode line** (bottom of pane): the persistent status surface -- modal mode indicator, file info, cursor position, encoding, line ending, major mode, minor modes summary, LSP status, plugin contributions.

**Header line** (top of pane, optional): contextual breadcrumb or symbol context -- file path with breadcrumbs, the current symbol (LSP `documentSymbol` containing cursor), tab-context, plugin contributions.

Both are composed of **status segments** registered in a registry.

##### Status segment registry

```rust
struct StatusSegmentSpec {
	id: SegmentId,
	line: StatusLine,                      // ModeLine | HeaderLine
	position: SegmentPosition,             // Left | Center | Right
	priority: i32,                         // ordering within position
	update_trigger: UpdateTrigger,
	content_provider: ContentProviderId,
	visibility_predicate: Option<Predicate>,
}

enum UpdateTrigger {
	OnEvent(EventKind),                    // e.g., SelectionChanged
	Periodic(Duration),                    // e.g., clock segment
	Manual,                                // explicit update_segment call
}

struct ContentProvider {
	fn provide(&self, ctx: &SegmentContext) -> SegmentContent;
}

enum SegmentContent {
	Text(StyledText),
	Composite(Vec<SegmentContent>),
}
```

Segments are **contributed**, not hardcoded. The core ships default segments (mode indicator, file path, cursor position, etc.). Plugins, major modes, and minor modes contribute additional segments.

**Conflict resolution:** segments at the same `(position, priority)` are ordered by registration order. Users can override priority and visibility per segment in config.

**Performance:** segments are pull-not-push -- the renderer queries content providers only when their `update_trigger` fires. A segment subscribed to `SelectionChanged` updates on cursor movement; one with `Periodic(1s)` updates once a second. This avoids the Emacs problem of every keystroke causing every modeline element to recompute.

**Default mode line (rust-mode example):**

```
[NORMAL] [+] src/main.rs                       [42:18 (78%)]  rust  rust-analyzer  UTF-8  LF
   ^      ^      ^                                    ^         ^         ^           ^    ^
   |      |      |                                    |         |         |           |    |
   modal  dirty  file path                           cursor   major  LSP status    enc  EOL
```

#### 5.9.5 The gutter

A vertical strip at the left of each pane showing line-aligned annotations. Like the status lines, gutter content comes from a registry of contributors.

```rust
struct GutterSegmentSpec {
	id: SegmentId,
	column: GutterColumn,           // ordered columns left-to-right
	width: GutterWidth,             // Fixed(n_chars) | Auto
	content_provider: ContentProviderId,
	update_trigger: UpdateTrigger,
}

enum GutterContent {
	PerLine(Vec<LineContent>),
	Range(RangeMap<LineContent>),
}
```

**Default gutter columns** (left to right): diagnostic markers, git diff markers, line numbers, fold indicators.

Plugins add columns: breakpoint indicators (debugger plugin), git blame heatmap, custom markers.

#### 5.9.6 Popup system

Popups are floating, transient UI surfaces anchored to positions. They're rendered by `DocumentRenderer` and managed by a popup manager in the UI layer.

```rust
enum PopupAnchor {
	AtCursor,                                          // moves with cursor
	AtPosition { document: DocumentId, position: Position },
	AtScreenCoordinate { x: f32, y: f32 },
	AtPaneCenter { pane: PaneId },
	AtPaneCorner { pane: PaneId, corner: Corner },
}

struct Popup {
	id: PopupId,
	anchor: PopupAnchor,
	content: RichContent,
	layer: PopupLayer,                  // ordering for overlapping popups
	dismissal: DismissalRules,
	interaction: InteractionMode,
}

struct DismissalRules {
	on_cursor_move: bool,
	on_buffer_edit: bool,
	on_focus_change: bool,
	on_escape: bool,
	on_click_outside: bool,
	timeout: Option<Duration>,
}

enum InteractionMode {
	Passive,      // popup is informational; input goes to editor
	Modal,        // popup captures input; editor inactive
	Reactive,     // popup responds to specific keys (e.g., Tab to accept completion)
}
```

**Popup types and their typical configurations:**

| Popup | Anchor | Interaction | Dismissal |
|---|---|---|---|
| Completion | AtCursor | Reactive (Tab/Enter accept; Esc dismiss) | On cursor move, edit, focus change |
| Hover | AtCursor | Passive | On cursor move, edit |
| Signature help | AtCursor | Passive | On argument boundary or close paren |
| Diagnostic detail | AtCursor | Passive | On cursor move |
| Code action menu | AtCursor | Modal | On selection or Esc |
| Notification | AtPaneCorner | Passive | After timeout |

**Layering:** when multiple popups overlap, the popup manager z-orders by `layer`. Notifications stay on top; modals next; passive hovers below them.

**Performance:** popup content is laid out once on first display, cached, re-rendered on content change only. Showing or hiding a popup costs sub-millisecond -- it's just toggling visibility on cached geometry.

#### 5.9.7 Pickers

A picker is a fuzzy-search overlay used pervasively across the editor -- file open, symbol search, buffer switcher, command palette, plugin-defined pickers. **All of these use one primitive.**

```rust
struct PickerSpec {
	id: PickerId,
	title: String,
	content_provider: ContentProviderRef,    // streams matchable items
	item_renderer: ItemRendererRef,          // how each item appears
	preview_provider: Option<PreviewProviderRef>,
	selection_mode: SelectionMode,           // Single | Multi
	initial_query: Option<String>,
	on_select: ActionRef,
	on_cancel: Option<ActionRef>,
}

trait ContentProvider {
	/// Returns a stream of items matching the query.
	/// Implementations should be lazy -- don't enumerate everything upfront.
	fn items(&self, query: &str) -> BoxStream<PickerItem>;
}

struct PickerItem {
	id: ItemId,
	label: String,                           // matched against query
	secondary_label: Option<String>,         // shown but not matched
	metadata: serde_json::Value,             // arbitrary; used by renderer and preview
}

trait ItemRenderer {
	fn render(&self, item: &PickerItem, query: &str, focused: bool) -> RichContent;
}

trait PreviewProvider {
	fn preview(&self, item: &PickerItem) -> BoxFuture<RichContent>;
}
```

**Built-in pickers:**

| Picker | Content provider | Preview |
|---|---|---|
| File picker | Recursive workspace file enumeration (ripgrep-style) | First N lines of file |
| Buffer switcher | Open documents | Current view of buffer |
| Symbol picker | LSP `workspace/symbol` | Source surrounding symbol |
| Command palette | All registered commands | Command description and keybinding |
| Recent files | History | First lines of file |
| Live grep | ripgrep results streaming | Match in context |

**Streaming results:** the picker stays responsive even with millions of files because `ContentProvider::items()` returns a stream. Items appear as they're discovered. The first matches appear in milliseconds; the picker is usable while enumeration continues.

**Preview pane:** optional. When present, the picker layout splits into list-on-left, preview-on-right. Previews render via `EditorRenderer` (for code files) or `DocumentRenderer` (for documentation/non-code). Preview is async; missing previews show a placeholder.

**Plugin-defined pickers:** plugins create pickers by implementing `ContentProvider` and optionally `ItemRenderer`/`PreviewProvider` and calling `ui.open-picker(spec)`. Examples: a git-branch picker, a docker-container picker, a snippet picker.

**Implementation seed (v1).** A minimal but real picker primitive lives in `lattice-ui-tui::picker::Picker` today: holds a query line, a substring-filtered candidate list (fed by a `PickerSource` enum), a selection cursor, and a `PickerAction` tag the host's accept dispatcher pattern-matches on. The first instantiation is the **buffer switcher** -- `:b` with no arg walks `BufferRegistry`, builds one row per entry with a kind-tagged marginalia (`doc` / `tree` / `help`, plus `(current)` on the active buffer), and `<CR>` activates the selected `BufferId` via `App::activate_buffer`. Filtering today is case-insensitive substring; the full pipeline-driven path (`lattice-completion` matcher / ranker / annotators) graduates the picker once `CommandLineSlot` is lifted out of the slot detector.

**Renderer-agnostic by construction.** The picker module has no renderer-specific or host-specific imports beyond `lattice-completion`'s candidate shape. Host-coupled work (walking `BufferRegistry`, snapshotting the LSP supervisor, parsing host buffer ids) lives on the host side; the picker's only mutation entry is `Picker::set_raw_candidates(Vec<RawCandidate>)`. When the GPUI / wgpu renderer comes online, this module graduates to a sibling crate (`lattice-picker`) with zero file-by-file edits — only the host's render adapter is renderer-specific.

**Layout.** Vertico-style: the picker's query line takes over the cmdline / echo row at the bottom, candidates render in the row band immediately below. The selected row sits at the TOP of the band (closest to the prompt below), alternatives fan upward in match-rank order. Reuses the cmdline completion popup's per-row painter (matched ranges + marginalia), so styling is consistent across surfaces.

**Live preview.** While a `SwitchToBuffer` picker is open, every selection change activates the candidate buffer in the active pane (without pushing to position history). On `<CR>` the preview-active buffer is the real switch; on `<Esc>` the pane reverts to whatever buffer was active when the picker opened (`Picker::preview_origin`). Activate paths gate position-history pushes on `App.previewing` so preview hovers don't pollute the jump list. LSP-instance pickers (`OpenLspLog` / `OpenLspTraceLog`) skip preview today -- those targets create / surface real registry-tracked log buffers on accept; preview-on-hover for them is a follow-up.

#### 5.9.8 Buffer-backed views (panels-as-buffers)

The auxiliary surfaces other editors expose as fixed sidebars and bottom panels (file tree, outline, diagnostics list, search results, LSP / plugin logs, terminal, REPL, debugger, test runner, git branch viewer) are all just **buffers** here. They are placed in panes via the standard split / window operations.

Plugins that provide such views ship as **major-mode plugins** with a content provider:

```rust
pub trait BufferContentProvider: Send + Sync {
	/// Initial content (populated when the buffer opens).
	fn initial_content(&self, ctx: &BufferContext) -> RichContent;

	/// Triggers that mark the buffer dirty and re-pull content.
	fn update_triggers(&self) -> Vec<UpdateTrigger>;

	/// Recompute content on a trigger fire (off the UI thread).
	fn refresh(&self, ctx: &BufferContext) -> RichContent;

	/// Optional: handle input for interactive buffers (file tree expand,
	/// diagnostics-list jump-to, terminal pty input).
	fn handle_input(&mut self, ctx: &mut BufferContext, event: InputEvent)
		-> Vec<BufferEvent> { vec![] }

	/// Mutability classification.
	fn mutability(&self) -> BufferMutability;
}

pub enum BufferMutability {
	ReadOnly,            // diagnostics list, search results, logs
	Interactive,         // file tree (expand/collapse), terminal
	Editable,            // regular code/text buffers
}
```

**First-party buffer-backed views shipping with v1:**

| View | Provider | Mutability |
|---|---|---|
| `file-tree` | Workspace walker | Interactive |
| `outline` | LSP `documentSymbol` | Interactive (jump-to) |
| `diagnostics-list` | LSP diagnostics aggregator | ReadOnly |
| `search-results` | ripgrep stream | ReadOnly |
| `lsp-log` | Per-server stderr | ReadOnly |
| `plugin-log` | Plugin output | ReadOnly |
| `terminal` | PTY adapter (post-1.0) | Interactive |

The user opens any of these like opening a file: a command (`:open file-tree`, key binding, command palette entry) creates a new buffer of that major mode and places it in the active pane (or a new split, by user choice). To get a "left sidebar with a file tree," the user creates a vertical split and opens `file-tree` in the left pane; the layout is theirs to compose, save, and restore via tabs (§5.9.3).

**Implementation seed: help / log buffers in the registry.** The unified `BufferRegistry` carries the same `BufferData::Help(HelpBuffer)` variant the introspection layer (§5.11) uses for `:describe-*`, `:apropos`, `:diagnostics`, and the LSP log views. `App::open_help_in_pane(buffer)` is the in-pane entry point (durable record + active hot-path mirror); `App::open_help` is the popup overlay path (transient surfaces: hover, doc lookups, error toasts). De-dup by title means re-running `:lsp-log rust` surfaces the existing buffer rather than spawning a duplicate. The picker (§5.9.7) and the LSP command refactor (`:lsp-log` / `:lsp-server-log` / `:lsp-trace-log`) consume this primitive: candidate generation walks `BufferRegistry::help_ids_sorted()`, on-accept activates the chosen `BufferId` in the current pane.

**Performance:** content providers are lazy. They populate on first display; they refresh on declared triggers (event, periodic, manual), all off the UI thread on the rayon pool. A buffer that is open but whose pane is not visible can be configured to suspend updates entirely.

#### 5.9.9 Notifications

Transient messages anchored to a window corner, queued and animated.

```rust
struct Notification {
	id: NotificationId,
	level: NotificationLevel,            // Info | Warning | Error
	title: Option<String>,
	body: String,                        // can include markdown
	actions: Vec<NotificationAction>,    // optional buttons
	timeout: Option<Duration>,           // None = sticky until dismissed
	source: NotificationSource,          // who posted (plugin, core, lsp)
}

struct NotificationAction {
	label: String,
	command: ActionRef,
}
```

**Behavior:**
- Stack vertically in a chosen corner (default: bottom-right).
- New notifications appear above older ones; older ones expire.
- Maximum visible count (default 3); excess queued.
- Errors and warnings have higher visual priority and longer default timeouts.
- Notifications with actions display interactive buttons.

**Use cases:** "File saved", "Plugin X crashed", "LSP server restarted", "1 of 5 tests failed (click to view)", "Update available".

#### 5.9.10 Minibuffer and echo area

The "command line" surface is a **rich editing space, not a single-line widget.** It is implemented as a real buffer with a major mode, opened transiently, rendered by `EditorRenderer`. This is one of the highest-leverage simplifications in the design: every interactive prompt in the editor reuses the buffer, command-dispatch, decoration, popup, and renderer machinery -- nothing minibuffer-specific exists outside of "this buffer is currently the input focus."

##### Minibuffer as a buffer

```rust
struct Minibuffer {
	document: DocumentId,             // a real Document
	major_mode: MajorModeId,          // command-line, search-line, ...
	active: bool,                     // visible / focused
	on_submit: CommandInvocation,     // what to invoke on Enter
	on_cancel: Option<CommandInvocation>,
}
```

Built-in minibuffer major modes:

| Major mode | Triggered by | Purpose |
|---|---|---|
| `command-line` | `:` | ex-command parsing and dispatch |
| `search-line-forward` | `/` | incremental search forward |
| `search-line-backward` | `?` | incremental search backward |
| `git-commit-line` | git plugin | one-line commit messages |
| `repl-input` | REPL plugins | interactive evaluation |
| `picker-query` | pickers | fuzzy-match query input |
| `prompt` | interactive arg specs (§B.1) | typed argument prompts |

Each major mode brings its own keymap, parser, syntax highlighting, completion source, validator, and live-preview decorator.

##### What being a buffer gives us

Because the minibuffer is just a buffer:

- **Full vim grammar applies inside it.** You can `b`, `dw`, `0`, `c$` while editing a `:` command. The `command-line` major mode declares Insert as the entry mode for ergonomics, but Normal mode is one `<Esc>` away.
- **Tree-sitter syntax highlighting.** The `command-line` major mode uses a `command-line` tree-sitter grammar that highlights command names, ranges, regex bodies, flags, and string literals -- exactly the way code is highlighted.
- **Decorations.** Live error indicators (red squiggle under unknown command name; under malformed regex), parameter hints (virtual text after the cursor showing the next expected arg's name and type), live type validation (squiggle under `:set tabstop=hello`), are all `Decoration`s on the minibuffer document, identical to the LSP decorations on a code buffer.
- **Popups.** Completion popups (command name completion, file-path completion for `:write`, option-name completion for `:set`), hover popups (showing the docstring of the command being typed), parameter-info popups (the typed signature of a multi-arg command) -- all render through the existing popup manager.
- **Live preview decorations on other buffers.** The `:%s/foo/bar/g` parse, while incomplete, can publish decorations onto the active editor buffer: `foo` matches highlighted, `bar` rendered as virtual replacement text. Implemented as a buffer-event subscription on the minibuffer's `BufferEdit` event that re-runs the parser and pushes decorations to the target document. Cancelled on minibuffer dismiss; committed on submit.
- **Full editing power for power users.** Multi-line drafting of complex commands works because the minibuffer is just a (transiently shown) buffer. Crafting a regex, a script invocation, or a long substitute command uses the same editing model as any other text.
- **Plugins extend prompts the same way they extend buffers.** A plugin registers a new major mode and gets all of the above for free for its own prompts.

##### Live error indicators -- concretely

The `command-line` major mode subscribes to its own buffer's `BufferEdit` event with a debounced (~10ms) handler:

```text
1. On edit, the handler runs the ex-syntax parser incrementally.
2. The parser produces either:
   - A complete CommandInvocation, or
   - A partial parse with one or more typed parse errors at byte ranges.
3. Errors are published as Decoration::Diagnostic ranges on the minibuffer
   document (red squiggle + hover text). The decoration provider is the
   command-line major mode itself.
4. If the parse names a command, its registered argument schema is consulted
   to render parameter hints (virtual text) at the cursor position and to
   validate already-supplied args.
5. If the partial parse describes a substitution, search, or other
   buffer-targeting command, decorations are also published to the *target*
   buffer (live preview).
```

Submission (`<Enter>` in Insert mode, or whatever the keymap binds in Normal) runs the parsed `CommandInvocation` through `execute(...)`. Cancellation (`<Esc>`) closes the minibuffer and removes any preview decorations.

##### Performance

Parsing is allocation-bounded and runs on the dispatcher's task; the minibuffer never blocks the UI. Decoration updates piggy-back on the existing decoration pipeline (§5.6). Cold open of the minibuffer is sub-millisecond (instantiate or reuse a pre-sized buffer, swap focus); subsequent edits cost the same as edits in any other buffer.

##### Echo area

The echo area is a separate, single-line surface used for transient one-line output from the core or commands ("File saved", "No matches", "Pattern not found", "Plugin X crashed"). It shares screen real estate with the minibuffer: when the minibuffer is active, the echo area is hidden; when the minibuffer dismisses, the echo area shows the most recent message until its timeout elapses.

A rolling history of every echo-area message and every notification is kept in a `*messages*` buffer (read-only, auto-scrolling) that the user can open in any pane to scroll back.

#### 5.9.11 Scrollbars and minimap

**Scrollbars:** modern style -- appear on scroll, fade out after a moment of inactivity. Show position indicator + diagnostics summary marks (errors/warnings as colored ticks along the scrollbar). Configurable: always-visible, on-scroll, never.

**Minimap:** optional thumbnail of the file rendered at small scale on the right edge of the pane. Implemented as a low-resolution variant of `EditorRenderer` that re-renders only when the file or scroll position changes substantially (debounced). Shows diagnostics, search matches, current viewport indicator. Performance impact minimal due to debouncing and shared atlas.

#### 5.9.12 Performance characteristics of the UI layer

A frame containing:
- One editor pane (monospace path)
- An open completion popup
- A vertical split holding a `file-tree` buffer
- A horizontal split holding a `diagnostics-list` buffer
- A status line with 8 segments
- A header line with breadcrumbs
- 2 active notifications

...costs roughly the same to render as a frame with just the editor pane. Reasoning:

- The editor pane carries the heavy work; it's on the fast path.
- Popups, buffer-backed views, status lines are all `DocumentRenderer` content (or low-traffic `EditorRenderer` content) -- laid out once, cached, re-rendered only on change.
- Status segments update on triggers, not per-frame.
- All renderers share one GPU atlas and submission.

**This is the architectural win over Emacs in concrete terms.** In Emacs, every UI element shares the redisplay engine with the buffer, so a busy UI taxes editor rendering. Here, the UI layer's complexity has near-zero impact on editor input latency.

### 5.10 Event System and Hooks

Vim's `autocmd` and emacs's hooks both attach behavior to editor events. The two systems differ only in surface syntax; their semantics collapse into one primitive:

```rust
pub trait EventBus {
	fn subscribe(&self, filter: EventFilter, sink: SubscriptionTarget) -> SubscriptionId;
	fn unsubscribe(&self, id: SubscriptionId);
	fn publish(&self, event: Event);
}

pub struct EventFilter {
	pub kinds: Option<Vec<EventKind>>,        // BeforeSave, AfterSave, etc.
	pub document_pattern: Option<Pattern>,    // path glob
	pub major_mode: Option<MajorModeId>,
	pub predicate: Option<PredicateId>,       // arbitrary plugin-supplied
}

pub enum SubscriptionTarget {
	Channel(mpsc::Sender<Event>),
	Invocation(CommandInvocation),            // run a command in response
	Plugin { plugin: PluginId, handler: HandlerId },
}
```

#### 5.10.1 Event catalog

Every meaningful editor state transition publishes a typed event. The catalog grows over time; the v1 baseline includes:

- **Document lifecycle:** `DocumentOpened` (live; carries `{ id, path, version, text }`; published by `App::new` for the initial buffer and `App::do_edit` for subsequent opens; the LSP attach driver in `lattice_lsp::attach_driver` is the canonical subscriber, and §5.4.3 describes the event-driven LSP attach in full), `BeforeSave`, `AfterSave`, `BeforeClose`, `DocumentClosed`, `BufferChanged`, `LanguageDetected`.
- **Modal state:** `ModalModeChanged { from, to }`, `OperatorPendingEntered`, `OperatorPendingResolved`.
- **Mode lifecycle:** `MajorModeActivated`, `MajorModeDeactivated`, `MinorModeActivated`, `MinorModeDeactivated`.
- **Selection / cursor:** `SelectionsChanged`, `CursorMoved`, `JumpPushed { source }`.
- **LSP:** `LspServerStarted`, `LspResponseReceived`, `DiagnosticsUpdated`, `CompletionAvailable`, `LspLogPushed { server_id, level, source, message }` (every `LspLogger::log` append; powers live-tail of `*lsp:<server>*` / `*lsp:<server>:trace*` buffers).
- **UI:** `PaneFocused`, `PaneClosed`, `WindowFocused`, `BufferViewOpened`.
- **Plugin:** `PluginActivated`, `PluginCrashed`, `PluginDeactivated`.
- **System:** `Idle { duration }`, `FocusGained`, `FocusLost`, `BeforeQuit`.

Each event carries a typed payload. `BeforeSave` carries `{ document, path, content_hash }`; `SelectionsChanged` carries `{ document, old, new }`; etc.

#### 5.10.2 Hook handlers may *modify* events for "Before"-class events

A subscription registered as `Invocation(...)` for an event with the `Before` semantics receives the typed payload, may mutate fields the event declares as mutable, and may veto by returning `Err`. `BeforeSave` handlers can rewrite content (formatters do this); `BeforeQuit` handlers can veto (with a reason that surfaces as a notification). For non-Before events the handler's return value is ignored except for error-logging.

#### 5.10.3 Vim `:autocmd` and emacs hooks both desugar to this

```vim
" vim
autocmd BufWritePre *.rs RustFmt
```

```lisp
; emacs
(add-hook 'before-save-hook
          (lambda () (when (eq major-mode 'rust-mode)
                       (rust-format-buffer))))
```

```rust
// lattice (the underlying call -- both syntaxes desugar to this)
events.subscribe(
	EventFilter {
		kinds: Some(vec![EventKind::BeforeSave]),
		document_pattern: Some(Pattern::new("*.rs")),
		..Default::default()
	},
	SubscriptionTarget::Invocation(invocation_for("rust-fmt-format-buffer")),
);
```

The `:autocmd` and `:add-hook` ex-commands are parser front-ends for this call.

#### 5.10.4 Performance

Subscriptions live in indexed maps keyed by `(EventKind, document_pattern_bucket, major_mode)`. Publishing an event evaluates the filter for matching buckets only, never iterating the global subscription list. WASM-hosted subscription handlers run on the publisher's tokio task via the async ABI; a slow handler does not delay other subscribers because each `Invocation` target is dispatched as a separate task.

Subscriptions for `Before`-class events are bounded in count per event; if a user installs 100 `BeforeSave` hooks, the save runs them in registration order and each gets a fuel budget. A handler that exhausts fuel logs and is skipped; the save proceeds.

### 5.11 Introspection and Help

Every registered primitive in the editor -- commands, options, events, modes, keybindings -- carries metadata. The metadata is mandatory at registration time, not optional documentation, and the `:describe-...` family of commands renders it on demand.

```rust
pub struct CommandMetadata {
	pub id: CommandId,
	pub name: String,
	pub doc: String,                          // markdown, multi-paragraph
	pub args: Vec<ArgMetadata>,
	pub since_version: Version,
	pub source: SourceLocation,               // path, line, plugin
	pub category: Category,
	pub example_invocations: Vec<String>,
}

pub struct ArgMetadata {
	pub name: String,
	pub ty: ArgType,                          // typed (Path, Regex, OptionId, ...)
	pub doc: String,
	pub default: Option<DefaultValue>,
	pub completion: Option<CompletionSourceId>,
	pub validator: Option<ValidatorId>,
}
```

Built-in introspection commands:

| Command | Opens | Content |
|---|---|---|
| `:describe-key <chord>` | `*help:key:<chord>*` | Resolved command, arg presets, source layer, alternative bindings |
| `:describe-command <name>` | `*help:command:<name>*` | Signature, doc, default keymap entries, examples |
| `:describe-option <name>` | `*help:option:<name>*` | Type, default, current value, doc, group, validator |
| `:describe-event <kind>` | `*help:event:<kind>*` | Payload, mutability, current subscribers |
| `:describe-mode <id>` | `*help:mode:<id>*` | Major/minor mode summary, keymap, hooks, declared style mappings |
| `:describe-buffer` | `*help:buffer*` | Current buffer's mode stack, encoding, options, keymap chain |
| `:apropos <pattern>` | `*help:apropos:<pattern>*` | Fuzzy search across all metadata |

Each opens a buffer-backed help view (consistent with everything-is-a-buffer). The view is rendered by `EditorRenderer` for code-like content and `DocumentRenderer` for prose-heavy descriptions; cross-references inside it are clickable / followable via standard motions.

**Cost model.** Metadata lives next to registrations and is only materialized when an introspection command runs. The catalog is queryable in O(1) by id and O(log N) by name; `:apropos` is a streaming picker (§5.9.7) over all metadata.

The keymap descriptor metadata that backs `:describe-key` -- the typed `KeymapEntry` rows, their forgery-prevented `SourceLocation` capture, and the catalog/registry consistency invariants -- is specified in [`docs/keymap-architecture.md`](keymap-architecture.md) §3.5 and §6.

#### 5.11.1 Provenance: source-of-truth for every binding

Vim's `:verbose set X?` shows where an option was last changed; Emacs's `C-h k` only shows the function bound, not where the binding came from. Lattice unifies these: **every registered / bound / set thing carries a `SourceLocation` recording where it was created**, surfaced as a `[[file:...]]` link in every `:describe-*` output. The user can follow the link to inspect or edit the source.

```rust
pub struct SourceLocation {
	pub layer: SourceLayer,
	pub kind:  SourceKind,
}

pub enum SourceLayer {
	Builtin, UserConfig, ProjectConfig, Modeline, Runtime, Plugin(PluginId),
}

pub enum SourceKind {
	File { path: PathBuf, line: Option<u32> },
	CommandLine { history_index: usize },         // typed at `:`
	MacroReplay { register: char, step: u32 },    // replayed
	DotRepeat(Box<SourceLocation>),               // chains transitively
	Synthetic(&'static str),                      // <initial-load>, <test>
}
```

**Forgery prevention is structural.** There is **no public API that takes a `SourceLocation` parameter**. The four ways a `SourceLocation` can come into existence are:

1. **Built-in registration** uses `#[track_caller]` on `register_motion` / `register_operator` / `register_text_object` / `register_ex_command`. The compiler captures the caller's `(file, line)` automatically -- the caller cannot supply or override it. Untrusted code can only call from a different `(file, line)`, which is just being honest about where it actually is.
2. **Static-slice rows** use a per-row declarative macro (`keymap_entry!`, `option_spec!`, ...). `file!()` and `line!()` expand at *each row's* invocation site, so the captured location matches the row. The `source` field on the underlying struct is `pub(crate)`; the macro is the only construction path.
3. **Trusted subsystems** (config loader, plugin host bridge, runtime dispatcher) construct `SourceLocation` from their own ground truth -- the loader knows which TOML file and line it parsed, the host knows the plugin's identity from its `Store<PluginCtx>`, the dispatcher knows it's executing a `:` line. They reach `pub(crate) insert_*` registry methods directly. Visibility is `pub(crate)` today (everything trusted lives in the same crate); when cross-crate trusted subsystems land, visibility is granted via sealed-trait re-exports, never by exposing a public `_at` form.
4. **Tests** use `SourceLocation::synthetic("<test-fixture>")` behind `#[cfg(test)]`.

**Determinism guarantee.** A unit test (`track_caller_captures_register_motion_call_site`) registers a sentinel command at a known line and asserts the captured location matches. Any future refactor that breaks call-site capture -- wrapping `register_motion` in a `dyn Fn` dispatcher, hand-rolling source values somewhere, removing `#[track_caller]` from a helper in the chain -- fails CI on the line-number mismatch.

#### 5.11.2 Generic introspection (`Introspectable` trait)

Every `:describe-*` target implements one trait:

```rust
pub trait Introspectable {
	fn kind_label(&self) -> &'static str;
	fn identifier(&self) -> String;
	fn doc(&self) -> &str;
	fn sources(&self) -> Vec<SourceEntry<'_>>;
	fn extra_sections(&self) -> Vec<HelpSection> { Vec::new() }
}

pub fn render_introspection(item: &dyn Introspectable) -> Vec<String>;
```

`render_introspection` produces the help body in a uniform shape: `identifier (kind)` heading, doc, type-specific extra sections (e.g. `Arguments:` for commands), then one `[[file:...]]` link per source labelled (`Defined at:`, `Bound at:`, `Subscribed at:`, `Last set at:`, `Overridden at:`, `Activated at:`). Each `:describe-X` is a thin lookup-and-call.

`extra_sections()` is the open hook for type-specific structure: commands render their `args_schema`, options render their type and current value, events render their subscribers list, modes render their keymap and hooks. Adding a new registry means adding one trait impl; the renderer doesn't change.

**Multiple sources per item are first-class.** An option's `:describe-option` shows two source links: `Defined at:` for the registration (default-value source) and `Last set at:` for the most recent setter. A user-overridden built-in command shows `Defined at:` (the built-in) plus `Overridden at:` (the user config). The trait returns `Vec<SourceEntry>`; the renderer emits one labelled link per entry.

#### 5.11.3 Completion pipeline

The `:`-line and (eventually) every minibuffer-shaped prompt run their candidates through a four-stage pipeline modelled after emacs's `vertico` / `orderless` / `marginalia` ecosystem -- composability is the architectural property, not a future ambition. The crate `lattice-completion` is a standalone library with its own test corpus; it depends on `lattice-grammar` for the `CommandRegistry` shape but does not depend on any UI crate.

```rust
// Each stage is a trait. Plugin authors target these.
pub trait CandidateGenerator { fn generate(&self, ctx: &GenerateContext) -> Vec<RawCandidate>; ... }
pub trait CandidateMatcher   { fn matches(&self, query: &str, c: &RawCandidate) -> Option<(MatchScore, Vec<Range<usize>>)>; }
pub trait CandidateRanker    { fn rank(&self, scored: &mut Vec<ScoredCandidate>); }
pub trait CandidateAnnotator { fn annotate(&self, candidate: &mut RenderedCandidate); }

// One assembled pipeline runs all four in sequence.
pub struct CompletionPipeline {
	pub generators: Vec<Arc<dyn CandidateGenerator>>,
	pub matcher:    Arc<dyn CandidateMatcher>,
	pub ranker:     Arc<dyn CandidateRanker>,
	pub annotators: Vec<Arc<dyn CandidateAnnotator>>,
}
```

**Stages, mapped to the emacs analogue:**

| Lattice stage | Emacs analogue |
|---|---|
| `CandidateGenerator` | `consult`'s sources |
| `CandidateMatcher` | `orderless` (or default substring matching) |
| `CandidateRanker` | scoring step (custom in vertico-prescient, etc.) |
| `CandidateAnnotator` | `marginalia` |
| Renderer (in `lattice-ui-tui`) | `vertico` |

**Per-slot resolution.** The `:`-line driver computes the current slot (command name, arg N, delimiter-syntax body) via `current_slot(line, cursor, registry)`. The slot dictates which generator the pipeline uses for this query; the matcher / ranker / annotators come from the registry's user-configured defaults (`cmdline.matcher = "match:fuzzy"`, etc.).

**Forgery resistance.** Every registration is `#[track_caller]`; the registry has `pub(crate) insert_*` companions. Same invariant as commands and keymap entries: no public API takes a `SourceLocation`.

**Caching.** Opt-in per generator via `CandidateGenerator::cache_key` returning `Option<CacheKey>`. The pipeline reads from a shared `GeneratorCache` before invoking `generate`; on miss it caches the produced candidate set. Each generator declares its own TTL via `cache_ttl()` (default `Duration::MAX`). Built-in cache strategies:

| Generator | `cache_key` | `cache_ttl` |
|---|---|---|
| `gen:commands` | `"gen:commands:v1"` (fixed -- v1 commands don't change post-startup) | `MAX` |
| `gen:files` | `"gen:files:{dir}"` | 1 second (filesystem mutates) |
| `gen:options` (post-§5.12) | `"gen:options:v{N}"` | `MAX` until version bumps |

The matcher / ranker / annotators always run live; only generation is cached.

**Built-ins shipped with `lattice-completion`:**

| Stage | Built-in | Purpose |
|---|---|---|
| Generator | `gen:commands` | every `CommandSpec` |
| Generator | `gen:files` | filesystem walk |
| Matcher | `match:prefix` | exact-prefix (default) |
| Matcher | `match:substring` | case-insensitive contains |
| Matcher | `match:fuzzy` | subsequence with byte-range tracking; score decays with skipped chars; prefix-bonus |
| Ranker | `rank:score` | descending score (default) |
| Ranker | `rank:alphabetical` | A-Z |
| Annotator | `anno:kind-label` | `(motion)`, `(file)`, `(directory)`, etc. |
| Annotator | `anno:doc-snippet` | first line of doc |

Host-state generators (`gen:chords`, `gen:registers`, `gen:marks`, `gen:buffers`) live in the host crate (`lattice-ui-tui`) because they read App-level state; they register against the same `CompletionRegistry` like any plugin would.

**Vertico-style rendering** (post-popup work): a vertical list of candidates, one per row, with the matched byte ranges from `ScoredCandidate.match_ranges` painted with a distinct style. Annotations rendered right-aligned. Selected row marked. Renderer is replaceable -- when the rich minibuffer (§5.9.10) lands, the popup graduates to a tree-sitter-styled buffer view; the underlying `RenderedCandidate` shape doesn't change.

**Insert-mode completion** (Phase 4.2.g) is the editor surface that turns this pipeline into a buffer-level input flow: trigger evaluation per-keystroke, async sources (LSP / snippets / buffer-words / path / tree-sitter / plugin), multi-column popup display, side documentation popup with lazy `completionItem/resolve`, snippet engine with placeholder navigation, frequency-aware ranking. Spec lives in [`insert-completion.md`](insert-completion.md); behavioural choices are explained alongside surveyed precedents (VS Code / Neovim `blink.cmp` / Helix / JetBrains / Sublime / Emacs `corfu`).

### 5.12 Configuration System (typed options + code-as-config)

Vim's `:set option=value` is a string-bag with no typing or validation, and vimscript fills the gaps with a string-shaped scripting language users have to learn separately. Emacs's `customize` is a typed system bridged awkwardly to `setq` for non-curated variables, and elisp fills the gaps with a second authoring environment plugin authors must also master. We unify both halves: a typed option registry for **data**, and the Rust→WASM plugin substrate (§5.5) reused as the **code** layer. There is no third surface and no second language.

> **Implementation status:** the typed-option registry below ships in its own renderer-agnostic crate, `lattice-config`. The current implementation uses a slightly tightened shape vs. the sketch (each `Option<T>` is generic and owns its own `ArcSwap<T>` value cell; the registry stores `Arc<dyn ErasedOption>`; consumers hold typed `OptionHandle<T>` for zero-overhead reads). `:set` syntax, `gen:options` completion, `:describe-option`, and the `register_core_options` / `register_*_options` API all flow through this crate. The `options.toml` + `init.rs` layers below are post-Phase-7 (gated on the WASM plugin host).

#### 5.12.1 The typed option registry

```rust
pub struct OptionSpec {
	pub id: OptionId,
	pub name: String,                 // dotted path: "editor.line-numbers"
	pub ty: OptionType,
	pub default: Value,
	pub doc: String,
	pub group: GroupPath,             // "Editor" / "UI" / "LSP" / "Plugin: git-gutter"
	pub validator: Option<ValidatorId>,
	pub on_change: Option<EventKind>, // event published when value changes
	pub scope: OptionScope,           // Global | PerDocument | PerWindow
}

pub enum OptionType {
	Bool, Int { min: Option<i64>, max: Option<i64> },
	Float, String, Path, Regex,
	Enum(Vec<String>),
	List(Box<OptionType>),
	Map(Box<OptionType>, Box<OptionType>),
	Custom(TypeId),
}
```

The registry is the single source of truth: every option's name, type, default, doc, group, validator, scope, and on-change event live here. `:set`, `:describe-option`, the customize buffer, the TOML deserializer, and any plugin / `init.rs` call all read from and write to the same `OptionSpec`.

#### 5.12.2 Two layers, both optional

User configuration lives in two layered files at `~/.config/lattice/`:

```
~/.config/lattice/
├── options.toml      # static option overrides; data only; no toolchain needed
└── init.rs           # Rust source, compiled to WASM, loaded as a plugin with `boot` capability
```

| Layer          | Format                | Toolchain                                                  | Loaded                                                          | What it expresses                                                                                                                                                                   |
|----------------|-----------------------|------------------------------------------------------------|-----------------------------------------------------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `options.toml` | TOML                  | none                                                       | deserialized into the option registry                           | typed option overrides; static keymap entries (chord → invocation string); static autocmds (event + filter + invocation string)                                                     |
| `init.rs`      | Rust → WASM Component | rustup + cargo-component (auto-detected; banner if absent) | loaded as a plugin via the §5.5 host with the `boot` capability | everything `options.toml` can express, plus closures, conditionals, custom command/motion/operator registration, autocmd handlers with logic, and any other plugin-shaped extension |

Either, both, or neither can be present. Both fall back to defaults when absent. Both reach the same internal state through the same functional API -- TOML via deserialize-then-call-the-setter, `init.rs` via WASM-call-into-the-host-which-calls-the-setter. There is no functional gap between them; the cost difference is "instant data load" vs. "5-15s first-boot compile, then cached".

The intended progression: a new user copies an `options.toml` example. When they outgrow declaration -- a keymap that needs context, a hook that does real work -- they migrate the affected piece into `init.rs`. The graduation cost is "learn the same API the plugin SDK exposes", because **`init.rs` is a plugin** -- the only thing distinguishing it from a third-party plugin is the `boot` capability and the well-known load path.

#### 5.12.3 The `init.rs` plugin

`init.rs` is a single source file. The host wraps it in a small generated crate (`Cargo.toml`, `src/lib.rs` shim, `[package.metadata.component]` entry) under `~/.cache/lattice/init-build/` and compiles it through `cargo-component build` against the published `lattice-config-api` crate, which re-exports the §5.5 / §9 WIT bindings under an ergonomic Rust-native shape.

```rust
// ~/.config/lattice/init.rs

use lattice::config::*;

#[lattice::init]
fn init(c: &mut Config) {
	// Static settings (could equally live in options.toml).
	c.set("editor.tabstop", 4);
	c.set("editor.relativenumber", true);

	// Programmatic -- the bit TOML can't do.
	c.keymap("normal", "<C-s>", "write()");
	c.keymap("normal", "<leader>fe", |ctx| {
		let path = ctx.active_buffer().path()?;
		ctx.invoke(format!("Tree(\"{}\")", path.parent().unwrap().display()))
	});

	c.autocmd("BeforeSave", "*.rs", "format()");

	// Custom command registration -- same API third-party plugins use.
	c.register_motion("motion:my-fancy-jump", "Jump to next paragraph header", |mctx| {
		/* ... */
	});
}
```

`Config` is the WIT-defined facet of the host plugins use, scoped to operations sensible at boot time: option setting, keymap registration, command registration, autocmd subscription, event-bus subscription, and the `invoke(CommandInvocation)` host call. Capabilities beyond `boot` (filesystem, network) are declared in the manifest the same way they would be for a third-party plugin and require the user's explicit acknowledgement; the `boot` capability alone is bounded.

#### 5.12.4 Auto-build on first boot

The user does not run a build command manually. Boot sequence:

1. Read `options.toml` if present -- pure data; deserialize and apply.
2. Look for `~/.config/lattice/init.rs` (or escalate to `init/` if the user has split into a multi-file crate).
3. Compute cache key: `sha256(source + lattice_version + wit_revision)`.
4. Probe `~/.cache/lattice/init-<key>.wasm`.
   - **Hit:** load via the §5.5 plugin host with the `boot` capability set; run `init(...)`.
   - **Miss:** spawn a background tokio task that materialises the build scaffold, runs `cargo-component build`, places the artifact at `~/.cache/lattice/init-<key>.wasm`, then loads it. The UI shows a "Compiling config..." splash if the build doesn't complete within ~200 ms; cargo's stdout streams to `:messages`.
5. If toolchain is missing (rustup / cargo-component), boot continues with defaults and a non-fatal banner: *init.rs found but no Rust toolchain detected; install rustup + cargo-component to enable, or run `lattice config build --help`.*
6. If the build fails, boot continues with defaults; the compile error is rendered in a help-style buffer (Rust syntax-highlighted) reachable via `:describe-config-error` and surfaced as a non-fatal banner.

Subsequent boots are dominated by the `dlopen`-equivalent of the cached WASM artifact -- in the high-tens-of-microseconds range. The compile cost is one-time per source change or editor version.

A filesystem watcher on `init.rs` triggers an in-background recompile when the user saves; on success the host hot-swaps the loaded module (the §5.5 host already supports plugin reload). Live config feedback without restart, with the same compile-then-load pipeline; just incremental.

The `lattice config build` CLI subcommand is **not** on the user's critical path -- it exists as a debugging / scripting tool. Use cases:

- Pre-warming a cache on a fresh machine before first launch (dotfile-bootstrap scripts).
- Diagnosing "config didn't load" with full verbose cargo output.
- CI-checking a config (validate it compiles before pushing dotfiles changes).
- Cross-compiling and shipping a pre-built `init.wasm` alongside the source for sharing -- the host loads the pre-built artifact when its hash matches the source.

#### 5.12.5 Sources and precedence

Values come from layered sources, resolved in this order (later wins):

1. Built-in defaults (`OptionSpec::default`).
2. Bundled config files (default keymap, theme).
3. User options (`~/.config/lattice/options.toml`).
4. User init module (`~/.config/lattice/init.rs`, compiled to WASM, run with `boot` capability).
5. Project options (`.lattice/options.toml` at workspace root).
6. Per-buffer overrides (modeline-style `:setlocal`).
7. Programmatic / `:set` invocations during a session.

`init.rs` runs after `options.toml` is applied so it can read what TOML did and override or extend. Project-level `init.rs` is **deferred** -- arbitrary code execution by virtue of `cd`-ing into a directory is a real attack surface; the eventual mechanism is a per-directory trust prompt with a hashed allowlist (vim's `:set exrc` with explicit trust). Until that lands, project-local code-config is unsupported.

#### 5.12.6 The `:set` parser front-end

`:set option=value`, `:set option!`, `:set option+=value`, `:set option^=value` are all parsed by the `:set` command's `parse_args` into a typed `SetOption` invocation:

```rust
struct SetOption {
	option: OptionId,
	op: SetOp,                        // Replace | Toggle | Append | Prepend | Subtract | Reset
	value: Option<Value>,             // typed per option_spec.ty; absent for Toggle/Reset
	scope: SetScope,                  // Global | Local
}
```

Validation runs at the dispatcher; type errors surface as a parse error in the `command-line` minibuffer (live error indicator -- §5.9.10).

`:set` is itself a registered command that dispatches through `execute(...)`; `init.rs`'s `c.set(name, value)` call lowers to the same invocation. There is one path that mutates an option, and it publishes the option's `on_change` event so subscribers (autocmds, `:customize` redraw, dependent options) react uniformly regardless of source.

#### 5.12.7 Customize as a buffer-backed view

A built-in command (`:customize`, or the `customize` major mode) opens a buffer that lists every registered option grouped by `group`. The view is rendered with type-aware widgets: bools are checkboxes, enums are dropdowns, paths are completable strings, lists are addable-removable. Edits dispatch the same `:set` invocations described above. Saved customizations are written back to `options.toml` -- which the user can then move into `init.rs` if they need code around them. Filtering (`/`) and folding (`za`) work because it is a buffer.

#### 5.12.8 Options are addressable from every entry point

- The `:set` line.
- `:describe-option <name>` (introspection).
- The customize buffer.
- `options.toml` (deserializer-driven).
- `init.rs` (WIT host call: `config.set` / `config.get`).
- A third-party plugin (same WIT call as `init.rs`).

All six entry points produce or consume the same typed `Value` against the same `OptionSpec`. `init.rs` and third-party plugins use *literally the same call*; the only thing that distinguishes them is the capability set the host loaded them with.

#### 5.12.9 Invariants

- **No second config language, ever.** TOML for data, Rust-WASM for code. Lua / vimscript / elisp / Rhai / Janet / a custom config DSL are all out of scope -- the doubling of API surface, binding maintenance, ecosystem fragmentation, and learning cost is the explicit cost we refuse. Users who want a no-toolchain logic surface use TOML's static keymap / autocmd entries; users who want logic install a Rust toolchain like every other Rust author. The graduation step from "config" to "third-party plugin" is purely packaging.
- **`init.rs` is a plugin.** It is loaded by the §5.5 host, declared in WIT, capability-gated, fuel-limited, crash-isolated. The only thing privileging it is the `boot` capability and the well-known load path. A bug in `init.rs` cannot crash the editor; it surfaces as a banner and falls back to defaults.
- **Exactly one path per concern.** One option mutation path (`:set` → `execute(...)`), one `CommandInvocation` shape, one `Effect` wire format, one cancellation token, one event bus. Config is a consumer of these surfaces, not a parallel surface.
- **Project-local code-config is gated on a trust mechanism.** Until that mechanism ships, project-level overrides are TOML-only; project-level `init.rs` is unsupported.
- **The compile cost is paid once per source change.** Auto-build is a one-time event amortised across many boots; the user never types a build command on the happy path.

---

## 6. The Core Protocol

### 6.1 Commands (clients to core)

```rust
enum Command {
	// Document management
	OpenDocument { path: PathBuf, reply: oneshot::Sender<DocumentId> },
	CloseDocument { id: DocumentId },
	SaveDocument { id: DocumentId, reply: oneshot::Sender<Result<()>> },

	// Editing
	ApplyEdit { id: DocumentId, edit: Edit, reply: oneshot::Sender<EditResult> },
	Undo { id: DocumentId },
	Redo { id: DocumentId },

	// Selection
	SetSelections { id: DocumentId, selections: SelectionSet },
	AddCursor { id: DocumentId, position: Position },

	// Modal mode
	EnterModalMode { mode: ModalMode },

	// Major/Minor modes
	SetMajorMode { id: DocumentId, mode: MajorModeId },
	ActivateMinorMode { id: DocumentId, mode: MinorModeId },
	DeactivateMinorMode { id: DocumentId, mode: MinorModeId },

	// LSP-mediated
	Complete { id: DocumentId, position: Position, reply: oneshot::Sender<CompletionList> },
	Hover { id: DocumentId, position: Position, reply: oneshot::Sender<Option<Hover>> },
	GotoDefinition { id: DocumentId, position: Position, reply: oneshot::Sender<Vec<Location>> },

	// Decorations
	AddDecoration { id: DocumentId, decoration: Decoration, reply: oneshot::Sender<DecorationId> },
	RemoveDecoration { id: DocumentId, decoration_id: DecorationId },

	// UI contributions
	RegisterStatusSegment { spec: StatusSegmentSpec },
	UpdateStatusSegment { id: SegmentId, content: SegmentContent },
	RegisterGutterSegment { spec: GutterSegmentSpec },
	RegisterBufferBackedView { spec: BufferViewSpec },        // file-tree, outline, etc.
	OpenBufferView { view: BufferViewId, target: PaneTarget }, // place a buffer-view in a pane
	OpenPicker { spec: PickerSpec, reply: oneshot::Sender<Option<ItemId>> },
	ShowPopup { popup: Popup, reply: oneshot::Sender<PopupId> },
	DismissPopup { id: PopupId },
	PostNotification { notification: Notification },

	// Grammar extension (new in v0.4)
	RegisterMotion { spec: MotionSpec, reply: oneshot::Sender<MotionId> },
	RegisterTextObject { spec: TextObjectSpec, reply: oneshot::Sender<TextObjectId> },
	RegisterOperator { spec: OperatorSpec, reply: oneshot::Sender<OperatorId> },
	RegisterExCommand { spec: ExCommandSpec, reply: oneshot::Sender<ExCommandId> },
	InvokeCommand { invocation: CommandInvocation, reply: oneshot::Sender<Result<Effect>> },

	// Subscriptions
	Subscribe { events: EventFilter, sink: mpsc::Sender<Event> },
}
```

### 6.2 Events (core to clients)

```rust
enum Event {
	// Document
	DocumentOpened { id: DocumentId, path: PathBuf, language: Option<LanguageId> },
	DocumentChanged { id: DocumentId, version: u64, edits: Vec<AppliedEdit> },
	DocumentSaved { id: DocumentId },
	DocumentClosed { id: DocumentId },

	SelectionsChanged { id: DocumentId, selections: SelectionSet },
	ModalModeChanged { from: ModalMode, to: ModalMode },

	MajorModeChanged { id: DocumentId, from: Option<MajorModeId>, to: MajorModeId },
	MinorModeActivated { id: DocumentId, mode: MinorModeId },
	MinorModeDeactivated { id: DocumentId, mode: MinorModeId },

	DiagnosticsUpdated { id: DocumentId, diagnostics: Vec<Diagnostic> },

	LspServerStarted { server: ServerId, language: LanguageId },
	LspServerCrashed { server: ServerId, error: String },

	PluginActivated { plugin: PluginId },
	PluginCrashed { plugin: PluginId, error: String },

	// UI events
	PaneFocused { pane: PaneId },
	PaneClosed { pane: PaneId },
	PopupShown { id: PopupId },
	PopupDismissed { id: PopupId, reason: DismissalReason },
	PickerOpened { id: PickerId },
	PickerClosed { id: PickerId, selection: Option<ItemId> },
	BufferViewOpened { view: BufferViewId, document: DocumentId, pane: PaneId },
	NotificationPosted { id: NotificationId },
}
```

### 6.3 Wire format

In-process: tokio mpsc channels with enums. Cross-process: same enums via MessagePack over Unix socket or TCP. Channel types are generic; in-process callers pay zero serialization cost.

---

## 7. Data Flow Examples

### 7.1 User types `x` in normal mode (delete character) -- code buffer

UI receives KeyEvent -> Command::DispatchKey -> Core resolves to Operator::Delete -> ApplyEdit -> buffer mutates -> Event::DocumentChanged -> tree-sitter reparse on worker, LSP didChange (debounced), plugins notified, renderer marks viewport dirty -> next vsync renders.

End-to-end 1-7: <2ms. Background work in parallel.

### 7.2 User types in a markdown heading -- rich buffer

Same as above through buffer mutation. EditorRenderer (shaped path) marks line N's layout cache as dirty -> shape job dispatched to rayon worker -> worker shapes line (~100us) and updates Fenwick index -> next vsync uses fresh layout (or stale for one frame if not done).

End-to-end input: <2ms regardless of shaping completion.

### 7.3 User triggers completion (insert mode, after `.`)

Command::Complete -> LSP request -> 50-500ms wait (user keeps typing) -> version-aware cancellation if buffer changes -> eventual matching response -> Event::CompletionAvailable -> UI shows popup via DocumentRenderer -> first paint ~5-10ms; cached afterward.

### 7.4 User opens command palette (Ctrl+Shift+P)

```
1. UI receives keypress, recognizes binding for "open-command-palette".
2. UI sends Command::OpenPicker with spec:
   - content_provider: built-in CommandPaletteProvider (returns all registered commands)
   - item_renderer: shows command name + binding + description
   - preview_provider: None
3. Picker overlay appears (DocumentRenderer), centered, with input field focused.
4. User types "form".
5. Picker filters items via fuzzy match on each provider yield.
6. User presses Enter on "format-buffer".
7. Picker emits Event::PickerClosed with selected item.
8. Selection callback runs the format-buffer command.
```

Throughout: editor pane keeps rendering, LSP keeps running, no other UI work is blocked.

### 7.5 A plugin crashes mid-render

```
1. Plugin contributes a status segment showing "git branch".
2. Plugin's content provider, when called, panics.
3. Wasmtime catches the trap.
4. Core marks the segment as failed, logs the error.
5. Status line renders without that segment (others unaffected).
6. Notification posted: "Plugin 'git-info' crashed (segment disabled)".
7. User sees the notification; the rest of the editor is unaffected.
```

---

## 8. Performance Strategy

### 8.1 Invariants

1. UI thread does no I/O, no parsing, no shaping.
2. No mutex on the buffer. Actor pattern.
3. Every async operation has cancellation.
4. Plugins cannot block the host (wasmtime async + fuel).
5. Allocations on the input path are bounded.
6. Rendering paths are explicit per-buffer; no silent upgrade.
7. Text shaping for rich buffers happens on workers.
8. UI furniture (popups, buffer-backed views, status) is layout-once, cached, off the input path.

### 8.2 Performance commitments per path

The shape here is "what's the *physically credible* best we can hit
on this path, and what do we commit to ship at v1.0?" not
"how much margin do we want above neovim?" Where the architecture
genuinely permits ns numbers (atomic loads, format-only segments),
we don't settle for µs targets just because incumbent editors do.

#### Columns

- **Floor.** The physics-credible best given our architecture --
  derived from the cost of the underlying primitives (atomic
  acquire-load, `parking_lot` mutex acquire, tokio cross-task
  wakeup, ropey rope op, ratatui draw, tree-sitter parse). When a
  row's floor is bounded by a known-hard limit (cache bandwidth,
  scheduler latency, allocator), we say so in the rationale. This
  isn't aspirational -- it's "what microbenchmarks of the
  individual ops add up to."
- **Target (v1).** What every implementation MUST hit by v1.0.
  Tighter than the Today column on rows where we know the
  engineering path; relaxed where the Today column reflects a
  legitimate trade we're keeping. CI fails on >10% regression
  vs. main on any benchmark.
- **Today.** The current `docs/BENCHMARKS.md` median. "—" means
  unmeasured (a gap; backs a row in `BENCHMARKS.md`'s "what's
  NOT here" section).
- **Stretch.** Credible with N-months-of-known-engineering, not
  novel research. Cited paths: GPU renderer, suffix-array search
  index, single-thread tokio runtime, sync edit fast-path,
  tree-sitter `Tree::edit` deltas threaded through the actor.

The "vs neovim" framing the previous revision carried is
deliberately gone -- the targets here are derived from primitive
costs, not from being "X× faster than vim." Where we end up
significantly faster than incumbents on a row, the rationale
column says why our architecture permits it; where we're
constrained by physics (cache bandwidth on a 200k-line full-buffer
search; tokio scheduler latency on a multi-thread async actor),
the rationale states the constraint.

#### Read path (renderer reads buffer state)

| Operation                                        | Floor  | Target (v1) | Today                                           | Stretch | Rationale                                                                                                                                                 |
|--------------------------------------------------|--------|-------------|-------------------------------------------------|---------|-----------------------------------------------------------------------------------------------------------------------------------------------------------|
| Snapshot load (renderer, `load_full`)            | ~16ns  | <20ns       | **16ns**                                        | ⏹️       | At the floor for `load_full` semantics: one atomic acquire-load + one `Arc` bump. The renderer's hot path uses `Cache::load` instead.                       |
| Snapshot load (renderer, `Cache::load`)          | ~300ps | <500ps      | **305ps**                                       | ⏹️       | Wait-free thread-local-cached steady-state load: one `Relaxed` atomic compare, register-cached `Arc` returned. ~50× faster than `load_full` -- below 1ns. |
| Status segment update (1 snapshot read + format) | ~50ns  | <100ns      | **56ns**                                        | ⏹️       | Measured ~56ns. One `ArcSwap::load_full` (~17ns) + Arc deref + `format!` of a few u64s. Already at the practical floor.                                   |
| Frame render (code, TUI, 80×24)                  | ~200µs | <500µs      | highlight **178µs** + compose **14µs** = ~192µs | <100µs  | `compose_visible_lines` is fast (~14µs viewport-bounded); highlight dominates. Stretch is a viewport-bounded highlight cache that survives across frames. |
| Frame render (code, TUI, 200×60)                 | ~325µs | <800µs      | highlight **289µs** + compose **35µs** = ~324µs | <200µs  | Same shape, larger viewport. Linear in highlighted-line count; the per-frame compose cost is essentially free. Compose dropped from 40µs to 35µs after the renderer moved to `Cache::load` (one snapshot pinned at frame start; no internal `load_full` per inner helper).                                            |
| Frame render (code, GPU, 1080p)                  | ~150µs | <1ms        | n/a                                             | <300µs  | Variable-font shaping cached per-line; only diff repaints. GPU path post v1 design (§5.6).                                                                |
| Frame render (markdown, GPU, 1080p)              | ~600µs | <3ms        | n/a                                             | <1.5ms  | Per-line layout cache + Fenwick height index; floor scales with shape-changed lines.                                                                      |
| Highlight span cache hit (steady-state)          | ~10ns  | <50ns       | not built (B.3)                                 | ⏹️       | Cache keyed on `(text_version, viewport_range, fold_hash)`. Steady-state norm -- cursor blinking, no edit -- ~100% hit; miss falls through to `highlight_lines`. Drops per-frame highlight cost from ~178µs to floor. |

#### Write path (mutation → published snapshot)

| Operation                                                                         | Floor  | Target (v1)         | Today                                                         | Stretch | Rationale                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
|-----------------------------------------------------------------------------------|--------|---------------------|---------------------------------------------------------------|---------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Snapshot publish standalone (`from_document` + `ArcSwap::store`)                  | ~80ns  | <500ns              | **101ns**                                                     | <50ns   | Measured ~101ns constant across buffer sizes -- Buffer::clone is O(1) Arc bump; the cost is the allocator (`Arc::new`) + atomic release-store. Below original 2µs target.                                                                                                                                                                                                                                                                                                         |
| InputEdit construction (per `Document::apply_edit`)                               | ~2ns   | <10ns               | not built (B.1)                                               | ⏹️       | Six u32 writes on the stack alongside `AppliedEdit`. Below the regime where caching matters; floor is "no allocation, no work beyond field copies."                                                                                                                                                                                                                                                                                                                                |
| `tree.edit()` per-edit (syntax worker, sync pre-step)                             | ~500ns | <1µs                | not built (B.2)                                               | ~200ns  | Bounded tree walk adjusting affected nodes' byte/Position fields. Floor scales with affected-node count. Runs on the worker (where it has exclusive `Tree` ownership), not the input thread.                                                                                                                                                                                                                                                                                       |
| Apply-edit (sync fast-path, `with_document_mut(closure)`) *(deferred to Phase 7)* | ~5µs   | not v1              | not built                                                     | <2µs    | parking_lot mutex acquire (~30ns) + ropey op (~1-3µs) + snapshot publish (~500ns). **Deferred** because the plugin-holds-mutex starvation risk is unbounded under today's cooperative-only timeout enforcement. Lands once Phase 7's WASM fuel makes plugin call duration infrastructure-bounded (rather than discipline-bounded). Until then the actor envelope below is the keystroke floor; that's well under §8.2's <16ms-per-frame budget so we're not actually constrained. |
| Apply-edit round-trip (async actor, `block_on(handle.apply_edit(...))`)           | ~50µs  | <100µs              | 85µs                                                          | <50µs   | Two cross-thread tokio wakeups (~30µs each) + actor work. Floor is scheduler-bound on a multi-thread runtime; single-thread runtime would close to ~30µs.                                                                                                                                                                                                                                                                                                                         |
| Dispatch round-trip (motion + Effect commit)                                      | ~50µs  | <100µs (small bufs) | **78µs** (10 lines) / **86µs** (1k) / 513µs (50k motion walk) | <50µs   | Scheduler-bound on small buffers (matches apply-edit envelope). On large buffers the motion's own walk dominates -- the `word_forward` walk on 50kloc is the cost, not the envelope.                                                                                                                                                                                                                                                                                              |
| Keystroke to glyph (code, TUI)                                                    | ~250µs | <2ms                | unmeasured                                                    | <800µs  | Sync fast-path + reparse + viewport highlight + frame render. Stretch when sync fast-path + incremental reparse both land.                                                                                                                                                                                                                                                                                                                                                        |
| Keystroke to glyph (code, GPU)                                                    | ~200µs | <2ms                | n/a                                                           | <500µs  | Same minus ratatui's terminal-write overhead.                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| Keystroke to glyph (code w/ LSP)                                                  | ~300µs | <3ms                | n/a                                                           | <800µs  | Decoupled: LSP results land on a later frame; the keystroke itself doesn't wait. Floor unchanged from non-LSP.                                                                                                                                                                                                                                                                                                                                                                    |
| Keystroke to glyph (markdown render, GPU)                                         | ~700µs | <5ms                | n/a                                                           | <2ms    | Inline-shape cost dominates; per-line layout cache lets unchanged lines reuse glyph runs.                                                                                                                                                                                                                                                                                                                                                                                         |

#### Search

| Operation                                    | Floor         | Target (v1)   | Today       | Stretch          | Rationale                                                                                                                                                  |
|----------------------------------------------|---------------|---------------|-------------|------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Literal substring, first-match-near-cursor   | ~50ns         | <1µs          | 200ns–2µs   | <100ns           | memmem on one rope chunk. Floor is the SIMD prefilter cost. Today's number is fancy-regex's per-call setup; trivial-pattern fast-path could land at floor. |
| Literal substring, worst-case 200k buffer    | ~300µs        | <2ms          | 659µs       | <50µs (post-1.0) | L2 bandwidth limit on a sequential scan. Stretch needs suffix-array index (~5× memory; rebuild on edit; deferred).                                         |
| Regex typical (lazy DFA + literal prefilter) | ~20µs         | <2ms          | 1.1ms       | <500µs           | regex crate's lazy DFA. Stretch via larger scan window amortising per-call setup.                                                                          |
| Regex pathological (backref)                 | n/a (bounded) | abort at 50ms | 169ms (50k) | abort at 50ms    | fancy-regex backtracking; bounded by 1M-iteration recursion limit. Per-search timeout via cancellation token (§5.2.5) is the credible bound.               |

#### File open + parse

| Operation                                             | Floor | Target (v1) | Today                                   | Stretch | Rationale                                                                                                                                                                                             |
|-------------------------------------------------------|-------|-------------|-----------------------------------------|---------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Open 100MB log (first paint, viewport only)           | ~80ms | <100ms      | **76ms** (rope only, render unmeasured) | <30ms   | ropey rope construction at 1.3 GiB/s -- 76ms for 100MB measured. Initial viewport render is on top; well within budget. Tree-sitter parse runs in background; first paint shows raw text immediately. |
| Open 100MB log (full ready, syntax + folds)           | ~80ms | <500ms      | unmeasured                              | <200ms  | Tree-sitter full parse (50ms-class on 100MB depending on grammar) + initial fold compute.                                                                                                             |
| Open 10K-line markdown (full ready)                   | ~50ms | <500ms      | n/a                                     | <200ms  | Block parse + inline injection per visible paragraph; layout cache prebuilt on rayon pool.                                                                                                            |
| Tree-sitter incremental reparse (50kloc, 1-line edit) | ~50µs | <100µs      | unmeasured                              | <50µs   | `Tree::edit` byte-delta + `Parser::parse(.., Some(&old_tree))` reuses unchanged subtrees. Owned-Tree work in Option B unblocked the seam; B.2 threads InputEdit deltas. v1 Target tightened to Stretch -- the engineering path is just "thread the deltas," no novel work. |
| Tree-sitter full reparse (50kloc)                     | ~5ms  | <20ms       | unmeasured                              | <2ms    | The "user pasted a thousand lines" path. Background; doesn't block keystroke. Bench documents the cost.                                                                                               |

#### Folds

| Operation                               | Floor | Target (v1) | Today | Stretch | Rationale                                                                                                                           |
|-----------------------------------------|-------|-------------|-------|---------|-------------------------------------------------------------------------------------------------------------------------------------|
| Fold recompute (indent, 200-fn rust)    | ~30µs | <100µs      | 33µs  | ⏹️       | Linear single-pass scan; near floor.                                                                                                |
| Fold recompute (markdown, 100 sections) | ~5µs  | <50µs       | 6.3µs | ⏹️       | Linear ATX heading walk; near floor.                                                                                                |
| Fold recompute (syntax, 200-fn rust)    | ~3ms  | <5ms        | 3.9ms | <1ms    | `QueryCursor::matches` traversal across many pattern alternatives. Stretch via per-pattern caching + pruning never-folded captures. |

#### Plugin host (§5.5; no host yet, targets gated when phase 7 lands)

| Operation                               | Floor  | Target (v1) | Today | Stretch | Rationale                                                                            |
|-----------------------------------------|--------|-------------|-------|---------|--------------------------------------------------------------------------------------|
| Typed host fn call (1 scalar in, 1 out) | ~150ns | <500ns      | n/a   | <100ns  | wasmtime trampoline + 2 word copies. Floor is Cranelift's ABI marshalling.           |
| Grammar-extension round-trip            | ~2µs   | <5µs        | n/a   | <1µs    | Two trampolines + closure invocation. Wasmtime AOT closes most of this.              |
| Cold start, 50 lazily-loaded plugins    | ~10ms  | <30ms       | n/a   | <5ms    | Module deserialise + import resolution per plugin. Disk cache amortises across runs. |

#### Architectural levers, by row

The Floor / Target / Stretch numbers above are not asserted in
isolation -- specific architecture decisions enable each one:

- **`ArcSwap::Cache::load` (~2ns)** is the renderer's read floor; the
  full DESIGN.md §5.6.8 split between editor thread (writer) and
  render thread (reader) is what permits one atomic primitive on
  the read path.
- **Sync edit fast-path (planned)** drops the keystroke round-trip
  from the actor's 85µs envelope to ~5µs by bypassing the mailbox
  for the editor thread's own writes (see #191).
- **Owned `tree_sitter::Parser` + `Tree`** (Option B, post-Step-4)
  collapses the previous dual-parse (one for highlight + one for
  folds) onto a single parse per keystroke; folds, highlights,
  and any future query consumer all walk the same `Tree`.
- **InputEdit threading + incremental
  `Parser::parse(text, Some(&old_tree))`** (Option B.1 / B.2)
  shrinks the keystroke→fresh-tree window from full-reparse
  (~5ms on 50kloc) to incremental (~50µs floor). Backs the
  reparse row's tightened v1 Target. Worker still runs the parse
  on `spawn_blocking`; input thread emits the InputEdit alongside
  `AppliedEdit` (~2ns) and the worker applies `tree.edit()` as
  the sync pre-step before the async parse.
- **Frame-level highlight span cache** (Option B.3) drops
  steady-state per-frame highlight cost from ~178µs to ~10ns by
  skipping recomputation when no input changed. Cache key is
  `(text_version, viewport_range, fold_hash)`; invalidation is
  one comparison. Load-bearing for paramount goal #1's strict
  reading on the steady-state floor -- without it the input
  thread spends ~178µs/frame doing recoverable work.
- **memmem-driven literal search on rope chunks** (B-α + B-β)
  replaces a naive char-by-char walk with SIMD-prefiltered
  scanning; backs the search floors above.
- **Per-call WASM overhead budgeted in CI** prevents
  plugin-introduced regressions from creeping into any of the
  bolded targets.
- **Latency classes (§5.2.5)** make per-call budgets enforceable;
  any code claiming the keystroke path declares which class it
  belongs to so the arithmetic doesn't drift.

CI fails on >10% regression vs. main on any benchmark, regardless
of whether the row is "today" or "target."

### 8.3 Memory

- Rope memory shared via Arc; snapshots ~free.
- Tree-sitter trees compact; old trees dropped on swap.
- Per-line layout cache (shaped buffers): ~200 bytes/line. 100K-line markdown: ~20MB.
- Glyph atlas: bounded LRU; cap 64MB default.
- Plugin memory capped at instantiation (default 64MB per plugin).
- Status segments / panel content: bounded by visible UI surface.

---

## 9. Plugin API

### 9.1 Principles

1. **Capability-based.** Filesystem, network, subprocess access is granted explicitly per plugin and enforced by the wasmtime runtime.
2. **No global mutation APIs; submit edits.** All state changes flow through the actor-protected document.
3. **Events are facts, not requests.** Plugins observe; they don't ask the editor to do things by mutating event data.
4. **The plugin surface is data, not code.** Plugins emit content (segment text, decoration ranges, picker items), not draw calls.
5. **Async by default.** The Component Model async ABI is the canonical pattern. Host functions yield; the plugin task suspends; the OS thread runs other work.
6. **The grammar is the API.** Operators, motions, text objects, registers, ranges, counts -- all are first-class WIT types and first-class extension points.
7. **Cross-language by construction.** WIT is the source of truth. Plugins ship in any language with component-model toolchain support; Rust ships with first-party convenience bindings via `wit-bindgen`.

### 9.2 What plugins can do

**Buffer operations:**
- Subscribe to events
- Read buffer content via resource handles (no copies; range-based slicing)
- Submit edits
- Persist plugin-local state

**Mode contributions:**
- Register major modes
- Register minor modes
- Activate / deactivate minor modes per buffer

**Grammar contributions** (first-class):
- Register motions
- Register text objects
- Register operators
- Register ex-commands and ex-ranges

**Command contributions:**
- Register named commands callable from the command line or keymap
- Register keymaps (with conflict detection against the layered keymap stack)
- Invoke any built-in or registered command (composition is plugin-visible)

**UI contributions:**
- Register status segments (mode line, header line)
- Register gutter segments
- Register buffer-backed views (file-tree, outline, diagnostics-list, terminal, REPL, ...)
- Open pickers (with custom content / item / preview providers)
- Show popups anchored to positions
- Post notifications (with optional actions)
- Add inline decorations (squiggles, virtual text, hints)

**External I/O** (capability-gated):
- Spawn subprocesses (`cap-subprocess`)
- Read / write files (`cap-fs-read`, `cap-fs-write`, scoped to a path prefix)
- Make HTTP requests (`cap-net-http`, scoped to host allowlist)

### 9.3 What plugins cannot do

- Render arbitrary GPU content into the editor view (no draw calls; emit data only).
- Block or stall any other plugin or the UI (enforced by the async ABI + fuel + per-task isolation).
- Read or modify another plugin's state.
- Modify built-in keymaps directly (overrides go through the layered keymap stack).
- Access the raw filesystem outside their capability grant.
- Spawn native OS threads (use async tasks via host primitives instead; see §5.5.1).

### 9.4 WIT interface (sketch)

```wit
package lattice:plugin@0.1.0;

interface buffer {
	resource document;

	type document-id = u64;
	record range { start: u32, end: u32 }
	record text-edit { range: range, new-text: string }

	open: func(id: document-id) -> result<document, error>;

	// Methods on the resource read from host memory; no copy of the rope crosses the boundary.
	get-version: func(doc: borrow<document>) -> u64;
	get-line-count: func(doc: borrow<document>) -> u64;
	get-text-range: func(doc: borrow<document>, r: range) -> result<string, error>;
	apply-edits: func(doc: borrow<document>, edits: list<text-edit>) -> result<_, error>;
}

interface grammar {
	// First-class extension points for the vim grammar.
	register-motion: func(spec: motion-spec) -> motion-id;
	register-text-object: func(spec: text-object-spec) -> text-object-id;
	register-operator: func(spec: operator-spec) -> operator-id;
	register-ex-command: func(spec: ex-command-spec) -> ex-command-id;

	invoke: func(inv: command-invocation) -> result<effect, command-error>;
}

interface modes {
	register-major-mode: func(spec: major-mode-spec) -> result<major-mode-id, error>;
	register-minor-mode: func(spec: minor-mode-spec) -> result<minor-mode-id, error>;
	activate-minor-mode: func(doc: document-id, mode: minor-mode-id) -> result<_, error>;
	deactivate-minor-mode: func(doc: document-id, mode: minor-mode-id) -> result<_, error>;
}

interface decorations {
	add-decoration: func(doc: document-id, dec: decoration) -> decoration-id;
	remove-decoration: func(id: decoration-id);
	update-decoration: func(id: decoration-id, dec: decoration);
}

interface ui {
	// Sprites / icons (file-type icons, severity icons, status indicators, ...)
	register-sprite-set: func(set: sprite-set) -> sprite-set-id;
	register-sprite: func(spec: sprite-spec) -> sprite-id;

	// Status / gutter
	register-status-segment: func(spec: status-segment-spec) -> segment-id;
	update-status-segment: func(id: segment-id, content: segment-content);
	register-gutter-segment: func(spec: gutter-segment-spec) -> segment-id;

	// Popups
	show-popup: func(popup: popup-spec) -> popup-id;
	dismiss-popup: func(id: popup-id);

	// Pickers
	open-picker: func(spec: picker-spec) -> result<option<item-id>, error>;

	// Buffer-backed views (replaces v0.3 panels)
	register-buffer-view: func(spec: buffer-view-spec) -> buffer-view-id;
	open-buffer-view: func(view: buffer-view-id, target: pane-target) -> result<document-id, error>;

	// Notifications
	post-notification: func(notification: notification-spec) -> notification-id;
	dismiss-notification: func(id: notification-id);
}

interface host-services {
	// Native host APIs that plugins call into. Heavy work runs native;
	// the plugin orchestrates.
	tree-sitter-query: func(doc: document-id, query: string, range: option<range>)
		-> result<list<node-match>, error>;
	ripgrep-search: func(query: rg-query) -> stream<rg-match>;
	regex-find: func(pattern: string, haystack: string) -> result<list<match-range>, error>;
	http-request: func(req: http-request) -> result<http-response, error>; // capability-gated
	read-file: func(path: string) -> result<list<u8>, error>;             // capability-gated
	spawn: func(cmd: subprocess-spec) -> result<subprocess-handle, error>; // capability-gated
}

interface plugin {
	activate: func() -> result<_, error>;
	on-event: func(e: event) -> result<_, error>;
	deactivate: func() -> result<_, error>;
}
```

### 9.5 Concurrency for plugin authors

Plugins are written **async-first**. All `host-services` calls are async; the runtime takes the WASM stack out of execution at every host call so other tasks (other plugins, the document actor, LSP I/O) keep moving.

Background work is expressed as additional plugin-side tasks composed via the host's async primitives. There is no `std::thread::spawn`; there is `host-services.spawn-task(future)` (or the language-binding equivalent) which schedules onto the host's tokio runtime.

The §5.5.1 concurrency model gives each plugin its own `wasmtime::Store`, which means:
- A plugin's tasks run on whatever tokio worker thread is free; plugin work is genuinely parallel across plugins.
- A plugin invocation that triggers re-entry into another plugin (e.g., A registers a motion that calls B's command) is composed via the host -- there is no recursive re-entry into a single Store.

### 9.6 Performance contract

Per §5.5.2, every WIT host function has a budget enforced in CI. Plugin authors see these as guarantees:

- **Host-call latency is bounded** -- typed call returns in < 500ns p99 with negligible fuel cost.
- **Buffer access is zero-copy at the slice level** -- `get-text-range` returns a string view backed by host memory; the boundary cost is the call, not the data.
- **No per-frame plugin work.** The renderer does not call into plugins on the UI tick. Plugins compute on triggers; the renderer reads cached results.

### 9.7 Reference plugins shipping with v1.0

- **`fuzzy-finder`** -- file / symbol / buffer pickers (validates picker primitive end-to-end).
- **`git-gutter`** (minor mode) -- diff markers in gutter, blame popup.
- **`linter-bridge`** -- adapter for non-LSP linters (eslint, ruff, shellcheck).
- **`markdown-mode`** (major mode) -- markdown editing with live preview minor mode.
- **`rust-mode`**, **`python-mode`**, **`javascript-mode`**, etc. -- bundled major modes.
- **`file-tree`** -- buffer-backed workspace navigation view.
- **`outline`** -- buffer-backed document symbol view.
- **`diagnostics-list`** -- buffer-backed workspace diagnostics view.
- **`tree-sitter-motions`** (post-1.0 candidate; built early to validate the grammar extension API) -- motions and text objects driven by tree-sitter queries (`]f` next function, `iaf` inner argument, etc.).

The reference plugins exercise every primitive: pickers, popups, buffer-backed views, status segments, gutter segments, modes, decorations, notifications, grammar extensions. If any is painful to write, the API needs to grow.

---

## 10. Configuration and Extension Tiers

**Two tiers: TOML and WASM.** TOML covers configuration -- options, keymaps, layouts, theme, default minor-modes per major-mode. WASM (Component Model + WIT) is the single substrate for everything else: extensions, custom motions/operators/text-objects, plugin-provided modes, and live evaluation.

**Live evaluation in lattice means plugin authoring without restart**, not REPL-style sub-keystroke evaluation. A built-in `*scratch:rust*` buffer accepts Rust source; on `:eval` (or whatever the user binds), the host writes the source to a temp directory, invokes the system `rustc --target wasm32-wasip2`, dynamically loads the resulting component, and instantiates it against the same plugin host substrate shipped plugins use. The new commands / motions / decorations / event subscriptions become available immediately. Compile latency is 1-3 s -- explicitly *not* an emacs `M-x ielm` experience. Users wanting a sub-keystroke REPL install a community-shipped plugin that exposes a typed S-expression evaluator over the `CommandRegistry`; it is not a host concern.

**Why no in-process scripting language.** A second runtime (Lua via mlua, embedded Scheme, Rhai) doubles the API surface plugin authors must learn, doubles the binding maintenance, and divides the ecosystem between "plugin-shaped" and "scripting-shaped" extensions that should be the same shape. The Rust-WASM-only choice keeps every extension on one substrate with one set of tooling. The cost is the live-eval-experience tradeoff above; we accept it.

```toml
[editor]
line-numbers = "relative"
soft-wrap = true

[ui]
tab-bar = "auto"               # always | auto | never
mode-line = { segments = ["modal", "file", "git", "lsp", "position", "encoding"] }
header-line = { enabled = true, segments = ["breadcrumbs", "symbol-context"] }
notifications = { corner = "bottom-right", max-visible = 3, default-timeout = "4s" }

# No fixed sidebars or bottom panels. Layouts are buffers in panes;
# the user composes them and may save reusable layouts as named tabs.
[layouts.coding]
description = "File tree on the left, code in the center, diagnostics at the bottom."
splits = [
	{ orientation = "horizontal", ratio = 0.2, buffer-view = "file-tree" },
	{ orientation = "horizontal", ratio = 0.8, content = "active" },
	{ orientation = "vertical", ratio = 0.25, buffer-view = "diagnostics-list", placement = "bottom" },
]

[keys.normal]
"space f f" = "fuzzy-finder.files"
"space p" = "command-palette.open"
"space e" = { command = "ui.open-buffer-view", args = { view = "file-tree", target = "split-left" } }

[major-mode.rust]
language-server = "rust-analyzer"
default-minor-modes = ["git-gutter", "rainbow-delimiters", "auto-pair"]
```

---

## 11. Project Layout (Cargo Workspace)

```
lattice/
|-- Cargo.toml
|-- crates/
|   |-- lattice-core/                  # buffers, documents, undo, dispatcher
|   |-- lattice-grammar/               # vim modal state machine + command API
|   |                                  #   (operators, motions, text objects, registers,
|   |                                  #    ranges, ex-commands, dot-repeat, macros)
|   |-- lattice-syntax/                # tree-sitter integration
|   |-- lattice-lsp/                   # LSP client
|   |-- lattice-plugin-host/           # wasmtime + Component Model + WIT bindings
|   |-- lattice-modes/                 # major / minor mode registry
|   |-- lattice-protocol/              # Command / Event enums; serde + msgpack
|   |-- lattice-config/                # typed-options registry: `OptionType` trait,
|   |                                  #   `Option<T>` (ArcSwap-backed value cell),
|   |                                  #   `ErasedOption`, `ConfigRegistry`,
|   |                                  #   `OptionsGenerator` (gen:options), `:set` parser,
|   |                                  #   renderer-agnostic core options
|   |                                  #   (`register_core_options → CoreOptions`).
|   |                                  #   Phase 7+ adds options.toml + init.rs build/load.
|   |-- lattice-config-api/            # WIT-bindings reexport consumed by user `init.rs`
|   |-- lattice-render/                # Renderer trait, atlas, frame, fonts
|   |-- lattice-render-editor/         # EditorRenderer (all paths)
|   |-- lattice-render-document/       # DocumentRenderer (taffy-based)
|   |-- lattice-ui-gpui/               # compositor, panes, popups, pickers,
|   |                                  #   notifications, status lines
|   |-- lattice-ui-tui/                # terminal UI for bootstrap / headless
|   |-- lattice-headless/              # headless server (remote / SSH)
|   `-- lattice-cli/                   # `lattice` binary
|-- wit/                               # canonical WIT interface definitions
|   |-- buffer.wit
|   |-- grammar.wit
|   |-- modes.wit
|   |-- decorations.wit
|   |-- ui.wit
|   |-- host-services.wit
|   `-- plugin.wit
|-- plugins/                           # first-party plugins (compiled to component-model WASM)
|   |-- fuzzy-finder/
|   |-- git-gutter/
|   |-- linter-bridge/
|   |-- markdown-mode/
|   |-- rust-mode/
|   |-- python-mode/
|   |-- file-tree/
|   |-- outline/
|   |-- diagnostics-list/
|   `-- tree-sitter-motions/           # validates grammar extension API
|-- grammars/                          # tree-sitter grammars
|-- runtime/                           # bundled runtime assets (default keymap, themes)
|-- docs/
`-- tests/
```

---

## 12. Testing Strategy

| Test type | Tool | Coverage |
|---|---|---|
| Unit | `cargo test` | Per-module logic |
| Integration | `cargo test` (workspace) | End-to-end command/event flows |
| Snapshot | `insta` | Editor scenarios; status segment outputs |
| Property | `proptest` | Buffer invariants, layout cache coherence |
| Benchmark | `criterion` | Hot paths in CI |
| Fuzz | `cargo-fuzz` | Edits, command parser, MessagePack |
| LSP integration | Real servers in Docker | rust-analyzer, pyright, gopls |
| Plugin | Mock-core harness | Plugin API correctness |
| Visual regression | Headless GPUI + screenshot diff | Rendering, popups, pickers, panels |
| UI scenario | Synthetic input -> screenshot | Full-stack: keystroke through to rendered frame |

---

## 13. Roadmap

### Phase 0: Foundation (weeks 1-2)
Workspace, `lattice-core`, document/buffer/undo, file I/O, protocol enums, snapshot tests. **Exit:** programmatic edit roundtrip.

### Phase 1: Modal Editing (weeks 3-4)
`lattice-grammar` crate. Vim modal state machine (Normal, Insert, Visual, Op-pending, Command, Search, Replace). Strict vim grammar parser: counts, registers, operators, motions, text objects, ex-ranges. Built-in command catalog (Operator / Motion / TextObject / Register / Range / Count types as the public command API). Default vim keymap as a config file. Macros, marks, registers, dot-repeat. Grammar extension hooks (`register_motion` / `register_text_object` / `register_operator`) wired and exercised by an internal test plugin. **Exit:** complex editing through typed command invocations in tests; default keymap fully functional.

### Phase 2: Terminal UI Bootstrap (weeks 5-6)
`lattice-ui-tui` with crossterm/ratatui. Wire input -> core -> render. **Exit:** modally edit text in a terminal.

### Phase 3: Tree-Sitter (weeks 7-8)
`lattice-syntax`, highlighting (10 grammars), structural motions, reparse pipeline. **Exit:** highlighted code, structural motions work.

### Phase 4: LSP (weeks 9-11)
Diagnostics, completion, hover, definition, references; cancellation; version tracking. **Exit:** rust workflow with diagnostics, completion, jump-to-def.

### Phase 5: GPU Rendering Foundation (weeks 12-14)
`lattice-render`, `lattice-render-editor` monospace path, `lattice-ui-gpui` compositor with splits and tabs, theme system. **Exit:** GPU code editor with terminal feature parity, smooth scrolling.

### Phase 6: Document Renderer + UI Components (weeks 15-18)
- `lattice-render-document` (taffy + cosmic-text).
- Popup system (completion, hover, signature help, diagnostic, code action).
- Picker primitive (file picker as first user).
- Status lines (mode line, header line) with segment registry.
- Buffer-backed view scaffolding (file-tree, diagnostics-list as first users).
- Notifications.
- Command line / echo area.

**Exit:** full UX surface in place; everything but the plugin host.

### Phase 7: Plugin Host (weeks 19-22)
`lattice-plugin-host` with wasmtime + Component Model + WIT bindings (`wit-bindgen`). AOT module cache; lazy instantiation; capability manifests; fuel limits; per-call overhead benchmarks gated in CI (typed call < 500ns p99; grammar-extension round-trip < 5μs p99). Async ABI wired through tokio. Reference plugin: `fuzzy-finder` (validates picker primitive end-to-end). **Exit:** a WASM plugin replicates the file picker without host changes; CI enforces overhead budgets.

### Phase 8: Major/Minor Modes + Reference Plugins (weeks 23-25)
`lattice-modes` registry. Built-in major modes ship as components (rust, python, js, go, c, json, yaml, toml, markdown). Reference minor modes (git-gutter, auto-pair, rainbow-delimiters). Buffer-backed views (`file-tree`, `outline`, `diagnostics-list`) ship as components. **Exit:** mode and view systems fully exercised; the everything-is-a-buffer principle validated by the layout-from-config flow.

### Phase 9: Rich Buffer Rendering (weeks 26-28)
Shaped path in `lattice-render-editor`. Per-line layout cache + Fenwick index. `markdown-mode`. Style mappings system. **Exit:** edit markdown with variable headings; latency indistinguishable from code.

### Phase 10: Polish and v1.0 (weeks 29-32)
Live-eval (`*scratch:rust*` -> `rustc` -> dynamic plugin load). Accessibility. Cross-platform packaging. Crash reporter. Documentation. Themes. **Exit:** 1.0 release.

### Post-1.0
Path 4 (inline blocks). `org-mode`. `WebRenderer` (decision time). Remote / SSH. Collaborative editing. Multi-cursor as first-class editing model. Pluggable editing paradigms (e.g., emacs / readline-style alternative). `tree-sitter-motions` plugin promoted to a bundled extension. PTY-backed `terminal` buffer view.

**Total estimate:** ~8 months solo, faster with a small team. Realistic with rework: 11-15 months.

---

## 14. Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| GPUI API churns | Medium | Medium | UI crate isolated; protocol-only dependency; wgpu + parley fallback. |
| Plugin API (WIT) design proves wrong | High | High | Built-in modes / views / grammar extensions all built against the WIT before 1.0; `tree-sitter-motions` plugin built early to validate the grammar extension API. SemVer only after 1.0. WIT changes ARE breaking; design carefully upfront. |
| **WASM host-call overhead exceeds budget** (new in v0.4) | **Medium** | **High** | **Per-call benchmarks gated in CI (typed call < 500ns p99; grammar-extension round-trip < 5μs p99). AOT compilation; module cache; resource handles; native host APIs for hot work. Built-in motions / text objects / operators stay native; WASM is for extensions only.** |
| **Plugin cold-start tax at editor launch** (new in v0.4) | **Medium** | **Medium** | **Lazy instantiation; AOT module cache reused across launches; deferred activation on first invocation. Cold-start budget for 50 plugins: < 30ms total.** |
| Per-line layout cache invalidation bugs | Medium | High | Property tests for cache coherence; content + style hashes. |
| Tree-sitter performance on huge files | Low | High | Per-file thresholds; disable structural features above N MB. |
| LSP server quirks | High | Low | Per-server compatibility shims. |
| Async cancellation correctness | Medium | High | Property tests; `tracing` spans on every async op. |
| **Vim grammar edge cases** (revised in v0.4) | **High** | **Medium** | **Vim semantics are committed -- the grammar is not modified. Specific edge cases (rare register quirks, obscure block-visual behaviors) may be deferred for v1 with explicit tests documenting the gaps. The default keymap is a config file so users can patch differences locally.** |
| **Grammar extension API churn** (new in v0.4) | **High** | **High** | **API is exercised end-to-end in v1 by the `tree-sitter-motions` plugin (built but possibly not shipped) plus internal test plugins. SemVer freeze on the grammar WIT only after the extension API has supported at least three real plugins.** |
| Major mode plugin churn breaks buffers | Medium | High | Version pinning per plugin manifest; WIT protocol version declared per binding. |
| Rich buffer scroll on huge documents | Medium | Medium | Eager layout < 50K lines; lazy with overscan above. |
| Status segment thrash | Medium | Medium | Pull-not-push update model; per-segment update triggers; fail-isolated rendering. |
| Popup z-ordering / dismissal bugs | Medium | Low | Centralized popup manager; declarative dismissal rules; visual regression tests. |
| **Buffer-backed view layout UX** (new in v0.4) | **Medium** | **Low** | **Ship named layouts in default config (e.g., `coding`, `writing`) so the everything-is-a-buffer model has zero-cost defaults. Users keep full freedom to compose their own.** |

---

## 15. Open Questions

1. Persistence of undo trees -- GC story when source file is deleted / renamed.
2. Plugin distribution -- own registry or piggyback on existing infrastructure.
3. Crash recovery for unsaved buffers -- periodic snapshot or append-only edit log.
4. Workspace concept granularity -- per-folder or per-project-root.
5. Remote development story -- designed for from day one or retrofitted in v2.
6. Telemetry -- opt-in, privacy story matters.
7. Plugin versioning and compatibility -- Component Model versioning, our policy.
8. Built-in mode boundary -- bundled vs separate downloads (binary size vs. OOB experience).
9. Live-reload of plugin-defined modes -- without restart?
10. Style mapping override layering -- formal precedence: theme vs. mode vs. user vs. plugin.
11. Picker keymap defaults -- Tab vs. Enter vs. Ctrl-N for next item; align with which existing tool.
12. ~~Multi-window state synchronization -- when the same document is open in two windows, how do scroll / selection / focus events propagate.~~ Resolved in §5.6.8: selections are transformed against `AppliedEdit` and published as part of the next `DocumentSnapshot`; per-pane scroll / focus is pane-local state.
13. Notification persistence -- should errors persist across sessions until acknowledged.
14. **Default layouts** (new in v0.4) -- which named layouts ship in default config so the everything-is-a-buffer model has good zero-config defaults.
15. **Grammar extension API surface for tree-sitter motions** (new in v0.4) -- exact shape of the host's tree-sitter query API exposed to plugins (one-shot vs. cursor-based query iterator; range scoping; query caching).
16. **Plugin async-task host primitive** (new in v0.4) -- name and shape of the host function that lets a plugin schedule background async work (`spawn-task` vs. `with-cancellation` vs. structured-concurrency primitive).
17. **WASM AOT cache invalidation** (new in v0.4) -- when do we invalidate cached compiled modules (wasmtime version, target triple, plugin checksum, all three).
18. **Folds** (deferred) -- vim has manual / indent / syntax / expr folds; tree-sitter gives us syntax folds nearly free. Open: storage (rope-side metadata vs. computed view), interaction with motions (`zj`/`zk`/`[z`/`]z`) and operators that target folded ranges, persistence across sessions.
19. **Replace mode (`R`) dispatch** (deferred) -- overstrike is a third edit mode beside Normal/Insert. Open: whether to model it as a flag on Insert or as its own modal state in the state machine, and how dot-repeat records overstrike spans.
20. ~~**Live evaluation / REPL parity** -- emacs's `M-x ielm`, `eval-last-sexp`, scratch buffer.~~ Resolved per §10 / §2.2: live evaluation in lattice means *plugin authoring without restart* via `*scratch:rust*` -> `rustc` -> dynamic plugin load, sharing the WASM plugin host substrate. In-process REPL with sub-keystroke evaluation is an explicit non-goal; users wanting it install a community-shipped plugin.
21. **File watcher / auto-revert** (deferred) -- emacs's `auto-revert-mode` and external-change detection. Open: notify-based watcher per workspace, mtime poll fallback, conflict resolution UI when external + local edits diverge.
22. **Bookmarks and cross-file marks** (deferred) -- vim's `'A`-`'Z` global marks and emacs's bookmark facility cover overlapping ground. Position history (§5.1.1) handles in-process navigation; bookmarks need persistence, naming, and a picker.
23. **Function rebinding / advice** (deferred) -- emacs's `defadvice` / `advice-add`. The dispatcher already mediates every command, so wrapping is a registry-side concern. Open: advice ordering, removal semantics, interaction with WASM-defined commands, fuel accounting for advice chains.
24. **Narrow-to-region** (deferred) -- emacs's `narrow-to-region` confines all operations to a sub-range. Open: model as a buffer-local view (cheap; commands see the narrowed range as `Whole`) vs. a transient overlay; interaction with multi-cursor and Visual.
25. **Snippets and abbrev** (deferred) -- a built-in snippet engine vs. plugin-only. If built-in, integration with completion popups, with the rich minibuffer's parameter hints, and with LSP `textDocument/completion` snippet results.
26. **Frames (multi-OS-window)** (deferred) -- emacs's frame concept. Decoupling is clean (each frame is a top-level window with its own pane tree); open question is workspace boundaries (one workspace per frame vs. shared) and how the position-history ring partitions across frames.
27. **Session save / restore** (deferred) -- emacs `desktop.el`. What state is captured (open buffers, panes, layouts, registers, marks, position history, command history, ex-history, search history) and what is per-workspace vs. per-user-global.
28. **DAP support** (deferred to post-1.0 plugin) -- LSP is in-host (§5.4) for latency reasons; DAP is similar shape. Open: in-host like LSP, or first reference plugin that exercises the WIT async/event surfaces under a real adversarial workload.
29. **AI / completion-as-you-type integration** (deferred to post-1.0 plugin) -- Copilot / Codeium / Claude / local-LLM. The everything-is-a-buffer model gives chat / inline-suggestion surfaces for free; the question is whether the plugin host's WIT API has the right shape (streaming completions, ghost-text rendering hook, accept/reject arbitration with vim modal state).
30. **Magit-class VCS integration** (deferred to post-1.0 plugin) -- a reference plugin will land in v1.0 (basic git status / diff / blame); the open question is whether the buffer protocol exposes enough for a plugin to reach Magit-equivalent fidelity (interactive rebase, hunk staging, log graph), or if specific hooks need adding.
31. **EditorConfig / project-build awareness** (deferred) -- whether the host reads `.editorconfig` natively or via a reference plugin, and how project-detected build commands (cargo, npm, etc.) integrate with `:!` and the compilation-buffer (§B.4).

---

## 16. Glossary

- **Buffer**: rope-backed in-memory text.
- **Document**: buffer + metadata.
- **Selection / Selection set**: (anchor, head) pairs; multi-cursor default.
- **Edit**: a primitive change at a range.
- **Command / Event**: protocol messages between layers.
- **Modal mode**: input interpretation context (Normal, Insert, Visual).
- **Major mode**: a buffer's primary content-type identity. Exactly one per buffer.
- **Minor mode**: composable feature toggle. Zero or more per buffer.
- **Rendering profile**: which EditorRenderer fast path a buffer uses.
- **Style mapping**: major mode's table from syntax-tree node types to text styles.
- **Layout cache**: per-line cache of shaped glyphs and metrics.
- **Damage region**: range that has changed and needs re-rendering.
- **Decoration**: inline annotation (squiggle, virtual text, gutter marker).
- **Operator / Motion / Text object**: components of the modal grammar.
- **Plugin**: a WASM component extending the editor.
- **Capability**: explicit permission grant for a plugin.
- **Fuel**: wasmtime's CPU budget mechanism.
- **WIT**: WebAssembly Interface Types.
- **Renderer**: trait implementation drawing a specific kind of content.
- **Compositor**: arranges panes and composes renderer output into one GPU frame.
- **Pane**: a region of a window holding one document's view.
- **Pane tree**: recursive split structure of a tab's panes.
- **Tab**: per-window grouping holding a pane tree.
- **Status segment**: contributed unit of mode line / header line content.
- **Gutter segment**: contributed unit of gutter content (a column).
- **Popup**: floating overlay anchored to a position.
- **Picker**: fuzzy-search overlay for selecting from a list (file / symbol / command / etc.).
- **Buffer-backed view**: a non-file buffer (file tree, outline, diagnostics, terminal, ...) placed in a pane like any other buffer.
- **Notification**: transient corner-anchored message.
- **Command API**: typed, scriptable surface of every editor primitive -- operators, motions, text objects, registers, ranges, counts. Keymaps are bindings from chord sequences to command invocations.
- **CommandInvocation**: the unified call type carrying `(command, count, register, range, args)` through the single dispatcher. Vim's ex-syntax and plugin function calls both produce these.
- **Grammar extension**: a registered motion, text object, operator, or ex-command that participates in the vim grammar exactly like a built-in.
- **Position history**: a per-buffer-and-global ring tracking cursor positions, with each entry tagged by source (`AutoJump`, `ExplicitMark`, `PluginPush`, `NamedMark`). Unifies vim's jump list with emacs's mark ring.
- **Event / Hook**: a typed editor state-transition with a payload. Subscriptions are typed; vim's `:autocmd` and emacs's hooks both desugar to subscriptions on the unified event bus.
- **Minibuffer**: a transient editing buffer for `:` commands, `/` searches, and any interactive prompt. Has a major mode (`command-line`, `search-line`, ...), supports the full vim grammar, tree-sitter highlighting, decorations, and popups.
- **Echo area**: single-line surface for transient core/command messages; sharing screen real estate with the minibuffer; rolling history kept in `*messages*`.
- **Option / Customize**: every option is a typed registered value with metadata; the customize buffer is a type-aware editing view that writes back to user TOML.
- **Component Model**: WebAssembly's typed-interface, multi-language plugin model; the runtime substrate for all plugins.
- **WIT (WebAssembly Interface Types)**: the canonical interface description language for the plugin API.
- **Resource handle**: an opaque WIT type that lets a plugin reference host-owned data (a buffer, a document) without copying it across the boundary.
- **Sprite / Sprite atlas**: small line-height-sized graphical element (SVG or PNG) referenced by id. Used for file-type icons, severity icons, status indicators, etc. Atlas-backed; shares the GPU pipeline with glyphs but lives in a separate texture. Distinct from Path 4 inline media blocks.

---

## Appendix A: Performance Comparison with Neovim and Emacs

This appendix establishes a baseline for "what good looks like" so we can evaluate our progress during implementation. Comparisons here are honest assessments, not aspirational marketing.

### A.1 Methodology

Numbers below are from a mix of published benchmarks, our own measurements where available, and reasonable estimates from architectural analysis. They assume:

- A mid-range modern laptop (Apple M-series or recent x86 mobile, integrated GPU).
- A 60-120Hz display.
- Editing typical source code files (1K-10K lines).
- LSP enabled with a representative server (rust-analyzer, pyright, etc.).
- Default-or-minimal user configuration (i.e., not heavily-customized Doom Emacs).

The metric "keystroke-to-glyph" measures the time from when the OS delivers a key event to when the corresponding glyph is on screen, including all intermediate work (buffer mutation, parse, render, present).

### A.2 Headline comparison

| Dimension | Lattice (target) | Neovim | Emacs (vanilla) | Emacs (Doom/Spacemacs) |
|---|---|---|---|---|
| Cold startup | 150-400ms | 30-80ms | 200-500ms | 500-2000ms |
| Daemon/instant launch | Possible post-1.0 | N/A | `emacs --daemon` | `emacs --daemon` |
| Steady-state typing (code) | <2ms | 2-4ms (terminal RTT) | 5-10ms | 10-25ms |
| Steady-state typing (markdown rich) | <5ms | N/A (grid only) | 10-20ms (org) | 15-40ms (org) |
| Open 100MB log file | <500ms | <500ms | Slow / requires `so-long-mode` | Slow |
| Tree-sitter incremental reparse | <1ms | ~1ms | ~1ms (treesit native) | ~1ms |
| LSP completion latency (popup) | <10ms after server | ~10ms after server | 20-50ms after server | 30-80ms after server |
| Plugin overhead in heavy config | Architecturally bounded | Variable; can block | Cumulative; serialized | Cumulative; can be severe |
| Worst-case input lag under plugin load | UI unaffected | Plugin can block briefly | Plugin can block significantly | Plugin can block significantly |
| Memory baseline | 150-250MB | 30-60MB | 60-100MB | 200-500MB |

### A.3 Dimension-by-dimension assessment

**Cold startup.** Neovim wins clearly -- terminal-only, minimal init. We will not match it because we initialize a GPU surface, font system, and atlas at startup. We will be in line with vanilla Emacs and significantly faster than configured Emacs distributions. Daemon mode (post-1.0) makes subsequent launches near-instant, eliminating this penalty for the actual day-to-day workflow.

**Steady-state typing (code).** This is the most important metric and we should be at parity with or slightly faster than Neovim. Our pipeline (buffer mutation -> tree-sitter incremental edit -> atomic tree swap -> next-vsync render) is microseconds of Rust on the input path; the display work is GPU-accelerated with monospace fast path. Neovim's terminal-based pipeline adds a small PTY round-trip; ours doesn't. Realistic targets: ours <2ms, Neovim 2-4ms, both well below human perception. Against Emacs we should be 3-5x faster because Emacs's display engine carries machinery (variable fonts, mixed content) we don't pay for in code buffers.

**Steady-state typing (rich content / markdown / org).** Neovim doesn't natively render variable fonts -- its rich-document plugins (`render-markdown.nvim`) work through grid-cell tricks rather than true variable-font rendering. So this is apples-to-oranges. The fair comparison is Emacs `org-mode`, where we should be 2-3x faster because shaping work runs on rayon workers off the input thread. Emacs's redisplay is single-threaded; shaping cost goes directly to perceived latency.

**Large file handling.** Neovim and Lattice both use rope-like structures and handle 100MB+ files comfortably. Emacs uses a gap buffer that performs poorly on large files; mitigated by `so-long-mode` but not gracefully.

**Project-wide search.** All three shell out to ripgrep. This is a wash -- performance is determined by ripgrep, not by the editor.

**LSP responsiveness.** Neovim's built-in LSP client is fully async and well-implemented; we should be at parity in terms of raw responsiveness. Architecturally we have a subtle advantage: third-party plugins in our system *cannot* accidentally make LSP synchronous (the WASM async boundary prevents it), while in Neovim a poorly-written Lua plugin could. Against Emacs `lsp-mode` / `eglot`, we should be substantially faster -- both run on the main elisp thread, and heavy LSP traffic causes visible stutters in Emacs.

**Plugin overhead.** This is where the architectural difference is most pronounced. In Neovim, Lua plugins run on the main thread; well-written plugins are async, but synchronous plugins (still common) directly add latency. In Emacs, every elisp hook on every event runs serialized on the main thread; heavy configurations accumulate measurable lag. In Lattice, plugins are WASM actors with fuel limits and async host calls -- they cannot block the UI by construction. A plugin can be slow, but its slowness is bounded to its own computation; the editor keeps rendering.

**Memory.** Neovim is the lightest. We're heavier because of GPU resources, atlas memory, layout caches, and the WASM runtime. Vanilla Emacs is comparable to us; configured Emacs distributions exceed us.

### A.4 What we won't beat

Honesty matters here:

- **Neovim's startup time.** Inherent cost of being a GUI app.
- **Neovim's memory footprint.** GPU and runtime overhead.
- **Neovim's binary size.** Our binary will be 20-80MB (Rust + GPU dependencies + bundled plugins); Neovim is 5-15MB.
- **Decades of edge-case shaking.** Both Neovim and Emacs have had thousands of users for thousands of corner cases. Our v1.0 will have rougher edges.

These aren't dealbreakers but they're real. Users for whom startup time and memory dominate may prefer Neovim. Users for whom decades of plugin ecosystem dominate may prefer Emacs. Our pitch is for users who want **modern UX, predictable latency under load, and safe extensibility** -- and are willing to trade 100MB of RAM and 200ms of startup for it.

### A.5 What we should beat decisively

- **Worst-case latency under heavy plugin load.** This is the headline. In Emacs, twenty active plugins each adding a few milliseconds to a hook pile up into perceptible lag. In Neovim, a single ill-behaved plugin can stall the whole editor. Lattice's architecture makes plugin work parallel and bounded -- we should *never* exhibit the kind of "Emacs is slow today" experience that comes from accumulated hook overhead.
- **Rich-buffer editing latency.** With shaping on workers, we deliver code-like latency for markdown/org-style content. Emacs cannot match this on its current architecture.
- **Large-file responsiveness.** Rope + tree-sitter + worker-based shaping handles huge files without the Emacs-style "this file is too big" failure mode.
- **Predictability.** This is qualitative but real. With the actor model and explicit fast paths, latency variance should be tight. Emacs and Neovim both have "good days and bad days" depending on what plugins triggered when. We should be more consistent.

### A.6 Risks to performance posture

These are the things that could erode our position if we're not vigilant:

1. **Allocation discipline on the input path.** Rust makes allocation easy. The hot path must be allocation-free -- verified by `criterion` benchmarks and `tracing` spans gated in CI.
2. **GPU driver variance.** Frame times that are tight on Apple Metal might be loose on Linux Mesa or Windows DirectX. Cross-platform benchmark coverage is required.
3. **Tree-sitter pathological grammars.** TypeScript with heavy generics, Rust with macro chains, can spike parse times. Inherit lessons from Helix/Neovim; benchmark our top 20 grammars.
4. **WASM cold-start overhead.** Wasmtime instantiation is a few milliseconds per plugin. Loading 50 plugins serially at startup adds up. Mitigations: lazy plugin loading, AOT-cache modules, parallel instantiation.
5. **Atlas thrashing.** If a buffer mixes many fonts/sizes, atlas eviction can cause slow first-paint of unfamiliar glyphs. Mitigation: pre-warm atlas on file open with the file's glyph census.
6. **UI furniture creep.** Each new panel, status segment, and notification adds work to every frame. Even though it's cached, "free" repaints are not actually free at scale. Discipline: budgets per frame for UI work, profiling in CI.

### A.7 The headline assertion

**Better than Emacs across nearly every dimension. At parity with Neovim on the things that matter for daily editing. Worse than Neovim on startup time and memory.**

If we deliver against the Section 8.2 commitments and avoid the Section A.6 risks, this is achievable. If we miss, we degrade gracefully toward "comparable to Emacs," which is still acceptable. The unacceptable outcome -- slower than Emacs in any common workflow -- is precluded by architecture, not by optimization effort. That's the point.

---

## Appendix B: Vim / Emacs Unifications (smaller wins)

This appendix collects the smaller-scale unifications that fall out naturally from the v0.4 architecture. Each is a clarification or convenience -- none introduces new primitives, all reuse the existing command registry, event bus, buffer model, and minibuffer.

### B.1 Interactive arg specs (emacs `(interactive ...)` done right)

Each `CommandSpec` carries an `args_schema: Vec<ArgMetadata>` (§5.11). The schema declares per-argument:

- name, type, doc
- prompt text (used when invoked interactively without a value)
- completion source (used by the minibuffer completion popup)
- validator (used for live error indicators in the minibuffer)
- default value or "use selection" / "use cursor word" / "use last response"

Three entry paths consume the same schema:

1. **`:` line.** The `command-line` parser fills positional args from the input string; missing args trigger a follow-up prompt in a `prompt` minibuffer with the schema-supplied prompt text and completion.
2. **Keymap binding.** A binding may pre-supply some args (`"<leader>fr"` -> `format(scope=region)`) while leaving others to prompt. The unsupplied args trigger sequential prompts.
3. **Command palette.** Selection of a command opens a guided form built from the schema -- one prompt per missing arg, in declaration order, with completion and validation throughout.

Result: one declaration covers `:` invocation, keymap-with-prompt, and palette-with-form. There is no second mechanism.

**Per-kind input modes.** `ArgKind` is more than a type tag -- it picks the cmdline's input mode while the cursor sits in that arg's slot. v1 implements `Chord`: when the active arg has `kind == Chord` (e.g. `:describe-key`'s sole arg, the future `:map <lhs> <rhs>`), the cmdline switches into chord-capture: every key event renders to its canonical chord token (`<C-c>`, `<Up>`, `gg`) and gets appended; `<BS>` deletes one full token (not a byte); `<Esc>` cancels, `<CR>` submits. The `Ctrl-c -> Quit` global hatch is intentionally suppressed inside chord-capture so `:describe-key <C-c>` is reachable; `<Esc>` remains the abort.

When a `Chord`-required arg is submitted empty (`:describe-key<CR>`), instead of erroring the cmdline pre-fills with the command word + space, arms a one-shot auto-submit, and shows the schema-supplied `prompt` as a status hint. The very next chord captured fires the lookup -- no second `<CR>` needed. This is the v1 implementation of the missing-arg prompt path; richer prompt minibuffers (multi-arg, `String` / `Pattern` / completion-driven) layer onto the same arming mechanism.

### B.2 `:g` and `:v` are normal commands

```vim
:g/TODO/d        " delete every line matching TODO
:v/^[^#]/d       " delete every line not starting with #
```

Both are registered ex-commands taking `(pattern: Regex, body: CommandInvocation)`. The `:g` parser produces a `CommandInvocation` for `global` whose `body` arg is itself a parsed `CommandInvocation` for `delete` with `range = Some(CurrentLine)`. The dispatcher iterates matching lines and invokes `body` for each. Same registry, no special form.

The same pattern lets `:windo`, `:bufdo`, `:tabdo`, `:argdo` all be normal commands taking a `body` invocation arg.

### B.3 Histories as pickers over registries

Command history, search history, register history, position history (§5.1.1) all live in registries. A built-in picker over each gives:

- `:history-command` -> picker over recent `:` invocations
- `:history-search` -> picker over recent search queries
- `:history-register <reg>` -> picker over content stored in a numbered register
- `:history-position` -> picker over the position history with source filtering

Plugins can register additional histories the same way.

### B.4 Scratch, messages, compilation, REPL -- all buffers

```text
*scratch*           a persistent editable buffer for ad-hoc text / experiments
*messages*          rolling log of echo-area messages and notifications
*compilation*       output of `:compile` and external builds, errors made jumpable
*shell:zsh*         a PTY-backed terminal buffer (post-1.0)
*repl:python*       a comint-style REPL for the python plugin
```

Each is a regular buffer with a major mode tailored to its purpose; users open them in panes like any other buffer. The buffer-naming convention `*name*` is itself convention, not a special path -- the buffer just doesn't have a file backing.

### B.5 `:redir` as an effect-capture wrapper

```vim
:redir > out.txt
:set
:redir END
```

becomes a single composed call: `capture-output(target=Path("out.txt"), invocations=[set])`. The `capture` command takes an output `target` (file path, register, scratch buffer) and a list of invocations to run; it tees `Effect::Output` from each invocation into the target. No special-form parser; composition is data.

### B.6 `:!cmd` and `:read !cmd` -- subprocess effects through the dispatcher

`:!ls -la` becomes `shell-execute(cmd="ls -la")` returning an `Effect::Output`. `:read !ls -la` is `shell-execute` composed with `insert-output-at(position=CurrentLine.below)`. Subprocess capability is gated; the parser front-end produces typed invocations regardless.

### B.7 The status line / mode line sees minor modes the emacs way

Emacs's mode line shows the active minor modes as a flat list; vim's shows none of this by default. The default `minor-modes` status segment shows them as a small comma-separated list with a configurable filter (hide always-on minor modes; show only the unusual ones). Click / cursor-move on a minor-mode label opens its `:describe-mode` view.

### B.8 Idle hooks

Many emacs workflows depend on `run-with-idle-timer`. We promote idle to a first-class event: `Idle { duration }` fires when the user has been quiescent for the declared duration. Subscriptions filter by `duration` threshold. Plugins use this for low-priority work (refresh git status, prefetch LSP hovers, etc.) without polling.

### B.9 The `*messages*` buffer is also where notifications live

Notifications (§5.9.9) are transient corner-anchored UI but they are *also* logged to `*messages*` so the user can scroll back. Errors that lack a corresponding notification still surface there. Closing the user-facing notification doesn't lose the record.

### B.10 Buffer-local commands

A buffer can have commands registered against it specifically (the buffer-local keymap, its major mode, an active minor mode). `:describe-buffer` enumerates which commands are reachable in the current context, with their resolved key bindings. Both vim's `:map` and emacs's `C-h b` collapse into this.

---

*End of design document v0.4.*
