# Fold architecture — slice plan

Sequencing companion to
[`docs/dev/architecture/fold-architecture.md`](../../architecture/fold-architecture.md).
The design fragment is the source of truth for *what* and *why*;
this file owns *when* and *in what order*. Authoritative status
per slice lives in [`../implementation.md`](../implementation.md).

Each slice ships green-on-merge with the four artefacts
CLAUDE.md mandates: architecture documentation (the design
fragment, updated as needed), benchmark coverage where
load-bearing, test coverage of the new scenarios + failure
modes, graceful error handling.

| Slice       | Title                                     | What lands |
|-------------|-------------------------------------------|------------|
| **D.3.f.0** | `FoldProvider` trait + registry refactor  | Substrate-only. Add `ProviderKind` / `ProviderId` to `lattice-core::folding`. Add `FoldProvider` trait, `FoldContext`, `FoldRegistry` in new `lattice-host/src/fold_provider.rs`. Wrap the 5 existing `compute_*_folds` functions as `Primary` providers (`ManualPrimary`, `IndentPrimary`, `MarkdownPrimary`, `SyntaxPrimary`, `LspPrimary`). Refactor `Editor::recompute_folds()` to drive the registry instead of `match self.foldmethod`. **No behaviour change** — existing fold tests stay green; the renderer / `z*` family / option parsing is untouched. New unit tests on the registry: primary swap, overlay add/remove, closed-state survives both. |
| **D.3.f.1** ✅ | `HunkFoldProvider` overlay (2026-05-29) | New `lattice-host/src/diff/fold.rs`: provider at `ProviderId(100)`, `compute()` reads `FoldContext::diff_hunks` and emits one `Fold` per non-empty current-side hunk of length >= 2 (skips pure-`Remove` + single-line); identity = `hash(("diff:hunk", start_line, end_line))`. **Always-on registration:** `FoldRegistry::with_builtins()` pre-seeds the overlay so `Editor::default()` matches editor-boot composition; gated by `ctx.diff_hunks` being `Some`. **Eventual-consistency model for publish→freshness:** `Editor::recompute_folds` loads the session's currently-published `HunkIndex` from `DiffSubsystem::lookup`; hunks appear on the next recompute_folds trigger (edit / mode change / buffer switch). No new wake mechanism — matches virtual-rows composition today. Updated `diff-system.md` §6.5 to point here. **10 unit tests** (no-session, empty-index, Add inclusive-end, Remove skipped, single-line skipped, Change + Conflict foldable, identity stable, identity distinguishes spans, provider id, malformed → no panic) + **3 dispatch integration tests** (recompute adds hunk fold; drop_session removes it; closed-state survives republish) + **1 fold_provider test** (with_builtins pre-seeds the overlay). 466 host tests + 1461 workspace tests green. |
| **D.3.f.2** ✅ | Bench `fold_recompute` (2026-05-29) | New `crates/lattice-host/benches/fold_recompute.rs` + `[[bench]] fold_recompute` in Cargo.toml. Three workloads at N = 0 / 10 / 100 / 1000 hunks: `overlay_only_at_n_hunks` (end-to-end `Editor::recompute_folds` at foldmethod=Manual — isolates registry + overlay + carry-over), `hunk_provider_compute_pure` (isolated `HunkFoldProvider::compute`), `fold_identity_hash` (raw hasher). Baselines recorded in `benchmarks.md` §Folds-D.3.f.2: 100-hunk recompute **3.1 µs** (2580× under 8 ms budget), 1000-hunk **144 µs** (55× under). CI gate enforcement deferred — headroom large enough that bench-on-PR catches regressions. Closes D.3.f. |

Slice sequencing:

- **D.3.f.0** is the foundational refactor. Lands green with
  existing tests; no user-visible change.
- **D.3.f.1** depends on D.3.f.0 (consumes the overlay
  registration API) and on D.2 (consumes `DiffSession`
  lifecycle hooks). Both are already in tree.
- **D.3.f.2** depends on D.3.f.1 (need a real overlay
  consumer to bench against).

After D.3.f.2, M.7 (excerpt fold overlay) and M.8
(file-boundary fold overlay) reuse the same overlay
registration path without further fold-engine changes.
Plugin-registered fold providers (Primary or Overlay) fall
out free in the Phase 7 WIT shim.
