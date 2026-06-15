# Typed motion dispatch — motions are actionable via the unified command API

**Status:** designed 2026-06-15, not yet implemented. Slice sequencing lives in
[the typed-motion-dispatch slice plan](../operations/slice-plans/typed-motion-dispatch.md).

## 1. The gap

A motion invoked through the typed command API — `:motion:goto-first-line`,
`:motion:word-forward`, a plugin calling `execute(...)`, a replayed
`CommandInvocation` — parses correctly but **does not move the cursor** on a
normal document buffer. This is why `gen:commands` filters motions out of
completion (offering a command that no-ops when invoked is misleading), which is
what surfaced the gap (see the parked
`arg_slot_completion_for_describe_command` test).

The cause is two divergent motion-execution paths:

- **Keystroke path** — `run_document_invocation` detects `CommandKind::Motion`
  and calls `lattice_grammar::execute_motion_only`, then writes
  `self.cursor = new_pos` by hand (`dispatch.rs`).
- **Typed / `:` path** — `execute_ex_line → dispatch_blocking →
  lattice_grammar::execute()`. `execute()` *does* route
  `CommandKind::Motion → execute_motion`, but that returns
  `Effect::SelectionChange(cursor)`, and the host's `SelectionChange` arm only
  adopts the cursor **when modal is Visual/Select**. After a `:` submit you are
  in Normal, so the effect is dropped. `execute_ex_line`'s own comment records
  the smell: *"The ex path bypasses run_invocation."* `:N` / `:gg` / `:G`
  half-work only because they special-case jump-history before the dropped
  effect.

