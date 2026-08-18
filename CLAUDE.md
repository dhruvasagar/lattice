# Lattice

A modal, GPU-accelerated, plugin-first text editor written in Rust. Combines vim's modal editing power with emacs's extensibility model on a non-blocking, multi-threaded core.

## Status

**Phase 1+ -- modal engine, ex-commands, async actor, snapshot model, completion, help, basic perf benches, CI.** The authoritative ledger is `docs/dev/operations/implementation.md` (read it whenever you need to know what's done vs. spec'd). The design spec is `docs/dev/architecture/design.md` (current revision: v0.4). Bench results are in `docs/dev/operations/benchmarks.md`. Do not assume any module, type, or symbol mentioned in the design doc is implemented -- check `docs/dev/operations/implementation.md` first.

## Paramount goals (in priority order when they conflict)

1. **Performance.** Keystroke->glyph latency the user **cannot perceive** -- indistinguishable from the terminal/compositor echoing the key itself. *Not a fixed budget* but three examinable parts: **aspiration** -- match-or-beat the best-in-class reference (vim in the TUI, the compositor in GPUI); "best possible", not a line you cross once. **Hard ceiling** -- the glyph lands within **one display frame** under any background load (8.3ms at 120Hz, 16ms at 60Hz; this is display physics, not a target -- below one frame, faster output is imperceptible, so the point is to never *miss* a frame). **Ratchet** -- CI records the measured keystroke->glyph distribution and fails on regression; the bar only moves down, toward the I/O-hardware floor we can't beat. The current number is **descriptive** (≈0.7ms p50 today: TUI, debug build, 2300-line file), never a finish line. UI thread does no I/O, no parsing, no shaping. Per-call WASM overhead budgeted in CI (typed call < 500ns p99; grammar-extension round-trip < 5us p99). See `docs/dev/architecture/input-pipeline.md`.
2. **Extensibility.** WebAssembly Component Model plugin host from day one. WIT is the canonical API. Plugins ship in any language with component-model toolchain support (Rust, Zig, Go, AssemblyScript, ...). Capability-gated, fuel-limited, crash-isolated.
3. **Extensible vim modal editing.** Strict vim semantics. The grammar (operators, motions, text objects, registers, ranges, counts) IS the public command API. Adding new motions / text objects / operators is first-class -- including future tree-sitter-driven variants. One deliberate deviation from vim: the `:` command line and the functional API are unified into a single typed `CommandRegistry` with one dispatcher.
4. **Asynchronicity.** Three-layer architecture (UI / Core / Plugins) communicating via typed message passing. Multi-threaded by construction. Each plugin instance owns its own `wasmtime::Store` and runs as a tokio task; many plugins execute in parallel across cores. Nothing blocks the UI -- enforced architecturally, not by discipline.

## User experience is the higher court

The four paramount goals and the heuristics below are critical, but they are NOT upheld at the cost of user experience. When a goal or heuristic conflicts with a materially worse UX, **UX wins -- within reason**. Architectural purity that produces a visibly bad editor is the wrong trade. The priority order is: **UX > paramount goals > heuristics.**

