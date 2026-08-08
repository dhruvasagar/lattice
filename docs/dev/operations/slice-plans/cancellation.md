# Foreground cancellation — slice plan

Sequencing companion to
[`docs/dev/architecture/cancellation.md`](../../architecture/cancellation.md).
The design fragment is the source of truth for *what* and *why*;
this file owns *when* and *in what order*. Authoritative status per
slice lives in [`../implementation.md`](../implementation.md).

> **Status: 🚧 in progress — CG.1 ✅ (2026-08-07), CG.2–CG.4 📝, CG.5 ⛔
> deferred.** The cancel triggers, the `active_cancel` seam and the mode
> reset are in place, so every remaining slice is "hand `arm_cancel()`'s
> token to the thing that spawns". CG.5 (stack-based multi-op) stays
> deferred behind a future status-line/progress subsystem.
>
> **The binding took three attempts.** The design fragment specified
> `<C-g>` at `KeymapLayer::Builtin` in *every* mode; SN.3d has since
> taken `<C-g>` as the Visual↔Select toggle, so that was narrowed.
> `<C-c>` was tried next and abandoned: it is a mode *prefix* here
> (`<C-c>g` magit dispatch, `<C-c><C-c>` commit confirm, …) and the trie
> resolves a terminal node before its children, so a depth-1 binding made
> every one of those chords unreachable — it broke magit's transients,
> caught by three existing tests. `<Esc>` was tried third and rejected on
> UX review: vim users press it reflexively, so a long search would die
> to a habitual double-tap carrying no intent to cancel.
>
> What ships: **`<C-g>` at `Builtin`** in Normal / Insert / Replace
> (+ Command / Search / Prompt via the Insert table). Visual and Select
> keep SN.3d's toggle and have no cancel chord — `<Esc>` then `<C-g>`.
> See [`cancellation.md`](../../architecture/cancellation.md) §4.5,
> §4.5.1, §4.5.2.

Each slice ships green-on-merge with the four artefacts CLAUDE.md
mandates: architecture doc (updated as needed), benchmark coverage
where load-bearing, test coverage of new scenarios + failure modes,
graceful error handling.

| Slice    | Title                                         | What lands |
|----------|-----------------------------------------------|------------|
| **CG.1** ✅ | Core substrate: token field + binding      | **Landed 2026-08-07.** `Editor.active_cancel` (`editor.rs`); `arm_cancel()` / `cancel_foreground()` / `reset_to_normal()` / `do_command_line_dismiss()` (`dispatch.rs`); `AppEffect::Cancel` → `Action::Cancel` → host-resident dispatch arm (so TUI and GPUI share one body); `action:cancel` in `ActionIds`. `<C-g>` bound at `KeymapLayer::Builtin` for Normal / Insert / Replace in the new `keymap_cancel.rs` — Builtin rather than `emacs-keys-mode` so `:set noemacs-keys` cannot take the cancel key away. Visual / Select untouched (SN.3d toggle). No plugin (WIT) surface — typed error in `boundary_app_effect.rs`. **No async plumbing yet**; with nothing armed it degrades to a mode reset. Tests: `keymap_cancel` (3), `dispatch::tests` (5), and the key-driven `lattice-ui-tui::app::cancel` (12). |
| **CG.2** | Search cancellation                           | Thread `arm_cancel()` token through `ProjectSearchService::scan()` and the `MultibufferSearchProvider` spawn path in `lattice-multibuffer`. The search loop in `lattice-core::search` already checks the token at each chunk boundary — it just receives `CancellationToken::never()` today; CG.2 passes the live user token instead. Pressing `<C-g>` during a running search cancels the scan and leaves the multibuffer in its partial state (populated so far). Tests: start search, cancel mid-scan → `SearchError::Cancelled` observed; start search, second search before first completes → first token cancelled, second runs to completion. |
| **CG.3** | LSP command cancellation + hover fold-in      | Thread `arm_cancel()` token through LSP command dispatch (hover, rename, code-actions, format) in `lattice-host/src/dispatch.rs`. Fold `pending_hover_token` into `active_cancel` — `pending_hover_token` is removed; all hover cancellation goes through `cancel_foreground()`. Update `lattice-ui-tui/src/app/lsp.rs` callsites that currently set `pending_hover_token`. Tests: `cancel_foreground()` cancels an in-flight hover; removing `pending_hover_token` field does not break the LSP hover path; `<C-g>` during rename prompt returns to Normal without leaving LSP session in undefined state. |
| **CG.4** | WASM plugin cancellation                      | Thread the foreground token into the plugin-host call path. At each WASM fuel-exhaustion trampoline, check `token.is_cancelled()` before re-fuelling. A cancelled token causes the trampoline to return `Err(CommandError::Cancelled)` to the host. The host logs at `debug!` level and discards the result. Tests: a fuel-hungry plugin call cancelled mid-flight → `CommandError::Cancelled` returned, no panic, no partial state visible in the buffer. |
| **CG.5** | Stack-based multi-op (deferred)               | Replace `active_cancel: Option<…>` with `cancel_stack: Vec<(CancellationToken, &'static str)>`. `arm_cancel()` pushes; natural completion pops; `cancel_foreground()` pops-and-cancels the top. Status-line badge shows `[search…]` when the stack is non-empty. **Not sliced yet** — prerequisite is at least two concurrent visible async ops surfaced to the user. Revisit during the status-line / progress subsystem design. |

## Slice sequencing

- **CG.1** ✅ is the foundational substrate. Provides the binding +
  mode-reset guarantee even before any async op is wired. Every later
  slice reduces to "hand `editor.arm_cancel()`'s token to the spawn".
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

## Entry criteria (CG.1 — resolved)

Checked against source before CG.1 executed, and two of the three were
wrong. Recorded because the same assumptions recur in CG.2–CG.4:

1. ✅ `lattice_protocol::CancellationToken` is in `lattice-host`'s
   dependency graph (via `lattice-lsp` → `lattice-protocol`).
2. ❌ **`reset_to_normal()` did not exist.** Only `set_modal()` and the
   per-mode exit handlers did. CG.1 authored it — see
   [`cancellation.md`](../../architecture/cancellation.md) §4.6.
3. ❌ **`<C-g>` was already taken** by SN.3d's Visual↔Select toggle in
   both Visual and Select, and vim reserves Normal `<C-g>` for file
   info. `<C-c>` is structurally unusable (mode prefix) and `<Esc>` was
   rejected on UX review (pressed reflexively). The binding landed on
   `<C-g>`, narrowed to the modes SN.3d does not own. §4.5–§4.5.2
   record the reasoning.

## Incidental fix in CG.1

`dispatch_visual` and `native_select_action` each short-circuited
**every** CONTROL-bearing chord before consulting the trie, so no
binding registered under `BindingMode::Visual` / `Select` with a CTRL
chord could ever fire. Invisible in-tree (neither catalog registers
one), but `keymap_host.rs` maps WIT `Visual` / `Select` straight
through — so a plugin binding `<C-x>` in Visual got silent nothing.
That is an extensibility hole (paramount goal #2), the same shape as
the PBH.3 defect that made every `<C-digit>` unreachable in Normal.

Both guards are gone; the trie is authoritative. `<C-g>` (SN.3d
toggle) and Select's `<C-o>` (deliberate post-MVP swallow) stay as
explicit pre-lookup arms because both are mode *control*, not command
lookup. Select's `printable_overtype_fallback` now rejects
modifier-bearing chords itself — it had been relying on the removed
guard, and without that check `<C-w>` in Select would have overtyped
the selection with a literal `w`.
