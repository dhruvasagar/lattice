# Lattice — Implementation Tracker

This doc is the **current-state ledger** for the v1.0 build. It maps every
feature back to its anchor in ../architecture/design.md / CLAUDE.md and shows what's done,
what's in flight, and what's still pending.

Commit history is the authoritative log of *what changed when*. This file is
the authoritative answer to *where are we against the spec*.

When closing a session, update the status of every row touched. When opening
a session, read the **In-progress** and **Up next** sections — those are the
session pickup points.

---

## v1.0 scope at a glance

The four paramount goals from CLAUDE.md (in priority order when they
conflict):

1. **Performance.** Sub-frame keystroke-to-glyph (§8.2 commitments).
2. **Extensibility.** WebAssembly Component Model plugin host (§5.5, §9).
3. **Strict vim modal editing** with one deviation: unified command/grammar
   dispatch (§5.2.1).
4. **Asynchronous, multi-threaded core** that never blocks the UI (§3, §5.7).

The roadmap (§13) is divided into 11 phases (Phase 0 through Phase 10).
Phases 0–3 land the foundation, modal engine, terminal UI, and tree-sitter.
Phases 4–10 add LSP, GPU rendering, plugin host, modes, rich buffer
rendering, and v1.0 polish.

### Build order: core first, plugins later

../architecture/design.md §3.1 codifies the fast-path-vs-orchestration split:
features that fire per-keystroke (LSP wire + dispatch, snippet
engine, picker UI, completion engine, modal grammar) live in
core; opinionated tools built *on top of* the editor (magit
clone, fuzzy file finder, git inline overlays, markdown
preview, test runners) ship as plugins.

Concretely: Phases 4–6 grow the core surface. Phase 7 (plugin
host) lands after, designing WIT against an exercised set of
trait seams rather than speculative ones. Phase 8 / 8b
build the bundled-plugin catalog on top of a stable host.

Reasons this ordering wins:

- **Performance guard.** WASM-call overhead (typed call p99
  < 500ns; round-trip < 5μs) is real on the keystroke
  hot-path. Per-keystroke subsystems amortize that cost
  poorly even with batching.
- **Trait surface quality.** Each phase-4 feature
  (LspSupervisor, Picker, completion AsyncCandidateGenerator,
  active-snippet engine) is itself the trait surface plugins
  will eventually implement against. Designing WIT after we
  see five concrete patterns is far better than after two.
- **No sunk cost.** The crates we've built so far
  (`lattice-lsp`, `lattice-completion`, the picker module)
  are correctly placed -- no migration debt is accumulating.

---

## Phase status

| Phase | Title                                 | Status                   | Notes                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
|-------|---------------------------------------|--------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| 0     | Foundation                            | ✅ done                  | Workspace, lattice-core, document/buffer/undo, file I/O, protocol enums                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| 1     | Modal Editing                         | ✅ done                  | Modal engine, full chord routing, motions / operators / text objects / counts / registers / marks / macros / dot-repeat (incl. insert-replay) / search (incl. hlsearch + substitute live preview) / folds / ex-commands (every command -- including `:s` / `:g` / `:v` via `Args::List` -- registered as `ExCommandSpec` peers, dispatched through unified `grammar::execute()` per §5.2.1, §B.2). Blockwise visual: per-row dispatch for `d` / `y` / `c` plus blockwise paste; `>` / `<` indent each line in the block; `I` / `A` enter Insert at the block's left/right column with the typed prefix replicated to every row on Esc. Every operator lands as a single undo unit -- counts on linewise ops (`2dd`, `2>>`), block-visual rectangle ops, and I/A replications all collapse to one `u`. |
| 2     | Terminal UI Bootstrap                 | ✅ done                  | crossterm + ratatui; modal cursor; mode line; gutter                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| 3     | Tree-Sitter                           | ✅ done (Rust/Python/JS/Markdown) + Option B incremental reparse | Highlights wired through a shared `LangRegistry` (process-wide `Arc`); injection callback resolves fenced ` ```rust ``` ` blocks in markdown to the rust config (and any registered language to its config) without per-document copies. Markdown is the dual-grammar split (block + inline). Grammar extension API used by builtins, not yet by plugins. New `Style` variants (`Heading1..6`, `Bold`, `Italic`, `Link`, `Url`, `MarkupRaw`, `Markup`) for precise theme targeting. **Option B (B.1–B.5) lit up incremental reparse + frame-level span cache** — see "Option B: incremental reparse + span cache" below. |
| 4     | LSP                                   | 🚧 in progress (4.5 — wire complete; 3 trigger-UX rows deferred) | `lattice-lsp` crate plus full App-side wiring across five phases. **4.1** foundation: wire + actor + handshake + utf-8/16/32 doc sync + diagnostics broadcast + supervisor + edit-dispatch + open-on-`:e`. **4.2** navigation: hover (`K`), definition family (`gd`/`gD`/`gy`/`gI`), references (`gr`), symbols (`:lsp-symbols` / `:lsp-workspace-symbol` w/ resolve), completion (Insert-mode shell + LSP source + docs popup + `completionItem/resolve` + snippets + ranking + per-language overrides + tree-sitter symbols + path source + commit chars + ghost text + cross-source dedup). **4.3** edits: formatting + rangeFormatting + onTypeFormatting; signatureHelp + Insert autopilot; rename + prepareRename; codeAction + resolve + executeCommand; willSave + willSaveWaitUntil (format-on-save) + didSave; `workspace/applyEdit` inbound channel. **4.4** polish: `window/showMessage` + `showMessageRequest` + `showDocument`, `$/progress` + modeline + cancel, `:lsp-restart`, `documentHighlight` overlay, `selectionRange` + `:lsp-expand/shrink-region`, `foldingRange` + `FoldMethod::Lsp` + `lsp-folding-mode`, `inlayHint` virtual-text overlay, `semanticTokens/full` + `/full/delta` overlay, `textDocument/diagnostic` pull, `workspace/didChangeConfiguration` fan-out from typed-option cascade, `workspace/didChangeWatchedFiles` (notify watcher + globset matcher), `workspace/didCreateFiles`, dynamic `registerCapability` / `unregisterCapability` two-way index, `lsp.log_level` / `lsp.log_capacity` TOML. **4.5** expansion (wire + host complete): call hierarchy (`:lsp-incoming-calls` / `:lsp-outgoing-calls`), type hierarchy (`:lsp-supertypes` / `:lsp-subtypes`), `documentLink` + resolve (`gx`), `codeLens` + resolve (`:lsp-code-lens`), `documentColor` + `colorPresentation` (`:lsp-color-presentation`), `moniker` (`:lsp-moniker`). **Strong-reason deferred at the trigger UX layer**: `linkedEditingRange` (needs shadow-edit machinery), `inlineValue` (needs DAP), `inlineCompletion` (lsp-types `proposed` flag); `willRenameFiles` / `willDeleteFiles` / `inlayHint/resolve` (need editor-driven rename/delete + inlay-interaction UX). All multi-result LSP lookups + `:diagnostics` route through one vertico picker. Cancellation tokens plumbed through every wrapper. M.6 sub-mode cascade: 15 sub-modes (completion, diagnostics, hover, signature, format, rename, symbols, code-action, nav, progress, document-highlight, selection-range, folding, inlay-hint, semantic-tokens) toggle individually or via the `lsp-mode` umbrella. Per-feature matrix in [`../notes/lsp-features.md`](../notes/lsp-features.md). |
| 5     | GPU Rendering Foundation              | 🟡 in progress (5.0–5.1 ✅) | Phase 5 splits the host out of `lattice-ui-tui` so GPUI can land as a peer renderer. Plan: [`../architecture/phase-5-extraction.md`](../architecture/phase-5-extraction.md) -- 71k LoC audited, ~11k truly TUI-coupled, slices 5.1→5.last defined. 5.0 (the audit doc) + 5.1 (empty `lattice-host` shell, workspace seam) shipped.                                                                                                                                                                                       |
| 6     | Document Renderer + UI Components     | ⛔ not started           | Popups, pickers, panels-as-buffers all live in §5.9                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| 7     | Plugin Host                           | ⛔ not started           | wasmtime + Component Model + WIT scaffolding                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| 8     | Major/Minor Modes + Reference Plugins | ⛔ not started           | Major / minor modes are themselves plugins (§5.8.3)                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| 8b    | Bundled plugins                       | ⛔ not started           | Curated set of first-party WASM Component plugins shipping with the editor binary -- LSP server manager (lighthouse), plugin manager, fuzzy-finder, project grep, git client, snippet engine, editing helpers, diff viewer, outline sidebar, format-on-save, test runner, markdown preview. Each crate lives at `crates/lattice-plugin-<name>/`. See ../architecture/design.md §5.5.6 for the strategy + the seven WIT prerequisites Phase 7 must expose. Depends on Phase 7. |
| 9     | Rich Buffer Rendering                 | ⛔ not started           | Per-line shaped path, Fenwick height index                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| 10    | Polish + v1.0                         | ⛔ not started           | `*scratch:rust*` live-eval workflow (§10), accessibility, packaging, themes                                                                                                                                                                                                                                                                                                                                                                                                                                          |

Active focus: **Phase 4 (LSP) wind-down → Phase 7 (Plugin Host)
on deck.** Phases 4.1–4.5 are wire-and-host complete; the
remaining LSP rows are all "strong-reason defer at the trigger
UX layer" (linkedEditingRange, inlineValue, inlineCompletion,
willRename/willDelete, inlayHint/resolve) -- each blocked on
host UX design that doesn't belong inside Phase 4.

The natural next architectural step is **Phase 7 (Plugin
Host)**: paramount goal #2 (extensibility) is the most-deferred
goal today, and Phase 8 (modes-as-plugins), Phase 8b (bundled
plugins), and the user `init.rs` → WASM config path all gate on
it. The rustfmt + rustdoc cleanup just landed gives the WIT
surface a clean baseline to grow against.

LSP docs are comprehensive across audiences: design-doc readers
(`../architecture/design.md` §5.4), implementers
([`../architecture/lsp-architecture.md`](../architecture/lsp-architecture.md)),
users ([`../../user/lsp.md`](../../user/lsp.md) +
[`../../user/lsp-mode.md`](../../user/lsp-mode.md)), per-feature
trackers ([`../notes/lsp-features.md`](../notes/lsp-features.md) --
every LSP 3.17 capability with status), and a manual verification
checklist ([`verify.md`](verify.md) §17–18 covering all 15
sub-modes + 4.4/4.5 features).

Phase 4 roadmap (history): 4.1 wire + actor + sync + diagnostics →
4.2 navigation + completion → 4.3 edits (rename / format / code
action / will-save) → 4.4 polish (semantic tokens delta, inlay
hints, folding, document highlight, selection range, pull
diagnostics, dynamic registration, file watcher, configuration
fan-out, progress, server-initiated UX) → 4.5 expansion (call /
type hierarchy, code lens, document link, document color,
moniker).

---

## Option B: incremental reparse + span cache

**The keystroke-to-fresh-tree architecture** specified in
../architecture/design.md §5.3 / §8.2: every edit produces an `EditDelta`,
threaded to the syntax worker which applies `tree.edit()` and
runs `Parser::parse(.., Some(&old_tree))` for incremental
reparse. The frame-level span cache turns steady-state
re-highlighting into a no-op.

