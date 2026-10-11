# GUI by default, and the peer contract

> **Design fragment.** Contracts, rationale, rejected alternatives,
> paramount-goal alignment. Slice sequencing lives in
> [`../operations/slice-plans/gui-default.md`](../operations/slice-plans/gui-default.md).
> Related: [`input-pipeline.md`](input-pipeline.md) (latency),
> [`boot-composition.md`](boot-composition.md) (boot, the inbound wake).

## 1. The decision

`lattice` opens the GPUI window. `--tui` opens the terminal renderer. Both stay
first-class: the TUI is what runs over SSH and on a machine with no display,
and that does not change.

What changes is which peer a gap is allowed to sit in. While the TUI was the
default, a GPUI gap was a known limitation of an opt-in renderer. As the
default, the same gap is a broken editor. This document is the contract that
keeps the two from drifting, written because they had.

## 2. The peer contract

A renderer peer is a thin composition root around `Editor`: it turns its
toolkit's input into `Action`s, and paints published `RenderState`. Everything
between those two is the host's. Concretely, a peer does three things around a
dispatch and nothing else.

**It calls `Editor::dispatch_fused`, never bare `Editor::dispatch`.**
`dispatch_fused` is dispatch plus the post-dispatch tail, in the actor:

```
maybe_adopt_owner_write        before the action: pick up an owner's cursor move
dispatch(action)
ensure_cursor_visible          unless this action dismissed a focused popup
maybe_reparse_syntax           no-op when the text did not change
sync_keymap_overlays           completion popup + session-backed minor layers
run_tick_pending               drain async results
```

The tail is a list that grows. Each entry was added for a reason that applies
to every renderer, and a peer that re-creates the list by hand has, by
construction, the list as it stood on the day it was copied. That is not a
hypothetical: GPUI called bare `dispatch` and kept its own tail, and what
followed was `maybe_reparse_syntax` missing (syntax colour vanished on the
first edit, `53dac8dc`), then `sync_keymap_overlays` missing (the completion
popup opened and none of its keys reached it; snippet tabstops likewise), and
`maybe_adopt_owner_write` missing. Three steps, three silent regressions in one
peer, each found by a user.

**When the host declines the fuse, the peer runs the renderer-coupled work
and then the same tail.** The fuse is declined when the dispatch left effects,
signals or follow-up actions, or a popup is up. The order is fixed:

1. `outcome.effects`, through the peer's renderer-coupled arms. The host has
   already applied its half of each.
2. Signals: those queued on the editor, then the outcome's own.
3. Each follow-up action through the peer's whole dispatch entry — not a bare
   dispatch. A macro replays as follow-up actions, and "open a file, then type"
   has to open before it types.
4. The tail, in one actor crossing, in the order above, then
   `drain_pending_renderer_effects`.

**It does none of this from its paint path.** Dispatch is a blocking actor
round-trip with a channel drain on its tail. `Render::render` is the UI thread.
A peer that needs the cursor kept in view gets that from the tail and from the
viewport setters, not from a per-frame dispatch (paramount goal #1).

### What a peer may differ in

How it reads input, how it paints, and the renderer-coupled arm of each effect
(`OpenBuffer` ends in a different window-management call). Nothing else.
`Action` and `Effect` matches in both peers are exhaustive with no wildcard, so
a new variant fails to compile in the peer that forgot it; the tail had no such
guard, which is why it is now a host function and not a convention.

## 3. What the fuse does not cover

`dispatch_fused` declines whenever `outcome.effects` is non-empty, and a motion
leaves its `Effect::CursorMove` there although the host has applied it. So a
typed character is one actor crossing and `j` is three, in both peers. The
contract above is unaffected — the non-fused path is correct, only slower — but
the claim in `input-latency.md` § I.7 that motions fuse does not hold. Whether
to fuse on host-complete effects is slice GD.0b.

## 4. Rejected alternatives

**Patch the missing calls into GPUI.** Smaller, and it fixes every symptom
known today. Rejected on heuristic #1: it leaves two tails, and the defect is
that there are two.

**A `trait Peer` the host drives.** The host would own the whole sequence and
call back into the peer for effects and signals. It makes the order
unbreakable, and it inverts the ownership the Phase 5 extraction settled on —
a peer is a composition root that owns its loop, because ratatui's pull-based
draw and GPUI's retained tree do not share one (`implementation.md`, the
dropped `lattice-render` row). The fused call gets the tail shared without
that inversion; the remaining order (steps 1–3) is pinned by the conformance
suite (GD.2) instead of by a trait.

## 5. Paramount-goal alignment

- **#1 performance.** One crossing per typed character in the GUI, down from
  four; no dispatch on the paint path.
- **#4 asynchronicity.** The tick drain rides the keystroke's own crossing in
  both peers; idle results are GD.1.
- **UX (higher court).** Every item in §2 was user-visible when it was missing.
