# Unified command dispatch — slice plan

Sequencing for [typed-motion-dispatch.md](../../architecture/typed-motion-dispatch.md)
(design: the *what* and *why*). This file is the *when* and *in what order*.

Goal: ONE host-side command-dispatch entry (`Editor::dispatch_invocation`)
consumed by the keymap, the `:` line, and (eventually) plugins — no thin
`:`-only duplicate. See the design doc §1–§2.

> **History:** this plan originally scoped an `Effect::CursorMove`
> unification on the premise that `:` motions didn't execute. That premise
> was **wrong** (motions did move the cursor; the duplication was the two
> dispatchers). The plan was re-grounded 2026-06-15 to the actual gap.

## Slices

- **UD.1 ✅ — consolidate dispatch** (`2b598bf6`). Renamed `run_invocation`
  → `Editor::dispatch_invocation` (the rich path); `execute_ex_line` now
  routes the parsed invocation through it instead of a thin
  `dispatch_blocking + apply_effect_host` (deleting the duplicated goto
  jump-history). Keymap (`Action::Invoke`), `:`, and macro replay all call
  the one entry. Full suite green (host 748, ui-tui 1474); +1 acceptance
  test (`ex_line_motion_routes_through_unified_dispatch`).

- **UD.2 🗒 — re-enable motion completion (DECISION PENDING).** With UD.1,
  motions are actionable via `:`, so the `gen:commands` motion filter's
  "not actionable" rationale no longer holds for motions. Re-enabling their
  completion in `:describe-command` / `:apropos` is now a **UX call** (do
  motions belong in `:`-completion?) — awaiting Dhruva. If yes: drop the
  motion filter at `builtins/generators.rs` (~L232), restore the `motion:*`
  expectation in `arg_slot_completion_for_describe_command_shows_command_names`
  (parked in `17fb3d77`), update the generator's filter test. Operators stay
  filtered (no-target = genuinely not actionable).

- **UD.3 🗒 — `Effect::CursorMove` (DEFERRED, design §3).** A clean
  motion-target effect for the plugin contract. No behavior win today (the
  `SelectionChange` path works through the unified dispatch); revisit at the
  plugin-host stage when the extension effect-vocabulary is designed. Not
  scheduled.

## Out of scope (separate initiative)

- **Operators via `:`** (`operator:delete` + target) — needs a
  target-argument / ex-range design; operators remain completion-filtered.
- **Plugin host call** — `dispatch_invocation` is the entry plugins will
  call (capability-gated); lands with the WASM plugin host.
