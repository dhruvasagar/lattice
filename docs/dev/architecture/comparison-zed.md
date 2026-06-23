# Lattice vs. Zed — architectural comparison

> **What this is.** A design-doc fragment positioning lattice against its
> closest *architectural* peer, **Zed**, and explaining why **Helix** is a
> contrast rather than a peer. Feature benchmarks (Vim/Neovim, Emacs, VS Code)
> live in the README's "Why another editor?"; this file is about *structure* —
> the fundamental design choices and what they buy or cost. Honest about
> trade-offs by intent; this is not marketing.

## 1. Why Zed is the peer (and Helix is not)

Zed shares lattice's substrate lineage: Rust, Tree-sitter, a rope-backed text
model, GPU rendering via **GPUI** (the framework Zed authored), and the
**multibuffer** concept (excerpts from many buffers in one view) that lattice
borrows directly. Those are deep, architecture-shaping commonalities — so the
*remaining* differences are the interesting ones.

**Helix** shares only Rust, Tree-sitter, and `ropey` — substrate *libraries*,
not architecture. On everything that shapes the design the two are opposite:

| | Lattice | Helix |
|---|---|---|
| Modal model | vim verb→object grammar | Kakoune-lineage selection→action |
| Config | code-first (Rust/WASM `init`) | TOML-only (Scheme planned) |
| Plugins | plugin-first (WASM, day one) | none yet |
| Concurrency | multi-threaded actor | single event loop (+ tokio for LSP) |
| Rendering | GPU + TUI as a *peer* | TUI only |

So Helix is a useful *contrast* — what lattice deliberately is **not** — but not
an architectural peer. The rest of this document is lattice vs. Zed.

## 2. The axis-by-axis difference

| Axis | Lattice | Zed |
|---|---|---|
| **Editor ↔ UI** | Editor runs on its own dedicated thread; the renderer is a pure consumer of a published per-pane `DisplayMatrix`. `&mut Editor` escaping the actor is **compile-time impossible** (`EditorActorHandle`). | Editor state lives in GPUI's **entity graph on the main thread**; heavy work is offloaded to background executors. |
| **Renderer** | `Renderer` trait — **TUI is a first-class peer** (headless / SSH) beside the GPUI peer; both consume the *same* `DisplayMatrix`. | GPU-only; no headless / TUI path. |
| **Editing model** | Strict **vim grammar IS the public command API**; motions / operators / text objects are extensible from WASM. | Not modal by default (Vim is a context-predicated addon crate); core editing is fixed Rust. |
| **Buffers** | **Everything is a buffer** — file tree, diagnostics, terminal, REPL — atop multibuffer. | Multibuffer (excerpts), but file-tree / terminal are **bespoke panels**. |
| **Extensibility** | WASM Component Model — any language, capability-gated, fuel-limited, crash-isolated. | WASM extensions (languages / themes / slash-commands); core editing is not extensible at the grammar level. |
| **Collaboration** | None (single-user, deliberate scope). | **CRDT real-time multiplayer** — the rope is a CRDT; its headline feature. |
| **Latency discipline** | keystroke→glyph **ratcheted in CI**; UI thread does **zero** I/O / parse / shape. | GPU-smooth + SumTree for huge files; no public per-keystroke CI gate. |

## 3. The spine — mode architecture

The single deepest difference: **what each editor puts at the center.**

**Lattice's spine is the modal grammar + mode system.** Two orthogonal axes:

- **Vim modal state** (Normal / Insert / Visual / Op-pending / Command /
  Search) — a per-buffer state machine.
- **Major mode** (content-type identity — rust, markdown) **×** **minor modes**
  (feature layers — lsp-mode, diff-mode, narrow-mode) — the Emacs axis, kept
  orthogonal to the modal state.

Three load-bearing rules sit on top:

1. **The grammar IS the public API.** Operators / motions / text objects /
   ex-commands all become one `CommandInvocation` through one `execute()`.
   Adding `zn` or `af` is the same act as a plugin contributing one.
2. **Modes own their full surface** — keymaps (at `KeymapLayer::MinorMode` /
   `MajorMode`, never `Builtin`), action-handler bodies, lifecycle
   subscriptions, decorations, completion sources. The host is a thin
   substrate. Acid test: a new provider crate adds **zero** `Editor::` methods
   and **zero** host `Action` variants. (Named as a first-class design
   commitment in DESIGN.md §5.8.2; trait detail in `mode-architecture.md`
   §5.2 / §5.3.)
3. **Everything is a buffer.** The modal grammar therefore applies uniformly to
   the terminal, the file tree, diagnostics, ...

**Zed's spine is the entity graph + GPU view system.** There is no modal state
machine in the core. Input resolves through a **key-context system**
(CSS-selector-like predicates: `Editor`, `Editor && vim_mode == normal`), and
**Vim is a separate crate** that keeps its own modal state and registers
context-predicated bindings. Features are Rust entities (`Model` / `View`)
wired through GPUI's `Context`s; cross-feature data flows through the shared
entity graph.

**In one sentence:** lattice elevates *the mode system itself* to the extension
mechanism; Zed elevates *the entity graph*, and treats modes as one feature
layered over it.

