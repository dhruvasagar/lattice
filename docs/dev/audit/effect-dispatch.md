# Effect dispatch — how an `Effect` reaches its appliers

**Audited 2026-07-03**, prompted by dashboard URL links (`GitHub`, `Issues`)
silently not opening. The trace found the effect architecture is coherent; the
bug was a lone violation of its central invariant. This document records the
invariant so the next change doesn't re-introduce the same class of bug.

## The invariant (load-bearing)

> **Every `Effect` placed in `DispatchOutcome::effects` has already been
> host-applied via `handle_effect`.** The renderer that later drains
> `out.effects` runs only the *renderer half* and trusts the host half already
> ran.

Violate it — push a raw effect onto `out.effects` without calling
`handle_effect` — and the effect's host action silently never happens, because
the renderer half deliberately no-ops host-applied effects.

## The two halves of an effect

An `Effect` can carry work in two layers, and most carry only one:

- **Host half** — mutates editor state / does OS work. Lives in the single host
  applier `handle_effect` (`lattice-host/src/dispatch.rs`, the big `match`).
  Examples: `OpenExternalUri` (spawn `open`/`xdg-open`), `SaveBuffer`,
  `SetOption`.
- **Renderer half** — renderer-coupled work a mode crate / host can't do
  (open a window, drive the LSP runtime, run the tutor's file-open). Lives in
  each peer's renderer-only applier: `App::apply_effect_app_arms`
  (`lattice-ui-tui/src/app/dispatch.rs`) and `GpuiApp::apply_effect_gpui`
  (`lattice-ui-gpui/src/lib.rs`). These two mirror each other and **no-op** any
  effect whose work is entirely host-side.

`Effect::Tutor` is renderer-half-only (`do_tutor` opens a lesson file, peer
work); `Effect::OpenExternalUri` is host-half-only (spawn an OS handler);
`Effect::SetColorscheme` touches both.

## The paths

### 1. Synchronous, on-keystroke (the common follow / command path)

A handler running *inside* `dispatch` that needs a host action calls the
concrete host method **directly** — it does not emit an effect:

- `do_help_follow_link` arms: `self.do_edit(...)`, `self.do_open_help_topic(...)`,
  `self.open_external_uri(&url)`, …
- `follow_document_link_target` (LSP documentLink → URL):
  `self.open_external_uri(target_str)`.

Direct calls sidestep `out.effects` entirely, so the invariant can't apply to
them. This is the pattern to copy for any new synchronous follow.

### 2. Effects produced during dispatch → `apply_effect_host`

When dispatch machinery *does* need to surface an effect (e.g. an ex-command's
resolved `Effect`, an `Effect::Many` fan-out), it goes through the bridge:

```
apply_effect_host(editor, effect, out):
    handle_effect(editor, effect, out)   // host half runs NOW
    out.effects.push(effect)             // surfaced for the renderer half
```

This is what upholds the invariant: the push is always preceded by
`handle_effect`. (`dispatch.rs:~14846` is a second push site — a per-line
re-emit — and it too calls `handle_effect` first.)

### 3. Asynchronous, off-keystroke (inbound bus) → emit the `Effect`

Work that arrives off the keystroke path (LSP `show_document`, claude-code
inbound tools) is *produced as* an `Effect` by a mode-owned handler and drained
later by `run_tick_pending → handle_effect`. Here the effect **must** be
host-applied, because peer-applied effects are discarded on the off-keystroke
tick. This is the *only* correct producer of `Effect::OpenExternalUri`
(`lattice-lsp/src/show_document.rs`).

### Renderer drain (both peers)

After dispatch returns, the peer drains `out.effects` through its renderer-only
applier (`apply_effect_app_arms` / `apply_effect_gpui`) — renderer half only,
never `handle_effect` again (that would double-apply the host half).

The actor's `drain_outcome` (`editor_actor.rs`, the `EditorCommand::Apply` /
`HandleEffect` path) is a *different* entry point that calls `handle_effect`
itself; the renderer keystroke path does not use it (it uses
`mutate_editor_with(dispatch_fused)` / `dispatch_action`).

## The bug this audit came from

`do_help_follow_link`'s `Url` arm was written as:

```rust
out.effects.push(Effect::OpenExternalUri { uri: url });   // WRONG
```

A raw push from a *synchronous* handler: `handle_effect` never ran (so
`open_external_uri` never spawned), and the renderer half no-ops
`OpenExternalUri` — the effect was silently dropped. It looked plausible because
`out.effects` did contain the effect (a test asserting that even passed), but
"present in `out.effects`" ≠ "applied".

**Fix:** use path 1 — `self.open_external_uri(&url)` inline, identical to
`follow_document_link_target`. Commit `5ce85b9f`.

## Rules of thumb

- **Synchronous handler needs a host action → call the host method directly.**
  Don't manufacture an `Effect`.
- **Only push to `out.effects` via `apply_effect_host`** (or right after a
  `handle_effect` call). A bare `out.effects.push(...)` is a red flag.
- **Emitting an `Effect` is for the async / inbound path**, where the drain
  runs `handle_effect`.
- **"It's in `out.effects`" does not mean it ran.** A test that asserts the
  effect is queued proves the wrong thing; assert the observable host outcome,
  or (when that's an untestable OS spawn) the classification that routes to it.
