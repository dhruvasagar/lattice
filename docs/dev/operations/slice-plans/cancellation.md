# Foreground cancellation — slice plan

Sequencing companion to
[`docs/dev/architecture/cancellation.md`](../../architecture/cancellation.md).
The design fragment is the source of truth for *what* and *why*;
this file owns *when* and *in what order*. Authoritative status per
slice lives in [`../implementation.md`](../implementation.md).

> **Status: 📝 planned — design complete, implementation NOT started
> (verified-from-source 2026-06-17).** The design fragment + this plan
> exist (`ea302a6b`), and the lower-level `CancellationToken` substrate is
> in place (grammar/runtime/actor plumbing + cooperative scan checks). But
> the user-facing CG.1–CG.4 work is unbuilt: no `Action::Cancel`, no
> `arm_cancel()` / `cancel_foreground()` / `active_cancel`, and `<C-g>` is
> NOT bound to foreground cancellation. CG.5 (stack-based multi-op) is
> explicitly deferred behind a future status-line/progress subsystem.
> **This is the most valuable small Phase-4-adjacent item still open** —
> CG.3 surfaces `<C-g>` cancellation of in-flight LSP commands.

Each slice ships green-on-merge with the four artefacts CLAUDE.md
mandates: architecture doc (updated as needed), benchmark coverage
where load-bearing, test coverage of new scenarios + failure modes,
graceful error handling.

| Slice    | Title                                         | What lands |
|----------|-----------------------------------------------|------------|
| **CG.1** | Core substrate: token field + binding         | Add `active_cancel: Option<CancellationToken>` to `Editor` in `lattice-host/src/lib.rs`. Add `arm_cancel()` and `cancel_foreground()` methods on `Editor`. Add `Action::Cancel` variant. Register `<C-g>` at `KeymapLayer::Builtin` (all modes) and `<C-c>` at Builtin for Normal/Op-pending/Visual. Dispatch arm calls `editor.cancel_foreground()` + emits `RendererSignal::Redraw`. **No async plumbing yet** — `<C-g>` always snaps to Normal and clears the partial chord, even when idle. Tests: pressing `<C-g>` in Normal resets chord; pressing `<C-g>` in Op-pending returns to Normal; `cancel_foreground()` with `None` token is a no-op; `arm_cancel()` cancels the prior token before returning the new one. |
| **CG.2** | Search cancellation                           | Thread `arm_cancel()` token through `ProjectSearchService::scan()` and the `MultibufferSearchProvider` spawn path in `lattice-multibuffer`. The search loop in `lattice-core::search` already checks the token at each chunk boundary — it just receives `CancellationToken::never()` today; CG.2 passes the live user token instead. Pressing `<C-g>` during a running search cancels the scan and leaves the multibuffer in its partial state (populated so far). Tests: start search, cancel mid-scan → `SearchError::Cancelled` observed; start search, second search before first completes → first token cancelled, second runs to completion. |
| **CG.3** | LSP command cancellation + hover fold-in      | Thread `arm_cancel()` token through LSP command dispatch (hover, rename, code-actions, format) in `lattice-host/src/dispatch.rs`. Fold `pending_hover_token` into `active_cancel` — `pending_hover_token` is removed; all hover cancellation goes through `cancel_foreground()`. Update `lattice-ui-tui/src/app/lsp.rs` callsites that currently set `pending_hover_token`. Tests: `cancel_foreground()` cancels an in-flight hover; removing `pending_hover_token` field does not break the LSP hover path; `<C-g>` during rename prompt returns to Normal without leaving LSP session in undefined state. |
| **CG.4** | WASM plugin cancellation                      | Thread the foreground token into the plugin-host call path. At each WASM fuel-exhaustion trampoline, check `token.is_cancelled()` before re-fuelling. A cancelled token causes the trampoline to return `Err(CommandError::Cancelled)` to the host. The host logs at `debug!` level and discards the result. Tests: a fuel-hungry plugin call cancelled mid-flight → `CommandError::Cancelled` returned, no panic, no partial state visible in the buffer. |
| **CG.5** | Stack-based multi-op (deferred)               | Replace `active_cancel: Option<…>` with `cancel_stack: Vec<(CancellationToken, &'static str)>`. `arm_cancel()` pushes; natural completion pops; `cancel_foreground()` pops-and-cancels the top. Status-line badge shows `[search…]` when the stack is non-empty. **Not sliced yet** — prerequisite is at least two concurrent visible async ops surfaced to the user. Revisit during the status-line / progress subsystem design. |

## Slice sequencing

- **CG.1** is the foundational substrate. Lands first; provides the
  binding + mode-reset guarantee even before any async op is wired.
- **CG.2** depends on CG.1 (needs `arm_cancel()` + `Action::Cancel`).
  Can land as soon as the search provider is the primary pain point.
- **CG.3** depends on CG.1. Independent of CG.2 — can land before or
  after depending on which stall the user hits first. Removes
  `pending_hover_token` so CG.3 must be self-contained (no split
  migration).
- **CG.4** depends on CG.1. Independent of CG.2 and CG.3; requires
  the plugin host to be in tree (Phase 3+).
- **CG.5** depends on CG.1–CG.4 all complete. Not sequenced until
  the status-line / progress subsystem is designed.

## Entry criteria

Before starting CG.1, verify:

1. `lattice_protocol::CancellationToken` is in the dependency graph
   of `lattice-host` (it already is via `lattice-lsp` → `lattice-protocol`).
2. `reset_to_normal()` exists and is called by the `<Esc>` path
   (confirm the method name in `dispatch.rs` before wiring).
3. `KeymapLayer::Builtin` registration pattern is established
   (confirmed — `<Esc>` and partial-chord clear already use it).
