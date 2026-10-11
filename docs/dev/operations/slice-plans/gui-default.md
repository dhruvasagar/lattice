# GUI by default — slice plan

> **Slice plan.** Sequencing, slice IDs, dependencies, status icons.
> Design contract: [`../../architecture/gui-default.md`](../../architecture/gui-default.md)
> (the peer contract; GD.9 extends it with the flip).
> Closes the ledger's `5.last` row in [`../implementation.md`](../implementation.md).

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred · ❌ dropped.
Every non-trivial slice ships the four artefacts (doc + bench-where-perf-relevant
+ test incl. failure modes + graceful error handling).

**Status: 🚧 GD.0 ✅, GD.0b ✅, GD.1 ✅, GD.2 ✅, GD.3 ✅, GD.6a ✅, GD.6b ✅, GD.5a ✅, GD.7a ✅, GD.8a ✅; GD.4, GD.5b, GD.6c, GD.6d, GD.7b, GD.7c, GD.8b, GD.9 📝.** Target release: **0.9.8**.

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

### GD.3 ✅ — window lifecycle

Landed 2026-10-11.

- **Close button, Cmd+Q, the Quit menu item** all go through
  `GpuiApp::request_close`, which is `:qa`: an unsaved buffer refuses, the
  reason is echoed, and the window stays. Plugin stores are flushed before the
  window goes. When the last window closes the application quits.
- **Files from the OS.** `Application::on_open_urls` queues the path and wakes
  the window (the callback has no app context and can fire before the window
  exists); a file dropped on the window opens directly. Both are `:e <path>`.
- **`--tutor [N]`** reaches `lattice_ui_gpui::run`.
- **Window title** is the active buffer's file name, computed on the repaint
  wake and handed to the platform from `render` only when it changed.

Tests cover the logic: the close guard both ways, an OS-opened file and its
title, URL-to-path parsing including the malformed cases, and the tutor flag.

**None of the GPUI wiring has been seen running.** It compiles against gpui
0.2.2; that is all. Specifically unverified: that `on_window_should_close`
returning `false` keeps the window on each platform; that `cmd-q` reaches the
action before `on_key_down` sees it; that `on_open_urls` fires for a `.app`
launch; that a drop on the root element delivers `ExternalPaths`.

Left out: the menu is the application menu with Quit only. Edit items (cut,
copy, paste, undo) need actions that route into the editor's registers and
undo, which is its own piece of work — noted under GD.8. File associations in
the bundle's `Info.plist` are a packaging change — GD.9.

### GD.4 📝 — text input

IME, dead keys, composition, and the layouts where `Keystroke.key` is not the
character typed. Needs a GPUI `InputHandler`. Establish first (by running it)
what `Keystroke.key` delivers for shifted symbols and non-US layouts, and
macOS option-as-meta.

### GD.5 — completion and command-line paint

**GD.5a ✅ — the completion popup at the cursor** (2026-10-11). The
insert-completion popup was pinned to the window's top-right corner. It is
anchored to the completion's anchor (where the replaced text starts, so it does
not slide as you type), below the row or above it when it does not fit, and
kept inside the window. The docs panel sits beside it, right if there is room
and left otherwise. The placement is one pure function shared with the hover
popup, which is what the tests cover.

Not seen in a window, and two things about it are approximate by construction:
the popup's size is estimated before layout (candidate rows + footer), and the
horizontal origin is `pane padding + byte × cell width` — it does not add the
gutter's width or a split pane's offset. The hover popup has always had that
second limitation; sharing the function means fixing it once fixes both, and it
needs eyes.

**GD.5b 📝:**

- Ghost text for the focused candidate.
- `:` line caret at the cursor's byte, not appended to the row.
- Expanded command band: syntax spans, caret in place.
- Command-line hints: `press a chord`, `(chord)`, `(n/m)`.
- Popup origin accounts for the gutter and the pane's position.

### GD.6 — buffer text paint

Split when the slice started: the four items turned out to live in three
different places.

**GD.6a ✅ — `relativenumber`** (2026-10-11). Renderer-only. The gutter row
carries an optional distance, measured from the pane's own cursor (an inactive
pane's, not the focused one's), blank on the cursor's line and off on a
multibuffer, as in the TUI. Tested at the formatter: the distance is printed in
the same cells the number was. Not seen in a window.

