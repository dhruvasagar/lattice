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

---

## 4. Technology Stack

| Concern | Choice | Rationale |
|---|---|---|
| Language | **Rust (stable)** | Memory and data-race safety; mature async; strong editor ecosystem. |
| Async runtime | **tokio** (multi-thread) | Default; integrates everywhere. |
| Buffer | **`ropey`** | Battle-tested rope; O(log n) edits; cheap clones. |
| Parser | **`tree-sitter`** | Incremental, error-recovering, ubiquitous. |
| LSP types | **`lsp-types`** | Generated bindings; we write our own client. |
| GPU rendering | **GPUI** (preferred) or **`wgpu`** (fallback) | GPUI purpose-built; wgpu the fallback. |
| Layout (UI furniture) | **`taffy`** | Standalone flexbox/block layout. |
| Text shaping | **`cosmic-text`** or **`parley`** | Full Unicode when needed; bypassed on monospace fast path. |
| Plugin runtime | **`wasmtime`** + Component Model + WASI | Sandboxing, fuel limits, async host. |
| Serialization | **`serde`** + MessagePack (`rmp-serde`); WIT for plugin interfaces | Zero-cost in-process; Component Model for plugins. |
| Config | **TOML** + **Lua** (via `mlua`) for tier-2 | TOML 90% case; Lua for power users. |
| CLI | **`clap`** | Standard. |
| Logging | **`tracing`** + `tracing-subscriber` | Structured logs, span timing. |
| Build | **`cargo`** workspace | Crate boundaries enforce architecture. |
| Testing | **`cargo test`**, `insta`, `criterion` | Snapshot + benchmarks. |

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

**Macros, marks, registers, and dot-repeat** are mechanical because every change flows through `execute(invocation)`. The `last change` for `.` is the most recent recorded `CommandInvocation`. **Macros record `CommandInvocation` sequences -- not raw keystrokes.** Replay survives keymap changes, plays back faster (no parse pass), and is editable as data: opening the macro register in a buffer-backed view (`*macro:q*`) yields a one-invocation-per-line buffer the user can hand-edit and re-store.

**Visual mode IS the active region.** When Visual is active, the current selection is automatically supplied as the `range` argument to any range-accepting command. Vim users see "operate on visual selection"; users coming from emacs see "operate on region." Both reduce to: the dispatcher receives `range = Some(Range::Selection)` when no explicit range is given and Visual is active. This is the `Range::Selection` variant added to the range type for exactly this purpose.

**Multi-cursor (post-1.0).** The selection set already permits it. Adding multi-cursor later requires per-feature semantic spec (which operators broadcast, how registers behave, how dot-repeat interacts) but no fundamental rework of the grammar, the dispatcher, or the command API.

### 5.3 Syntax: Tree-Sitter Integration

Tree-sitter is responsible for **all** structural code understanding.

**Update flow:** edit -> `tree.edit()` (sync, microseconds) -> reparse on `spawn_blocking` worker -> atomic tree swap -> renderer queries new tree on next frame. Renderer never blocks waiting for fresh tree; one-frame-stale highlights are acceptable.

**Highlight queries** evaluated lazily on visible viewport + overscan, on rayon pool, cached per (tree-version, line-range).

**Structural motions:** `]f`/`[f`, `]c`/`[c`, `af`/`if`, `ae`/`ie`. `locals.scm` for scope-aware rename.

**Injections** are first-class: markdown code blocks, JSX in JS, regexes in strings.

### 5.4 LSP Subsystem

We write our own client. `tower-lsp` is for servers.

**Per-language-per-workspace client.** Per-buffer version tracking, automatic cancellation on stale requests, debouncing of `didChange`, backpressure on slow servers, transparent crash recovery.

**Features in roadmap order:** diagnostics, completion + resolve, hover, definition/references, rename, code actions, formatting, workspace symbols, semantic tokens, inlay hints.

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

Major and minor modes are implemented as WASM plugins (or tier-2 Lua scripts). No privileged built-in path. Built-in modes ship as bundled plugins.

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

- **Document lifecycle:** `DocumentOpened`, `BeforeSave`, `AfterSave`, `BeforeClose`, `DocumentClosed`, `BufferChanged`, `LanguageDetected`.
- **Modal state:** `ModalModeChanged { from, to }`, `OperatorPendingEntered`, `OperatorPendingResolved`.
- **Mode lifecycle:** `MajorModeActivated`, `MajorModeDeactivated`, `MinorModeActivated`, `MinorModeDeactivated`.
- **Selection / cursor:** `SelectionsChanged`, `CursorMoved`, `JumpPushed { source }`.
- **LSP:** `LspServerStarted`, `LspResponseReceived`, `DiagnosticsUpdated`, `CompletionAvailable`.
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

