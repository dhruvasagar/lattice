# Typed motion dispatch — slice plan

Sequencing for [typed-motion-dispatch.md](../../architecture/typed-motion-dispatch.md)
(design: the *what* and *why*). This file is the *when* and *in what order*.

Goal: naked motions invoked through the unified command API / `:` line move the
cursor on a normal document buffer, via a single vim-correct path
(`Effect::CursorMove`, honored per modal frame), collapsing the current
keystroke-vs-typed dual motion path. Operators are explicitly out of scope
(design §5).

Approach **(C)** is LOCKED (2026-06-15, Dhruva): the principled `Effect::CursorMove`
unification, chosen over the (B) ex-path patch because it encodes vim's
motion-target grammar rather than blessing the two-path split. See design §3–§4.

## Slices

- **TMD.0 🗒 — pin the gap.** A failing test: `:motion:goto-first-line` (and
  `:motion:word-forward`) submitted on a multi-line normal buffer leaves the
  cursor unmoved. Lands red, documents the bug, becomes the acceptance test for
  TMD.2. (Host integration test.)
- **TMD.1 🗒 — `Effect::CursorMove(Position)` + host honoring (both renderers).**
  Add the variant to the grammar `Effect` enum. Host effect pipeline
  (`apply_effect_host`) honors it per modal frame: Normal → `self.cursor` +
  collapse primary selection + jump-history for jump-class motions; Visual/Select
  → move head, keep anchor; terminal/synthetic → `sync_terminal_nav_cursor_from_doc`.
  TUI **and** GPUI effect classifiers updated in THIS patch
  (`feedback_tui_gpui_parity`; grep gate on `lattice-ui-gpui`). No emitter yet —
  the variant is dead but the tree is green. Host tests per frame.
- **TMD.2 🗒 — `execute_motion` emits `CursorMove`.** Flip the grammar's
  bare-motion return from `SelectionChange(cursor)` to `CursorMove(target)`.
  Bare motions now move the cursor through `execute()` in every frame, so the
  `:` / typed path works — TMD.0 goes green. Remove the now-redundant
  "SelectionChange adopted only in Visual/Select" handling *for motions* (text
  objects keep `SelectionChange`). Grammar unit tests + integration: count
  (`:3motion:line-down`), Visual head-extend via `:`, empty-buffer no-op.
- **TMD.3 🗒 — collapse the dual path.** Route `run_document_invocation`'s
  motion branch (and any other `execute_motion_only` + manual-`self.cursor`
  caller) through the single `CursorMove`-application helper, retiring the
  bespoke host cursor-set. One motion-application path for keystroke + typed +
  terminal. Verify keystroke motions, `:` motions, and terminal-nav motions all
  still behave (regression net: existing motion tests stay green).
- **TMD.4 🗒 — re-enable motion completion.** Drop the motion filter in
  `gen:commands` (`builtins/generators.rs` ~L232) so `:describe-command` /
  `:apropos` complete the whole grammar surface. Restore the `motion:*`
  expectation in `arg_slot_completion_for_describe_command_shows_command_names`
  (parked in commit `17fb3d77`). Operators stay filtered (still not actionable —
  separate initiative). Update the generator's own filter test.
- **TMD.5 🗒 — docs + ledger.** Mark slices ✅ as they land; update the
  todo.org entry ("make motion/operator commands actionable…") to point at this
  plan and narrow it to the operators-remaining follow-up.

## Sequencing notes

- TMD.0 before TMD.1/2 (red-first; it is the acceptance test).
- TMD.1 must land green with no emitter (additive); TMD.2 flips the emitter so
  motions never break mid-slice. Splitting them keeps each commit green and
  isolates the renderer-parity change from the behavior change.
- TMD.3 is the unification; it can land after TMD.2 (motions already work) and
  is the slice most likely to surface latent assumptions about the two paths —
  keep its diff reviewable and lean on the existing motion test corpus.
- TMD.4 depends only on TMD.2 (motions actionable) — it can land before or after
  TMD.3.

## Out of scope (separate initiative)

- **Operators via `:`** (`operator:delete` + target). Needs a target-argument /
  ex-range design; tracked separately. Operators remain completion-filtered.
- **`Effect::CursorMove { target, motion_type }`** and multi-cursor targets —
  deferred per design §4; grow the variant when a consumer needs it.