## 4. Impact

- **Architecture.** Lattice's mode-ownership + everything-is-a-buffer yields a
  clean, uniform extension seam — a new mode is a crate contributing
  keymaps/handlers/decorations with no host edits. The cost is **ceremony and
  the risk of half-migrations** (the host must stay thin); that exact class of
  drift recurs and is caught by tests, not the type system. Zed's shared entity
  graph is more *ergonomic* for cross-feature data and is what made CRDT
  collaboration tractable — but features are bespoke Rust, not uniform modes, so
  there is no "everything composes" guarantee.
- **Performance.** Lattice's modal dispatch is a keymap-trie lookup → typed
  `CommandInvocation` → `execute()`, entirely **off** the UI thread; the
  renderer only consumes the published `DisplayMatrix`. Zed resolves contexts →
  actions on the main thread with background offload for heavy work. Both are
  fast; lattice's is *more strictly bounded* — the separation is compile-time
  and the latency is CI-ratcheted, so it cannot regress silently.
- **User experience.** Lattice gives *exact vim everywhere* (incl. terminal and
  file tree), plus SSH/headless and any-language mode plugins. Zed gives
  *polished, bespoke panels* and an excellent Vim mode — but modal is a layer,
  and the panels, while individually more polished, don't compose.

## 5. Honest assessment

**Where lattice's design is genuinely stronger.** The two-axis modal ×
major/minor model with mode-ownership and everything-is-a-buffer is more
principled than either peer: Zed has no modal core; Helix is modal but *flat*
(no major/minor axis, no plugin-driven modes). Lattice is the closest thing to
"Emacs's major/minor model done right, with vim's grammar as the spine." The
proof is that disparate features fall out of the spine rather than being bolted
on: the terminal became a real editor buffer (vim motions / search / marks /
text objects) with almost no bespoke code; operators gained Visual-selection
behavior *by design*; decorations survive focus changes because the renderer is
a pure consumer.

**Where it is riskier / costlier than Zed.**

- **Mode-ownership is mostly enforced *structurally*; a small convention residue
  remains (and it is bounded, not fragile).** An earlier draft called this
  "fragile, enforced by convention + tests, and it drifts." Scrutiny corrected
  that. The load-bearing drift classes are already sealed *by construction*: a
  mode cannot register at the universal keymap layer (the `keymap_entry!` macro
  carries no layer; `PushLayerKind` has no `Builtin` variant; modes hold no raw
  keymap handle), renderer kind-branching is disciplined by an aligned-by-
  fallback rule, and provider-specific behaviour routes through a generic
  `ActionId`. What is left is a *bounded, cosmetic* residue — two first-party
  feature-buffers (oil, file-tree) whose dispatch still lives host-side — plus
  the fact that *core* features and migration *completeness* are checked by
  tests, not types (the latter being a limit of any type system). Removing even
  that residue means inverting those features behind a generic host-API facade —
  which *is* the plugin API, deliberately deferred to the WASM-host phase so it
  is designed against real plugins, not retrofitted from a file browser. So the
  discipline holds largely by construction; the gap is real but small and
  ratcheted, not a fragile system in danger of silent breakage.
- **Unproven at ecosystem scale.** The WASM host exists; the plugin *ecosystem*
  is empty. Zed's value compounds from its extensions, LSP/DAP, and AI;
  lattice's compounding hasn't started.
- **The actor rigidity that buys the latency guarantee** also makes some
  cross-feature flows ceremony that Zed gets for free from the shared graph.

**What is structurally missing vs. Zed (and expensive to add).**

- **Collaboration.** Zed's rope *is* a CRDT; multiplayer is the headline
  feature. Lattice's `ropey` text layer is single-user — retrofitting
  collaboration is a **text-substrate-level change**, not a feature.
- **AI integration & polish/maturity** — Zed is years and a team ahead.

## 6. The one "decide early" item

Almost everything in lattice is **additive** — modes, grammar, renderers,
plugins all layer on without disturbing the core. The **text substrate is the
exception.** If real-time collaboration is ever in scope, the CRDT-vs-rope
decision must be made *now*, while the text layer is still small, not later:
swapping `ropey` for a CRDT rope after a large surface depends on
`DocumentSnapshot` semantics would be a deep, cross-cutting migration. This is
the single architectural choice where lattice's "add it later" default does not
hold.

## 7. Positioning

Lattice and Zed are not really competing for the same user. Lattice is a
*power-user / Emacs-successor* bet: uniformity (everything is a buffer),
extensibility (grammar-as-API + WASM), and latency rigor (compile-time
separation + CI ratchet). Zed is a *modern-IDE / team-collaboration* bet:
polish, CRDT multiplayer, GPU, and AI. Lattice's thesis, condensed: **Zed's
substrate discipline + Neovim's grammar + Emacs's extensibility model, with the
UI-thread guarantee moved into the type system** — at the explicit, current
cost of Zed's collaboration and maturity.

---

*See also: [`design.md`](design.md) (the full design), [`input-pipeline.md`](input-pipeline.md)
(the keystroke→glyph latency contract), [`keymap-architecture.md`](keymap-architecture.md)
(the modal/keymap layering).*
