# Unified command dispatch — slice plan

Sequencing for [typed-motion-dispatch.md](../../../../architecture/typed-motion-dispatch.md)
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

- **UD.2 ✅ — re-enable motion completion.** `CommandsGenerator`
  (`gen:commands`) now emits `CommandKind::Motion` candidates alongside
  ex-commands (it matched `ExCommand` only before). Motions keep their
  `motion:` prefix (the name the user types); ex-commands still strip `ex:`.
  Operators / text-objects / `action:*` stay filtered (an operator with no
  target is not standalone-actionable). Restored the `motion:*` expectation
  in `arg_slot_completion_for_describe_command_shows_command_names` +
  `accept_in_arg_slot_*` (parked in `17fb3d77`); updated the generator's own
  filter test to pin the new boundary (motions in, operators out).

- **UD.3 🗒 — `Effect::CursorMove` (DEFERRED, design §3).** A clean
  motion-target effect for the plugin contract. No behavior win today (the
  `SelectionChange` path works through the unified dispatch); revisit at the
  plugin-host stage when the extension effect-vocabulary is designed. Not
  scheduled.

## Out of scope (separate / low-value)

- **Operator *completion*.** Operators WITH a target already *dispatch* via
  `:` through the unified path (`:operator delete word-forward` deletes a
  word, verified by `ex_line_operator_with_target_mutates_via_unified_dispatch`)
  — no target-argument/ex-range design is needed for that. What's missing is
  only *completion* of the two-part `:operator <op> <target>` form, which is
  low value (the keystroke grammar `dw` / `ci"` is the ergonomic path), so
  operators stay out of `gen:commands`. A helpful error for a *naked*
  `:operator:delete` (no target) is a possible small polish.
- **Plugin host call** — `dispatch_invocation` is the entry plugins will
  call (capability-gated); lands with the WASM plugin host.