- Acceptable compromises in service of a goal: **eventual consistency** and the like (e.g. an edited line's syntax colour catching up a frame or two later).
- Not acceptable: visible flicker, whole-viewport restyle, or any pixel change to content the user did not edit. "Even a 100-year-old editor doesn't do this" is a valid veto.
- The keystroke UX contract: only the edited line may visibly change per keystroke; everything else stays pixel-stable; the typed character appears immediately (text synchronous; syntax recolour may be eventual).
- When choosing an architecture, weight "does this make good UX the natural, reliable outcome?" -- a cleaner design that risks NEW visual regressions is the worse UX bet unless the regression risk is controlled by slicing + tests.

See `feedback_ux_over_paramount_goals`.

## Decision-making heuristics

When the four paramount goals conflict with each other or with an implementation constraint, fall back to these. They came out of the architectural debates that shaped Phase 4 and are meant to keep design judgement consistent across sessions and contributors.

1. **Best long-term fit beats easy implementation -- decided on merit.** Ease of coding is not a tiebreaker. Choose the genuinely-better long-term design (performance, correctness, clean abstraction); when it is clearly better, pick it even if the rewrite is large. Two equal-and-opposite failures to avoid: (a) keeping an inferior primitive out of risk-aversion -- "it works / it's well-tested / the rewrite is big" is NOT a sufficient reason; (b) rewriting or abstracting for its own sake without a concrete merit win. "Doesn't strictly violate a paramount goal" is not licence to keep the inferior design. Name the specific technical advantage; call the trade-off out in prose, don't smuggle it. See `feedback_long_term_design_over_quick_fix`.
2. **Evaluate against the paramount goals, not against other editors.** Neovim, helix, emacs, zed solved different problems on different substrates; "X does Y" is data, not justification. When choosing between approaches, name the paramount goal each one protects (and the one it sacrifices).
3. **Treat user-suggested options as input, not the menu.** When the user proposes A vs B, also surface C if it fits the goals better. The right answer may be neither. Say so explicitly with the trade-off.
4. **Confirm the plan before non-trivial work.** For any change touching architecture, public API, or cross-crate boundaries: walk through the chosen approach, the trade-offs accepted, and the impact surface (which crates / docs / benches / tests get touched) before writing code. Slice large changes; land each slice green.
5. **Non-trivial design changes ship four artefacts together.**
   - architecture documentation (in `docs/`, prose-led, tone matches the existing files),
   - benchmark coverage so perf impact is visible in `BENCHMARKS.md` and CI,
   - test coverage that exercises the new scenarios *and* the failure modes, not just the happy path,
   - graceful error handling -- log + skip on recoverable failures, never panic on the hot path, never swallow silently.

A change that ships only code is incomplete; the doc, the bench, and the test are part of the deliverable.

6. **A new crate needs a new mechanism, not a new feature.** Only a feature that introduces a genuinely new mechanism — its own substrate, its own dependency surface, a boundary that must be enforced structurally rather than by discipline — earns a dedicated crate. Everything that *overlaps or extends* an existing native feature belongs in the crate or mode that already owns that domain. Which-key extends the keymap, so it lives in `lattice-keymap`; it does not become `lattice-which-key`. The test is not "is this feature big / self-contained / conceptually distinct" — most features are all three. The test is: **name the dependency surface the new crate carves out, and the thing that would break if it did not exist.** `lattice-picker` stays free of syntax / cells so the off-thread guarantee is structural; `lattice-ui-gpui` is a renderer peer behind a cargo feature. Those are mechanisms. "It groups this feature's files" is not — that is what a module is for. Ask which existing crate owns the domain BEFORE presenting design options, and offer "new crate" only with the dependency-surface argument attached. See `dont-mint-a-crate-per-feature`.

### Presenting design choices (heuristic-mapping rule)

Every time options are presented for a non-trivial design choice — an `AskUserQuestion`, a plan summary, a slice-plan recommendation, a review doc — each option MUST be evaluated against the four paramount goals + six heuristics above by name. Shape diagrams, LOC counts, and "matches pattern X" / "consistent with Y" are context, not reasoning.

For every option include this block BEFORE the implementation shape:

> **UX (higher court):** does this option degrade user experience (flicker, latency, instability, or a pixel change to unedited content)? If so it loses regardless of goal/heuristic scores, within reason.
> **Paramount goals:** protects #N (specific reason); sacrifices #M (specific reason).
> **Heuristic #1 (long-term fit, on merit):** is this the genuinely-better long-term design, or kept out of risk-aversion / rewritten for novelty?
> **Heuristic #2 (paramount, not other editors):** is the justification anchored on a paramount goal, or on "X does it this way" / "consistent with"?
> **Heuristic #3 (third option):** is the option-set complete, or am I missing a (C) that fits the goals better than (A) or (B)?
> **Heuristic #6 (crate boundary):** for any option that proposes a new crate — what dependency surface does it carve out, and what breaks structurally without it? If the answer is "it groups the feature's files", name the existing crate that owns the domain and put it there instead.
> **Standing-rule check (mode ownership):** for any option that touches a chord, ex-command, action body, or buffer behavior — does this keep BOTH the binding choice AND the handler body with the mode that owns the buffer, or does it leave half the surface in the host? See `feedback_mode_owns_its_surface`. Half-migrations (substrate publishes data but host still wires the chord) DO NOT satisfy the rule.

The recommendation line must name the heuristic that drove it, not just the option label:

> Recommend **(a)** because it protects paramount-#3 (everything-is-a-buffer: the multibuffer becomes a self-sufficient Document, no host-layer kind-branch needed).

Not:

> Recommend (a) for simplicity. / for consistency with X. / because it's smaller.

Slice plans and design fragments follow the same rule. If a locked recommendation in a slice plan cites a non-heuristic reason ("consistent with" / "simpler" / "matches Y editor"), it is suspect and must be re-evaluated against the paramount goals when the slice executes. The recommendation is a starting point, not authority.

This rule exists because past unmapped presentations masked actual paramount-goal violations (K.3.2 silent failure → extensibility; K.4.x kind-branching → everything-is-a-buffer; M.7 audit → mode-ownership half-migration). Without explicit mapping, "looks cleaner" defaults to "easier to implement," which is exactly what heuristic #1 forbids.

### Substrate vs. mode helper — the distinguishing rule

When deciding whether mode-relevant behavior belongs as a `Document` trait method or a substrate helper function the mode imports:

- **Trait method on `Document`** — for data that *generic host machinery* (renderer, generic dispatch loop, position-history walker) reads uniformly across all buffer kinds. K.4.6 `display_line_numbers` (renderer reads for gutter) and K.4.11 `dispatch_with_cancel` (host's chord-dispatch loop reads to execute grammar) are correct trait methods because the *consumer is generic*.
- **Substrate helper function in the owning crate** — for data only a *specific mode's handler* reads. Composed→source translation, excerpt expansion, scan refresh state — all consumed only by the search/multibuffer mode's handlers. These live as free functions or handle methods in `lattice-multibuffer` and modes import them.

**Rule of thumb:** trait method = uniform-host consumer; helper = mode consumer. Mode-consumed data must NOT extend the Document trait surface.

The M.7 audit caught this: proposing `Document::resolve_jump_target` and `Document::expand_excerpts_at` looked like substrate publishing, but the only consumer of either was the search/multibuffer mode's handler. Putting them on the trait would mean the mode reads through trait dispatch when a direct helper call is sharper, and worse, would imply the chord-to-action wiring stays on the host side — leaving half the mode-ownership surface with the host.

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

## Standing rules from prior sessions

These rules emerged from prior architectural debates and corrections. They are LOAD-BEARING — each was a real correction Dhruva made that I kept re-violating before the rule got codified. Re-violation is the failure mode this section prevents. If a rule appears to conflict with a paramount goal or heuristic, the paramount goal / heuristic wins and the rule is the suspect — surface the conflict explicitly rather than silently picking one.

### Workflow

- **One slice, one commit.** Each slice in a plan lands as its own commit, with its own message explaining why that slice exists. Do NOT batch several slices into one commit because they happened to be written in the same sitting — the plan's slice boundaries are the review and bisect boundaries, and collapsing them throws away exactly the structure the plan was for.

  **Commit each slice as it goes green**, rather than writing three and committing at the end. Batching creates interleaved hunks in shared files (`dispatch.rs`, `effect.rs`) that then have to be carved apart by hand, and a botched carve produces a commit that does not compile — worse than the batching it was fixing.

  **The one legitimate exception is a slice that cannot compile without its neighbour.** EP.3's diagnostics feed reads the option EP.4 was specified to add, so the option had to ship with EP.3 or EP.3 would not build. When that happens: land them together, and say in the message which slice absorbed what and why. Do not split a commit into a state that fails to build merely to match the plan's numbering.

- **A commit is fmt-clean, warning-clean and green — verified BEFORE committing, not after.** Three gates, in this order, every time:

  ```
  scripts/precommit.sh <touched-crate>...   # runs all three, scoped
  scripts/precommit.sh                      # whole workspace (slow)
  ```

  The script is the gates made runnable — it separates rustc warnings from
  clippy ones, filters the deliberate `unwrap`/`panic` policy warns out of
  the way, and attributes warnings only to the crates you named (building
  `-p lattice-magit` also compiles its dependencies, and reporting *their*
  warnings as yours makes the gate cry wolf, which is how a gate stops
  being read). Run it by hand if you prefer:

  ```
  cargo fmt --all                       # then --check must pass
  cargo clippy -p <touched> --all-targets
  cargo test -p <touched>
  ```

  What each gate means, because they are not the same standard:

  1. **`cargo fmt --all -- --check` must pass.** CI gates on this strictly and nothing else about style. Run `cargo fmt --all` before staging; drift is never intentional.
  2. **ZERO rustc warnings in code you touched.** `unused_macros`, `dead_code`, `unused_variables`, `unused_mut` are always real — they mean something you edited left an orphan behind. A conversion that leaves its old helper unused is not finished. Do not `#[allow]` these; delete the dead thing, or `#[cfg(test)]` it if it is genuinely a test fixture.
  3. **No NEW clippy warnings in code you touched.** Not "zero clippy warnings in the workspace" — that is a different and much larger job, and attempting it mid-slice buries the actual change. The workspace carries a known residue (`type_complexity`, `too_many_arguments`, `result_large_err` and peers) that needs real refactors of hot-path code; leaving those visible is deliberate.

  **Do NOT try to zero the `unwrap_used` / `panic` / `todo` warnings.** `[workspace.lints]` sets them to `warn` ON PURPOSE and they are overwhelmingly test code (~650 of them). CI deliberately does not pass `-D warnings` — see the comment block at the top of `.github/workflows/ci.yml`, which explains that a toolchain bump adding new stylistic lints must not fail the build. The authoritative blocking gate is `[workspace.lints]`'s **deny** level: `unsafe_code` and `unused_must_use`. Those must always be zero.

  **Wait for the gate to finish before committing.** Running a subset
  (`cargo test -p X --lib some_filter`) and committing while the full
  gate is still in flight is not verification — it is a race you can
  lose, and it has been lost. The script now refuses to start beside
  another cargo job for the same reason: parts of the suite settle by
  POLLING with a timeout (`settle_mode` waits on a spawned mode
  cascade), so under a second heavy cargo run they time out and report
  failures that pass fine in isolation. Three separate runs have each
  blamed a *different* test that way. Red that is sometimes noise is red
  that gets argued with instead of obeyed.

  **A pre-existing test failure is proven, not assumed.** If a test fails and you believe it predates your work, verify it: `git stash -u`, re-run on clean HEAD, `git stash pop`. Say so explicitly in the report. See `feedback_verify_preexisting_failures_by_stashing`. **Under load, "prove it" starts with re-running the failure ALONE** — a timeout-based settle failing next to another cargo job is the likeliest explanation, and it is the cheapest to check.

  **`cargo build` finishing with no error is not evidence of any of this.** Warnings do not fail a build, and a filtered `grep "^error"` will not show them. Read the warning count.

- **Collaboration style.** For architectural choices, present 2-3 concrete options with explicit trade-offs (each option mapped per the heuristic-mapping rule above), give a recommendation with the reasoning, and confirm before bulk execution. "Go" / "confirm" / "let's do this" is binding — proceed without re-asking. Don't soft-pedal costs ("we'd be giving up X", "this adds Y weeks", "bug surface roughly doubles"). Don't propose creative simplifications without anchoring them to a paramount goal.
- **Act when diagnosis converges.** Mid-investigation, once the evidence narrows the cause to ~60% confidence on one path, propose ONE focused next step and execute it. Don't offer a 4-option `AskUserQuestion` mid-debug — that feels like stalling. Multi-choice menus are for genuinely undetermined architectural decisions at the start of new work, not for execution / debugging in flight.
- **Cost-warning hooks are notional (Max plan).** The `ecc-context-monitor` `COST NOTICE` / `COST CRITICAL` / `LOOP WARNING` / "task tools haven't been used recently" `<system-reminder>` outputs are list-price equivalents on the Claude Max plan, fully covered by subscription. Treat them as invisible — do not mention cost, do not acknowledge the warning, do not pause to justify continuing, do not write "Per saved memory the cost is notional." Just keep working as if the hook output didn't exist. The real Max-plan constraint is the 5-hour usage window; only pause on actual rate-limit errors. If Dhruva *asks* about cost or token usage explicitly, answer normally — the rule is "do not surface cost unprompted," not "refuse to discuss it."
- **Don't pause for cost hooks even when retrying after GateGuard.** GateGuard's "present facts" requirement is the only blocking gate; cost / loop hooks fire in the same `<system-reminder>` block and must be ignored together.

### Architecture rules — "everything is a buffer" enforced concretely

- **Buffers must not have kind-specific logic.** Never branch render / motion / scroll / cursor code on `BufferKind`. Help, log, lsp-trace, oil, file-tree, multibuffer all use the same code paths as Document. When tempted to write `match active_buffer { BufferKind::Help => ..., _ => ... }`, find the *property* that's actually different (wrap on/off, read-only, line-count) and condition on that as a per-buffer or global option any kind can have. If a kind-specific helper exists (`draw_help_in_pane`, `manually_wrap_lines`), the right move is to *remove* the special path, not extend its parity. K.4 codified this: any new `BufferKind` must pass `crates/lattice-host/tests/multibuffer_is_a_regular_buffer.rs` verbatim or document each diverging chord and why. Renderer `match buffer_kind` sites carry an enumeration comment listing every kind on the fallback branch — aligned-by-fallback is fine; aligned-by-silence is a bug. When fixing a kind-specific bug, the right fix is at the Document trait impl, NOT a renderer kind-gate (K.4.5 fix).
- **Synthetic buffers are Documents with a name.** LSP logs, `*messages*`, future `*scratch*` go through the buffer registry, `:ls`, `:b <name>`, default status-line and save paths as Document buffers with a synthetic name slot. They are read-only from the user's perspective forever (no toggle-off-read-only path); subsystem owners write via `apply_edit_batch_blocking` (debounced) and read-only enforcement at the dispatcher's modal Insert / operators path means owner writes naturally bypass without a capability token. Help-mode features (syntax highlight, link rendering) may be reused but the buffer is not a HelpBuffer. The Group-1 set (`:help`, `:describe-*`, `:apropos`, `:list-*`, `:keymap`, `:options`, `:diagnostics`, `:lsp-status`) genuinely uses help-mode features (links, anchors, dismiss-on-Esc) and stays as HelpBuffer.
- **Modes own their full surface** — keymaps, lifecycle subscriptions, status-line contributions, decoration providers, completion sources, option overrides, capability requirements, the *production* of their buffers (created through the `&mut`-backed **`ModeActivator::ensure_named_document`** creation seam — or its declarative peer **`Effect::OpenSyntheticBuffer`** for pure ex-commands; NOT the `&self` `BufferStore`, which can't activate a mode), the synthetic-buffer streaming logic, **AND the action-handler bodies that fire on the mode's chords or ex-commands**. No `ensure_<x>_buffer`, `drain_<x>_events`, `append_to_owned_buffer`, `do_<mode-or-provider-specific>_action` on `App` / `Editor`. The App is a thin host exposing generic primitives (buffer-store service, the `ModeActivator` creation/activation seam, tick-callback registry, event bus, action-handler registry, generic chord dispatcher); a provider's trigger creates + activates its buffer through the creation seam, and the mode's `on_activate` (running *on* the created buffer — it cannot create the buffer it is activating on) sets up the drain / subscriptions; tick callbacks are registered via the host service; **action handlers are registered as closures bound to the mode's contributed `ActionId`s**. Feature-specific keymaps live at `KeymapLayer::MinorMode(mode_id)` (or `MajorMode(mode_id)`), NEVER at `KeymapLayer::Builtin` — Builtin is universal vim grammar that fires in every buffer. New modes register via `register_<mode>_keymap(handle, action_ids)` in the mode's owning crate, returning a trie pushed under `PushLayerKind::MinorMode(<mode>::mode_id())`. K.1.c's per-keystroke filter then scopes the chord to mode-active buffers. Same shape generalises to `Mode::decorations()`, `Mode::subscriptions()`, future `status_line_items()`. **Half-migration is the failure mode this rule prevents:** moving the keymap into the mode while leaving `Editor::do_<x>` in the host's dispatcher (or moving substrate data into a Document trait method while the host still binds the chord) violates the rule. The acid test: a new provider crate landing should require ZERO `Editor::` method additions in `lattice-host`, and ZERO new variants in the host's `Action` enum — the provider contributes ActionIds via the registry and the handler bodies live in its own crate.
- **Shared behaviour is a minor mode, never a copied keymap.** When behaviour is wanted in more than one major, it belongs on a **minor mode** activated on the buffers that should have it — NOT the same chords and handler bodies declared again in each major. Before adding a chord to a major, ask whether another major already has it or would want it; if so, the chord and its handler go on a minor with an `ActivationPolicy` naming those majors. This composes with "Modes own their full surface" rather than competing with it: the minor owns keymap *and* handler bodies, so the surface is still whole, just carried by the mode that spans the majors. `magit-core-mode` is the in-repo precedent (`gr`, `q`, `]]`/`[[`, `A`/`_`/`O`, `a`/`-` across every magit buffer). **Duplication is the failure mode this prevents, and it is a silent one:** `magit-diff-mode` was hand-given `s` and `u` but not `x`, and nobody noticed the third was missing because there was no single place it should have been. A gap in a copied set does not announce itself. See `prefer-minor-modes-over-duplication`.
- **No UI-thread work.** The renderer's per-frame body does ZERO I/O / parsing / shaping / tree-sitter walks / LSP event drains / element-tree construction proportional to document content. Element fan-out is O(viewport-lines), NOT O(chars). Paramount goal #1, non-negotiable. When a violation is found, the FIRST slice is architectural relocation (move it off the UI thread); defensive filtering is a follow-up polish or skipped if relocation makes it moot. Forbidden patterns: `editor.run_tick_pending()` inside `Render::render`; `app.refresh_highlights()` inside `Render::render`; per-char `div()` cells inside paint paths. Text peers emit per-line text-with-attribute-spans, not per-cell widgets. Provider tests require both throughput AND runtime-responsiveness coverage — `current_thread` runtimes (the editor actor's config) make every `tokio::spawn` land on the actor thread; default to `spawn_blocking` for fs / cpu work. If you find yourself writing `yield_now().await` in a tight loop, that's a strong smell — the right shape is almost certainly `spawn_blocking`.
- **Async-buffer status in headerline.** Multibuffer providers + future async-buffer mechanisms surface progress + completion via the buffer's headerline (view-header virtual row), NOT status lines or notification badges.

### Implementation pitfalls

- **Async results must reach the screen WITHOUT a keypress — go through the inbound primitive.** `run_tick_pending` is the drain aggregator, and in production it is reached two ways: `App::apply`'s tail (i.e. **the next keystroke**) and the editor actor's `async_landed` select arm (off-keystroke, which also does `publish_render_state` + `paint_request`). Anything that lands asynchronously — a refreshed buffer, a computed cursor position, a provider's results — reaches the user only if something fires `async_landed`. Registering a bare `TickCallback` does NOT: it will sit until the user happens to press a key, and the symptom ("it works, but only after I hit something") reads like a rendering bug rather than a missing wake. **Use `SubsystemBoot::inbound::<T>(handler)`** (`lattice-mode/src/inbound.rs`): the wake is baked into `InboundBus::send`, so it is structurally impossible to forget, and the handler maps the item to `Effect`s the host applies. `BootContext::wake_on_event::<E>()` is the peer for event-bus-driven wakes. Paramount goal #4, and `docs/dev/architecture/boot-composition.md` §3 designs this exact bug class out — it has been re-introduced repeatedly by reaching for `tick_callback` directly. **Test it the way it fails:** assert the async result is visible *without* dispatching another action; a test that presses a key first passes on the broken version too (this is the same hole `test_helpers::settle` was added for). Related: an async producer that moves the cursor uses `Effect::CursorMoveIn { target, position }`, not `Effect::CursorMove` — by the time an async result lands the focused buffer may not be the one the position was computed in.
- **The GPUI binary is feature-gated — `cargo build -p lattice-cli` is TUI-only.** `lattice-ui-gpui` is an *optional* dependency of `lattice-cli` behind the `gui` cargo feature (`crates/lattice-cli/Cargo.toml`: `gui = ["dep:lattice-ui-gpui", "lattice-ui-gpui/window"]`, `default = []`). A plain `cargo build/run -p lattice-cli` does NOT put `lattice-ui-gpui` in the dependency graph, so edits to `crates/lattice-ui-gpui/**` are never compiled and the build reports "Fresh" / "Finished 0.3s" with no error even when the gpui rlib is absent — a silent no-op that wastes a whole debug cycle. To build/run the GPUI peer Dhruva uses `cargo run --features gui -- --gui`. For a quick type-check of a gpui edit, `cargo build -p lattice-ui-gpui` compiles the lib (default features, no `window`); add `--features window` to match the real-gpui config. Separately, on the WSL2 dev box cargo's mtime fingerprinting is unreliable (clock skew future-dates artifacts), so incremental builds can wrongly report "Fresh" after a real edit — `cargo clean -p lattice-ui-gpui` then rebuild forces it.
- **ServiceRegistry Arc/TypeId pitfall.** `lattice_mode::ServiceRegistry::register::<T>(value: T)` keys under `TypeId::of::<T>()`. If boot registers `s.register(event_bus.clone())` where `event_bus: Arc<EventBus>`, the entry is stored under `TypeId::of::<Arc<EventBus>>()`. `services.get::<EventBus>()` silently returns `None` (TypeId mismatch). Lookups MUST use the same `T` as registration. Convention: when registering an already-shared handle, define `XHandle = Arc<X>` and use `XHandle` for both register and lookup (canonical pattern: `ProjectSearchServiceHandle = Arc<dyn ProjectSearchService>`). When `services.get::<X>()` returns None for a service you're sure was registered, the first hypothesis is TypeId mismatch.
- **Diagnostic logs go to `debug!`, never `info!`.** Per-keystroke, per-frame, per-tick, probe, timing, instrumentation spans use `tracing::debug!` (or `trace!`). `info!` fans out to both stderr and `*messages*` via `MessagesLayer`; held-key bursts at 30 Hz flood and hide genuine info-class events. Reserve `info!` for one-shot user-actionable events: "LSP server attached", ":q on dirty buffer", "macro recording stopped". When in doubt, the user can opt into debug via `--log-level debug` — they shouldn't have to opt OUT of info.
- **Ex-command naming: dashed + namespaced.** LSP-coupled (and similarly subsystem-coupled) ex-commands register exactly one alias: the dashed namespaced form (`lsp-format`, `lsp-rename`). NO collapsed forms (`lspformat`, `signaturehelp`), NO generic-name aliases (`format`, `rename`, `complete`, `code-actions`). Generic names imply they should work regardless of LSP; hard-wiring to LSP-only paths is misleading + premature naming-territory grab. Vim-tradition 1-2 letter shorts (`cn`, `cp`, `bn`, `bp`, `wq`, `x`) stay for vim-canonical commands; do NOT invent new 1-2 letter shorts for novel commands — those slots are scarce, reserved for the WIT-shaped user-config / plugin alias mechanism.

### UX rules

- **Icon palette must degrade gracefully.** For file-tree / oil / any glyph surface, support two palettes — Nerd Fonts v3 when `ui.nerd_fonts=on`, AND a BMP-block fallback (Geometric Shapes + Misc Symbols: `◆ ≡ ◇ ■ ♪ ▶ ·`) when off. Default to the fallback so the first frame works in every terminal font; users on a patched font opt in. Both palettes occupy the same cell width so column geometry doesn't shift on toggle; the toggle handler must re-render every affected buffer.
- **UX follows convention; architecture follows paramount goals.** For features with established cross-editor convention (diff, signcolumn, fold markers, modeline format, search highlight, completion popup placement), lead with "the convention across Vim/Helix/Zed/VSCode is X" before recommending alternatives. Users carry muscle memory across editors; for important common features, convention beats local rationale. This is NOT heuristic #2 — heuristic #2 governs architecture (paramount-goal-driven). UX convention applies to user-facing surfaces where muscle memory is the dominant cost.
- **Editor design references weighted by substrate.** When evaluating UX or architectural patterns from other editors: weight **Helix and Zed heavily** for substrate-driven decisions (renderer, concurrency, gutter/signcolumn shape, plugin host shape) because they're Rust-built on similar substrates; weight **Vim and Emacs heavily** for grammar / extensibility-convention decisions (motion vocabulary, `]c` / `[c`, ex-command names, autocmd shapes, magit/fugitive UX). When all four agree, strong signal. When they diverge, explicitly state which constraint differentiates them.

### Cross-renderer + cross-artefact discipline

- **TUI and GPUI parity in lockstep.** When a slice touches `lattice-ui-tui` (effect classifier, renderer match arms, theme propagation, virtual rows, diff/sign rendering), update `lattice-ui-gpui` IN THE SAME PATCH. GPUI is a first-class peer renderer, not a fallback. Per slice plan: `Effect::*` enum extension → GPUI's effect-classifier match arm; new `DiffSignKind` / `Severity` variant → both gutter-glyph + row-tint sites in `window.rs`; new `host_theme.*` field → propagate through `window.rs`'s host_theme → editor_element conversion + sensible default. End-of-slice audit shortcut: `grep -rn "Effect::<NewVariant>\|DiffSignKind::<NewVariant>" crates/lattice-ui-gpui/ --include="*.rs"` — empty grep = GPUI was missed.
- **Separate design fragments from slice plans.** Design fragments (`docs/dev/architecture/<feature>.md`) carry contracts, data models, rationale, rejected alternatives, paramount-goal alignment, foldability + grammar surface — the stable "what" and "why". Slice plans (`docs/dev/operations/slice-plans/<feature>.md` or appended to `implementation.md`) carry slice IDs, sequencing, dependencies, status icons (✅ / 🚧 / 📝), test/commit counts — the churning "when" and "how". Cross-reference; don't co-locate. When carving a slice mid-build, update the slice plan, not the design fragment (unless the design itself changed). See "Doc organisation" section below for the structural guidance.

## Doc organisation: design vs. slice plan

Keep **design fragments** and **slice plans** in separate files. Don't co-locate.

- **Design** (`docs/dev/architecture/<feature>.md`) — contracts, data model, rationale, paramount-goal alignment, rejected alternatives, foldability + grammar surface, the "what" and "why". Stable across implementation iterations.
- **Slice plan** — concrete sequencing, slice IDs, dependencies between slices, status icons (✅ / 🚧 / 📝), test/commit counts. Churns frequently as work lands. Lives in `docs/dev/operations/implementation.md` (the global ledger) or in a per-feature file under `docs/dev/operations/slice-plans/<feature>.md` once the global ledger gets too dense.
- **Cross-reference** — design doc links to the slice plan ("see slice plan for sequencing"), slice plan links to the design sections it implements.
- When carving a slice mid-build, update the slice plan, not the design fragment, unless the design itself changed (e.g., a rejected alternative became the chosen path).

2026-05-29 cleanup migrated all existing per-subsystem slice plans into `docs/dev/operations/slice-plans/` — design fragments now own *what* and *why*, slice plans own *when* and *in what order*. New design fragments must follow the same pattern from day one.

### Archiving slice plans — completed plans ONLY

A slice plan moves to `docs/dev/operations/slice-plans/archive/` **only when every one of its slices is genuinely complete (✅)**. A plan with even one slice that is in-progress (🚧), planned (📝), or **deferred (⛔)** stays active — it is NOT archived. **Deferred is not complete:** a deferred slice is open work (postponed to a later phase, blocked, or accepted-later polish), and keeping its plan active is what keeps that work visible. Archiving a plan because "only the deferred bits are left" is the specific mistake this rule prevents (it buried modeline's `ML.4`/`ML.6` and would have buried dashboard `DB.8`, plugin-host `PH7.4e`, acp-ux `PU-B.2b-iv`).

- **Verify against source, not the status icons.** Icons drift stale (a landed slice left marked 🚧, a heading out of sync with the plan's own status table). Before archiving, confirm each slice is actually implemented in code + tests — fan out review agents for large plans. Fix stale icons as you go, whether or not the plan ends up archived.
- **Only the slice plan moves.** The design fragment (`docs/dev/architecture/<feature>.md`) stays put — it is the stable *what/why*. `git mv` the slice plan into `archive/`, then repoint inbound references (typically `implementation.md` and the design fragment) to the `slice-plans/archive/<feature>.md` path; leave references to the *design fragment* untouched. Same-directory relative links between two archived plans self-resolve once both are in `archive/`.
- **Un-archiving is legitimate.** If a plan was archived while a slice was still deferred/open, move it back to active and revert its inbound refs — the archive is for finished work, and a wrongly-filed plan is corrected, not grandfathered.
