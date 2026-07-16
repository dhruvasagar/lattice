+++
title = "Unified command dispatch — one path for `:`, the keymap, and plugins"
+++

# Unified command dispatch — one path for `:`, the keymap, and plugins

**Status:** landed 2026-06-15 (the consolidation). Slice sequencing +
remaining items live in
[the slice plan](../operations/slice-plans/archive/typed-motion-dispatch).

> **Correction (2026-06-15).** An earlier revision of this doc claimed
> "`:motion:*` does not execute the motion." That was **wrong** — verified
> empirically. A `:`-invoked motion *does* move the cursor: `execute()`
> returns `Effect::SelectionChange(cursor)`, `handle_effect` sets
> `self.cursor`, and `build_render_state` publishes `cursor: self.cursor`
> (dispatch.rs:757), which is what the active pane renders. The real
> problem was architectural, below.

## 1. The real gap — two dispatchers

`CommandInvocation` is the universal currency: motions, operators, text
objects, ex-commands, and plugin contributions all share it and flow
through one `execute(...)` (design.md §5.2.1). But the *host-side* dispatch
had split into two entries:

- **Keymap** (`Action::Invoke`) → `run_invocation`: the rich path — mode
  action-handlers → per-kind buffer runner → grammar Action gate →
  `run_document_invocation` (jump-history, find/till capture for `;`/`,`,
  count + fold-aware expansion, dot-repeat recording, visual-exit,
  cursor-clamp) → `dispatch_blocking` → `apply_effect_host`.
- **`:` line** (`execute_ex_line`) → a **thin re-implementation**: it
  duplicated the goto jump-history push, then ran `dispatch_blocking` +
  `apply_effect_host` and *skipped everything else*.

So a `:`-invoked (or plugin-invoked) motion moved the cursor but silently
skipped fold-opening, find-repeat capture, and dot-repeat — second-class
to the same motion typed as a keystroke. That asymmetry — duplicate effort,
no single API — is what this work removes.

## 2. The consolidation (landed)

One entry, consumed by every caller:

- `run_invocation` is renamed **`Editor::dispatch_invocation(inv, out)`** —
  the single command-dispatch path. (`dispatch(Action)` already owns the
  Action-level name; the invocation-level entry takes the `_invocation`
  suffix.)
- `execute_ex_line` parses the `:` line and routes the invocation straight
  through `dispatch_invocation`. The thin ex-only path and its duplicated
  jump-history push are deleted.
- The keymap already calls it (`Action::Invoke`). Plugins will call the
  same method (capability-gated host call) when the WASM host lands.

Result: a motion or command behaves *identically* whoever invokes it —
keymap, `:`, macro replay, plugin. No `:`-specific subset.

This is the unified-dispatch design decision (§5.2.1) finally holding for
the *host* dispatch layer, not just the grammar `execute()`.

## 3. Effects: `SelectionChange` carries the motion target (today)

A bare motion's result is surfaced as `Effect::SelectionChange(cursor)` and
`handle_effect` decodes it per modal frame — Normal: move the cursor;
Visual/Select: extend the head, keep the anchor (the `anchor == head`
discriminant). This already gives vim-correct, frame-dependent behavior
(motion = target; the frame decides the effect), and it now runs through
the single dispatch for every caller.

**Deferred: `Effect::CursorMove(Position)`.** A distinct motion-target
effect (motion = point, vs `SelectionChange` = span) would be a *cleaner
contract* for the eventual plugin API — a plugin gets back "the motion
landed here," not "a selection set whose anchor happens to equal its head."
It was scoped here but is **not implemented**: with the dispatch unified and
motions already working through `SelectionChange`, it carries no behavior
win today, only a new `Effect` variant + both renderers' classifiers
(heuristic #1 — no rewrite without a concrete merit win). Revisit when the
plugin effect-vocabulary is designed (the plugin host stage), where the
explicit contract earns its cost.

## 4. Completion (resolved — motions in)

With the consolidation, **motions *are* actionable** via `:` (identically to
keystrokes), so the old "not actionable" rationale for excluding them from
completion no longer holds. `gen:commands` (`CommandsGenerator`) now emits
`CommandKind::Motion` candidates alongside ex-commands, so `:`-completion and
`:describe-command` / `:apropos` surface `motion:line-down` etc. (with their
`motion:` prefix — the invocable name).

**Operators stay out of completion — but they *are* dispatchable.** An
operator WITH a target already runs via `:` through the same unified path:
`:operator delete word-forward` (or canonical
`:operator:delete motion:word-forward`) resolves the motion as a range and
commits the edit, exactly like the keystroke `dw` (verified by
`ex_line_operator_with_target_mutates_via_unified_dispatch`). They are
excluded from *completion* not because they "don't work" but because (a) a
bare `operator:delete` candidate is misleading — it needs a target to do
anything; and (b) completing the full `:operator <op> <target>` form is a
two-part (operator + target) problem of low value, since the keystroke
grammar (`dw`, `ci"`) is the ergonomic path. The generator's filter test
pins the boundary (motions in, operators out of completion).

## 5. Rejected alternatives

- **(A) Adopt `SelectionChange`'s cursor in Normal as a special host
  branch / patch the pane cursor.** Rejected: chased a non-bug (the pane
  cursor stash isn't what the active pane renders) and would not address the
  actual duplication (two dispatchers).
- **(B) Wire only the `:` path to a second motion executor.** Rejected: it
  blesses the split instead of collapsing it — the opposite of one API.
- **(C) Introduce `Effect::CursorMove` now.** Deferred (§3): real as a
  plugin-contract refinement, but no behavior merit once dispatch is
  unified — premature.

## 6. Paramount-goal alignment

> **UX (higher court):** no visible change — `:` motions already moved the
> cursor; they now additionally get fold-open / find-repeat / dot-repeat,
> matching keystrokes. No flicker, no unedited-content change.
> **Paramount #3 (extensible vim modal editing):** the grammar IS the
> public command API — one host dispatch for every invocation source.
> **Paramount #2 (extensibility):** plugins consume the same
> `dispatch_invocation`; no per-source wiring.
> **Heuristic #1 (long-term fit):** collapses two dispatchers into one —
> the merit is the unification, not novelty. `CursorMove` is *not* added
> precisely because it lacks a merit win today.

## See also

- [slice plan](../operations/slice-plans/archive/typed-motion-dispatch)
- design.md §5.2.1 (unified command / grammar dispatch)