**GD.6b ✅ — LSP semantic tokens** (2026-10-11). Decided: baked into the
display matrix host-side, not merged per renderer (paramount #1 — no merge on
either paint path — and the parity rule). `semantic_spans.rs` turns the tokens
covering the window being built into spans layered over the grammar's and under
a buffer's static extra spans; `MatrixVersion` gained a `semantic_tokens` axis,
rebuild-class like `syntax`, stamped by `TokenStamps` (an O(1) "is this the same
`Arc`" check on the actor — the tokens themselves are read only in the worker).
The TUI's render-time merge and its fixed terminal colours are gone; token
colours now come from the theme's syntax elements in both peers.

Two behaviours worth knowing. A new token set takes the full window build, never
the incremental one, and an untouched line comes out equal — tested, because a
recolour of unedited text was the risk named when this was chosen. And an edit
rebuilds its line with the *stale* tokens when the line count did not change
(typing inside a line keeps its colours) and without them when it did (stale
tokens would be on the wrong lines).

Gate: targeted — `lattice-cells` whole, the host's `cells_worker` /
`semantic_spans` / `render_state` / `display_matrix` modules and the multibuffer
integration test, the TUI's `render::` and `cells_render::` modules. The whole
`lattice-host` and `lattice-ui-tui` suites were not run. Not seen in a window.

**GD.6d 📝 — semantic-token modifiers.** `deprecated` (strikethrough),
`readonly` / `static` (italic) and `defaultLibrary` (dim) were painted by the
TUI's merge and are **not** carried by GD.6b: a display run holds one `Style`,
a modifier is a second one layered on it, and the cell flags have no
strikethrough bit. This is a small regression in the terminal that GD.6b
accepted to get the tokens shared. Needs: modifier bits on `DisplayRun` (or
layered resolution in the row build), a `STRIKETHROUGH` cell flag, and both
peers' paint. Shares that flag with GD.6c.

**GD.6c 📝 — highlight kinds and strikethrough.** Document-highlight
read / write / text need the host's overlay worker to carry the kind
(`OverlayLayer::DocHighlight` has none). Substitute-preview strikethrough and
the current match's foreground need a per-run text attribute on an overlay that
is currently a background quad.

### GD.7 — terminal, modeline, scaled rows

Split when the slice started, for the same reason as GD.6.

**GD.7a ✅ — terminal cell attributes** (2026-10-11). Bold, italic, dim and
strikethrough are painted (only reverse and underline were); underline is a
text decoration and no longer a bottom border. Search hits are underlined and
keep their colours; the current match and the Visual selection invert — the
TUI's three styles, where this peer had one. The paint decision is a pure
function (`terminal_run_paint`) and is what the tests cover. Not seen in a
window; in particular that a bold run keeps the cell grid aligned depends on
the terminal font's bold face having the regular face's advance.

**GD.7b 📝 — modeline labels for help, file-tree and oil panes.** The TUI gets
`[help] <title>`, `[tree] <root>`, `[oil] <dir> [+]` from a renderer-typed
provider registry (`build_pane_render_registry`); the GUI passes `None` and
shows the path segment. All three labels read only published state.
**Design decision before starting:** the label is content, so it should be
produced once. Either the owning mode contributes it as a modeline element
(the ML-series direction, and what "modes own their status-line contributions"
asks for), or the three functions move into the host's modeline resolver
(quicker, and puts three mode names in the host). A second registry in the GUI
is the option to rule out — it is the copy this plan exists to remove.

**GD.7c 📝 — heading-scaled rows.** Honour `leftcol`; make viewport capacity
account for variable row height (theme-system F.1, deferred there).

### GD.8 — small behaviours

**GD.8a ✅** (2026-10-11):

- `<Esc>` dismisses a popup only while the document has focus. With focus
  inside the popup it takes the keymap path, so a search in there is cancelled
  before the popup is. Same gate as the TUI.
- `<C-l>` is acknowledged, on the repaint wake and only when one is pending.
- The window sends its width in columns when it changes, so pane geometry
  stops assuming 120.

**❌ `ui.mouse` in the GUI — dropped.** The audit listed it as ignored. It is
ignored on purpose, and the option's own documentation says so: `ui.mouse`
exists because a terminal loses its native click-drag selection while Lattice
reads the mouse, and a window that owns its input takes nothing away by
listening. Decided by `core_options.rs` and `docs/user/mouse.md`, not by this
plan.

**GD.8b 📝:**

- Hard-coded popup colours go through the theme (light themes).
- An Edit menu on macOS (cut, copy, paste, undo) wired to the editor.
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
