# Foreground cancellation — slice plan

Sequencing companion to
[`docs/dev/architecture/cancellation.md`](../../architecture/cancellation.md).
The design fragment is the source of truth for *what* and *why*;
this file owns *when* and *in what order*. Authoritative status per
slice lives in [`../implementation.md`](../implementation.md).

> **Status: 🚧 in progress — CG.1 ✅ (2026-08-07), CG.2 ✅ + CG.3 ✅ (2026-08-08),
> CG.4 ✅ (2026-08-22), CG.5 ⛔ deferred; keyed supersede CG.6 ✅, CG.7a ✅, CG.7b-i ✅, CG.7b-ii ✅, CG.7c ✅, CG.8 ✅, CG.9 ✅, CG.10 ✅, CG.11–CG.13 📝, CG.14 ⛔
> (design §9, 2026-10-20; same inputs keep the run in flight, new inputs supersede).** The cancel triggers, the `active_cancel` seam and the mode
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
| **CG.6** ✅ | Keyed supersede: the primitive + process runner | `TaskKey { kind, scope, inputs, policy }` / `TaskTicket` (token + generation, `is_current()`, drop frees its slot) / `AlreadyRunning` on `ForegroundCancel::begin` (design §9.1): same inputs → refused, the run in flight carries on; new inputs → `Supersede` cancels it, `RunToCompletion` refuses (§9.2); a cancelled run counts as gone, so `<C-g>` + re-trigger restarts. `Supersede` runs reachable by `<C-g>`. `lattice_core::process::run_cancellable` — own process group, group kill via `kill(1)` / `taskkill /T`, reader threads (§9.3). `cancel.rs` headers no longer say supersede is "NOT YET WIRED". Tests: 8 keyed (`foreground_cancel`), 4 runner (incl. the grandchild kill, which fails with no process group). Bench `task_key`: ~145 ns per trigger, flat across 0/8/64 tasks in flight; supersede ~110 ns; refusal ~40 ns. |
| **CG.7a** ✅ | Magit remote ops on keys | Every `RemoteOp` declares `retrigger` (a field, so a new op cannot forget): push / fetch / notes-prune `Supersede`, the other 22 — pull, the sequencer verbs, stage/unstage-all, stashes — `RunToCompletion`. `spawn_remote_op` / `spawn_remote_op_to` begin a key — identity = op + repo + destination row + prompted name (resolving the remote needs git, and the refusal must be decided before spawning); inputs = git's argv — and echo "*push is already running in lattice*" on refusal. `run_remote_op` through `run_cancellable` with `ProcessGroup::Shared` (an ssh passphrase prompt must not be suspended). A stopped or no-longer-current run logs one line and publishes nothing. The cancel service is captured at install like the event bus, and boot now registers it BEFORE the installs (it was registered after them, so an install looking it up got `None`); install warns if it is missing. Tests (real git, a sleeping `pre-receive`): a double-pressed push runs once; a push re-run with `-f` supersedes (only the new one reports); pull refuses even new flags; a prompted destination is another task — all four fail with keying removed. |
| **CG.7b-i** ✅ | Magit repository mutations, `magit_global_mode` | `spawn_repo_task(workdir, identity, label, work)`: identity = repo + the git command (or the label naming op + target); the same one running refuses (`RunToCompletion`), a different one waits its turn on `RepoTurn` — a per-repository FIFO ticket gate reserved at trigger time, waited on the blocking pool, abandoned reservations skipped (decided: option b — git has one index writer, and rapid operations raced on `.git/index.lock`). Through it: `file_mutate!` (`C-c f`), branch ops (`spawn_repo_op`), `spawn_git`, `spawn_git_sequence`, rebase verbs, `spawn_computed`, subtree, notes merge, gitignore, commit ops; bisect inline (refreshes views after). Clone keyed without the gate (no repo yet). RTC remote ops (CG.7a) take turns too. Tests: different ops take turns in trigger order; other repos don't wait; same op refused while running AND while queued; an abandoned turn is skipped — the first two fail with no gate. |
| **CG.7b-ii** ✅ | Magit mutations in the mode files | `RepoMutation { begin, run }` — the keyed turn for call sites that drive their own spawn and refresh (`spawn_repo_task` now builds on it). Through it: hunk apply / discard (`s` `u` `x` in the status buffer; identity = op + patch), the views' `spawn_mutation_and_refresh` (status, branch, stash, diff, refs), remote / submodule mutations (prompt-driven: a refusal is logged), and the compose confirms — commit, rebase, note, `:magit-branch-create` — through `spawn_repo_task`, which also gives them the `finish_task` report they lacked (open views now refresh after a commit). Guard: `every_mutation_helper_in_the_crate_reports_completion` also requires `RepoMutation::begin`, refs' helper included. Test: a repeat is refused until the first has run. |
| **CG.7c** ✅ | Magit view refreshes | A `RefreshStamp` per buffer, not a `TaskTicket` — refreshes are mostly automatic, and a keyed ticket would let any `<C-g>` drop every pending repaint (design §9.3a). Taken when a refresh starts; it paints only if still the latest. Every refreshable view: status (`do_refresh`), branch, stash, log, diff, refs, remote, submodule, cherry — their `refresh`, their mutation helpers' rebuild, and their first load in `on_activate`. The headerline's busy flag now counts holders, so a superseded refresh finishing first no longer clears "refreshing". Guard `every_view_that_repaints_takes_a_stamp` reads `src/` at test time, so a new view is checked unlisted (one-shot views and the paint helpers exempt, by name); fails with a stamp removed. |
| **CG.8** ✅ | Compilation re-run | Keyed on `ForegroundCancel` (compile / shell-command; inputs = cwd + command): the same command running is refused (`run` now returns the message; `start_compilation` shows the running build's buffer and echoes it), a different one supersedes. Each run has a generation (`current_run`): its child is stored with it so a coordinator reaps only its own (the old one took the new run's child → "terminated"), its readers stop publishing once stale, and a superseded run's `Finished` is not published. The re-run kill is `kill_group` (was `Child::kill`, the shell only — grandchildren kept the pipes and streamed into the next run). The coordinator watches the token, so `<C-g>` stops a build ("terminated"). Boot now registers `ForegroundCancel` in Phase A, before every install (compilation installs before magit). Tests: a different command stops the old (a pipeline) with nothing leaking and the new finishing normally — fails cleanly with the old kill; the same command refused, first carries on; `<C-g>` stops promptly and twice. User doc: compilation-mode "While a build is running". |
| **CG.9** ✅ | Formatters | A user `:format` (LSP or external rung) begins one key — `format` + buffer, inputs = document version + rung/range — in `begin_format_task`: the same format running is refused ("already running"), a format of changed text supersedes (the LSP request's token / the external formatter is stopped). `format_result_applies` checks the ticket is current AND the active buffer + version are the ones it was computed for (they were not checked: edits landed in another buffer, or in moved text); otherwise "not applied". The LSP request no longer mints an unrelated token or `enrol`s (the ticket's token is `<C-g>`-reachable). External: `lattice_format::run_cancellable` + `FormatError::Cancelled` (not noteworthy). On-type formatting stays automatic and unkeyed; it drops a pending user format's ticket/target, since it replaces the shared receiver. Tests: an edit made mid-format is not overwritten; `:format` twice — refused, the first lands; a cancelled formatter stops promptly. |
| **CG.10** ✅ | Plugin loader | `LoaderServices.foreground_cancel`, captured at install (warns if missing). `rebuild` / `update` begin `plugin:build` + plugin name, `RunToCompletion` — they drive git in the plugin's source cache, where a kill can leave `index.lock`, so any second trigger is refused (`ALREADY_BUILDING`, now `pub` so the plugins view logs a refusal as info, not "failed"); a bulk leg refused that way is `Skipped`. `run_bulk` begins `plugin:bulk`; a second bulk run is refused via `BulkReport.refused`. Fixes the build-activity flag being cleared by the first of two concurrent builds. **Not keyed:** `:reload-config` — its build is `cargo`'s, whose target-dir lock already serialises two, and a double reload re-applies a cached build; noted rather than forced. Tests: a building plugin refuses another build of itself only; `rebuild` / `update` refused while it builds; a bulk leg for it is skipped; a second bulk run is refused and says so. |
| **CG.11** 📝 | Agenda + project-diff scans | Keyed by view; the last to start wins. |
| **CG.12** 📝 | Project search onto keys; retire `arm()` | Search keyed by view (a search in one view stops cancelling another's); the batch-after-`gr` window closed by the ticket. `arm()` goes once nothing uses it — the plugin host's per-call token (`current_token`, CG.4) moves to the enrolled set first. |
| **CG.13** 📝 | Pickers stop their work | File walk and grep stop on the token (today results are dropped but the work runs to the end). |
| **CG.14** ⛔ | LSP `pending_*_token`s onto keys | Optional: 21 hand-written per-feature supersede tokens become keys. They behave correctly today; worth it only if it removes code. |

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
