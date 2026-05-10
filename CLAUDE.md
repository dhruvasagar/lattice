# Lattice

A modal, GPU-accelerated, plugin-first text editor written in Rust. Combines vim's modal editing power with emacs's extensibility model on a non-blocking, multi-threaded core.

## Status

**Phase 1+ -- modal engine, ex-commands, async actor, snapshot model, completion, help, basic perf benches, CI.** The authoritative ledger is `docs/dev/operations/implementation.md` (read it whenever you need to know what's done vs. spec'd). The design spec is `docs/dev/architecture/design.md` (current revision: v0.4). Bench results are in `docs/dev/operations/benchmarks.md`. Do not assume any module, type, or symbol mentioned in the design doc is implemented -- check `docs/dev/operations/implementation.md` first.

## Paramount goals (in priority order when they conflict)

1. **Performance.** Sub-frame input latency: keystroke -> glyph <= 8ms at 120Hz, <= 16ms at 60Hz. UI thread does no I/O, no parsing, no shaping. Per-call WASM overhead budgeted in CI (typed call < 500ns p99; grammar-extension round-trip < 5us p99).
2. **Extensibility.** WebAssembly Component Model plugin host from day one. WIT is the canonical API. Plugins ship in any language with component-model toolchain support (Rust, Zig, Go, AssemblyScript, ...). Capability-gated, fuel-limited, crash-isolated.
3. **Extensible vim modal editing.** Strict vim semantics. The grammar (operators, motions, text objects, registers, ranges, counts) IS the public command API. Adding new motions / text objects / operators is first-class -- including future tree-sitter-driven variants. One deliberate deviation from vim: the `:` command line and the functional API are unified into a single typed `CommandRegistry` with one dispatcher.
4. **Asynchronicity.** Three-layer architecture (UI / Core / Plugins) communicating via typed message passing. Multi-threaded by construction. Each plugin instance owns its own `wasmtime::Store` and runs as a tokio task; many plugins execute in parallel across cores. Nothing blocks the UI -- enforced architecturally, not by discipline.

## Decision-making heuristics

When the four paramount goals conflict with each other or with an implementation constraint, fall back to these. They came out of the architectural debates that shaped Phase 4 and are meant to keep design judgement consistent across sessions and contributors.

1. **Best long-term fit beats easy implementation.** Ease of coding is not a tiebreaker. If the simpler approach contradicts a paramount goal, the harder one wins. Call the trade-off out in prose, don't smuggle it.
2. **Evaluate against the paramount goals, not against other editors.** Neovim, helix, emacs, zed solved different problems on different substrates; "X does Y" is data, not justification. When choosing between approaches, name the paramount goal each one protects (and the one it sacrifices).
3. **Treat user-suggested options as input, not the menu.** When the user proposes A vs B, also surface C if it fits the goals better. The right answer may be neither. Say so explicitly with the trade-off.
4. **Confirm the plan before non-trivial work.** For any change touching architecture, public API, or cross-crate boundaries: walk through the chosen approach, the trade-offs accepted, and the impact surface (which crates / docs / benches / tests get touched) before writing code. Slice large changes; land each slice green.
5. **Non-trivial design changes ship four artefacts together.**
   - architecture documentation (in `docs/`, prose-led, tone matches the existing files),
   - benchmark coverage so perf impact is visible in `BENCHMARKS.md` and CI,
   - test coverage that exercises the new scenarios *and* the failure modes, not just the happy path,
   - graceful error handling -- log + skip on recoverable failures, never panic on the hot path, never swallow silently.

A change that ships only code is incomplete; the doc, the bench, and the test are part of the deliverable.

## Key design decisions

- **Vim modal state is a buffer-level state machine** (Normal / Insert / Visual / Op-pending / Command / Search), orthogonal to major / minor modes. Major mode = content-type identity (rust, markdown). The two axes do not collapse.
- **Unified command / grammar dispatch.** Operators, motions, text objects, ex-commands, plugin contributions, and palette entries all share `CommandInvocation` and flow through one `execute(...)`. The `:` line is a parser front-end. See docs/dev/architecture/design.md §5.2.1.
- **Everything is a buffer.** File tree, outline, diagnostics, search results, terminal, REPL -- all are buffers (read-only, interactive, or editable) placed by the user into panes via splits. There is no fixed sidebar or bottom-panel concept. See docs/dev/architecture/design.md §5.9. Concretely: documents and file trees today live in a single `BufferRegistry` keyed by `BufferId`; `:bn` / `:bp` / `:ls` / `:bd` / `:b N` operate uniformly across kinds. Each entry carries `BufferFlags { listed, hidden }` for vim-style `nobuflisted` / `hidden` semantics. Help is still rendered as a transient popup overlay (registry move queued).
- **Renderer trait** abstracts rendering paths: `EditorRenderer` (code + rich, four explicit fast paths), `DocumentRenderer` (popups, status lines, pickers, previews), `TuiRenderer` (terminal -- first-class peer for headless / SSH, not throwaway), future `WebRenderer`.
- **Iconography is v1 (§5.6.7).** A sprite atlas (separate from glyphs; same GPU pipeline) backs file-type icons, severity icons, gutter markers, status indicators, picker leading icons, notification badges. Bundled `builtin-icons` set + plugin sprites + user overrides. Path 4 (full inline media blocks) stays post-1.0.
- **Built-in vim grammar stays native.** `lattice-grammar` is a Rust crate. The default keymap never crosses the WASM boundary; WASM exists for *extensions*.

