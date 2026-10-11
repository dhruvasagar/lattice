# GUI by default — slice plan

> **Slice plan.** Sequencing, slice IDs, dependencies, status icons.
> Design contract: [`../../architecture/gui-default.md`](../../architecture/gui-default.md)
> (the peer contract; GD.9 extends it with the flip).
> Closes the ledger's `5.last` row in [`../implementation.md`](../implementation.md).

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred · ❌ dropped.
Every non-trivial slice ships the four artefacts (doc + bench-where-perf-relevant
+ test incl. failure modes + graceful error handling).

**Status: 🚧 GD.0 ✅, GD.0b ✅, GD.1 ✅, GD.2 ✅; GD.3–GD.9 📝.** Target release: **0.9.8**.

## What this is

`lattice` with no flag opens the GPUI window; `--tui` opens the terminal
renderer. Decided 2026-10-11. The ledger had scheduled the flip for "after
parity + a release cycle as opt-in"; this plan does parity and the flip in one
release, with the flip as the last slice so it can slip without the rest.

The flip is one line in `main.rs`. The plan is everything that has to be true
before that line is honest.

## What the 2026-10-11 audit found

Two read-only passes over both peers (nothing was run, so every entry below is
a claim to verify in the slice that fixes it — a slice that finds its gap was
not real says so and is marked ❌ with the reason).

The paint side is close. The behaviour side is not, and for a structural
reason: the TUI reaches the host through `Editor::dispatch_fused`, which runs
the post-dispatch tail in the actor; GPUI calls bare `Editor::dispatch` and
re-implements a subset of that tail by hand. Each tail step added host-side
since reached one peer. `maybe_reparse_syntax` was caught that way in
`53dac8dc`; `sync_keymap_overlays`, `maybe_adopt_owner_write` and the
effect-ordering of follow-up actions are still missing. The same shape repeats
for the idle drain and for renderer signals.

So the order below is: make the contract shared (GD.0, GD.1), make it tested
against both peers (GD.2), then close the surface gaps (GD.3–GD.8), then flip
(GD.9).

## Sequencing

```
GD.0 shared tail ─► GD.1 idle drain + signals ─► GD.2 peer conformance suite
                                                        │
        ┌───────────────┬───────────────┬───────────────┼───────────────┐
        ▼               ▼               ▼               ▼               ▼
   GD.3 window     GD.4 text       GD.5 completion  GD.6 buffer     GD.7 terminal,
   lifecycle       input (IME)     + `:` line paint text paint      modeline, rows
        └───────────────┴───────────────┴───────┬───────┴───────────────┘
                                                ▼
                                      GD.8 small behaviours
                                                ▼
                                          GD.9 the flip
```

GD.0–GD.2 are strictly ordered. GD.3–GD.7 are independent of each other; the
order given is by user-visible severity. GD.9 depends on all of them.

## Slices

### GD.0 ✅ — one dispatch tail, shared

GPUI's `dispatch_action` adopts `Editor::dispatch_fused`.

- Fused path: one actor crossing per plain key (was two or more). Retires the
  "GPUI parity — deferred" note in [`input-latency.md`](input-latency.md) § I.7.
- Non-fused path: effects, then queued + returned signals, then each follow-up
  action through the full `dispatch_action` (so an effect lands before the next
  action runs — macro replay, autopilot chains), then the tail in ONE crossing:
  `ensure_cursor_visible` (skipped on popup dismiss) → `maybe_reparse_syntax` →
  `sync_keymap_overlays` → `run_tick_pending`, then queued renderer effects.
- `sync_keymap_overlays` is the fix for completion-popup and snippet-tabstop
  keys: nothing else activates `completion-popup-mode` / `active-snippet-mode`.
- `Render::render` stops dispatching. The per-frame
  `dispatch_action(EnsureCursorVisible)` goes; the tail and
  `set_viewport_height` own cursor visibility, as in the TUI. This is a
  CLAUDE.md forbidden pattern (dispatch + tick drain inside `render`), so it is
  relocation, not polish.
- The `PickerAccept` / `PickerDismiss` / `TransientDismiss` early-return arms
  stop skipping the tail.

