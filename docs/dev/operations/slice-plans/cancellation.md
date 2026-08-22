# Foreground cancellation — slice plan

Sequencing companion to
[`docs/dev/architecture/cancellation.md`](../../architecture/cancellation.md).
The design fragment is the source of truth for *what* and *why*;
this file owns *when* and *in what order*. Authoritative status per
slice lives in [`../implementation.md`](../implementation.md).

> **Status: 🚧 in progress — CG.1 ✅ (2026-08-07), CG.2 ✅ + CG.3 ✅ (2026-08-08),
> CG.4 ✅ (2026-08-22), CG.5 ⛔ deferred.** The cancel triggers, the `active_cancel` seam and the mode
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
| **CG.2** ✅ | Search cancellation                        | **Landed 2026-08-08.** The seam moved from `Editor::arm_cancel()` (`&mut`) to a registered `ForegroundCancelHandle` service (`lattice-mode/src/foreground_cancel.rs`) armed from any `&self` context — necessary because project search's `gr` refresh is an action-handler closure with only `&self` services, and enrolling just the `&mut`-reachable initial spawn would leave a refreshed scan silently uncancellable. `Editor.active_cancel` became `Editor.foreground_cancel`, the same `Arc` boot registers. Project search's private `Arc<AtomicBool>` supersede flag is gone: `ProjectSearchState.cancel_token` is now the armed `CancellationToken`, required by `ProjectSearchState::scanning`, and `arm()`'s cancel-the-predecessor gives `gr` supersede for free. Tests: `foreground_cancel` (5), `providers::search` (2 rewritten onto the real mechanism), `app::cancel` (2 new, incl. the same-`Arc` end-to-end proof). |
| **CG.3** ✅ | LSP command cancellation                  | **Landed 2026-08-08.** `ForegroundCancel` gained a second verb: `arm()` supersedes (search), `enrol()` joins the set (LSP), `cancel()` walks both. Twelve user-triggered commands enrol — hover, the `gd`/`gD`/`gy`/`gI` nav family, references, document + workspace symbols, rename, format, code-actions (request + resolve-apply), call hierarchy, type hierarchy, selection-range. **`pending_hover_token` is NOT removed** as the plan specified, nor are its 20 siblings: they are per-feature *supersede* tokens at a granularity the single slot cannot express (a hover must not cancel a rename), and `enrol` gives `<C-g>` a route to them without disturbing that. The CG.1 hover special-case in `cancel_foreground` is gone — it covered hover alone. **Automatic requests deliberately excluded** (`maybe_request_*` family, completion, signature-help, on-type-format, completion-resolve): not user-triggered per design §3, and arming them would have made every keystroke cancel a running search. Tests: `foreground_cancel` (4 new), `dispatch::tests` (2 rewritten/new). |
| **CG.4** ✅ | WASM plugin cancellation | **Landed 2026-08-22.** *The plan's mechanism did not exist* — the third CG slice whose entry criteria were wrong, exactly as the CG.1 note warned. There is no "fuel-exhaustion trampoline": `arm_store` sets fuel once and exhaustion traps; nothing re-fuels. The interruption mechanism that *does* exist is **epoch** (a 1ms ticker + `set_epoch_deadline`). Option (A) was chosen with Dhruva over between-call cancellation: `arm_store` now re-arms the deadline **one tick at a time** and installs an `epoch_deadline_callback` that polls the armed token, so `<C-g>` lands within ~1ms instead of waiting out the budget. **The cost, accepted knowingly:** enforcement of `budget.epoch_deadline` moved *into* the callback — wasmtime counts to 1 repeatedly and we count the total — so every running call pays a host callback per millisecond. **The token is snapshotted per call**, not read live: a call is cancelled by the operation armed when it *started*, so a later `arm()` cannot reach back into a call already in flight. **`TrapKind::Cancelled` does not quarantine** — fuel/epoch/other mean the plugin misbehaved, cancellation means the user changed their mind, and quarantining would punish a plugin for being interruptible. Tests: 2 (`cancel_running_call.rs`) — mid-flight cancel reports `Cancelled` under a `u64::MAX` fuel budget (the inverse of `runtime.rs`'s fuel-trap test, so the budget cannot be what ended it); a call started with nothing armed runs to its own budget despite a later arm+cancel. |
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