### Vim/Emacs unifications baked into the design

- **Hooks ≡ autocmds ≡ typed event subscriptions** (§5.10). One event bus with typed payloads. `:autocmd` and `add-hook` desugar to the same `subscribe(filter, target)` call.
- **Self-documenting help from day one** (§5.11). Every command, option, event, mode, keybinding carries metadata. `:describe-key`, `:describe-command`, `:describe-event`, `:describe-option`, `:describe-mode`, `:describe-buffer`, `:apropos` open buffer-backed help views.
- **Position history unifies jump list + mark ring** (§5.1.1). One ring with tagged sources (`AutoJump`, `ExplicitMark`, `PluginPush`, `NamedMark`). Different keybindings walk different filtered views of the same data.
- **Visual mode IS the active region.** `Range::Selection` is the default range arg when Visual is active.
- **Macros record `CommandInvocation` sequences, not keystrokes.** Replay survives keymap changes; recorded macros are editable as data in a buffer-backed view.
- **Typed options + customize buffer view** (§5.12). Every option is a typed registered value. `:set` is a parser front-end. `:customize` opens a type-aware editing buffer that writes back to user TOML.
- **Rich minibuffer** (§5.9.10). The `:` line and every interactive prompt is a real buffer with a major mode (`command-line`, `search-line`, `git-commit-line`, `repl-input`, ...). Full vim grammar, tree-sitter highlighting, completion popups, live error indicators, parameter hints, and substitution preview decorations.
- **Smaller wins** in Appendix B: interactive arg specs, `:g`/`:v`/`:windo` as normal commands, history pickers, scratch/messages/compilation as buffers, idle hooks, `:redir` as effect-capture wrapper, `:!` as `shell-execute` invocation.

## Tech stack

Rust + tokio (multi-thread) + ropey + tree-sitter + GPUI (preferred) or wgpu fallback + cosmic-text/parley + wasmtime (Component Model + WASI) + taffy + serde/MessagePack + TOML.

**One extension substrate.** No Lua, no vimscript, no elisp, no embedded Scheme. WASM (Rust today; any Component-Model language tomorrow) is the single substrate for plugins *and* user configuration. The user's `init.rs` is compiled to WASM and loaded by the plugin host with a boot-capability set. TOML covers static option overrides only; anything programmable (keymaps, autocmds, hooks, custom commands) lives in the Rust-WASM init module. Static settings stay declarative; logic stays code; one toolchain.

## Where to look in docs/dev/architecture/design.md

- §1 Vision; §2 Goals / non-goals
- §3 Architectural overview (three layers, threading)
- §5.1 Buffer / Document model (and §5.1.1 position history)
- §5.2 Modal Editing Engine (vim grammar + unified command API)
- §5.5 Plugin Subsystem (WASM Component Model, concurrency, performance discipline)
- §5.6 Rendering (layered architecture)
- §5.7 Async runtime and threading
- §5.9 UI Components (everything-is-a-buffer; §5.9.10 rich minibuffer)
- §5.10 Event System and Hooks
- §5.11 Introspection and Help
- §5.12 Configuration system (typed options + customize)
- §6 Core Protocol
- §8 Performance commitments
- §9 Plugin API (WIT interfaces)
- §11 Project layout (crates / wit / plugins)
- §13 Roadmap
- §14 Risks (host-call overhead, cold start, grammar API churn)
- Appendix A: Performance comparison vs. Neovim and Emacs
- Appendix B: Vim / emacs unifications (smaller wins)

## Conventions for Claude in future sessions

- The four paramount goals override stylistic preferences when they conflict.
- Match the doc's tone in any new design fragments: terse, principle-led, tradeoffs flagged honestly.
- Code blocks in docs/dev/architecture/design.md use **tab indentation** (the existing pattern).
- Don't introduce backwards-compat hacks for vim or emacs configs -- explicit non-goal.
- Do not add features, refactors, or abstractions beyond what a task requires; design discipline matters here because the doc is being lived-in, not legacy.
- Verify before recommending: a memory or doc reference to a file / type / symbol is *not* evidence it exists. Only `docs/dev/architecture/design.md` and `docs/dev/operations/implementation.md` are authoritative; verify against the source.