### 5.12 Configuration System (typed options + customize)

Vim's `:set option=value` is a string-bag with no typing or validation. Emacs's `customize` is a typed system bridged awkwardly to `setq` for non-curated variables. We unify into one typed option registry.

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

#### 5.12.1 Sources and precedence

Values come from layered sources, resolved in this order (later wins):

1. Built-in defaults (`OptionSpec::default`).
2. Bundled config files (default keymap, theme).
3. User config (`~/.config/lattice/config.toml`).
4. Project config (`.lattice/config.toml` at workspace root).
5. Per-buffer overrides (modeline-style `:setlocal`).
6. Programmatic / `:set` invocations during a session.

#### 5.12.2 The `:set` parser front-end

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

#### 5.12.3 Customize as a buffer-backed view

A built-in command (`:customize`, or the `customize` major mode) opens a buffer that lists every registered option grouped by `group`. The view is rendered with type-aware widgets: bools are checkboxes, enums are dropdowns, paths are completable strings, lists are addable-removable. Edits write back to the user TOML file; in-session value changes happen immediately. Filtering (`/`) and folding (`za`) work because it is a buffer.

#### 5.12.4 Options are addressable from every entry point

- The `:set` line.
- `:describe-option <name>` (introspection).
- The customize buffer.
- A plugin's WIT call (`config.set` / `config.get`).
- Programmatic Lua / TOML bindings.

All four entry points produce or consume the same typed `Value` against the same `OptionSpec`.

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

| Operation | Target (p99) | Path |
|---|---|---|
| Keystroke to buffer mutation | <100us | All |
| Keystroke to glyph (code) | <2ms | Monospace |
| Keystroke to glyph (code w/ LSP) | <3ms | Monospace + decorations |
| Keystroke to glyph (markdown) | <5ms | Shaped |
| Frame render (code, 1080p) | <2ms | Monospace |
| Frame render (markdown, 1080p) | <5ms | Shaped |
| Open 100MB log (first paint) | <100ms | Monospace |
| Open 100MB log (full ready) | <500ms | Monospace |
| Open 10K-line markdown (full ready) | <500ms | Shaped (eager layout) |
| Tree-sitter incremental reparse | <1ms | (50K-line file) |
| Completion popup (first paint) | <10ms after LSP | Document |
| Hover popup (first paint) | <15ms after LSP | Document |
| Picker (first match shown) | <50ms | Document |
| Status segment update | <500us | Document |

CI fails on >10% regression vs. main on any benchmark.

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

Three tiers: TOML, Lua, WASM. (Unchanged from v0.2.) Same logical extension expressible at any tier.

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
|   |-- lattice-script/                # Lua tier-2 config bridge
|   |-- lattice-protocol/              # Command / Event enums; serde + msgpack
|   |-- lattice-config/                # TOML config parsing
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
Lua tier-2. Accessibility. Cross-platform packaging. Crash reporter. Documentation. Themes. **Exit:** 1.0 release.

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
12. Multi-window state synchronization -- when the same document is open in two windows, how do scroll / selection / focus events propagate.
13. Notification persistence -- should errors persist across sessions until acknowledged.
14. **Default layouts** (new in v0.4) -- which named layouts ship in default config so the everything-is-a-buffer model has good zero-config defaults.
15. **Grammar extension API surface for tree-sitter motions** (new in v0.4) -- exact shape of the host's tree-sitter query API exposed to plugins (one-shot vs. cursor-based query iterator; range scoping; query caching).
16. **Plugin async-task host primitive** (new in v0.4) -- name and shape of the host function that lets a plugin schedule background async work (`spawn-task` vs. `with-cancellation` vs. structured-concurrency primitive).
17. **WASM AOT cache invalidation** (new in v0.4) -- when do we invalidate cached compiled modules (wasmtime version, target triple, plugin checksum, all three).
18. **Folds** (deferred) -- vim has manual / indent / syntax / expr folds; tree-sitter gives us syntax folds nearly free. Open: storage (rope-side metadata vs. computed view), interaction with motions (`zj`/`zk`/`[z`/`]z`) and operators that target folded ranges, persistence across sessions.
19. **Replace mode (`R`) dispatch** (deferred) -- overstrike is a third edit mode beside Normal/Insert. Open: whether to model it as a flag on Insert or as its own modal state in the state machine, and how dot-repeat records overstrike spans.
20. **Live evaluation / REPL parity** (deferred) -- emacs's `M-x ielm`, `eval-last-sexp`, scratch buffer. Open: do we expose a host-side scripting REPL (Lua via mlua), a per-plugin WASM eval surface, both, or neither for v1.
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