Originally diagnosed against a user-visible bug ("after `>>`
the highlighting breaks instantly; characters bleed into stale
colours"). Root cause: spans computed against an old syntax
snapshot were being painted onto current document bytes; the
spans never caught up because (a) full reparse was the only
path, and (b) `document.text()` allocated the full buffer on
the input thread per keystroke. Both fixed.

| Slice | Status | What landed                                                                                                                                                                                                                                                                                                                                                       |
|-------|--------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| B.1   | ✅     | `Buffer::apply_edit` returns an `AppliedEdit { ..., delta: EditDelta }`. New `EditDelta` type in `lattice-protocol::edit` carrying tree-sitter-shaped byte/Position deltas. Six u32 writes + three Position copies at the tail of `apply_edit` -- bench `input_edit_construction` measures **1.87ns**, at §8.2's ~2ns floor. Pure-data slice.                                                                          |
| B.2/1 | ✅     | `Syntax::parse_at_with_edits(source, text_version, from_version, edits)` applies each delta via `tree.edit()` then `Parser::parse(_, Some(&old_tree))`. Layered guards fall back to full reparse on any inconsistency (no cached tree, from_version mismatch, byte-length mismatch). `SyntaxHandle` worker coalesces queued requests by accumulating edits in arrival order, capped at 256 per burst. |
| B.2/2 | ✅     | App accumulates `EditDelta`s on `pending_syntax_edits` via the four edit chokepoints (`apply_edit_blocking`, `apply_edit_batch_blocking`, `undo_blocking`, `redo_blocking`). `maybe_reparse_syntax` drains and ships them to the worker. Per-document `last_synced_syntax_version` on App + `DocumentEntry`. Includes the `apply_edit_*` family's `&self → &mut self` refactor (no interior mutability introduced). |
| B.3   | ✅     | Frame-level highlight span cache on App, keyed `(snapshot_ptr, text_version, scroll, viewport_height, fold_hash)`. Cache hit at **20ns flat** across all corpus sizes -- **8900× speedup** vs the pre-B.3 ~178µs full QueryCursor walk every frame. Steady-state norm (cursor blinking, no edit) → ~100% hit rate. Load-bearing for paramount goal #1's strict reading on the steady-state floor. |
| B.4   | ✅     | Parametrized parity matrix in `lattice-syntax::syntax::tests`: 27 tests pinning `incremental tree.to_sexp() == full-reparse tree.to_sexp()` across edge positions, multi-line shape changes, whitespace-only edits, sequential batches, per-language coverage (Rust / Python / JavaScript / Markdown), pathological / minimal-buffer cases. Hardens the silent-wrong-tree surface. |
| B.5   | ✅     | `ReparseRequest` carries `Buffer` (O(1) Arc-bump clone via ropey's internal sharing) instead of pre-materialized `String`. Worker calls `buffer.as_string()` on `spawn_blocking` thread. Bench `clone_vs_text`: **7.7ns** flat across sizes vs. pre-B.5 path's 79ns / 990ns / 189µs (10 / 1k / 100k lines). **24,500× faster at 100k lines** -- closes the last input-thread allocation in the syntax-reparse hot path per goal #1. |

**Combined post-Option-B per-keystroke cost on the input
thread**: ~100ns (Buffer::clone + Vec::take + mpsc send +
counter bump). The actual reparse (tree.edit + Parser::parse)
runs on the worker's `spawn_blocking` thread, ~50µs at 1600
lines / ~100µs at 16k lines.

**Combined steady-state per-frame cost on the input thread**:
~33µs (compose 13µs + cache check 20ns), down from ~192µs
pre-Option-B (the ~178µs `highlight_lines` walk every frame
was wasted work whenever nothing changed).

Bench rows live in `benchmarks.md` under "Native highlight"
(B.2/B.4 incremental rows), "Frame render" (B.3 cache rows),
and "Buffer ops" (B.1/B.5 rows). §8.2 floor-rows updated with
measured numbers.

Two follow-ups deliberately deferred:

- **Per-pattern QueryCursor caching** (benchmarks.md "Improvement
  target" on `highlight::rust/200`): drops the cache-miss highlight
  cost when an edit invalidates the span cache. Lower priority --
  cache miss only fires on actual changes.
- **Per-pane edit accumulation** for inactive panes (currently
  falls back to full reparse on the inactive-pane refresh path).
  Rare in practice; bounded.

---

## C-series: runtime fixes + flicker elimination

A follow-up to Option B that turned the algorithmically-correct
incremental-reparse pipeline into a user-visibly-flicker-free
syntax-highlighting experience. Originally diagnosed against a
specific user complaint: "even after Option B landed, syntax
highlighting still goes through a brief 'gone, then back' phase
on every edit -- especially noticeable on `>>` and `dd`. Vim has
zero visual indication of any update. We should match that bar."

The story across five slices:

| Slice | Status | What landed |
|-------|--------|-------------|
| C.1   | ✅     | `#[tokio::main]` on `lattice-cli`. Pre-C.1, `main` was synchronous and `tokio::runtime::Handle::try_current()` failed silently inside `App::new`, causing `SyntaxHandle::seeded`'s worker to never spawn. Option B's entire incremental-reparse pipeline was routing through a non-existent worker -- spans stayed at the initial-seeded snapshot forever. Also adds nested-runtime-safe `block_on` (via `block_in_place`) so `apply_edit_blocking` etc. still work from inside the runtime. |
| C.2   | ✅     | Worker publishes an intermediate snapshot AFTER `tree.edit()` shifts byte ranges, BEFORE running `Parser::parse`. The intermediate has the new source + tree-edited tree (byte ranges aligned with new content; tree shape pre-parse for the changed regions). Renderers see byte-aligned spans during the entire parse window -- lines below a delete or after a multi-byte insert paint at correct positions immediately. Splits `parse_at_with_edits` into `try_apply_intermediate` + `reparse_with_cached_tree`. |
| C.3   | ✅     | Two complementary fixes: `shift_highlights_for_edit` runs synchronously in `publish_document_changed` to keep `App.visible_highlights` line-aligned across line-deletes / line-inserts (drains entries on delete; inserts empty placeholders on insert). `refresh_highlights` HOLDs the existing spans when `snap.text_version() < document.text_version()` instead of recomputing against stale data -- the cache only updates from one CORRECT set to another, never through an empty intermediate. |
| C.4   | ✅     | Byte-shift spans within the affected line for in-line edits. `>>` (insert "    " at line start) shifts every span on the line right by 4 bytes immediately. Crossing spans get their end extended/contracted; spans collapsed to empty get dropped. Held spans on frame N+1 are byte-aligned with new content, identical to what the worker's recompute will produce on frame N+2 -- zero visual transition. |
| C.5   | ✅     | **The missing link.** Grammar-driven edits (`>>`, `dd`, `cc`, `c`, `y`, `x`, `D`, `C`, `Y`, every operator) flow through the dispatcher → actor → `Effect::Edits(applied)` → `App::handle_edits`, which previously only updated the cursor. They bypassed `publish_document_changed` entirely, so all the C.1–C.4 machinery had no effect on the most common edit shape (operators). Routing them through the chokepoint fixed three things at once: spans byte-shift on input thread, `pending_syntax_edits` accumulates so the worker does incremental reparse instead of falling back to full, AND LSP `didChange` fires for operator edits (server-side document drift on `>>` / `dd` is gone). |

**User-visible result.** The complete chain (B + C) gives:

- **No flicker on `>>`** — the indented line's spans byte-shift
  immediately on the input thread; the worker's recompute lands
  with identical spans; the renderer never paints a transition.
- **No flicker on `dd`** — the deleted line's entry drains;
  lines below inherit their (still-correct) spans at their new
  indices; the worker's recompute confirms.
- **No flicker on typing** — single-char insert/delete byte-shifts
  spans on the affected line; the user sees colors track the
  cursor's edit point continuously.
- **Steady-state highlight cost stays at ~20ns/frame** (the B.3
  cache hit). The C-series is correctness-not-perf for the hot
  path; it adds a sub-µs `shift_highlights_for_edit` call per
  edit but no per-frame overhead.

**Implementation surface.**

- `lattice-cli/src/main.rs`: `#[tokio::main(flavor = "multi_thread")]`.
- `lattice-runtime/src/runtime.rs::block_on`: `block_in_place`
  wrap when nested in a runtime context.
- `lattice-syntax/src/handle.rs::worker_main`: two-stage parse
  with intermediate `ArcSwap::store` between `try_apply_intermediate`
  and `reparse_with_cached_tree`. New `seeded_with_runtime`
  constructor (kept alongside `seeded` for tests in tokio
  context).
- `lattice-syntax/src/syntax.rs`: split `parse_at_with_edits`
  into `try_apply_intermediate` (fast, no parse) +
  `reparse_with_cached_tree` (slow, parse). The convenience
  method runs both back-to-back.
- `lattice-ui-tui/src/app/highlights.rs`:
  - `App.visible_highlights_key`: cache key holds `syntax_text_version`
    only (not `document_text_version`) so edits don't trigger
    recomputation against stale snapshots.
  - `refresh_highlights` stale-snapshot HOLD path.
  - `shift_highlights_for_edit` — line-shift on edit.
  - `shift_spans_within_line` — byte-shift on in-line edit.
- `lattice-ui-tui/src/app/dispatch.rs`:
  - `handle_edits` — routes grammar-driven edits through
    `publish_document_changed` (the latter lives in
    `app/lifecycle.rs`).

Three follow-ups deliberately deferred (open):

- **LSP-side staleness symptoms**: diagnostics retention (`:diagnostics`
  shows errors that no longer exist), hover-position offset on
  rapid edits. C.5's `didChange`-now-fires-for-operators may have
  partially fixed hover staleness as a side effect; diagnostics
  retention is a separate decoration-pipeline bug.
- **Per-pattern QueryCursor caching**: the benchmarks.md improvement
  target on `highlight::rust/200` (3.2ms → ~1ms achievable). Cache
  miss only fires on actual changes; lower priority.
- **Per-DocumentEntry edit accumulation** for inactive panes:
  currently they fall back to full reparse on the rare cross-doc
  refresh path. Bounded scope.

---

## B': mode-owned synthetic buffers

**The "modes own their buffers" contract** specified in
../architecture/design.md §5.10.5: every synthetic buffer
(`*lsp*`, per-instance `*lsp:<server>:<workspace>*`, per-instance
`*...:trace*`, `*messages*`, future `*scratch*` / `*compilation*`
/ REPL / plugin-emitted streams) is owned by a dedicated mode
end-to-end. The App holds no subsystem-specific buffer-handling
code; two host primitives -- `BufferStore` + `ServiceRegistry` --
carry the contract. Anchors: design.md §5.10.5 + §5.4.7 +
mode-architecture.md §7.

Originally motivated by user-visible bugs (modeline `[no name]`
on `:lsp-log`, `<C-o>` "no jumps" from synthetic buffers, missing
per-instance separation for multiple `rust-analyzer` processes
against different workspaces) plus the architectural observation
that `App::ensure_lsp_subsystem_buffer` / `App::drain_lsp_log_events`
/ `App::append_to_owned_buffer` were anti-extensible: every new
subsystem would copy-paste into the App. The recipe needs to be
mode-shaped instead.

| Slice | Status | What landed / what lands                                                                                                                                                                                                                                                                                                                                                       |
|-------|--------|-------------|
| B'.1a | ✅     | `BufferStore` trait + `BufferStoreHandle` defined in `lattice-mode`. `Send + Sync` so a mode's tokio task can call from any thread. Three methods: `find_by_name`, `ensure_named_document(name, major, flags)`, `handle_for(id)`. No implementation yet -- the trait is the contract surface for B'.3 onward.                                                                  |
| B'.1b | ✅     | `BufferRegistry` switched to interior mutability (`Arc<Mutex<BufferRegistryInner>>`). Methods take `&self` and lock internally; the type is `Clone` (Arc bump) so it threads naturally into `BufferStoreHandle`. Callback-based read/write API (`with_entry`, `with_help`, `with_oil`, `with_file_tree`, `for_each`) replaces reference returns -- guards never escape the lock. 30+ App-side call sites migrated; 1574 lattice-ui-tui tests stay green. Sharp edges to honour: no lock-across-`.await`, no re-entrant `with_X(\|_\| with_Y(...))` (`std::sync::Mutex` is not reentrant). |
| B'.2  | ✅     | Per-instance LSP logger keying. `InstanceKey { server_id: Arc<str>, workspace: Arc<Path> }` is the new canonical key; `LspLogger`'s per-server map becomes per-instance. `LogRecord` and `LspLogPushed` carry workspace alongside server_id. All four `Inbound*` bus payloads (applyEdit / configuration / showDocument / showMessageRequest) carry workspace so the App-side drain can route logs and side-effects to the correct instance. `ServerHandle::instance()` / `::workspace_root()` expose the canonical pair. App ex-commands (`:lsp-log-level` / `:lsp-log-clear` / `:lsp-trace`) walk running actors + the logger's `known_instances`, with a cwd-synth fallback so pre-spawn toggling still works. 1574 lattice-ui-tui tests pass. |
| B'.3  | ✅     | `LspLogMode` owns `*lsp*`. Hand-written `Mode` impl: `on_activate` pulls the buffer's `DocumentHandle` via the `BufferStoreHandle` service, subscribes to `LspLogPushed` events, spawns a tokio task that drains the subscription, coalesces, and applies one batched edit per drain cycle. `on_deactivate` removes the buffer-local subscription stash + unsubscribes. The App's `drain_lsp_log_events` skips the subsystem write when `LspLogMode` is the active major on `*lsp*`. `BufferStore` impl for `BufferRegistry`; `BufferStoreHandle` registered in `ServiceRegistry` at boot. `format_log_event_line` hoisted to `lattice-lsp::logging` so the mode can reuse it. Mode degrades gracefully when no service or runtime is wired (test paths). |
| B'.4  | ✅     | `LspServerLogMode` owns per-instance `*lsp:<server>:<workspace>*`. Hand-written `Mode` impl reads its `InstanceKey` from the `LspServerLogInstance` buffer-local (seeded by App before activation), subscribes to `LspLogPushed`, filters by `(server_id, workspace)`, skips trace records, batches appends. Buffer name + ensure helpers take `&InstanceKey`; App drain accumulates by `(server_id, workspace)` and skips per-server write when the mode is the active major. Both B'.3 and B'.4 modes also seed their buffer from the in-memory ring on activate so pre-existing records show up immediately; `LspLogger` registered as a service for the seed path. `open_lsp_log_in_pane` / `open_lsp_trace_log_in_pane` use a `resolve_lsp_instance_for` helper (running actor → known-by-logger → cwd-synth fallback). |
| B'.5  | ✅     | `LspTraceLogMode` owns per-instance `*lsp:<server>:<workspace>:trace*`. Hand-written mirror of B'.4 with inverted filter (`level == "trace" \|\| source == "trace"`). Reuses the `LspServerLogInstance` buffer-local for instance identity; tracks its own subscription via `LspTraceLogSubscription` so a single instance can have both buffers open simultaneously. Seeds from the in-memory ring on activate. App drain skips per-trace write when the mode is the active major. Retired the `lsp_log_mode!` macro since all three log majors are now hand-written. |
| B'.6  | ✅     | App LSP cleanup. `App::drain_lsp_log_events` slimmed to only fan `window/showMessage`-sourced records to the minibuffer -- every buffer append is mode-owned now (B'.3 / B'.4 / B'.5). `format_log_event_line` alias retired from `lattice-ui-tui` (canonical is in `lattice-lsp`). `App::ensure_lsp_*` buffer creators and `append_to_owned_buffer` stay -- still used at boot (subsystem buffer) and by ex-commands (per-instance lazy create); the modes activate on top. Added `log_append_pipeline_100_records` Criterion bench in `lattice-lsp/benches/lsp.rs`: publish 100 records through `LspLogger → EventBus → mpsc → coalescing drain → format`, measure wall time. Baseline ~87µs / 100 records (≈870ns / record) on dev hardware. Excludes the `apply_edit_batch` tail (depends on `lattice-grammar`, out of scope for this crate's deps); the document-actor benches already measure that side. |
| B'.7  | ✅     | `:lsp-log` / `:lsp-server-log` / `:lsp-trace-log` reshaped as thin wrappers. Canonical name builders + inverse parsers moved into `lattice-lsp::buffer_names` (`LSP_SUBSYSTEM_LOG_NAME`, `lsp_server_log_name(&InstanceKey)`, `lsp_server_trace_log_name(&InstanceKey)`, `parse_lsp_server_log_name(&str) -> Option<InstanceKey>`, `parse_lsp_trace_log_name`). `BufferStore::name_for(id) -> Option<String>` added so modes can read their buffer's synthetic name. `LspServerLogMode::on_activate` / `LspTraceLogMode::on_activate` now derive their `InstanceKey` straight from the name — the `LspServerLogInstance` buffer-local + the App-side seeding before activation are gone. App side collapsed `ensure_lsp_subsystem_log_buffer` / `ensure_lsp_server_log_buffer` / `ensure_lsp_server_trace_buffer` / `ensure_lsp_log_buffer_with_instance` / `ensure_lsp_log_owned_buffer` into a single generic `ensure_named_synthetic_document(name, mode_id, flags) -> BufferId` host helper. Ex-command handlers (`do_open_lsp_log`, `open_lsp_log_in_pane`, `open_lsp_trace_log_in_pane`) + the `:lsp-trace` toggle path + the boot path + the `*messages*` creator all route through that one helper; the handler computes `name` via `lattice-lsp` and picks the major-mode id, and that's all the subsystem-shaped knowledge it has. Drains of `drain_lsp_log_events` removed from the open paths (modes drive buffer content from the event bus, independent of that minibuffer-only drain). 1574 lattice-ui-tui tests + 183 lattice-lsp tests stay green. |

**Design-call answers baked in (per architect's confirmations):**

- *Buffer name canonicalization* — full workspace path as the
  canonical registry key; display label may shorten with `~/`.
- *Server-detach lifecycle* — the mode's `on_deactivate` runs
  when the supervisor signals server-exit; the subscription
  drops; the buffer survives (frozen — no further appends).
  `:bd` is the explicit removal path.
- *Memory cap* — unbounded transcript by default; `:lsp-log clear`
  drops the rope. Matches user mental model ("the buffer is the
  full log").
- *Subscription strategy* — one publisher (`LspLogPushed`), three
  filtering subscribers (one per mode). Simpler than three
  sub-publishers; tokio `broadcast` is the fan-out channel.
- *Bench coverage* — single cross-task append-throughput bench in
  B'.6 alongside the App-side delete, so regressions show up in
  the same slice that removes the old fallback path.

---

## M-async — async mode lifecycle (v1 commitment)

**Async mode lifecycle** specified in mode-architecture.md §7.1.
`Mode::on_activate` returns a `LifecycleFuture<'_, Self::Guard>`
that the dispatcher schedules on the runtime; deactivation drops
the typed Guard, firing its `Drop` impl for synchronous cleanup
(async cleanup uses `spawn_task` fire-and-forget — Zed's pattern).
The UI thread never blocks on mode activation. Hooks that need to
spawn a supervisor, await a handshake, drain a watcher, or close
a server connection do so naturally with `.await`.

**Why v1, not post-v1.** Paramount goal #1 (sub-frame input
latency) and paramount goal #4 (asynchronicity) both demand
that no synchronous path on the App thread does I/O. Today's
`LspMode::on_activate` is the empirical counter-example: first
`cargo run` blocks measurably while the rust-analyzer supervisor
spawns + initialises. Subsequent runs don't block because the
work is cached. The architectural fix is async lifecycle with
typed Guards, not caching.

**Sequence:** M-async lands *after* B' (committed at `7aca41d`)
and *before* messages-mode v1. B' establishes buffer-ownership
with sync activation (all activation sites are ex-command
handlers — sync is correct for those). M-async generalises the
lifecycle so messages-mode v1 gets async + Drop-based cleanup
for free.

**Design ref:** `docs/dev/architecture/mode-architecture.md` §7.1
(Drop-based Guard rationale, ctx shape, activation/deactivation
flow, re-entrancy invariants). Settled 2026-05-14; do not
re-litigate.

| Slice    | Status | What lands                                                                                                                                                                                                                                                                                                            |
|----------|--------|-------------|
| M-async.1 | ✅ | **Drop-based Mode trait redesign (sync drive).** Workspace-green. `Mode` trait gained `type Guard: Send + 'static`; `on_activate` returns `LifecycleFuture<'_, Self::Guard>`; `on_deactivate` removed (Drop = cleanup contract). `DynMode` (pub adapter trait) + blanket `impl<M: Mode> DynMode for M` does `Box<dyn Any + Send>` erasure. `ModeContext` redesigned: `Send + 'static`, no `BufferLocals` field, owned Arc handles (`config`, `events`, `services`). `GuardStore` (`HashMap<(BufferId, ModeId), Box<dyn Any + Send>>`) passed `&mut` to registry methods. Registry drives lifecycle futures synchronously via `poll_now`; deactivation drops the Guard. All Mode impls migrated atomically — marker modes (lattice-mode / -syntax / -snippet / -file-tree / -oil) get `type Guard = ();`; the five hand-written LSP modes (LspServerLogMode, LspLogMode, LspTraceLogMode, LspMode, LspFoldingMode) get typed Drop-based Guards; `lsp_sub_mode!` macro emits markers. App: `services: ServiceRegistry → Arc<ServiceRegistry>`, new `mode_guards: GuardStore` field, dispatcher caller sites updated, registry tests rewritten with `MockMode`/`MockGuard` drop-counter, `TestLocalsMode` Guard test in dispatch.rs. Workspace green: 1573 lattice-ui-tui + 191 + 17 + 9 + 12 + 4 lattice-lsp + 113 lattice-mode + remainder. End state: same user-visible behavior as before (App still blocks during lifecycle); new API shape in place for M-async.2 and M-async.3. |
| M-async.2 | ✅ | **Spawn-based lifecycle dispatch.** Workspace-green. Registry's lifecycle drive swapped: `poll_now(...)` → `lattice_runtime::spawn_task(async move { ... })`. New `spawn_task` helper added to `lattice-runtime`. Sync prefix synchronously mutates `active_modes`; spawned task awaits `on_activate_dyn(ctx)`, stashes the Guard, publishes `MajorEntered` / `MinorActivated` on success or `ModeActivationFailed` on `Err` (variant added; carries stringified reason). `ModeEvent` registered as `TypedEvent` (`mode.lifecycle`) via `register_event!` so the bus carries it. Registry method signatures: `Result<Vec<ModeEvent>, _>` → `Result<(), _>` (validation errors only). `GuardStore` wrapped in `Arc<Mutex<...>>` newtype `GuardStoreHandle` so the spawned task (tokio worker) and the App thread can both lock briefly without `&mut` lifetime gymnastics. `linkme` added as direct dep on `lattice-mode` (proc-macro path requirement). App caller sites updated: `services` already `Arc`-wrapped from M-async.1, `mode_guards: GuardStore → GuardStoreHandle`, deactivate methods now take `&Arc<EventBus>` so they can publish the deactivation event. Registry tests are `#[tokio::test]`, subscribe to `ModeEvent` on the bus, `.await` the channel for spawned-task completion. Workspace green: 113 lattice-mode + 191 lattice-lsp + 1573 lattice-ui-tui + remainder pass. **User-visible win:** the App thread no longer blocks on `on_activate.await`. Today's modes happen to be immediately-ready (they `tokio::spawn` work and return), so the win is theoretical for now -- it lands fully when `LspMode::on_activate` is rewritten to `.await` the LSP initialize handshake (a separate refinement). |
| M-async.3 | ✅ | **Cascade ordering + rollback.** Workspace-green. Cascade refactor: sync prefix walks the requested mode + its `implies()` tree, validates each, mutates `active_modes` for the whole tree, builds an ordered `Vec<CascadeStep>`. One driver walks the plan DFS, awaiting each step's `on_activate.await` before the next — eliminates races where a sub-mode reads parent's not-yet-written state. The driver is **try-sync-then-spawn**: polled once with a no-op waker; if the cascade is immediately-ready (today's marker modes + sync LSP modes), it completes on the App thread with no spawn boundary. The first `Pending` yield trips the spawn path; the remainder runs on the runtime. On lifecycle error the driver publishes `ModeActivationFailed` for the failing step plus a synthetic `ModeActivationFailed { reason: "cascade aborted by X" }` for every remaining unrun step. App-side `drain_mode_lifecycle_events` subscriber drains `ModeEvent` from the bus per tick; for each `ModeActivationFailed` it calls `deactivate_mode_by_id`. New test `cascade_abort_publishes_synthetic_failures_for_unrun_steps` documents the abort path; `implies_auto_activates_dependency` updated to assert sequential DFS order (parent first). |
| M-async.4 | ✅ | **Per-pair epoch counter for activate / deactivate serialization.** Workspace-green. Eliminates the latent race that would surface when a mode's `on_activate` future genuinely `.await`s (yields `Pending` on first poll): a synchronous deactivate arriving while the spawn is in flight can no longer leave a leaked Guard in a logically-inactive store slot. `GuardStore` grew `epochs: HashMap<(BufferId, ModeId), u64>`. New API: `bump_epoch(buf, mode) -> u64`, `current_epoch(buf, mode)`, `try_insert(buf, mode, my_epoch, guard) -> Result<(), Box<dyn Any + Send>>`. `remove(buf, mode)` bumps the epoch internally before removing, so any in-flight spawn's later `try_insert` fails the match. `purge_buffer(buf)` also bumps all `(buf, *)` epochs. `CascadeStep` carries the `epoch: u64` captured when the sync prefix queued it; the spawn driver calls `try_insert` with that epoch on `on_activate.await` success. On stale (`Err(stale_guard)`), the Box is dropped on the spawn side — the original Guard's `Drop` fires for out-of-band cleanup (publishes `LspBufferDetached`, restores `foldmethod`, etc.). No `MajorEntered` / `MinorActivated` published for the stale activation. New test `rapid_deactivate_during_pending_activate_drops_guard_on_spawn_side` exercises the race with a synthetic `.await`ing mock mode gated on a `tokio::sync::oneshot`. Doc update in mode-architecture.md §7.3.1. **Mechanism: lock-free epoch counter, not per-pair Mutex.** Considered + rejected: `tokio::sync::Mutex` per pair (deactivate would block / spawn extra task); epoch counter is minimal, sync-deactivate-friendly, and matches the Drop-based cleanup contract. |
| M-async.5 | ✅ | **`LspMode::on_activate` awaits initialize handshake.** Workspace-green. Rewrote `LspMode::on_activate` to drive the supervisor's `open_buffer(path, text).await` directly. The mode's "active" state genuinely tracks LSP readiness; the previous split (LspMode publishes intent → `attach_driver` does the work async) is gone. Modes own their work; the App is no longer a coordinator between `Event::DocumentOpened` and the LSP attach path. Flow: `LspMode::on_activate` resolves path + text via `ctx.service::<BufferStoreHandle>()`, calls `supervisor.open_buffer(path, text).await`, then publishes `LspBufferAttached` *after* the await completes (subscribers now see it as "operational"). On error → `Err(LifecycleFailed)` → `ModeActivationFailed` → App rollback. Path-less (scratch) buffers and missing-supervisor (test) paths short-circuit gracefully (no `.await`, no spawn). Removed: `lattice-lsp/src/attach_driver.rs` (52 LOC) + its boot-time `attach_driver::spawn` call. M-async.4 epoch counter is the load-bearing primitive: rapid `:lsp-mode` toggle while initialize is in-flight is reconciled lock-free (try_insert mismatch → Guard drops on spawn side → `LspBufferDetached` publishes). Bench `crates/lattice-lsp/benches/mode_activate.rs`: dispatch latency for 16-step lsp-mode cascade (with sub-modes) under two cases — no-server-config (open_buffer round-trips supervisor mailbox, returns Ok(empty)) and unregistered-supervisor (mode short-circuits). Measured 6.2 µs per activate/deactivate cycle in optimized builds (both cases); pinned in CI to catch regressions. Three App-level tests updated to `#[tokio::test(flavor = "multi_thread")]` with poll-and-sleep waits (the activate is now genuinely async so deactivate races the spawn — wait pattern matches the M-async.4 race test). Workspace green: 1573 lattice-ui-tui + 180 lattice-lsp + 76 lattice-mode + remainder pass. |

---

## messages-mode v1 (tracing-bridged echo log)

**The `*messages*` buffer** specified in
../architecture/design.md §5.10.6: every record `App::set_message`
produces, plus every `tracing::*` event from the editor + plugins,
flows through a single subscriber into one mode-owned buffer.
Anchor: design.md §5.10.6 + §5.10.5 (the B' contract it builds on).

May land in parallel with B' (no dependency); the async pieces
(tracing subscriber's append task) benefit from M-async-activate
once that lands but can ship with sync activation if scheduling
dictates.

| Slice        | Status | What lands                                                                                                                                                                                                                                                                                                                                |
|--------------|--------|-------------|
| msg-mode.1   | ✅     | **Tracing bridge + MessagesMode (combined .1 + .2 of original spec).** Workspace-green. `lattice_grammar::EchoLevel` gains `Trace` + `Debug` variants + `From<EchoLevel> for tracing::Level` + inverse impls. New `MessagesLayer` (`tracing_subscriber::Layer`) in `lattice-runtime` captures every event into a shared `Arc<Mutex<MessagesRing>>` + publishes `MessagePushed` on the editor event bus. New `install_messages_subscriber(ring, bus)` (idempotent via OnceLock) called once at App boot. New `MessagesMode` (Major, `ReadOnly = true`) in `lattice-mode/src/modes/messages.rs` replaces the `text-mode + read-only-mode` combo on `*messages*` — symmetric with `lsp-log-mode` for `*lsp*`. `App.messages` now `Arc<Mutex<MessagesRing>>` so the boot-installed layer (running on whatever thread emitted the event) can push into the same ring the App reads on the main thread for backlog seeding. **Two paths, one destination:** `App::set_message` keeps its direct ring + bus push (preserves per-App isolation for tests); the `MessagesLayer` captures all *other* `tracing::*` events in the editor (LSP layer logs, plugin host once 1.0 lands) into the same ring + bus. Both produce identical `MessageRecord` shapes. **Deferred from spec:** mode-driven subscriber lifecycle (Drop the subscriber when `:messages-mode` toggles off) — `set_global_default` is process-wide; making it mode-owned needs reload-handle plumbing (v1.1). |
| msg-mode.2   | ✅     | **`messages.filter` typed option.** Workspace-green. New `Messages` option group + `MessagesFilter: String` (default `"info"`). `lattice_runtime::install_messages_subscriber` now takes an initial filter spec + wraps the layer in `tracing_subscriber::reload::Layer<EnvFilter>`; the reload handle is stashed in a process-global `OnceLock`. `:set messages.filter=editor=info,lsp=debug,grammar=trace` triggers the option-change cascade which calls `reload_messages_filter(spec)` to swap the directive live without restarting the editor. Validator (`validate_messages_filter`) calls `EnvFilter::try_new` at `:set` time so unparseable directives are rejected before they reach the runtime. `tracing-subscriber` workspace dep gained `env-filter` feature. `MessagesFilterReloadError` enum carries the parse / no-subscriber / reload-failed cases for diagnostics (App surfaces as a warn echo if the reload fails on a test path). |
| msg-mode.3   | ✅     | **Syntax highlighting for `*messages*`.** Workspace-green. Theme gains `messages_timestamp_style` (dim), `messages_trace_style` (dim), `_debug_style` (cyan), `_info_style` (default), `_warn_style` (yellow + bold), `_error_style` (red + bold). New `messages_line_spans(line, theme, max_width)` in `render.rs` scans the fixed `HH:MM:SS.mmm LEVEL text` format produced by `format_message_record` (byte offsets 0..12 timestamp, 13..18 level token, 19.. body) and emits a ratatui `Vec<Span<'static>>` with per-token styles. In `compose_visible_lines_inner`, when the active pane's major mode is `messages-mode` (checked via `app.active_modes.get(&id).and_then(|m| m.major())`), the messages-line builder replaces the syntax-spans pipeline for every visible line. Malformed lines (empty rope tail, future formatter changes) fall through to plain rendering — no panic, no wrong color. Four new tests pin: each level token gets the matching theme style; timestamp dims; malformed line falls back; unknown level token falls back. |
| msg-mode.5   | ⛔     | Plugin host bridge (post-1.0 WIT): `host.log(level, target, msg)` flows through the same subscriber. Plugin telemetry shows up in `*messages*` with no plumbing on the plugin side. Gated by Phase 7.                                                                                                                                       |

---

## Phase 5 — GPU rendering foundation (host extraction → GPUI peer)

Anchor: [`../architecture/phase-5-extraction.md`](../architecture/phase-5-extraction.md) (the audit + slice plan).
Anchor: ../architecture/design.md §5.6 (renderer trait + layered architecture), §11 (project layout), §13 (Phase 5 / Phase 6 roadmap).

`lattice-ui-tui` is currently the host -- it owns `App`, dispatch, every picker source generator, LSP coordination, mode lifecycle, options cascade, AND ratatui paint. Phase 5 extracts the renderer-agnostic substrate into a new `lattice-host` crate so GPUI can be added as a peer (under `lattice-ui-gpui`), with `lattice-ui-tui` shrinking to a ratatui adapter behind the `lattice-render::Renderer` trait. The TUI moves behind `lattice --tui`; GPUI becomes the default only after parity.

| Slice          | Status | What lands |
|----------------|--------|-----------|
| 5.0            | ✅     | **Extraction audit doc** (`docs/dev/architecture/phase-5-extraction.md`). Every module under `crates/lattice-ui-tui/src/` classified (HOST / TUI_RENDER / TUI_INPUT / THEME_TUI / BOOT / MIXED). Hard cases enumerated: theme is ratatui-typed throughout and leaks into `App.theme`; `chord.rs` + `input.rs` mix the renderer-neutral chord representation with crossterm adapters; `pane_render.rs` typedef hardcodes `&mut ratatui::Frame, Rect`. Target crate layout named. Slice ordering 5.1 → 5.last fixed. |
| 5.1            | ✅     | **`lattice-host` shell created.** New `crates/lattice-host` with empty `lib.rs` (docs only, no exports yet). Added to workspace members. `lattice-ui-tui` declares it as a dep (unused so far -- the seam is in place; 5.2 starts using it). Builds + 1592 ui-tui tests green; 0 lattice-host tests (intentional, the crate is intentionally empty). |
| 5.2            | 🟡 in progress | **Move pure HOST modules.** Everything classified HOST in the audit that doesn't transitively touch `theme.rs` or `chord.rs`. lattice-ui-tui re-exports the moved types via `pub use lattice_host::*;` so downstream consumers don't break. ~1-2 weeks, one or two modules per commit. **First wave shipped:** six trivial re-export shims (`buffers`, `file_tree`, `oil`, `popup`, `help`, `help_topics`) migrated -- established the migration pattern. **Second wave shipped:** `actions.rs` (1262 LoC) + `excommand.rs` (1332 LoC) migrated -- both pure leaves (zero `crate::*` references), depend only on `lattice-grammar` + `lattice-protocol` + `thiserror`. Their 60 tests moved with them. **Third wave shipped:** `host_generators.rs` (209 LoC) -- the registered-mode / registered-event / LSP-server / customize-name picker source generators. Pulls four more crates into lattice-host's dep list (`lattice-mode`, `lattice-picker`, `lattice-lsp`, `lattice-config`) since each generator enumerates state from one of those subsystems. Combined total stays at 1592. **Blocked next:** keymap family (~7,200 LoC) imports `crate::chord::KeyChord` -- needs the chord.rs neutral-types split (slice 5.4 pulled forward). |
| 5.3            | ⛔     | **Renderer-neutral theme.** `lattice_host::ui::Theme` with semantic styles + a `Color` enum carrying `Default`/`Named`/`Indexed`/`Rgb` variants. `App.theme` type changes. `lattice_ui_tui::tui_theme::TuiTheme` is the ratatui-typed adapter, cached + rebuilt on `:set ui.*`. Symmetric work in `lattice-ui-gpui` happens later. |
| 5.4            | ⛔     | **Split `chord.rs` + `input.rs`.** Renderer-neutral `KeyChord` + `dispatch_chord(ctx, chord) -> Option<Action>` move to host. Crossterm adapter `KeyChord::from_crossterm(&KeyEvent)` + `translate_event` shim stay in TUI. |
| 5.5            | ⛔     | **Pane render registry as trait.** `PaneRenderer` + `PaneStatus` traits in host; TUI provides ratatui-shaped impls. Registry shape unchanged. |
| 5.6            | ⛔     | **`lattice-render` crate.** `Renderer` trait, `Frame`, `InputEvent`, `LayoutConstraints`, `LayoutResult` -- pure type defs. TUI implements `Renderer`. `lattice-host::run(app, renderer)` is the new entry point. |
| 5.7            | ⛔     | **`lattice-cli` gains `--tui`.** Plumbed but default still TUI (GPUI doesn't exist yet). |
| 5.8            | ⛔     | **`lattice-ui-gpui` scaffold.** Hello-world window. |
| 5.9+           | ⛔     | **Real GPUI**: text rendering, atlas, panes, input, popups. Multiple sub-slices. |
| 5.last         | ⛔     | **Flip default to GPUI.** Separately scheduled, after parity + a release cycle as opt-in. |

**Decision-making heuristics applied (../architecture/CLAUDE.md):** Best long-term fit beats easy implementation -- extracting the host is months of work; the alternative (drop GPUI on top of the current entangled lattice-ui-tui) would compound the coupling that already needs unwinding. Confirm the plan before non-trivial work -- the 5.0 audit doc IS that confirmation step.

---

## live-picker (debounced re-run on query change)

**`:picker grep` and any future "engine produces the candidate set"
source** -- the source's external program (grep, future LSP
workspace-symbols, future shell-driven enumeration) re-runs on each
debounced keystroke instead of the picker fuzzy-filtering a single
inline batch. Anchor: ../architecture/design.md §5.9.7 ("Live
sources" subsection).

| Slice         | Status | What lands |
|---------------|--------|------------|
| live-pkr.1    | ✅     | **Trait + picker primitive seam.** `PickerSourceSpec.live: bool` (default false), `PickerSourceGenerator::on_query_changed(&ctx, &query) -> Option<SourceResult<PickerInitResult>>` (default `None`). `Picker.live_source_mode` + `set_live_source_mode(bool)`; `Picker::refilter` short-circuits when `live_source_mode == true` (renders `raw` 1:1, no fuzzy scoring, no MRU). `App::seat_picker_from_pairs` reads `entry.spec.live` and flips the picker into live mode. No production source opts in yet; all existing pickers bit-identical. Tests: `live_source_mode_bypasses_fuzzy_refilter`, `live_source_mode_off_keeps_existing_fuzzy_behaviour`. |
| live-pkr.2    | ✅     | **App-side debounce + cancellation.** `LivePickerQueryState` + `InFlightLiveQuery` on `App`, installed by `open_picker` when `entry.spec.live` is true. `LIVE_PICKER_DEBOUNCE = 150ms` constant. New methods: `bump_live_picker_debounce` (called from `PickerAppend` / `PickerBackspace` dispatch), `drain_pending_live_picker_query` (main-loop tick — fires `on_query_changed` on deadline expiry, pumps in-flight rx, stale-result detection via `launched_for_query` vs current `picker.query`). `do_picker_dismiss` cancels any in-flight task. Tested with synthetic `LiveStubSource` (registry sidestep — Arc-shared, so hand-installed `live_picker_query` state). Tests: keystroke→debounce→fire, burst-coalesce, dismiss-clears-state. |
| live-pkr.3    | ✅     | **`GrepSource` flips to live + optional initial pattern.** `spec.live = true`. `pattern` arg now `ArgDefault::None` — no-arg opens empty, arg seeds the prompt. `init()` returns `Inline(empty)` on no-arg, `Future(spawn_blocking(run_grep))` on initial-pattern (first grep no longer blocks UI). `on_query_changed()` returns `Inline(empty)` for empty/whitespace, `Future` for real queries. `spawn_blocking` because `run_grep` shells out via std-sync `Command::output` — running it on the async-runtime workers would pin a worker. Initial-query seed plumbing: `LivePickerQueryState.initial_query: Option<String>` set by `open_picker`, consumed (taken) by `seat_picker_from_pairs` so `:picker grep TODO` opens with `query = "TODO"` ready to extend. Tests: `grep_source_empty_args_returns_empty_inline`, `grep_source_on_query_changed_empty_short_circuits`, `grep_source_spec_is_live`, `open_picker_grep_no_args_installs_live_state`, `open_picker_grep_with_initial_pattern_stashes_query_until_seat`, `picker_grep_seeds_query_on_seat_when_initial_query_stashed`. |
| live-pkr.4    | ✅     | **Docs.** ../architecture/design.md §5.9.7 gains a "Live sources" subsection contrasting the v1 debounced-future shape against the future fully-streaming model; reasons why grep specifically fits Future-per-query rather than incremental Stream. Test-counts row updated. |

**Deferred (post-1.0):** result-cap echo when grep hits `max_hits` and truncates; in-row match-range highlights (the grep backends emit column info — `messages_line_spans`-style span-builder would work); per-source `is_live()` overrides for future sources that want live behaviour conditionally (e.g. workspace-symbols falls back to static when the server has no `workspace/symbol`).

---

## Vim grammar coverage (Phase 1 catalog)

This section enumerates every named primitive in vim's grammar against its
status here. Anchor: ../architecture/design.md §5.2 + the seven unifications in §5.10–§5.12.

### Modal states

| State              | Status           | Anchor    | Notes                                                                                                                                                                                                                                                                                                     |
|--------------------|------------------|-----------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Normal             | ✅               | §5.2      | Plus block cursor in TUI                                                                                                                                                                                                                                                                                  |
| Insert             | ✅               | §5.2      | Plus bar cursor                                                                                                                                                                                                                                                                                           |
| Visual (Charwise)  | ✅               | §5.2, B.1 | Selection extends, operators on Range::Selection                                                                                                                                                                                                                                                          |
| Visual (Linewise)  | ✅               | §5.2      |                                                                                                                                                                                                                                                                                                           |
| Visual (Blockwise) | ✅ d/y/c/I/A/>/< + paste | §15:18    | Ctrl-V (or Ctrl-Q on terminals that hijack Ctrl-V) enters; render highlights the rectangle; `d` / `y` / `c` dispatch per-row in the dispatcher with merged Edits + one Blockwise yank. `>` / `<` indent each covered line. `I` / `A` enter Insert at the block's left / right column on the top row; on Esc the typed prefix is replicated to every other row at the same column (rows shorter than the column are skipped, vim's behavior). `YankKind::Blockwise` paste replays each row at the same column. |
| Operator-Pending   | ✅               | §5.2      | Resolved through translate_normal pending state                                                                                                                                                                                                                                                           |
| Command (`:`)      | ✅               | §5.9.10   | Rich minibuffer scope is partial; full spec is post-Phase-1                                                                                                                                                                                                                                               |
| Search (`/`, `?`)  | ✅               | §5.9.10   | Live preview wired (hlsearch on every keystroke); fancy-regex backend                                                                                                                                                                                                                                     |
| Replace (`R`)      | ✅               | §5.2      | Backspace-restore wired                                                                                                                                                                                                                                                                                   |

### Motions (Reflex-class)

| Motion                            | Key            | Status | Anchor                                                |
|-----------------------------------|----------------|--------|-------------------------------------------------------|
| char_left / char_right            | h, l           | ✅     | §5.2.2                                                |
| line_up / line_down               | k, j           | ✅     | §5.2.2                                                |
| line_start / line_end             | 0, $           | ✅     | §5.2.2                                                |
| first_non_blank                   | ^              | ✅     | §5.2.2                                                |
| word_forward                      | w              | ✅     | §5.2.2                                                |
| word_backward                     | b              | ✅     | §5.2.2                                                |
| word_end                          | e              | ✅     | §5.2.2                                                |
| WORD_forward / backward / end     | W, B, E        | ✅     | Whitespace-delimited variants                         |
| paragraph_forward / backward      | }, {           | ✅     | §5.2.2                                                |
| sentence_forward / backward       | ), (           | ✅     |                                                       |
| goto_first_line / goto_last_line  | gg, G          | ✅     | §5.2.2                                                |
| find_char_forward / backward      | f, F           | ✅     | §5.2.2                                                |
| till_char_forward / backward      | t, T           | ✅     | §5.2.2                                                |
| find_repeat / find_repeat_reverse | ;, ,           | ✅     |                                                       |
| viewport_top / middle / bottom    | H, M, L        | ✅     | App-level (needs viewport_height)                     |
| word_search_forward / backward    | *, #           | ✅     | §B.3 informally                                       |
| match_bracket                     | %              | ✅     | App-level                                             |
| jump_history_back / forward       | Ctrl-O, Ctrl-I | ✅     | §5.1.1 unified ring (filtered to AutoJump+PluginPush) |
| mark_history_back / forward       | g;, g,         | ✅     | §5.1.1 unified ring (filtered to NamedMark)           |
| page_down / page_up               | Ctrl-F, Ctrl-B | ✅     | App-level                                             |
| scroll_line_up / down             | Ctrl-Y, Ctrl-E | ✅     | App-level                                             |
| half_page_down / up               | Ctrl-D, Ctrl-U | ✅     | Hardcoded count 10                                    |
| mark jumps                        | 'a, \`a        | ✅     | §5.1.1                                                |

### Operators (Reflex-class for sync prelude)

| Operator     | Key      | Status | Anchor                                     |
|--------------|----------|--------|--------------------------------------------|
| delete       | d, dd, D | ✅     | §5.2.2                                     |
| change       | c, cc, C | ✅     | §5.2.2                                     |
| yank         | y, yy, Y | ✅     | §5.2.2                                     |
| indent_left  | <        | ✅     | §5.2.2                                     |
| indent_right | >        | ✅     | §5.2.2                                     |
| upper        | gU       | ✅     | §5.2.2                                     |
| lower        | gu       | ✅     | §5.2.2                                     |
| toggle_case  | g~       | ✅     | §5.2.2                                     |
| filter       | !        | ⛔     | Subprocess pipe; depends on `:!cmd` (§B.6) |
| format       | gq       | ⛔     | Depends on plugin / formatter              |
| join_lines   | J, gJ    | ✅     | App-level (not a grammar operator)         |

### Text objects

| Text object              | Key       | Status | Anchor                                                                                                                                                                                                  |
|--------------------------|-----------|--------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| inner_word / around_word | iw / aw   | ✅     | §5.2.2; alphanum + `_` run                                                                                                                                                                              |
| inner_WORD / around_WORD | iW / aW   | ✅     | Whitespace-delimited; punctuation kept (`foo.bar` is one WORD). Shares `text_object_inner_word_class` / `_around_word_class` with `iw` / `aw` -- only the byte classifier differs (`is_big_word_byte`). |
| inner_quote_dbl / around | i" / a"   | ✅     |                                                                                                                                                                                                         |
| inner_quote_sgl / around | i' / a'   | ✅     |                                                                                                                                                                                                         |
| inner_quote_btk / around | i\` / a\` | ✅     |                                                                                                                                                                                                         |
| inner_paren / around     | i( / a(   | ✅     |                                                                                                                                                                                                         |
| inner_bracket / around   | i[ / a[   | ✅     |                                                                                                                                                                                                         |
| inner_brace / around     | i{ / a{   | ✅     |                                                                                                                                                                                                         |
| inner_angle / around     | i< / a<   | ✅     | `<…>` pair; reuses `text_object_inner_brackets` / `_around_brackets`. Both `<` and `>` resolve to the same target.                                                                                      |
| inner_tag / around       | it / at   | ✅     | XML/HTML tags                                                                                                                                                                                           |
| inner_paragraph / around | ip / ap   | ✅     |                                                                                                                                                                                                         |
| inner_sentence / around  | is / as   | ✅     |                                                                                                                                                                                                         |

### Counts, registers, ranges, marks, macros, dot-repeat

| Feature                                       | Status                                | Anchor         |
|------------------------------------------------|---------------------------------------|----------------|
| Numeric prefix counts (3w, 5j, 2dw, 2d3w)      | ✅                                    | §5.2.2         |
| Register prefix `"<reg>`                       | ✅                                    | §5.2.2         |
| Named registers `"a-z`, `"A-Z`                 | ✅ (uppercase replaces, append TBD)   | §5.2.2         |
| Numbered registers `"0-"9`                     | ✅ (storage; `"0` auto-populate TBD)  | §5.2.2         |
| Black hole register `"_`                       | ✅                                    |                |
| System / clipboard `"+`, `"*`                  | ⚠️ storage only; no clipboard wire-up  | §15 (deferred) |
| Last-yank `"0`                                 | ⚠️ readable; auto-populate TBD         |                |
| Ex-command ranges (1,5 / % / 'a,'b / patterns) | 🟡 % and CurrentLine work             | §5.2.2         |
| Marks (m / ' / \`)                             | ✅                                    | §5.1.1         |
| Mark for last visual (`'<`, `'>`)              | ⛔                                    |                |
| gv (reselect last visual)                      | ✅                                    |                |
| Macros (q, @, @@)                              | ✅                                    | §5.2.4         |
| Dot-repeat (.)                                 | ✅                                    | §5.2.4         |
| Insert-mode replay in dot-repeat               | ✅                                    | §5.2.4         |

### Search and substitution

| Feature                                    | Status                | Anchor                                                                                                  |
|--------------------------------------------|-----------------------|---------------------------------------------------------------------------------------------------------|
| `/` `?` `n` `N` regex search with wrap     | ✅                    | §5.9.10; backed by `fancy-regex` (DFA + bounded NFA fallback for backrefs)                              |
| `*` / `#` word-search                      | ✅                    | Word is regex-escaped before compile so literals containing `.` `*` `(` etc. don't trigger metachars   |
| Search highlight in buffer                 | ✅                    | §5.6.2                                                                                                  |
| `:s/PAT/REPL/[g]` substitute               | ✅                    | Full regex; replacement template uses `$1`/`${name}`/`$0` (regex crate / fancy-regex syntax, not vim's `\1`) |
| Pattern backrefs (`/(\w+).*\1/`)           | ✅                    | fancy-regex NFA path; bounded by 1M-iteration recursion limit                                            |
| Replacement backrefs (`:s/(a)(b)/$2$1/`)   | ✅                    | `$1` etc. via fancy-regex's `replace_all` template                                                       |
| Search-as-you-type live preview (hlsearch) | ✅                    | every match highlighted; persists after submit; compile errors silenced during preview                  |
| Substitute-as-you-type live preview        | ✅                    | ../architecture/design.md §5.9.10; magenta strike-through overlay on matches as the user types `:s/pat/repl/...`; honours `/g` flag and `%s` scope |
| Search cooperative cancellation            | ✅                    | ../architecture/design.md §5.2.5; search loops poll a `CancellationToken` per chunk + per match; flipped token returns `CoreError::Cancelled` |
| Per-search deadline timer                  | ⛔                    | the cancellation seam is in place; the deadline-flipper (Reflex < 2 ms) is the remaining piece          |

### Ex commands

Unification status (../architecture/design.md §5.2.1, §B.2): every ex-command is now a
registered `ExCommandSpec` peer of motions / operators / text objects.
The `:` parser front-end resolves aliases, looks up by name, calls the
spec's `parse_args` (or builds an `Args::List` directly for the
delimiter-syntax forms `:s/.../`, `:%s/.../`, `:g/.../`, `:v/.../`),
and dispatches through the unified `grammar::execute()`. Apply closures
emit `Effect` variants (`SaveBuffer`, `QuitEditor`, `OpenBuffer`,
`SetOption`, `Substitute`, `Global`, `Echo`, ...); `App::apply_effect`
owns the side effects.

**Kind-prefix form on `:` (§5.2.1 closure)** ✅. Every command --
motion, operator, text-object, ex-command, plugin contribution -- is
reachable from `:` by the `:<kind> <name>` form. Three kind words
(`motion`, `operator`, `text-object`) are reserved on the `:` line
and disambiguate the namespace; ex-commands keep their bare alias
surface unchanged.

```
:motion goto-first-line               # naked motion
:operator delete word-forward         # operator + bare target (motion)
:operator delete inner-word           # operator + bare target (text-object)
:operator delete motion:word-forward  # full canonical form (disambig)
:text-object inner-word               # errors helpfully
:write foo.txt                        # ex-command, vim alias surface
```

Operator targets resolve via implicit-namespace lookup: the bare tail
is tried as `motion:<tail>`, then `text-object:<tail>`, then as a full
canonical name. Plugin-registered names (e.g.
`motion:my-plugin:fancy-jump`) are reachable via
`:motion my-plugin:fancy-jump` -- the second-word colon is part of
the tail, not adjacent to the cmdline colon. See
`crates/lattice-ui-tui/src/excommand.rs::parse_kind_prefixed`.

**`:` surface invariant.** ../architecture/design.md §2.2 explicitly excludes a
function-call / palette / scripting syntax on `:`. The `:` line is
vim's ex-syntax DSL; code paths (plugins, `init.rs`, the Rust
functional API) construct `CommandInvocation` directly via the WIT
host. Two input surfaces, one dispatcher.

**Surface-form gating.** Each `ExCommandSpec` carries a
`surface_form: SurfaceForm` (`Keyword` or `Delimiter { hint }`).
Delimiter-form commands (`ex:substitute`, `ex:global`) are routed by
the parser's delimiter-detection pass; the keyword form
(`:ex:global`) returns `WrongSurfaceForm { name, hint }`, which
surfaces the intended syntax (`:g/pattern/body  (or :v/pattern/body
for inverted)`). The `gen:commands` completion generator filters
delimiter-form commands so the cmdline popup never offers a
candidate that would error when accepted. They remain reachable via
`:describe-command` / `:apropos` for introspection.

| Command                                           | Status                          | Anchor                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
|---------------------------------------------------|---------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| :w / :write [path]                                | ✅ registry                     | §5.2.1                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| :q / :q!                                          | ✅ registry                     | §5.2.1                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| :wq / :x / :wq! / :x!                             | ✅ registry                     | §5.2.1 (Effect::Many)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| :e / :edit [path] / :e!                           | ✅ registry                     | §5.2.1                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| :d / :delete                                      | ✅ registry                     | §5.2.1                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| :noh / :nohl / :nohlsearch                        | ✅ registry                     | §5.2.1                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| :reg / :registers                                 | ✅ registry                     | §5.2.1                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| :marks                                            | ✅ registry                     | §5.2.1                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| :set option=value                                 | ✅                              | §5.12. Typed-options registry in `lattice-config` (renderer-agnostic): `OptionType` trait + `Option<T>` with `ArcSwap<T>` cell + typed `OptionHandle<T>`. Core options (number, relativenumber, wrap, ignorecase, tabstop, foldenable, foldmethod, scrolloff, completion.auto_insert_single) register from `lattice-config::register_core_options`; renderer options register from each renderer crate. App integration: `do_set` ⇒ `config.parse_and_set_command` ⇒ `apply_post_set` for cascades (relativenumber ⇒ number, foldmethod ⇒ recompute, ui.* ⇒ theme refresh).      |
| :s/.../.../[g]                                    | ✅ registry (regex)             | §5.2.1 / §B.2; `Args::List([pattern, replacement, flags])`, scope via Range::CurrentLine/Whole. fancy-regex compile + per-line `replace_all`; replacement template uses `$1`/`${name}`/`$0`.                                                                                                                                                                                                                                                                                                                                                              |
| :g/pattern/cmd                                    | ✅ registry                     | §B.2; `Args::List([pattern, false, ArgValue::Invocation(body)])`. Body parsed once at `:g` parse time (no per-match re-parse); body syntax errors surface before `:g` fires.                                                                                                                                                                                                                                                                                                                                                                                                     |
| :v/pattern/cmd                                    | ✅ registry                     | §B.2; same shape as `:g`, inverted flag set.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| :describe-command                                 | ✅ buffer (popup)               | §5.11; renders `CommandSpec.doc` + each `args_schema` entry's name/kind/doc/default.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| :describe-buffer                                  | ✅ buffer (popup)               | §5.11; path / language / modal / cursor / dirty / line-count / registers / marks / position-history / macros / folds / view options.                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| :describe-key <chord>                             | ✅ buffer (popup)               | §5.11; renders every `KeymapEntry` for the chord through the unified `Introspectable` surface. Each binding shows `Bound at: [[file:keymap.rs:LINE]]` (built-in) -- the row's source captured by the `keymap_entry!` macro -- plus an Action: `[[command:...]]` cross-reference. The `chord` arg uses `ArgKind::Chord`: typing `:describe-key <space>` puts the cmdline into chord-capture mode (raw key events render as canonical tokens -- `Ctrl-c` -> `<C-c>`, `Up` -> `<Up>`); `:describe-key<CR>` with no arg arms a one-shot prompt and the very next chord auto-submits. |
| :keymap                                           | ✅ buffer (popup)               | §5.11; lists all default bindings grouped by mode, every chord linked via `[[key:...]]` for follow-up `:describe-key`.                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| :apropos <pattern>                                | ✅ buffer (popup)               | §5.11; case-insensitive substring over every `CommandSpec.name` + `doc`. Picker UI (§5.9.7) is post-1.0.                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| :describe-option                                  | ✅                              | §5.11 / §5.12. Reads from `lattice-config::ConfigRegistry`'s `ErasedOption` view: name, aliases, type label, current value, default value, enumerated values (where applicable), doc body. Markdown-rendered into a help buffer.                                                                                                                                                                                                                                                                                                                                                |
| :describe-event, :describe-mode                   | ⛔                              | §5.11; each lands when its registry does (event bus §5.10 / modes Phase 8).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| Command-line history (Up/Down)                    | ✅                              | §B.3                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| :history-*                                        | ⛔                              | §B.3 (picker UI; Up/Down already works)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| :customize                                        | ⛔                              | §5.12                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| :autocmd / :add-hook                              | ⛔                              | §5.10                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |

---

## Introspection architecture (../architecture/design.md §5.11)

Help is **buffer-backed from day one**, modeled after emacs's `*Help*`.
A `HelpBuffer` (in `lattice-ui-tui::help`) holds a real
`lattice_core::Buffer` (rope) plus the title, scroll offset, a real
**cursor** (`Position`), and an extracted `Vec<HelpLink>`. The popup
overlay treats the help buffer as a buffer: `j` `k` `h` `l` `0` `$`
`gg` `G` `Ctrl-D` `Ctrl-U` move the cursor and scroll auto-follows.
The terminal cursor is rendered at the screen translation of
`help.cursor` so the user sees their position. **`HelpDisplayMode` enumerates
Popup / Split / Tab / Window** for the per-user display preference;
the popup overlay is one strategy and stays available for transient
surfaces (hover, future doc lookups, error toasts).

Help also lives in the unified [`BufferRegistry`] as
`BufferData::Help(HelpBuffer)`. `App::open_help_in_pane(buffer)`
allocates a `BufferId`, inserts the registry entry, and swaps the
active pane to it -- the in-pane counterpart to `App::open_help`
(popup). The registry holds the durable record (`:ls` / `:bn` /
picker discovery); `App.help_buffer` mirrors the active in-pane
copy so the existing keymap + render paths stay single-path.
Pane-switch hooks (`snapshot_active_pane` / `load_active_pane`)
sync the two at boundaries, mirroring the Document buffer pattern
(`syntax`/`folds` snapshots). De-dup is by title -- re-running
`:lsp-log rust` surfaces the existing buffer rather than allocating
a duplicate. Persistent help views (LSP logs, `:diagnostics`,
`:apropos`, `:describe-*`) migrate to this path in Phase 3
alongside the picker (Phase 2).

### Provenance (§5.11.1)

Every registration / binding / set captures a `SourceLocation`
(`lattice-grammar::source`). `:describe-*` output always includes a
`[[file:...]]` link to where the thing came from -- vim's
`:verbose set` semantics, applied uniformly across commands, keys,
options, events, modes.

| Capture mechanism                           | Used for                                                                                    | Status                                        | Forgery resistance                                                                                        |
|---------------------------------------------|---------------------------------------------------------------------------------------------|-----------------------------------------------|-----------------------------------------------------------------------------------------------------------|
| `#[track_caller]` on `register_*`           | built-in command registrations in `builtins.rs` / `ex_commands.rs` -- zero call-site burden | ✅ implemented                                | compiler-captured, caller cannot supply or override                                                       |
| `keymap_entry!` declarative macro           | static keymap rows (per-row `file!()` + `line!()`)                                          | ✅ implemented                                | macro is the only construction path; `source` field is `pub(crate)`                                       |
| Trusted subsystem builds value              | config loader, plugin host bridge, runtime dispatcher                                       | ⛔ deferred (no trusted subsystems exist yet) | `pub(crate) insert_*` registry methods exist; sibling crates will use sealed-trait re-exports when needed |
| `SourceLocation::synthetic` (cfg-test only) | test fixtures                                                                               | ✅ implemented                                | invisible outside tests                                                                                   |

**Public API invariant**: there is no `pub fn` that takes a
`SourceLocation` parameter and stores it. Verified by:
- The `register_*` methods are all `#[track_caller]` and don't
  accept a source.
- The `pub(crate) insert_*` companions are visibility-gated to
  `lattice-grammar`.
- The `keymap_entry!` macro is the only path that constructs a
  `KeymapEntry`; its `source` field is `pub(crate)`.

**Determinism**: a unit test (`track_caller_captures_register_motion_call_site`) registers a sentinel at a known line and asserts the captured `SourceLocation` matches exactly. Six related tests cover propagation through `#[track_caller]` helpers, the negative case where unmarked helpers shift the captured line inward, and per-row line distinguishing across adjacent registrations. Any refactor that breaks call-site capture (a `dyn Fn` dispatcher around `register_*`, removed `#[track_caller]` on a helper in the chain) fails CI.

### Generic renderer (§5.11.2)

Every `:describe-*` target implements `Introspectable`
(`lattice-grammar::introspect`). One generic
`render_introspection(&dyn Introspectable) -> Vec<String>` produces
the help body in a uniform shape: identifier + kind + doc +
type-specific `extra_sections` + one `[[file:...]]` link per labelled
source (`Defined at:`, `Bound at:`, `Subscribed at:`, `Last set at:`,
`Overridden at:`, `Activated at:`). Adding a new introspection
target -- when typed options / events / modes land -- is one trait
impl plus one new ex-command; the renderer doesn't change.

### Completion pipeline (§5.11.3)

`lattice-completion` is a standalone crate with its own test corpus
(81 tests) wired into the `:`-line via the App's
`completion_registry` + `completion_state`. Four pluggable stages,
vertico-shaped:

| Stage      | Trait                | Built-ins                                                             |
|------------|----------------------|-----------------------------------------------------------------------|
| Generation | `CandidateGenerator` | `gen:commands`, `gen:files` (host-state generators in lattice-ui-tui) |
| Matching   | `CandidateMatcher`   | `match:prefix` (default), `match:substring`, `match:fuzzy`            |
| Ranking    | `CandidateRanker`    | `rank:score` (default), `rank:alphabetical`                           |
| Annotation | `CandidateAnnotator` | `anno:kind-label`, `anno:doc-snippet`                                 |

`CompletionRegistry` registers each stage with `#[track_caller]`
provenance. `CompletionPipeline::run(ctx, query, cache)` walks the
four stages and returns a `Vec<RenderedCandidate>` for the renderer.
Slot detection (`current_slot`) parses the `:`-line into
`CommandLineSlot::CommandName`, `Arg { command_name, arg_index,
arg_spec, .. }`, `DelimiterBody { command_name, body }`,
`UnknownCommand`, `BeyondSchema`, or `Empty`.

**Caching** is opt-in per generator via `cache_key()` returning
`Option<CacheKey>`. `gen:commands` returns a fixed `"gen:commands:v1"`
key (commands don't change at runtime in v1, so cached effectively
forever); `gen:files` keys per-directory with a 1-second TTL.

**Composability** is structural: every stage is a trait, plugins
register impls against the same registry as built-ins, the
default matcher / ranker / annotators are configurable per-user
(`cmdline.matcher = "match:fuzzy"` post-§5.12). The pluggable
shape mirrors emacs's `vertico` / `orderless` / `marginalia` /
`consult` -- composability by design, not retrofit.

**Forgery resistance** mirrors the §5.11.1 invariant: no public
API takes a `SourceLocation` parameter. `register_*` are all
`#[track_caller]`; `pub(crate) insert_*` companions exist for
trusted subsystems (config loader, plugin host bridge) but
deferred until first cross-crate trusted subsystem lands.

The crate doesn't depend on `lattice-ui-tui`, so it's tested
independently. CI runs `cargo test -p lattice-completion` as its
own line item.

**App integration:**

| Capability                               | Status      | Notes                                                                                                                                                                                                                     |
|------------------------------------------|-------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `<Tab>` opens completion popup           | ✅          | Slot-aware: command-name slot uses `gen:commands`; arg slot uses `arg_spec.completion`.                                                                                                                                   |
| `<Tab>` advances candidates while open   | ✅          | wraps at end                                                                                                                                                                                                              |
| `<S-Tab>` moves backward                 | ✅          | wraps at start                                                                                                                                                                                                            |
| `<CR>` accepts selected                  | ✅          | replaces `[replace_start, end)` with the candidate's `text`                                                                                                                                                               |
| `<Esc>` two-stage dismiss                | ✅          | first Esc closes popup; second cancels cmdline                                                                                                                                                                            |
| Typing dismisses popup                   | ✅          | re-trigger with Tab for fresh candidate set                                                                                                                                                                               |
| `<C-h>` describe under cursor            | ✅          | hybrid: word-at-cursor describes self if registered; else parent command at `arg:<name>` anchor                                                                                                                           |
| `<C-u>` clear cmdline                    | ✅          | also dismisses popup                                                                                                                                                                                                      |
| `<C-w>` delete trailing word             | ✅          | strips whitespace then last token                                                                                                                                                                                         |
| Vertico-style popup render               | ✅          | bordered, anchored BELOW the `:` prompt (cmdline shifts up to make room), selected row highlighted, matched byte ranges painted, annotations right-aligned                                                                |
| Alias-preferred candidate text           | ✅          | `gen:commands` returns canonical names (`ex:describe-command`); host post-process rewrites to the user-facing alias (`describe-command`) and recomputes match ranges. Parser accepts both forms via two-stage resolution. |
| Single-candidate auto-insert             | ✅          | `:set completion.auto_insert_single` (default on). When `<Tab>` would open a popup with exactly one candidate, applies it directly instead. Fires only at popup-open boundary; narrowing an open popup to one candidate while typing does NOT auto-insert (vim-style; less surprising). Phase 4.2 (#199) should reuse `App::open_completion_popup` (or factor a shared helper) when wiring Insert-mode / LSP completion so this option stays universal without a second knob. |
| Help overlay dismissed when entering `:` | ✅          | Q16: user can only focus on one thing                                                                                                                                                                                     |
| `<C-b>` / `<C-e>` cursor movement        | ⛔ deferred | needs full cmdline-cursor refactor; v1 cursor stays at end                                                                                                                                                                |
| `<C-r>` register paste                   | ⛔ deferred | needs `Pending::AfterCommandLineRegister` substate                                                                                                                                                                        |
| Completion inside `:s/.../.../` body     | ⛔ deferred | DelimiterBody slot returned; renderer doesn't recurse into pattern/replacement/flags                                                                                                                                      |

**ArgSpec.completion**: `Option<&'static str>` field naming a registered
generator. v1 wires `gen:files` for `:write`/`:edit` paths and
`gen:commands` for `:describe-command`'s name arg. Adding `gen:options`
when typed options land is one schema edit per command.

The default matcher (`match:fuzzy`) is set at registry construction;
users / configs can swap to `match:prefix` or `match:substring` once
typed options land via `cmdline.matcher = "match:prefix"`.

Link markup -- forward-compatible reference syntax in help bodies:

| Markup               | Resolution                           |
|----------------------|--------------------------------------|
| `[[command:NAME]]`   | re-dispatch `:describe-command NAME` |
| `[[key:CHORD]]`      | re-dispatch `:describe-key CHORD`    |
| `[[file:PATH:LINE]]` | open PATH at LINE                    |
| `[[anything-else]]`  | Unresolved (preserved verbatim)      |

The popup renderer is dumb today: links render verbatim. The
follow-link motion + styled link ranges + `[[file:...]]` source
navigation arrive incrementally:

| Capability                               | Status | Notes                                                             |
|------------------------------------------|--------|-------------------------------------------------------------------|
| Buffer-backed help (rope content)        | ✅     | `HelpBuffer.content: Buffer`                                      |
| Link markup defined + parsed             | ✅     | `parse_help_links` returns `Vec<HelpLink>` with byte ranges       |
| Help formatters emit links               | ✅     | `:describe-key`, `:apropos`, `:keymap` reference cross-targets    |
| Display: Popup overlay                   | ✅     | transient surface (hover, doc lookups); reachable via `App::open_help` |
| Display: In-pane (registry-tracked)      | ✅     | `BufferData::Help` + `App::open_help_in_pane` + `activate_buffer` Help arm; call-site migration to in-pane (`:lsp-log`, `:describe-*`, `:diagnostics`) follows in Phase 3 |
| Display: Split / Tab / Window            | 🟡     | in-pane lands with active pane today; multi-pane (split into a sibling pane) follows the picker / Phase 3 |
| Vertico-style picker primitive (§5.9.7)  | ✅     | `lattice-ui-tui::picker::Picker` -- query line + substring filter + selection cursor + `PickerAction` accept tag. **Renderer-agnostic data model**: the only imports are stdlib + `lattice_completion`; host-coupled candidate builders live on the host side (the buffer-source builder is `app::raw_buffer_candidates`; the LSP-source builder is `LspInstanceRow::into_candidate` fed by host-snapshotted rows). `Picker::set_raw_candidates` is the single mutation entry point. Drops into a sibling crate `lattice-picker` with no source edits when a second renderer needs it. Sources: `Buffers`, `LspInstances { prefilter }`. First instantiations: `:b` buffer switcher; `:lsp-log` / `:lsp-server-log` / `:lsp-trace-log` LSP picker (Phase 3). Layout: vertico-style (query takes over cmdline row; candidates render in band immediately below, selected row closest to prompt, no border). Live preview-in-pane for `SwitchToBuffer` (selection-change activates the buffer in active pane without pushing position history; `<Esc>` restores `preview_origin`). Pipeline-driven matcher / ranker / annotators graduate the picker in a follow-up. |
| Cmdline tab-completion: vertico-styled   | ✅     | `:` line tab-completion popup converged with the picker's visual shape. No bordered box / title; candidates render flush in the row band below the cmdline, reusing `candidate_to_line`. The cmdline appends a faint `(n/m)` count suffix when the popup is open, mirroring the picker prompt's inline indicator. |
| LSP picker -- log / trace dispatch       | ✅     | `:lsp-log [server]`, `:lsp-trace-log [server]`, `:lsp-server-log` route through `App::open_lsp_picker` (Phase 3). Single-match short-circuit opens directly; multi-match opens the picker pre-filtered. Opened buffer goes through `open_help_in_pane`. `:lsp-trace` is now a pure toggle (no buffer-open side-effect). |
| LSP log buffers live-tail (Phase 4)      | ✅     | `Event::LspLogPushed` fires on every `LspLogger::log` append; `App::drain_lsp_log_events` (called per main-loop tick) refreshes any open `*lsp*` / `*lsp:<server>*` / `*lsp:<server>:trace*` help buffer from a fresh logger snapshot. Coalesces by scope so a burst of N records resolves to one rebuild per affected scope. Cursor + scroll preserved across the rebuild (clamped to new line bounds); popup hot-path mirror synced. |
| LSP server-name resolution                | ✅     | `:lsp-trace`, `:lsp-log`, `:lsp-trace-log` accept either the canonical actor id (`rust`) or the binary name the user recognises (`rust-analyzer`); resolution priority: running actors -> registered config ids -> config binary file-name (with `.exe` stripped). Unknown names echo the running id list so the user can disambiguate. |
| Pane-aware viewport height                | ✅     | `App::active_pane_content_height(buffer_height)` returns the active pane's content rect (subtracting the per-pane status row in multi-pane mode). Runtime feeds it into `set_viewport_height` so motions / scroll / fold-aware ensure-cursor-visible agree with what the renderer paints. Without this, horizontal/vertical splits clipped the lower / right half of the active pane. |
| `K` / `gd` registered in keymap registry  | ✅     | The input translator already dispatched `K` -> `LspHoverRequest` and `gd` -> `LspDefinitionRequest`, but the keymap registry didn't list them, so `:describe-key K` / `:apropos hover` came back empty. Added entries in `keymap::build_default_keymap`. |
| Styled link ranges in renderer           | ⛔     | renderer ignores `links` today                                    |
| Follow-link motion (e.g. `<CR>` on link) | ⛔     | needs tree-sitter help grammar + link motion                      |
| Help major mode + tree-sitter grammar    | ⛔     | post-Phase-3-extension; sections / code-blocks / link-targets     |
| `SourceLocation` on `CommandSpec`        | ⛔     | needs `register_*` API extension; powers `[[file:...]]` auto-emit |
| `:source-of <command>`                   | ⛔     | depends on `SourceLocation`                                       |
| `:describe-key`                          | ✅     | keymap registry §5.2.3 -- see below                               |
| `:keymap`                                | ✅     | full default keymap, grouped by mode                              |
| `:describe-option`                       | ⛔     | needs typed options registry §5.12                                |
| `:describe-event`                        | ⛔     | needs event bus §5.10                                             |
| `:describe-mode`                         | ⛔     | needs major/minor modes (Phase 8)                                 |

### Keymap registry (../architecture/design.md §5.2.3)

`KeymapEntry { chord, mode, doc, command }` in `lattice-ui-tui::keymap`,
populated as a `&'static [KeymapEntry] DEFAULT_KEYMAP` covering every
chord in `input.rs`. v1 is a *descriptor table* the introspection layer
queries; the input layer (`input.rs::translate`) still owns the
chord-to-Action translation. A drift test
(`keymap_descriptors_dont_drift_from_translate`) walks every descriptor
through `translate()` and asserts a non-`None` Action -- catches
removed/moved bindings before they ship.

Registry-driven dispatch (the input layer *consuming* the keymap
registry rather than running a parallel `match`) is post-1.0 -- it
needs the layered keymap walker (built-in / major-mode / minor-modes /
user / per-buffer) from §5.2.3. The descriptor table is the migration
seed.

---

## Async / actor architecture (../architecture/design.md §5.2.1, §5.6.8, §5.7)

The async core lands in `lattice-runtime`. Every document is owned by
its own tokio task (the **document actor**); mutations route through a
bounded mpsc mailbox; commits publish an immutable `DocumentSnapshot`
to an `arc_swap::ArcSwap` cell that any reader (renderer, future LSP
client, future plugin) loads wait-free.

`lattice_grammar::execute` stays a pure sync function (no tokio
dependency, no async signature). The actor calls it *inside* its own
task via the `Dispatch` message; only the *boundary* between caller
and document is async.

App talks to the actor through `DocumentHandle` (cheap Clone -- an
`mpsc::Sender` + `Arc<PublishedSnapshot>`). The TUI input loop, which
is a blocking crossterm poll, bridges sync→async via
`lattice_runtime::block_on`. Every mutating method returns
`Pending<T>` (a typed wrapper around `oneshot::Receiver`); the App's
`*_blocking` helpers concentrate the bridge in one place.

**Publish-before-reply.** The actor's run loop publishes the new
snapshot *before* sending the `oneshot` reply for any mutation. This
guarantees that any caller observing the reply also observes the new
snapshot via `arc_swap::load` -- without this ordering, callers can
race past their own commit.

| Concern                                                    | Status               | Anchor                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
|------------------------------------------------------------|----------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Document actor / bounded mpsc mailbox                      | ✅                   | §5.7 (`lattice-runtime::DocumentActor`)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| `Pending<T>` returned by every mutating call               | ✅                   | §5.2.1 (`lattice-runtime::Pending`)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| Bounded backpressure (`RuntimeError::Busy`)                | ✅                   | §5.2.1 (mailbox cap = 64)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| `arc-swap` published `DocumentSnapshot`                    | ✅                   | §5.6.8 (`PublishedSnapshot`)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| Renderer reads via single snapshot load per frame          | ✅                   | §5.6.8 (`render::draw_*`)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| Publish-before-reply ordering                              | ✅                   | §5.6.8 (acquire/release contract)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| Sync `lattice_grammar::execute` (runs inside actor)        | ✅                   | §5.2.1 (purity preserved)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| Latency-class declarations (Reflex / Display / Background) | ✅ declarative       | §5.2.5 (`LatencyClass` field on `CommandSpec`; runtime enforcement deferred)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| Cancellation token contract                                | ✅ user-Esc          | §5.2.5; `CancellationToken` (Arc<AtomicBool>) plumbed through `dispatch_with_cancel` → grammar dispatcher → operator/motion/text-object contexts → search loops. Deadline-timer flipper (Reflex < 2 ms, Display < 10 ms) is the remaining piece.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| Event bus (observation baseline)                           | ✅                   | §5.10; `EventBus` in lattice-runtime: kind-indexed dispatch, `SubscriptionTarget::Channel` (mpsc) + `Invocation` (queued via `drain_pending_invocations`).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| App-side event publish                                     | ✅                   | §5.10; App publishes `DocumentChanged` (apply_edit / batch / undo / redo), `SelectionsChanged` (set_selections), `ModalModeChanged` (only on actual axis movement), `BeforeSave` + `DocumentSaved` (sync wrapper around save / save_as), `BeforeQuit` (Action::Quit + `:q` after dirty-check), `OptionChanged` (every typed-options registry write — including `:set foo=bar`, `:set nofoo`, and direct `config.set` paths; carries canonical name + old + new formatted strings).                                                                                                                                                                                                                                                                          |
| Config → event bus bridge                                  | ✅                   | §5.10 + §5.12; `lattice-config::ConfigRegistry` exposes `set_event_publisher(EventPublisher)`. App wires the bus at boot via a closure that calls `event_bus.publish(event)`. Subscribers see option changes through `Event::OptionChanged` instead of polling.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| App-side cascade via bus subscription                      | ✅                   | §5.10 + §5.12. App subscribes a `tokio::sync::mpsc::UnboundedReceiver<Event>` filtered to `EventKind::OptionChanged` at boot; `App::drain_option_changes` consumes it and runs the per-option cascade (`relativenumber⇒number`, `foldmethod⇒recompute_folds`, `ui.*⇒sync_theme_from_config`). Drained at the end of `do_set` (synchronous user-visible behaviour preserved) and at the top of every main_loop iteration (backstop for writes outside the keystroke path -- plugin tasks, customize buffer, init.rs). The chained cascade case (`relativenumber⇒number` itself fires another `OptionChanged`) is handled by the drain's `while let Ok` loop. No registry-mutex re-entrancy risk: publisher closure runs after the registry drops every lock. |
| Veto-class hooks (1ms p99)                                 | ⛔                   | §5.2.1 (needs Before-event return-path so handlers can mutate / abort; v1 publish is observation-only)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| Events-over-invocation rule                                | ⛔                   | §5.2.5 (needs `:autocmd` and `add-hook` parser front-ends to desugar into `subscribe`)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| Interactive arg-prompts (§B.1 phase 2)                     | ✅                   | Submitting bare `:cmd<CR>` with a Required first arg arms a prompt: prefills `:cmd `, surfaces the schema's prompt in the echo area, and waits for typed input (Chord-kind args additionally auto-submit on the next captured chord). Optional-default args take the parser's normal path.                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| Multi-pane selection transformation                        | n/a (single-pane v1) | §5.6.8                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |

This is **Phase 4 / 7's prerequisite** — LSP clients and the WASM
plugin host can now share `DocumentHandle` with the App; both
register against the same actor, both observe the same snapshot
stream. The remaining ⛔ rows (cancellation, latency classes, hook
classification, event bus) layer on top of the actor without
restructuring it.

---

## Performance posture

| Concern                                     | Status | Anchor                                          |
|---------------------------------------------|--------|-------------------------------------------------|
| Criterion bench harness                     | ✅     | §8.2                                            |
| Render hot-path is viewport-bounded         | ✅     | §8.2 (this commit)                              |
| Actor / runtime benches                     | ✅     | §5.6.8 / §8.2                                   |
| `LatencyClass` declaration on `CommandSpec` | ✅     | §5.2.5                                          |
| Test + clippy CI gate                       | ✅     | (.github/workflows/ci.yml)                      |
| Bench-compile CI gate                       | ✅     | (catches bench rot)                             |
| Bench baseline recording (push to main)     | ✅     | (artifact upload, no diff yet)                  |
| Bench regression detection (>10% threshold) | ⛔     | §8.2 -- needs stable runner                     |
| Per-class budget assertions in CI           | ⛔     | §5.2.5 -- needs cancellation/deadline machinery |
| Allocation discipline check in CI           | ⛔     | §A.6                                            |
| Long-running session benches                | ⛔     | §A.6                                            |
| Cross-platform acceptance suite             | ⛔     | §A.6                                            |

**Render hot path.** `compose_visible_lines` previously did
`buffer.as_string().split('\n').collect::<Vec<String>>()` once per
frame -- O(buffer size) bytes per paint, blowing the §8.2 <2ms frame
budget on any non-trivial buffer. Now uses ropey's O(log n) per-line
API via `Buffer::line(idx)` and materializes only the visible
window (`height` lines, typically 50). 100MB log files now pay the
same per-frame cost as 100-line files.

**Actor benches** (`crates/lattice-runtime/benches/actor.rs`)
characterize the load-bearing async primitives:

| Benchmark                                      | DESIGN target (p99) | Observed (median)                       |
|------------------------------------------------|---------------------|-----------------------------------------|
| `apply_edit` round-trip                        | <100µs              | ~80µs (constant across 10/1k/50k lines) |
| `snapshot_load` (`load_full`)                  | <20ns               | ~17ns (Arc bump path)                   |
| `snapshot_load_cached` (`Cache::load`, steady) | <500ps              | ~305ps                                  |
| `snapshot_post_publish_read`                   | --                  | ~17ns                                   |

The `apply_edit` round-trip meets §8.2 with 20% margin. The renderer
now reads through `arc_swap::Cache::load` per frame (~50× faster than
`load_full`) -- `App` holds a `SnapshotCache` rebuilt on each
document switch, the runtime calls `app.snapshot_cache.load_arc()`
once per frame in `runtime.rs`, and the resulting
`&DocumentSnapshot` is threaded through the active-pane render path
(`compose_visible_lines`, `cursor_screen_position`,
`closed_fold_display_span`, `buffer_line_to_visible_row`,
`draw_mode_line`). Inactive panes render different documents and
still go through `entry.handle.snapshot()` (`load_full`); a per-doc
cache map for inactive panes is queued behind a profiling motivator.
The 50+ `app.document.snapshot()` call sites in keystroke handlers
remain on `load_full` -- each runs ≤1× per keystroke, dominated by
other work, so the migration there is deferred.

**`LatencyClass` declaration** (../architecture/design.md §5.2.5) is now a field on
every `CommandSpec`. `:describe-command` surfaces it under a
"Latency:" section. v1 is purely declarative; the cancellation /
deadline machinery that enforces it lands with the §5.10 event-bus.
Default classifications:

- **Reflex** (<2ms p99): every motion, operator, text-object; cheap
  ex-commands (`:quit`, `:noh`, `:set`, `:delete`, `:s`, `:g`,
  `:v`).
- **Display** (<10ms p99 sync prelude): file I/O (`:write`,
  `:write-quit`, `:edit`); help-buffer builders (`:reg`, `:marks`,
  `:describe-*`, `:apropos`, `:keymap`).
- **Background**: none yet -- the indexer / file-watcher / LSP
  debounce paths arrive in Phases 4-7.

**CI** (`.github/workflows/ci.yml`) gates every push/PR on:

- `cargo test --workspace --locked` + `cargo clippy --workspace
  --tests --locked -- -D warnings` across a cross-platform matrix
  (ubuntu-latest, macos-latest, windows-latest; `fail-fast: false`).
- `cargo fmt --all -- --check` -- rejects unformatted code.
- `cargo doc --no-deps --workspace` with `RUSTDOCFLAGS=-D warnings`
  -- catches broken intra-doc links before merge.
- `cargo bench --workspace --no-run` per platform -- bench-compile
  rot detection.
- `bench-baseline` on push-to-main runs the benches in `--quick`
  mode and uploads the criterion reports as artifacts. Groundwork
  for the regression gate when stable bench infrastructure
  (self-hosted runner or `bencher.dev`) lands.

Current bench coverage: motions (word_forward / backward / end /
first_non_blank / counted), operators (dw / dd / d_whole / yw / cw / diw /
di_paren), search (forward first / last / no-match-with-wrap / backward),
buffer (insert at origin / middle, delete one byte, position round-trip),
runtime actor (apply_edit round-trip / snapshot_load /
snapshot_post_publish_read at 10/1k/50k lines).

---

## In-progress

**Phase 4.2 navigation -- 9/12 shipped + 1 partial.**
**Phase 4.3 -- 3/9 shipped.**

- ✅ hover (`K`), definition (`gd`), declaration (`gD`),
  typeDefinition (`gy`), implementation (`gI`), references
  (`gr`), documentSymbol (`:lsp-symbols`), workspaceSymbol
  (`:lsp-workspace-symbol`).
- 🚧 completion (`:complete` picker bridge -- buffer-level
  Insert-mode completion shell + snippet expansion + lazy
  resolve queued behind it).
- ✅ formatting + rangeFormatting (`:format` / `:format-range`).
- ✅ signatureHelp via `:signature-help` + Insert-mode
  trigger-char autopilot (typing `(` / `,` etc. fires the
  request automatically).
- ✅ rename + prepareRename via `:rename <name>` (alias
  `:rn`). Active buffer applies as one undo unit; cross-file
  edits open via `:e` then apply per-file. WorkspaceEdit
  flattening covers both legacy `changes` map and modern
  `document_changes` shape.
- ✅ willSave / didSave notifications fan out from
  `App::save_blocking`. Each attached server advertising
  the matching capability gets a fire-and-forget
  notification; didSave attaches the post-save rope text
  when `includeText` is set. willSaveWaitUntil typed
  wrapper exists; the App-side block-on-response
  (format-on-save) is queued.
- ✅ `:code-actions` (`:ca`) -- vertico picker over LSP
  code actions; resolves lazy `edit` via
  `codeAction/resolve` when the action arrived without
  inline edit + command; routes Command payloads through
  `workspace/executeCommand`. Edits land via the rename
  apply path (per-file one-undo-unit).
- ✅ onTypeFormatting Insert-mode autopilot. Typing a
  server-advertised trigger character fires
  `textDocument/onTypeFormatting`; edits apply via the
  format channel.
- ✅ willSaveWaitUntil format-on-save: `App::save_blocking`
  blocks for up to 500ms per server collecting pre-save
  edits, applies them as one undo unit, then writes to
  disk. Buggy / slow servers can't hang the save (token
  + timeout).
- Multi-result lookups + `:diagnostics` route through one
  vertico picker (`PickerSource::LspLocations` +
  `PickerAction::JumpToLspLocation`); single-result nav still
  jumps directly per vim convention.
- The four nav flavours share `do_lsp_nav_request(LspNavKind)`
  -- one dispatch path; the kind selects the LSP method and
  drives the kind-aware echo verb ("no implementations found"
  vs. "no definitions found").
- ✅ Tag stack: `gd` family + multi-result picker accept push
  onto `App.tag_stack`; `<C-t>` pops. Distinct from the jump
  list (`<C-o>`/`<C-i>`); the two have different push semantics
  and may have different lengths.
- ✅ Jump list propagation audited: every LSP nav, picker
  accept, search submit, `n`/`N`, help-link follow records
  `(BufferKind, BufferId, Position, source)` so cross-buffer
  walks just work. The previous `active_buffer == Document`
  gates on search submit + repeat search are gone.

Remaining 4.2:
- **Buffer-level Insert-mode completion** -- design spec at
  [`../architecture/insert-completion.md`](../architecture/insert-completion.md). 4.2.g.1
  (shell + buffer-words + popup widget + minor-mode keymap)
  ✅; 4.2.g.2 (LSP source + isIncomplete refresh + typed
  routing payload via `CandidateData::Extension` /
  `App.insert_completion_lsp_meta` sidecar) ✅; 4.2.g.3
  (docs side popup + lazy `completionItem/resolve` +
  `<C-f>` / `<C-b>` paging) ✅; 4.2.g.4 (`lattice-snippet`
  crate -- TextMate JSON parser + render walker + variable
  context + active-snippet state machine + friendly-snippets
  compat; host integration -- `gen:snippet` source, accept
  routing for snippet candidates AND LSP `insertTextFormat ==
  Snippet` items, `<C-x><C-s>` direct expand,
  active-snippet minor mode for `<Tab>` / `<S-Tab>` / `<Esc>`,
  `:snippet-expand` / `:reload-snippets` ex-commands) ✅;
  4.2.g.5 sliced 3 ways for landing -- (1/3) frequency
  ranking (App-side `(text, kind) -> u32` accept map bumped
  in `do_completion_accept`; ranker threads a host-supplied
  lookup closure through, capping the bonus at +50; all
  three refilter sites swapped over) ✅; (2/3) per-source
  priority (RawCandidate gains a `source: Option<SourceId>`
  field; `BufferWordsSource::produce` self-tags, host tags
  the snippet + LSP candidates with constants
  `SNIPPET_SOURCE_ID` / `LSP_COMPLETION_SOURCE_ID`; ranker
  surface renamed `rank_with_frequency` -> `rank_with_bonus`
  taking a single host-composed closure; three new typed
  options `completion.source.{lsp,snippet,buffer-words}.priority`
  with defaults 200 / 150 / 100 per spec §3.4; App's
  `priority_for_source` reads them and the closure adds
  `priority + freq.min(50)` for every candidate;
  unknown-source candidates get 0 priority gracefully) ✅.
  (3a/3) bare TOML loader infrastructure
  (`lattice-config::loader`: walks user config
  `~/.config/lattice/lattice.toml` and project config
  `<root>/.lattice/config.toml`, applies scalar leaves via
  `parse_and_set_command`, buckets structural namespaces
  (`completion.per-language.*`, `plugin.*`) keyed by
  full dotted path; warnings for unknown keys / validation
  rejects / list-at-scalar / read failures, never aborts
  startup; `App.pending_config_structural_sections` +
  `take_pending_structural_section` /
  `pending_structural_section_paths` API for the per-
  language layer + future plugin host to drain;
  `runtime::run` invokes loader between `App::new` and LSP
  boot with workspace root walked up from CWD to first
  `.git` / `.lattice/` marker) ✅;
  (3b/3) per-language overrides
  (`PerLanguageOverrides { sources, auto_trigger,
  auto_insert_single, suppress_in }` in
  `lattice-completion::insert`; spec defaults seeded at
  `App::new` via `per_language_defaults()` --
  markdown / text drop LSP for prose, rust enables auto-fire
  + auto-insert-single; TOML drains
  `[completion.per-language.<lang>]` structural sections via
  `apply_per_language_toml_overrides` with per-key merge onto
  defaults; `effective_completion_for(language)` walks
  per-language -> global option -> spec fallback;
  enforcement at `populate_insert_completion_sync` (skip
  emit for disabled sync sources) and
  `do_lsp_insert_completion_request` (short-circuit before
  the URI lookup); `auto_trigger` and `suppress_in` plumbed
  but not yet enforced -- auto-fire as a feature and
  tree-sitter scope detection are their own slices;
  `canonical_source_id` maps short labels (`lsp`, `snippet`,
  `buffer-words`, `path`, `tree-sitter`) to canonical ids;
  help refresh in `docs/../../user/completion.md` lists the
  built-in defaults table + recognised keys + merge
  semantics) ✅. **4.2.g.5 complete.** 4.2.g.6 (1/2)
  (tree-sitter local-symbol completion source --
  per-language `symbols.scm` queries in
  `crates/lattice-syntax/queries/{rust,python,javascript}/`
  capture definition-position identifiers; `LangRegistry`
  compiles them via `build_config(symbols, ...)` extension;
  `Syntax::collect_symbols()` walks the cached tree and
  returns deduped names; new
  `TREE_SITTER_SYMBOL_SOURCE_ID = "gen:tree-sitter-symbol"`
  constant; new typed option
  `completion.source.tree-sitter.priority` (default 80 per
  spec §3.4); App's `populate_insert_completion_sync`
  emits tagged candidates after buffer-words / snippets;
  `priority_for_source` resolves the new id; both sources
  emit independently when they overlap (cross-source visual
  dedup deferred to 4.2.g.7); help adds a dedicated
  `## Tree-sitter symbols` section with per-language
  capture coverage + ranking-vs-buffer-words explainer)
  ✅; 4.2.g.6 (2/2) (path completion source --
  `Syntax::cursor_in_string_scope` walks tree-sitter
  ancestors against a hardcoded string-shape set
  (`string` / `string_literal` / `raw_string_literal` /
  etc.); `App.completion_in_path_context` flag set by
  `do_completion_trigger` when the cursor is in a string
  scope and `gen:path` is enabled; path-aware anchor walks
  back over alphanumeric + `_-./~+@` until `/` (the
  dir/file boundary); `populate_path_completion` resolves
  the partial path against the document's parent dir or
  CWD, walks via `std::fs::read_dir` capped at 200 entries,
  skips dotfiles + `.git` / `node_modules` / `target` /
  `dist`, emits `File` / `Directory`-kind candidates with
  trailing `/` for directories; LSP fan-out + non-path
  sync sources short-circuit in path-completion mode so
  the popup shows filesystem entries cleanly; new constant
  `PATH_SOURCE_ID = "gen:path"`; new typed option
  `completion.source.path.priority` (default 90 per spec
  §3.4); `priority_for_source` wires the new id; help
  refresh adds a dedicated `## Path completion` section
  covering scope detection, resolution, ignore set, and
  popup behaviour) ✅. **4.2.g.6 complete.** 4.2.g.7 polish
  (sliced as independent items): commit chars
  (`Action::CompletionAcceptThenInsert(char)` routes every
  popup-time char through one handler;
  `effective_commit_chars_for` unions the focused candidate's
  per-item LSP `commitCharacters` with the new typed option
  `completion.extra_commit_chars`; popup layer claims
  unmodified character keys; non-commit chars fall through
  to plain `do_insert_text` so the popup refilters as
  before) ✅; additionalTextEdits coalesce for the LSP
  snippet accept path (new
  `expand_snippet_with_lsp_edits` builds one batch with the
  auto-import edits + the snippet body's main splice,
  reverse-sorts by start position so each edit's original-
  document positions stay valid, applies via
  `apply_edit_batch_blocking` so the whole accept lands as
  ONE undo unit; recovers the snippet's post-batch origin
  by indexing the applied vec at the main edit's
  position-after-sort; non-snippet LSP path was already
  coalesced via `apply_lsp_completion_accept`'s combined
  Vec<TextEdit>) ✅; ghost text for the top-ranked
  candidate (new typed option `completion.ghost_text`,
  default off; new App helper
  `completion_ghost_text_suffix()` returns the
  case-insensitive-prefix suffix when the popup is open
  with a non-empty query, the top candidate matches as
  prefix, and we're not in path-context;
  `compose_visible_lines` appends a dimmed-italic span on
  the cursor's row when the cursor is at end-of-line and
  the helper returns Some) ✅; cross-source visual dedup
  (new `dedup_rendered_by_text` helper retains the first
  occurrence per `raw.text` after the ranker has sorted
  descending, wired at all three refilter sites; the
  surviving row is the highest-ranked one per text, so
  the buffer-words copy of `outer` outranks the
  tree-sitter copy at the spec's 100/80 priority split
  and wins the popup row; selection / navigation / accept
  index the deduped vec naturally) ✅; picker-call-site
  typed routing payload (new `RoutingPayload` enum +
  `PICKER_ROUTING_KIND_ID` const + `Picker.routing_meta`
  sidecar Vec; new `set_raw_candidates_with_routing(items:
  Vec<(RawCandidate, RoutingPayload)>)` zips producer
  pairs into the picker, stamping each candidate with
  `Extension { kind_id, payload: index_le_bytes }`;
  `routing_for(candidate)` decodes the index and returns
  the typed `&RoutingPayload`; producers
  `LspInstanceRow::into_candidate_with_routing`,
  `LspLocationRow::into_candidate_with_routing`,
  App's `raw_buffer_candidates`, the LSP-completion picker
  open, and the code-action picker open all return pairs;
  `do_picker_accept` matches on the typed `RoutingPayload`
  variant; the string parsers `buffer_id_from_text`,
  `lsp_key_from_text`, `jump_target_from_text` are gone;
  `RawCandidate.text` is now user-facing label everywhere;
  no UX change -- pure technical-debt cleanup) ✅.
  **4.2.g.7 complete; 4.2.g done.**
- `completionItem/resolve` (lazy doc / additional edits) --
  shipped as part of 4.2.g.3 + 4.2.g.7.
- `workspaceSymbol/resolve` (lazy location) ✅. Client
  capability now advertises
  `workspace.symbol.resolveSupport.properties = ["location.range"]`;
  `Capabilities::workspace_symbol_resolve_provider` reads
  the server's matching flag from
  `workspaceSymbolProvider.resolveProvider`. New
  `ServerHandle::workspace_symbol_resolve(symbol, token)`
  client method routes the LSP `workspaceSymbol/resolve`
  request. The `workspace_symbol` response type upgrades
  from `Option<Vec<SymbolInformation>>` (legacy-only) to
  `Option<lsp_types::WorkspaceSymbolResponse>` -- the
  spec's `Flat | Nested` union covering both LSP 3.16 and
  3.17+ shapes. App's `do_lsp_workspace_symbol_request`
  handles both: `Flat(Vec<SymbolInformation>)` flows
  through `symbol_information_to_row` (range inline);
  `Nested(Vec<WorkspaceSymbol>)` flows through the new
  `workspace_symbol_to_row(handle, sym, token)` async
  helper which fires `workspaceSymbol/resolve` against
  the originating server when the location came back as
  `WorkspaceLocation` (URI only) and uses the resolved
  range. Eager-resolve at fan-out keeps picker rows
  uniform (every row is `(path, line, col)` resolved);
  the picker's accept path stays unchanged. Servers that
  don't advertise `resolveProvider` fall back to
  `(path, 0, 0)` so the user can still navigate to the
  file. Tests: legacy `Flat` shape round-trips; modern
  `Nested` shape with `WorkspaceLocation` decodes
  correctly; `workspaceSymbol/resolve` round-trip
  upgrades the location.

Remaining 4.3:
- workspace/applyEdit (server-initiated -- inbound channel
  on the actor that calls back into the App's
  `apply_workspace_edit` path) ✅. New
  `lattice-lsp::apply_edit` module exposes
  `ApplyEditBus` (mpsc Sender side cloned into every actor
  at spawn) + `InboundApplyEdit` (server_id + label + edit
  + oneshot for the response) + `ApplyEditOutcome` (the
  reply the App writes back). Actor's request branch routes
  `workspace/applyEdit` through a spawned task that
  dispatches via the bus, awaits the App's oneshot, and
  ferries the LSP `ApplyWorkspaceEditResponse` back to the
  wire; other server-initiated requests still resolve
  inline. Supervisor exposes
  `set_apply_edit_bus` for the App; `LspSupervisor::new`
  now starts with `apply_edit_bus = None` so existing
  tests / mocks that don't care about applyEdit see the
  pre-4.3 METHOD_NOT_FOUND fallback. App's
  `build_lsp_subsystem` creates the bus + receiver pair
  and stashes the receiver in
  `App.pending_apply_edit_rx`; `runtime::main_loop`
  invokes `App::drain_inbound_apply_edits` once per
  iteration alongside the other LSP drains. The drain
  flattens each WorkspaceEdit via the existing
  `flatten_workspace_edit` (same path `:rename` uses),
  applies per-file (active buffer direct, cross-file via
  `:e` then apply), echoes a status summary at Info
  (success) / Warn (partial), and replies via the
  embedded oneshot with `applied: bool` +
  `failure_reason`. Empty edits reply
  `applied: true` with the "empty workspace edit" reason
  so server logs see the no-op. v1 doesn't track
  `failed_change` (atomic-rollback queued for the
  follow-up `apply_workspace_edit_atomic`). Tests:
  lattice-lsp dispatch round-trip + drop-side error +
  oneshot round-trip; App drain applies edits to active
  buffer, replies applied=true on empty edit, and is a
  no-op when the channel is empty.

**4.x edit-path refactor: per-actor DocSync + bus-driven
fan-in.** Diagnostics testing surfaced that edits were being
silently dropped when the App's `try_lock`-on-supervisor
edit path raced the App-spawned debounce task. Architectural
fix (chosen against design goals, not ease of impl): move
`DocSync` into the per-server actor (single-writer mirror;
no shared mutex), enrich `Event::DocumentChanged` with
`path: Option<PathBuf>` + `inserted_text` per `AppliedEdit`,
spawn a per-actor `lattice_lsp::fan_in` task that subscribes
to the bus and forwards each applied edit as
`ActorCmd::RecordEdit` straight into the actor's mailbox.
The UI thread now does one `EventBus::publish` per applied
edit (1.9 µs at three subscribers) and never takes the
supervisor mutex; the actor coalesces on a 50 ms debounce
inside its `select!` loop. `App::lsp_record_edit` and the
App-side debounce task are gone; supervisor `record_edit /
flush / flush_all` survive as thin proxies for tests + the
will-save flush. `LspSupervisor` gained
`set_event_bus(Arc<EventBus>)` (called once at App startup
before any buffer opens) and tracks a per-actor
`SubscriptionId` so shutdown can unsubscribe. New benches
in `crates/lattice-lsp/benches/lsp.rs`:
`lsp_edit_publish_three_subs`, `lsp_edit_propagation_publish_to_recv`,
`lsp_didchange_flush_16_edits`. New tests in
`tests/fan_in.rs` cover end-to-end didChange after publish,
OpenDoc → RecordEdit FIFO, scratch-buffer (no path) skip,
unknown-URI warn-and-skip, 50-edit burst coalesce into one
didChange, shutdown unsubscribes the bus. Architecture
detail in `docs/../architecture/lsp-architecture.md` §5 + new §11
("Edit-path architecture") with the bus → fan-in → actor
diagram.

Update this section when picking up the in-flight item.

**4.x audit pass (post-LSP-edit refactor).** Once the per-actor
DocSync + bus-driven fan-in landed, ran a thorough design-
philosophy audit looking for the same class-of-bug elsewhere
(UI-thread / async contention, state-bearing best-effort
sends, paramount-goal violations). Findings closed in slices
1–6:

- **Slice 1** — retired `Arc<tokio::sync::Mutex<LspSupervisor>>`.
  Reads (`servers_for`, `running_actors`, ...) go wait-free
  through `ArcSwap<SupervisorSnapshot>`; writes route via
  the supervisor task's mailbox. Closed C2 (Insert-mode
  trigger probes), H1 (modeline `try_lock`), H2 (14
  supervisor `try_lock` sites silently dropping work), M5
  (App holding the supervisor mutex across `.await` in
  `drain_pending_lsp_opens`).
- **M1** (parallel agent worktree) — `EventBus::publish`
  snapshots subscriber list under a brief lock, then
  dispatches lock-free. No `Channel(bounded)` subscriber
  can stall the publisher under the inner lock.
- **Slice 2** — `DiagnosticsLayer` swaps inner `Mutex` for
  `Arc<ArcSwap<DiagnosticsSnapshot>>`. Render-frame
  `line_severity` calls (~3000/s on the render thread) drop
  from microseconds + per-call allocation to **25 ns**
  wait-free. Closed C3.
- **Slice 3** — `SyntaxActor`. `Syntax` wraps a
  `SyntaxSnapshot`; `SyntaxHandle` runs reparses on
  `tokio::task::spawn_blocking` with bursts coalesced.
  Renderer / folds / completion read the latest snapshot
  via `ArcSwap` -- tree-sitter parses no longer happen on
  the UI thread (paramount goal #1). Closed C1.
- **Slice 5** — `path_completion_cache` keyed by
  `(dir, mtime)` so consecutive Insert keystrokes inside
  string literals don't re-walk the directory; bounded-
  parallel `willSaveWaitUntil` (one shared 500ms budget
  across N servers via `tokio::task::JoinSet`) caps the
  total UI-thread save block at 500ms regardless of N.
  Closed H5, M4. H4's other FS sites
  (`open_lsp_locations_picker` reads, `:reload-snippets`,
  `:e` `metadata`) stay sync as user-command-triggered
  one-shots; documented as acceptable v1 cost.
- **Slice 6** — document-actor mailbox switches from
  bounded `mpsc::channel(64)` to `unbounded_channel`.
  `RuntimeError::Busy` retired entirely (App-side callers
  were silently discarding it under bursts). Closed H3.
- **Slice 7** — `FrameView` per-render-chain snapshot.
  Reading the architecture honestly: GPUI ships as part
  of 1.0 (TUI is a renderer peer, not the only target),
  so "Rust ownership prevents the race today" stops being
  a valid deferral once a second renderer that runs on a
  separate thread enters the picture. `FrameView::from_app`
  freezes `folds`, `visible_highlights`, and
  `show_line_numbers` once at chain entry; helpers
  (`compose_visible_lines_inner`, `render_gutter_for`,
  `closed_fold_display_span`, `buffer_line_to_visible_row_with`,
  `cursor_screen_position*`, `draw_inactive_document`)
  consume `&FrameView`. Mirror methods
  (`view.fold_start_at_any` / `fold_start_at` /
  `line_inside_closed_fold`) read from the snapshot's frozen
  Arc. Closed M2.
- **Slice 9** — event-driven LSP attach. The LSP open path
  used to park the UI thread on the `initialize` round-trip
  (initial document via `runtime::initialize_lsp_blocking`,
  subsequent `:e <path>` via the
  `pending_lsp_opens`-queue + `block_on(drain)` pattern in
  the main loop). Two block_on sites both violated paramount
  goal #4 (asynchronicity). The audit also surfaced a
  silent-failure bug -- `LspSupervisor::spawn` used
  `tokio::runtime::Handle::try_current()` and dropped
  `cmd_rx` if no ambient runtime existed; in production
  `App::new` runs before any tokio context, so the
  supervisor task never spawned and every write returned
  `LspError::ActorGone`. Both fixed in this slice:
  - `LspSupervisor::spawn` now takes an explicit
	`&tokio::runtime::Handle`; callers pass
	`runtime::lsp_runtime().handle()`. No more
	`try_current()` footgun.
  - Buffer-open is event-driven: `App::new` and
	`App::do_edit` set `BufferId → Uri` eagerly and publish
	[`Event::DocumentOpened { id, path, version, text }`](../crates/lattice-protocol/src/event.rs)
	on the bus. The new `lattice_lsp::attach_driver` module
	subscribes on the LSP runtime, runs a serial
	`recv → supervisor.open_buffer.await` loop, and logs
	failures. UI thread never parks. Single path for
	initial + subsequent opens.
  - Removed: `App::pending_lsp_opens`, `App::queue_lsp_open`,
	`App::drain_pending_lsp_opens`, `App::initialize_lsp`,
	`runtime::initialize_lsp_blocking`,
	`runtime::drain_pending_lsp_opens_blocking`, and the
	main-loop drain step. Net diff is removal-heavy.
  - Closes the no-rust-analyzer-attach regression that
	surfaced as `"server actor is no longer running"` on
	every editor launch.

Deferred with rationale:

- **M3 (input.rs trie-driven dispatch)** — `input.rs` is
  4365 lines of hand-rolled `KeyCode` matching; plugins
  can't bind chords (paramount goal #3 violation: "the
  grammar IS the public command API"). Real present-day
  capability gap, scoped as Slice 8 of this audit pass
  (in progress). The fix replaces the hand-rolled match
  with a trie consuming the `KeymapEntry { chord,
  command_invocation }` table from `keymap.rs`; metadata
  surface in `keymap.rs` becomes the source of truth;
  `input.rs` becomes a chord-string normaliser; plugins
  gain a structured extension point.

`docs/benchmarks.md` got the new perf rows
(`lsp_diagnostics_line_severity_wait_free` at 25ns, the
existing edit-path benches). Tests stayed green at every
slice boundary.

1. **Phase 4: LSP** — diagnostics, completion, hover, go-to-definition,
   references. The cancellation-token plumbing is in place
   (`dispatch_with_cancel` + cooperative search cancellation), so LSP
   request cancellation hooks into existing seams; the remaining work
   is the LSP client (tower-lsp or hand-rolled) + per-server shims.
2. **Computed folds** (per `docs/../../user/folding.md`) — **✅ done for
   all v1 providers except tree-sitter syntax queries.** Manual
   `zf` / `zo` / `zc` / `za` / `zR` / `zM` / `zd` / `zj` / `zk`,
   plus the new `zi` (`:set foldenable!`). Two computed providers:
   `compute_indent_folds` (universal) and `compute_markdown_folds`
   (ATX heading nesting, code-fence aware for both ``` and ~~~).
   `:set foldmethod=manual|indent|markdown|syntax` parses; `Syntax`
   is a v1 cascade (markdown for `.md`, indent otherwise) until the
   tree-sitter scope-query provider lands.

   Beyond storage, the user-facing pieces from `docs/../../user/folding.md`:
   identity-hash recompute (heading text + indent depth) preserves
   closed-state across edits in unrelated sections; closed folds
   render heading-preserved with a dim ` ┄ N lines folded` suffix
   (no `+--- N lines ---` line replacement); gutter glyphs ▾ open
   / ▸ closed; `dd` / `yy` / `cc` / `>>` on a closed fold expand
   to the full fold range as a single undo unit; jump-class motions
   (search, gg / G, H / M / L, marks, Ctrl-O / Ctrl-I, `%`) auto-
   open the destination fold; `:set foldenable` + `zi` short-circuit
   every fold-aware path while preserving closed-state.

   Tree-sitter-driven folds (function bodies, classes, blocks via
   `folds.scm`) remain queued. The `Fold` data type is shared, so
   a tree-sitter provider drops into the existing recompute /
   identity / render plumbing without a redesign.
3. **`:set option=value` + typed options** (§5.12) ✅ done — now
   in its own renderer-agnostic crate. The `lattice-config` crate
   owns the option machinery: an `OptionType` trait (with built-in
   impls for `bool`, `i64`, `String`); `Option<T>` whose value cell
   is an `arc_swap::ArcSwap<T>` for wait-free hot-path reads; an
   `ErasedOption` trait + `ConfigRegistry` backing both typed
   `OptionHandle<T>` access and by-name (`:set foo=bar`) access;
   the `:set` syntax parser; and an `OptionsGenerator` that the
   completion pipeline picks up via the `gen:options` source.
   `register_core_options(&registry) → CoreOptions` registers the
   nine renderer-agnostic options (number, relativenumber, wrap,
   ignorecase, tabstop, foldenable, foldmethod, scrolloff,
   completion.auto_insert_single); each renderer registers its
   own UI-specific options through the same `register` API and
   gets its own typed-handle struct back (`lattice-ui-tui` ships
   `register_tui_options → TuiOptions` for `ui.dim_inactive`,
   `ui.separator`, `ui.separator_color`,
   `ui.statusline_active_fg`, `ui.statusline_inactive_fg`).
   `FoldMethod` lives in `lattice-core::folding` (any renderer
   reads it the same way); the `OptionType for FoldMethod` impl
   lives in `lattice-config::domain` to keep core's dep direction
   inward. `:set name=value` echoes are byte-identical to the
   pre-migration wording, including all error messages
   (`E518: Unknown option`, `E474: not a boolean option`,
   `tabstop out of range [1, 32]: N`). Multi-option `:set`
   syntax (`:set ic hls scs`) still deferred.

   **App integration**: `App.config: Arc<ConfigRegistry>` plus
   typed handle structs (`core_options`, `tui_options`) replace
   the previous duplicated `App.foldmethod` /
   `App.show_line_numbers` / etc. fields. Read-side accessors
   (`app.foldmethod()`, `app.tabstop()`, ...) wrap the indirection
   so call sites read like a field access. `do_set` calls
   `config.parse_and_set_command(...)` and runs `apply_post_set`
   for cascade side effects: `relativenumber` ⇒ `number=true`,
   `foldmethod` ⇒ `recompute_folds()`, every `ui.*` ⇒
   `sync_theme_from_config()` to refresh the cached `Theme`
   `Style` projections.

   **§5.12 amendment landed in ../architecture/design.md (no plugin code yet).**
   Two-layer config codified: `~/.config/lattice/options.toml`
   for static data; `~/.config/lattice/init.rs` compiled to WASM
   Component, loaded by the §5.5 plugin host with a `boot`
   capability, for programmable config. Auto-build on first boot
   (cargo-component under `~/.cache/lattice/`); cache by source
   hash + lattice version + WIT revision. `lattice config build`
   exists as a diagnostic CLI. Project-local code-config deferred
   behind a future per-directory trust prompt; project-local
   `options.toml` supported. Implementation depends on Phase 7
   (plugin host); `lattice-config-api` crate added to the project
   layout as the WIT-bindings reexport user `init.rs` consumes.

3a. **§5.2.1 closure: kind-prefix form on `:`** ✅ done. The legacy
   parser-kind rejection is gone; every command (motion, operator,
   text-object, ex-command, plugin contribution) is reachable from
   `:` via `:<kind> <name>` syntax. Three reserved kind words on
   `:` (`motion`, `operator`, `text-object`); ex-commands keep
   their bare alias surface. Operator targets resolve via
   implicit-namespace lookup. ../architecture/design.md §2.2 codifies the
   no-function-call-syntax-on-`:` invariant; §5.2.1 specifies the
   kind-prefix grammar. See
   `crates/lattice-ui-tui/src/excommand.rs::parse_kind_prefixed`.
4. **Multi-buffer foundations** (§5.9) — the trigger for `HelpDisplayMode`
   beyond `Popup`. Until this lands, all introspection is overlay-rendered.
   - **B.1.a buffer abstraction + active-buffer routing** ✅ done.
	 `BufferKind { Document, Help }` + `BufferId` newtype; `App::active_buffer`
	 decides which cursor a motion / page / scroll / `<C-o>` / `<C-i>`
	 action mutates. Help routes through the same `translate_normal` chord
	 grammar as document buffers; only three buffer-local bindings differ
	 (`Esc` / `q` dismiss, `<CR>` follows the link under the cursor). The
	 unified position-history ring carries `(buffer, buffer_id)` so jump-list
	 walks switch active_buffer cleanly when crossing buffer boundaries.
	 `lattice_grammar::execute_motion_only` exposes a read-only motion
	 dispatch path that resolves a `CommandInvocation` against a bare
	 `Buffer` -- no `Document` / undo / selections required, suitable for
	 help and (later) file-tree / outline / diagnostics views.
   - **B.1.b pane tree + splits** ✅ done. Recursive binary-split
	 `PaneTree` with `<C-w>{s,v,c,q,h,j,k,l,w,W}` chord grammar. Each
	 leaf carries per-pane viewport stash (cursor + scroll); the
	 active pane's stash is hot-loaded into `App::cursor` /
	 `App::scroll` so motion code stays unchanged. Active-pane
	 switches snapshot back into the source pane's stash and load
	 from the destination's. Geometry-aware navigation
	 (`<C-w>{h,j,k,l}`) walks the spatial neighbour computed from
	 `compute_rects`. Inactive-pane rendering is a placeholder
	 until B.1.c brings meaningfully distinct buffer content.
   - **B.1.c multiple Document buffers** ✅ done. Replaced by the
	 unified registry below; original implementation used a
	 dedicated `documents: HashMap<BufferId, DocumentEntry>`.
   - **B.1.d buffer-as-content kinds: file-tree** ✅ done. New
	 `BufferKind::FileTree` variant + `FileTreeBuffer` (rope-
	 backed, same shape as `HelpBuffer`). `:Tree [path]` opens a
	 tree buffer rooted at `path` (or the document's parent dir /
	 cwd); same path de-dups (already-open trees are switched to,
	 not duplicated). Multiple trees coexist (one per
	 distinct root). `:TreeClose` removes from the registry.
	 Standard motions route via the same active-buffer dispatch
	 as Help. `<CR>` on a directory toggles expansion; on a file
	 opens it via the standard `:e FILE` path. Outline + diagnostics
	 panels queue behind their own integrations.
   - **Unified `BufferRegistry`** ✅ done. Documents and file trees
	 live in a single keyspace under `App.buffers`
	 (`HashMap<BufferId, BufferEntry>` with `BufferData::Document |
	 FileTree` discriminant). `:bn` / `:bp` / `:ls` / `:bd` /
	 `:b N` operate on the registry uniformly -- cycling between a
	 document and a tree feels the same as cycling between two
	 documents. Each entry carries `BufferFlags { listed, hidden }`;
	 unlisted buffers are skipped by `:bn` / `:bp`. `:e folder`
	 defers to `:Tree folder` (vim's `:Explore` semantics). Help
	 stays overlay-rendered for now -- moving it into the registry
	 is a follow-up that doesn't require structural change.
	 `DocumentEntry` stashes per-buffer hot-path state (syntax tree,
	 fold list, last-parsed version) when a buffer leaves active;
	 a single `App::activate_buffer_state` lifecycle hook fires on
	 every transition into a document buffer (both `:e <new>` and
	 `:b N` paths), reparsing if needed and seeding folds against
	 the active `foldmethod` so users no longer have to reach for
	 `<C-l>` after switching files. New buffer-level state plugs
	 into the same hook -- no per-option fixups across the
	 activation paths.
   - **Pane visuals** ✅ done. Each pane gets a vim-style status
	 line (active reverse-videoed via theme, inactive dim);
	 `│` separator drawn between vertically split panes. Inactive
	 Document panes keep their tree-sitter syntax highlights
	 (refreshed lazily by `App::refresh_pane_highlights`); a
	 `Theme::inactive_pane_overlay` modifier (default `DIM`)
	 layers on top so focus stays unambiguous without losing
	 color. Customizable via `:set ui.dim_inactive`,
	 `ui.separator`, `ui.separator_color`,
	 `ui.statusline_active_fg`, `ui.statusline_inactive_fg`.
5. **Hover popup + inline completion popup polish** — completion popup
   is wired (vertico-style); hover popup scaffolding ✅ done. New
   `HoverPopup` type with markdown body + buffer-position anchor;
   markdown highlights computed via the shared `LangRegistry`. The
   renderer floats the popup near the cursor (below if room, above
   otherwise). `:hover [text]` opens manually for now (Phase 4 LSP
   will source `text` from `textDocument/hover`); `:HoverClose`
   dismisses.
5b. **Help topic surface (`:help`)** ✅ done.
	`lattice-ui-tui::help_topics` defines a `HelpTopicRegistry`
	keyed by name; bodies are either `Static(&'static str)`
	(built-ins are sourced from `docs/user/*.md` via
	`include_str!` so the binary is self-contained) or
	`Dynamic(closure)` -- the seam for LSP / plugin / config-
	supplied topics. `:help` with no arg opens the registry's
	`index` topic (the README content); `:help <topic>` opens the
	named topic; `:h` is an alias. `<Tab>` enumerates topics via a
	new `gen:help-topics` completion source. `:describe-*`
	appends `See also: [topic](help:topic)` cross-links when a
	topic's `related_command_patterns` substring-matches the
	described command. New `HelpLinkTarget::Topic(name)` variant
	+ `help:` URL scheme so topic links are first-class everywhere
	a help body can render.
6. **Help major mode + tree-sitter grammar** — defines sections,
   link-targets, code-blocks. Needs the help mode registered as a major
   mode, which depends on the modes registry (Phase 8) but the *grammar*
   can be drafted earlier.
7. **Veto-class hooks + actor event publish** (§5.10.2 / §5.2.1) —
   observation-only event bus is in place; pre-mutation hooks
   (`BeforeSave`, `BeforeQuit`) need the mutation/abort return path.
   Actor wiring to publish `DocumentChanged` / `SelectionsChanged` /
   `ModalModeChanged` events. Unblocks autocmds.
8. **Per-`LatencyClass` deadline timers** — Reflex commands observe a
   2 ms deadline, Display commands 10 ms, both via the cancellation
   token already plumbed.
9. **Bench regression gate** — needs a stable runner (self-hosted or
   `bencher.dev`); shared GitHub runners have ~20% bench variance that
   dwarfs a 10% regression signal.
10. **Render-hot-path alloc discipline** — dhat-based assertion that
	steady-state frames produce no allocations.

---

## §15 open questions still load-bearing

These are tracked in ../architecture/design.md §15. Items the implementation has resolved
are crossed out there. Items that influence active tasks:

- §15:18 Folds storage / interaction — feeds the **Computed folds** task above.
- §15:19 Replace mode dispatch (resolved by current Replace impl).
- §15:20 Live evaluation (deferred per §10).
- §15:21 File watcher / auto-revert — unaddressed.
- §15:22 Bookmarks / cross-file marks — current marks are buffer-local.
- §15:23 Function rebinding / advice — unaddressed.
- §15:24 Narrow-to-region — unaddressed.
- §15:25 Snippets / abbrev — unaddressed.
- §15:26 Frames (multi-OS-window) — unaddressed.
- §15:27 Session save / restore — unaddressed.

---

## Test counts (snapshot)

Workspace tests as of the last commit. Coverage by crate:

| Crate                            | Tests |
|----------------------------------|-------|
| lattice-protocol                 | 30    |
| lattice-core (incl. integration) | 86    |
| lattice-grammar                  | 183   |
| lattice-completion               | 117   |
| lattice-config                   | 57    |
| lattice-syntax                   | 13    |
| lattice-runtime                  | 35    |
| lattice-lsp                      | 32    |
| lattice-ui-tui                   | 1394  |

Plus criterion benches for hot paths (search, buffer, motions, operators,
runtime actor) — see `docs/benchmarks.md` for the latest numbers.

---

## Conventions for updating this doc

- Update the **Phase status** table whenever a phase advances.
- Update the **Vim grammar coverage** table when a primitive lands; the
  status column uses ✅ done, 🔄 in progress, 🟡 partial, ⛔ pending,
  ⚠️ usable-with-caveats.
- Update **In-progress** before each commit that lands the in-flight item.
- Move completed items from **Up next** into the appropriate coverage table.
- Update **Test counts** at the end of each session.
- Don't write per-session log entries here — `git log --oneline` is the log.