Landed 2026-10-11. Four tests in `lattice-ui-gpui`, each run against the
pre-GD.0 dispatch and seen to fail there: a typed character is one actor
crossing (was four); a motion is at most three; the completion popup's mode
follows the popup; a replayed macro types into the buffer it opened. The
picker early-return arms are gone outright — the host already handled all
three actions, so the generic path covers them.

Not seen: the GUI itself. `Render::render` no longer clamps the cursor, on the
argument that the tail and the viewport setters cover every way the cursor or
viewport moves. That argument is by reading; scroll-follows-cursor needs eyes.

Left in `render`, deliberately: `set_viewport_height` and `SetCellMetrics`
still dispatch. Both are gated on the value changing, so they fire on a resize,
not per frame, and `SetViewportHeight`'s action arm carries the vim `scroll`
reset that the actor's typed command does not.

### GD.0b ✅ — fuse on host-complete effects

Found by GD.0's own test. `dispatch_fused` declined the fuse whenever
`outcome.effects` was non-empty, and a motion leaves `Effect::CursorMove` there
though the host has applied it. So only typed characters fused; every motion
and operator took the multi-crossing path in BOTH peers, which is not what
`input-latency.md` § I.7 said.

Landed 2026-10-11, after GD.2 so the conformance suite could hold it.
`lattice_host::effect_completeness::effect_is_host_complete` is an exhaustive
match over `Effect` with no wildcard; `dispatch_fused` fuses when every
returned effect answers `true`. Eleven variants do — the ones the editing hot
path produces and for which both peers' arm is a no-op (`CursorMove`,
`CursorMoveIn`, `SelectionChange`, `EnterMode`, `Edits`, `Yank`, `Echo`,
`ClearSearchHighlight`, `RecordJump`, `None`, `Declined`). The other 128 answer
`false`, including many that would qualify: `false` is always safe, and nobody
holds a key down on `:describe-option`.

Both peers still run every returned effect through their arms on the fused
path. For a host-complete effect that is a no-op, and it means a variant
wrongly answered `true` runs late rather than never.

Conformance scenario `a_motion_and_an_operator_are_one_actor_crossing_each`:
`j` and `x` are one crossing each in both peers (the GUI was three, and the
TUI's non-fused path makes more than that).

### GD.1 ✅ — idle drain, quit, and dropped signals

- Idle wake drains `pending_renderer_effects` (TUI: `drain_async_pending`). Off
  the render path: the bridge task does it, not `render`.
- `should_quit` is observed wherever it can be set (mouse, async), not only at
  the end of `on_key_down`.
- Signals GPUI drops today: `ToggleMode`'s return, `:g`'s `renderer_signals`,
  the `DisplayBuffer` cascade, `NerdFontsToggled` (must re-render every glyph
  buffer — the icon-degradation rule).
- `load_persistent_config` moves out of `GpuiApp::new` so GPUI unit tests stop
  reading the developer's real config; its boot signals are applied.

Test it the way it fails: the async result is visible with NO further action
dispatched.

Landed 2026-10-11.

- The host publishes `lifecycle.renderer_effects_pending`. The window's repaint
  wake reads it and crosses the actor only when it is set, so the drain is free
  on the wakes a keystroke causes. The TUI still crosses on every idle repaint
  (`drain_async_pending`); it can adopt the flag, and GD.2 is where both peers
  get one test for it.
- Quit is checked on every repaint wake as well as after a key.
- `GpuiApp::new` no longer loads the user's config or snippets;
  `window::run` calls `load_user_environment`, which applies the load's
  signals.
- Tests: a queued effect is applied with nothing dispatched; a nerd-font toggle
  reaches the open file tree.

Not tested: the quit-on-wake path and the three signal cascades (`:toggle-mode`,
`:g`, `DisplayBuffer`) — each is now a call to the existing signal handler, and
needs a mode whose activation raises a signal to observe. Not seen: any of it in
a window.

### GD.2 ✅ — peer conformance suite

One set of scenarios, driven through each peer's own key and dispatch entry,
asserting the same editor state. Scope is behaviour (state after a key
sequence, actor crossings), not pixels.

Landed 2026-10-11 as `lattice_host::peer_conformance`, behind the
`peer-conformance` feature that each peer enables from `[dev-dependencies]`.
A peer implements `PeerUnderTest` and writes
`lattice_host::peer_conformance_tests!(Peer)`; the scenario list is in the
host's macro, so a new scenario reaches both peers without either being
touched. Decided on heuristic #1 over a separate crate depending on both peers:
a peer exposes an in-process editor only under its own `cfg(test)`, and a
second crate would need an actor-backed, async-settling harness for every
scenario.

Seven scenarios, green in both peers: keys edit the buffer; a typed character
is one actor crossing; the cursor stays in view; the completion popup's layer
follows the popup; a replayed macro types into the buffer it opened; a queued
effect is applied on an idle wake; quitting is published. GD.0's and GD.1's
GUI-only copies of four of these were removed in favour of the shared ones.

Not covered, by construction: the decline-and-fall-through walk. It lives in
each peer's input loop above the chord entry the suite drives (the TUI's is
inline in `runtime.rs`), so the scenarios cannot reach it. Hoisting it behind a
shared entry is the precondition for a scenario; noted for GD.8.

