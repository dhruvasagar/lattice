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
| **D.3.f.1** | `HunkFoldProvider` overlay                | New provider in `lattice-host/src/diff_fold.rs`. Registers as an overlay when `DiffSubsystem::open_session` runs; deregisters in `drop_session`. Reads the active `HunkIndex` from `FoldContext::diff_hunks`; emits one `Fold` per hunk with non-empty current-side range; identity = `hash(("diff:hunk", start_line, end_line))`. Triggers `recompute_folds()` after a diff publish (via a forwarder task on `publish_notify`, mirroring the virtual-rows wake). Updates `diff-system.md` §6.5 to point at the new fold-architecture fragment. Tests: hunk fold range correct; `za` toggles a hunk fold without affecting other folds; closed-state survives publish; `:set foldmethod=indent` then `=syntax` swaps primaries without losing hunk overlay. |
| **D.3.f.2** | Bench: `fold_recompute_p99_us`            | New criterion bench: 100 hunks overlaid on top of a syntax primary against a 5k-line file. CI gate enforces the 8 ms keystroke budget. Catches registry-indirection regression. |

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
