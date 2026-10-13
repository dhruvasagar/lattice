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

The choice is made once, before either peer boots, by
`lattice-cli`'s `renderer::choose`:

| | renderer |
|---|---|
| `--tui` | terminal |
| `--gui` | window, or an error — never downgraded |
| a build without the `gui` feature | terminal |
| inside a macOS `.app` bundle | window |
| a display is reachable | window |
| otherwise | terminal |

"A display is reachable" is read from the environment: on Linux and the BSDs
`WAYLAND_DISPLAY` or `DISPLAY` is set; on macOS and Windows the session is not
an SSH one. With no display the terminal opens without a flag, because the
alternative is an editor that fails to start on exactly the machines the
terminal renderer exists for. An explicit `--gui` is a request, so it gets a
window or an error. The guess errs toward the terminal: a wrong "no" costs
typing `--gui`, a wrong "yes" costs a failed launch.

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

### Text input in the window

Reading input is the one place the peers differ by nature. A terminal
emulator composes text before the editor sees a byte: a dead key, an accent
picker and an input method all arrive as finished characters. A window gets
key-downs, and the operating system composes only for a view that registers
as a text-input client and leaves the composing keys unhandled.

So the window routes each key-down (`lattice-ui-gpui::text_input`):

- **Where text is entered** — Insert, Replace, the `:` line, search, a
  prompt, a picker's query — a key whose name is one character is left to the
  platform, which calls back with the text it types. That text is dispatched
  as the chords typing it would be, so it reaches every text-entry state and
  Insert-mode bindings on characters still see it.
- **Everywhere else** the key is dispatched as pressed. The grammar never
  passes through composition (paramount #3): an active input method does not
  swallow `j`.
- **`Option` + a printable key** runs a binding when the keymap claims the
  chord as pressed (`input::binds_as_pressed`), and types what the platform
  says otherwise. A terminal pane always takes it as Meta.

In-progress text (a dead key's accent, an input method's candidate) is window
state, drawn underlined at the caret. Nothing crosses to the editor until it
is committed, so composing costs no edit, undo entry, reparse or LSP change
(paramount #1).

Rejected: composition in every mode (an input method would eat Normal-mode
commands); publishing the in-progress text from the host (an actor crossing
per composition step for state only one peer can write); inserting it into
the buffer and replacing it (each step an edit); an "Option is Meta" setting
(the keymap can decide per key, so no setting is needed to have both).

Known limits: the in-progress text is drawn over the text at the caret
rather than pushing it aside; on the `:` line and in a search it is anchored
at the row's left edge, not the caret's column; the platform is not offered
surrounding text, so reconverting committed text is unavailable.

### How it is checked

`lattice_host::peer_conformance` holds one set of scenarios, and each peer
runs all of them through its own key and dispatch entry points. The host's own
tests cannot do this: they drive `Editor` directly, which goes around the code
that keeps the contract. A scenario asserts editor state after a key sequence
and the number of actor crossings taken to reach it; it never asserts paint.

## 3. When the host fuses

The fuse is taken when the peer has nothing to do before the tail: no signals,
no follow-up actions, no popup up, and every returned effect is one the host
has finished. That last test is
`effect_completeness::effect_is_host_complete`, an exhaustive match over
`Effect` with no wildcard arm — a new variant does not compile until it is
placed, and `false` is always safe.

It was `effects.is_empty()` until GD.0b. That is true for a typed character
and false for every motion and operator, whose effects (`CursorMove`, `Edits`,
`Yank`) are finished the moment the host applies them. So `j` cost several
blocking round-trips in both peers where a typed character cost one.

The classification is a claim about both peers at once — "neither has an arm
that does anything for this variant" — and nothing in the type system ties it
to their matches. Two things bound the damage of getting it wrong. The list of
`true` answers is short and limited to the editing hot path. And a peer runs
every returned effect through its arms on the fused path too, so a variant
wrongly called host-complete has its renderer half run after the tail instead
of being skipped.

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