### GD.3 📝 — window lifecycle

- Close button and Cmd+Q route through the same dirty-buffer guard as `:qa`,
  and flush plugin stores.
- An application menu on macOS (quit, close, the standard edit items).
- Files opened by the OS (Finder, `open -a`, drag-drop) reach the editor.
- `--tutor [N]` reaches `lattice_ui_gpui::run`.
- Window title follows the active buffer.

### GD.4 📝 — text input

IME, dead keys, composition, and the layouts where `Keystroke.key` is not the
character typed. Needs a GPUI `InputHandler`. Establish first (by running it)
what `Keystroke.key` delivers for shifted symbols and non-US layouts, and
macOS option-as-meta.

### GD.5 📝 — completion and command-line paint

- Insert-completion popup anchored at the cursor; docs panel beside it.
- Ghost text for the focused candidate.
- `:` line caret at the cursor's byte, not appended to the row.
- Expanded command band: syntax spans, caret in place.
- Command-line hints: `press a chord`, `(chord)`, `(n/m)`.

### GD.6 📝 — buffer text paint

- LSP semantic tokens, including the `deprecated` strikethrough.
- `relativenumber`.
- Document-highlight read / write / text kinds.
- Substitute-preview strikethrough; current-match foreground.

### GD.7 📝 — terminal, modeline, scaled rows

- Terminal cells: bold, italic, dim, strikethrough; distinct search, current
  match and selection styles.
- Modeline provider labels for help, file-tree and oil panes.
- Heading-scaled rows honour `leftcol`; viewport capacity accounts for
  variable row height (theme-system F.1, deferred there).

### GD.8 📝 — small behaviours

- `<Esc>` in a focused popup cancels the popup's own sub-mode (search) before
  it dismisses the popup.
- `ui.mouse` is honoured.
- `<C-l>` redraw is acknowledged.
- Pane geometry uses the window's real column count (the host falls back to
  120 when `SetTerminalWidth` is never sent).
- Hard-coded popup colours go through the theme (light themes).
- The decline-and-fall-through walk behind an entry the conformance suite can
  drive, and a scenario for it (see GD.2).

### GD.9 📝 — the flip

- `main.rs`: GUI unless `--tui`.
- **No display reachable** (SSH, headless Linux, a server): to settle when this
  slice starts. Either fall back to the terminal, or stop with a message that
  names `--tui`. The first never leaves the user without an editor; the second
  never surprises them with a renderer they did not ask for.
- A binary built without the `gui` feature keeps working as a terminal editor.
- Release: the main `lattice-*` archive is the GUI build; a terminal-only
  archive stays for servers and for the two arm targets where the GUI build is
  best-effort. `install.sh`, the `.deb` / AppImage names and the manifest check
  follow.
- Cargo's default feature set stays headless, so CI and `cargo test` need no
  display libraries.
- User docs (install, display, troubleshooting), `--help`, the README, and the
  ledger's `5.last` row.

## Verification that needs a person

Several of these cannot be checked from a headless session: IME, popup
placement, window close, light-theme colours, anything about how it looks. Each
such slice tests the logic underneath and says in its commit what was not seen.
