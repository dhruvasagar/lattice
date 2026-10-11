# GUI by default — slice plan

> **Slice plan.** Sequencing, slice IDs, dependencies, status icons.
> Design contract: [`../../architecture/gui-default.md`](../../architecture/gui-default.md)
> (lands with GD.0 — the peer contract — and is extended by GD.9, the flip).
> Closes the ledger's `5.last` row in [`../implementation.md`](../implementation.md).

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred · ❌ dropped.
Every non-trivial slice ships the four artefacts (doc + bench-where-perf-relevant
+ test incl. failure modes + graceful error handling).

**Status: 🚧 GD.0 in progress; GD.1–GD.9 📝.** Target release: **0.9.8**.

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

### GD.0 🚧 — one dispatch tail, shared

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

Tests: the fused path costs one actor call (the TUI's
`apply_noop_action_makes_bounded_actor_calls` shape); the completion popup's
keymap layer is active after the keystroke that opens it; a follow-up action
sees the effect of the one before it; a picker accept is followed by a reparse.

### GD.1 📝 — idle drain, quit, and dropped signals

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

### GD.2 📝 — peer conformance suite

One set of scenarios, driven through each peer's dispatch entry, asserting the
same editor state. Lives where both peers can be reached without either
depending on the other. This is the slice that stops GD.0's drift recurring; it
is also where "GPUI has no frame-level test harness" gets its answer for the
non-paint half. Scope is behaviour (state after a key sequence), not pixels.

Open design question, to settle when the slice starts: a shared `trait Peer`
harness in a dev-only test-support module vs. a macro instantiating one test
body per peer. Map both against heuristic #6 before choosing.

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