So the cursor-moving wiring lives on one path; the unified `execute()` lives on
the other. The grammar IS the public command API (paramount #3); a motion that
the API can name but cannot perform breaks that contract.

## 2. Why this is a vim-grammar question, not a host patch

In vim's grammar a motion is not an action — it is a **primitive that yields a
target** (a position, plus a motion type: charwise/linewise,
inclusive/exclusive). What *happens* with that target is decided by the
grammatical frame the motion appears in:

| Frame | Effect of the motion's target |
| --- | --- |
| bare (no operator), Normal | cursor jumps to the target; selection collapses to it |
| operator pending (`d` / `y` / `c` + motion) | operator acts on `[start, target)` |
| Visual / Select | selection head moves to the target; anchor fixed (extend) |

The operator-pending frame never reaches the bare-motion return — `execute()`
dispatches an operator invocation to `execute_operator`, which resolves the
motion as a *range* internally. So the only thing a **bare** motion's result
needs to express is "this is the target position"; the host then applies it
according to the current modal frame.

That is exactly what the current design gets wrong by reusing
`Effect::SelectionChange` (a full selection set, only honored in Visual/Select)
to carry a bare motion's target. The fix is to give the bare-motion result its
own effect that the host honors **in every frame**, the way vim's grammar
honors a motion in every frame.

## 3. The model: `Effect::CursorMove`

Introduce a distinct grammar effect:

```rust
Effect::CursorMove(Position)   // "the motion resolved to this target"
```

- `execute_motion` returns `CursorMove(target)` instead of
  `SelectionChange(cursor)`. The motion's intrinsic output (a target) now has an
  effect that means exactly that, and nothing more.
- The host honors `CursorMove` **per modal frame**, mirroring vim:
  - **Normal** → set `self.cursor` (+ pane cursor), collapse the primary
    selection to it, push jump-history for jump-class motions (`gg` / `G` /
    `{` / `}` / `%` / search — the existing `push_position_history` policy).
  - **Visual / Select** → move the selection **head** to the target, keep the
    anchor (extend), exactly as a motion does inside a visual selection today.
  - **Terminal / synthetic (read-only)** → apply against the SyntheticDoc and
    run `sync_terminal_nav_cursor_from_doc`, the existing terminal-nav sync.
- **Text objects keep `SelectionChange`.** A bare text object (`viw`, `vaf`)
  genuinely sets a *span*; that is a selection change, not a cursor move. The
  `CursorMove` / `SelectionChange` split is itself meaningful: motion → point,
  text object → span. The dispatcher does not branch on *which* motion or object
  — only on the kind, as it already does.

With `CursorMove` honored uniformly, **both** the keystroke path and the typed /
`:` path can flow through the single `execute()` and get identical, vim-correct
behavior. The bespoke `self.cursor = new_pos` wiring in `run_document_invocation`
and the `SelectionChange`-only-in-Visual special case both retire — the two
motion paths collapse into one. That collapse is the actual win: it is the
unified-dispatch design decision (`§5.2.1`, "operators, motions, text objects,
ex-commands … share `CommandInvocation` and flow through one `execute(...)`")
finally holding for motions.

## 4. Rejected alternatives

- **(A) Make the host adopt `SelectionChange`'s cursor in Normal mode too.**
  Rejected: `SelectionChange` is emitted by many sources (text objects, plugins,
  async selection updates); honoring it as a Normal-mode cursor move everywhere
  has a wide blast radius and would move the cursor on selection updates that are
  not motions. It patches the symptom and leaves the dual-path / dual-meaning
  smell intact (heuristic #1).
- **(B) Wire only the `:` path to `execute_motion_only` + `self.cursor`.**
  Rejected as the *end state* (though a valid smaller step): it makes `:`
  motions work but blesses the two-path split rather than collapsing it — the
  typed path and the keystroke path keep separate motion executors. It does not
  encode the vim grammar (motion = target, frame decides effect); it just copies
  the keystroke patch onto the ex path. Chosen against per the explicit
  direction to avoid an intermediate patchy solution.
- **A motion-type-aware `CursorMove { target, motion_type }`.** Deferred, not
  rejected: the bare-motion frames here (Normal jump, Visual head-extend) only
  need the target position; charwise/linewise/inclusive/exclusive distinctions
  matter to the *operator* frame, which resolves the motion as a range in
  `execute_operator` and never sees `CursorMove`. If a future bare-motion
  consumer needs the motion type, the variant can grow a field then.
- **Multi-cursor motion (`CursorMove(Vec<Position>)`).** Deferred: today
  `execute_motion` only `replace_primary`s, so motions are single-cursor already
  — `CursorMove(Position)` is no regression. When multi-cursor lands, the effect
  can carry a per-cursor target set.

## 5. Scope boundary — motions now, operators later

This design covers **naked motions**. Operators (`operator:delete`,
`operator:yank`) are deliberately out of scope: a naked operator is meaningless
without a *target* (a motion, text object, or range), so making them actionable
via `:` is a target-argument design that overlaps the ex-range work, not a
cursor-move question. Operators stay filtered from completion until that
separate initiative lands. Text objects already produce a `SelectionChange`
span and are unaffected.

## 6. Paramount-goal alignment

> **UX (higher court):** no visible change until a typed motion is used; then it
> behaves identically to the key-bound motion (same cursor move, same
> jump-history). No flicker, no unedited-content change — `CursorMove` moves only
> the cursor/head.
> **Paramount #3 (extensible vim modal editing):** the direct payoff — the
> grammar genuinely IS the public command API; every registered motion is
> invocable through the unified dispatch, and adding a motion lights it up on
> both the keystroke and typed paths with zero per-motion host wiring.
> **Paramount #2 (extensibility):** plugins / `init.rs` / macros that call
> `execute(motion)` get real cursor movement, not a dropped effect.
> **Paramount #1 (performance):** `CursorMove` is a point; applying it is O(1),
> no new per-frame work.
> **Heuristic #1 (long-term fit, on merit):** collapses two motion paths into
> one — the genuinely-better design, not a patch. The merit is the unification +
> vim-faithful framing, not novelty.
> **Heuristic #2 (paramount, not other editors):** anchored on Lattice's own
> unified-dispatch decision and vim's motion-target grammar, not "editor X does
> it."

## 7. Cross-renderer + test discipline

- `Effect::CursorMove` is a new `Effect` variant → the TUI effect classifier and
  the **GPUI** effect classifier are updated **in the same patch**
  (`feedback_tui_gpui_parity`); end-of-slice grep
  `grep -rn "Effect::CursorMove" crates/lattice-ui-gpui/` must be non-empty.
- Tests cover all three frames (Normal jump, Visual/Select head-extend,
  terminal sync), count (`:3motion:line-down`), and graceful failure (motion on
  an empty buffer echoes nothing, never panics).

## See also

- [typed-motion-dispatch slice plan](../operations/slice-plans/typed-motion-dispatch.md)
- design.md §5.2.1 (unified command / grammar dispatch)
- [select-mode.md](select-mode.md) (sibling Select-mode dispatch work)
