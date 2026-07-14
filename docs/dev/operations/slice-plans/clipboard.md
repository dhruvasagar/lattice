# System clipboard — slice plan (CB)

Sequencing for `docs/dev/architecture/clipboard.md`. Goal: yank/paste use the OS
clipboard by default (option-gated), non-blocking, across all buffer kinds including
terminal. The internal register core already works (verified); this adds the clipboard
backing + terminal routing.

**Status legend:** 📝 planned · 🚧 in progress · ✅ landed

**Fork 2 resolved (2026-07-03):** `clipboard` is a plain **`bool`** (default `true`),
not vim's `unnamed`/`unnamedplus` strings. `true` = **yank-only** clipboard sync (`y`
mirrors to the clipboard; `d`/`c`/`x` stay in registers); `false` = pure registers.
`"+`/`"*` are always the explicit manual clipboard registers. Registers stay fully
supported; the yank ring (future) unifies yank-history + named registers.

**Fork 1 resolved (2026-07-03):** backend = `arboard` native + OSC52 fallback +
gpui-native (GPUI peer) + `FakeClipboard` (CI). Confirmed by user.

| Slice | Status | Summary |
|---|---|---|
| CB.0 | ✅ | Clipboard trait + `FakeClipboard` + host `ClipboardHandle` service |
| CB.1 | ✅ | `clipboard` bool option + `store_yank`/`read_register` cutover (yank-only) |
| CB.2 | ✅ | TUI backend: `arboard` native + OSC52 fallback |
| CB.3 | ✅ | Terminal-mode yank/paste (PTY routing, mode-owned) |
| CB.4 | ✅ | GPUI backend parity (shared `arboard`, not gpui-native — see note) |
| CB.5 | ✅ | Bench + docs + graceful-degradation hardening |

---

## CB.0 — Clipboard trait + fake backend + service ✅

Pure substrate; no behavior change. **Landed:** `Clipboard` trait (`read`/`write`) +
`ClipboardHandle` + `FakeClipboard` in `lattice-core/src/clipboard.rs` (not a new crate
— `lattice-core` already hosts service traits like `FoldOverlayServiceHandle`, and
`lattice-terminal` depends on `lattice-core`, not `lattice-host`, so this is reachable
from terminal-mode for CB.3 without a bad dependency direction). Registered at host boot
(`editor_boot.rs`) as the default `ClipboardHandle` binding, ahead of CB.2/CB.4 real
backends. Tests: fake roundtrip + object-safe `dyn Clipboard` usable as the handle.

**Depends on:** none.

## CB.1 — `clipboard` bool option + register cutover ✅

The semantics seam (§5). Landed against `FakeClipboard` (headless-testable).

- `ClipboardEnabled` bool option (`#[name("clipboard")]`, default `true`) added to the
  `Editor` options group in `lattice-config/src/core_options.rs`.
- **Grammar change:** `Effect::Yank` gained an `explicit_yank: bool` field
  (`lattice-grammar/src/effect.rs`) — `true` only for the real yank operator's primary
  register write; `false` for delete/change's register-populating write and the `"0`
  numbered-ring mirror. The Visual-block combiner in `dispatcher.rs` threads the flag
  through per-register so a collapsed blockwise yank preserves eligibility.
- `store_yank` (now `(register, content, kind, explicit_yank)`) mirrors to
  `ClipboardHandle::write` when `explicit_yank && clipboard=true`, or unconditionally
  when the target is `Register::System` (`"+`/`"*`).
- `read_register` prefers a live `ClipboardHandle::read` for the unnamed register under
  `clipboard=true` (and always for `System`), falling back to the in-memory entry when
  the clipboard is empty/unavailable. YankKind isn't representable in plain clipboard
  text, so a clipboard-sourced read infers charwise from the last known in-memory entry
  (defaulting Charwise).
- **CB.2 obligation flagged in code** (`read_register` doc comment,
  `lattice-host/src/dispatch.rs`): dispatch — including `do_paste` — is a *blocking RPC
  from the render thread into the actor* (`input-pipeline.md`), so `read_register`'s
  synchronous `cb.read()` call is only correct today because the bound backend is
  `FakeClipboard` (instant, in-memory). **CB.2 must not carry a blocking OS clipboard
  round-trip into this call site unexamined** — either guarantee the real backend's read
  stays sub-frame (e.g. a bounded-wait / cached read) or restructure the call off the
  synchronous path before installing `arboard`.
- Tests (`lattice-host/src/dispatch.rs`, `tests::clipboard_*` + `system_register_*` +
  `paste_*`): default-true yank mirrors; delete AND change do not mirror (yank-only);
  `clipboard=false` opts out (registers still populate); `"+`-targeted yank always
  mirrors regardless of the option; paste prefers a live clipboard value over a stale
  internal register (simulates an external-app copy); clipboard=false ignores a
  populated clipboard on read. 7 tests, all passing; full `lattice-host` (627),
  `lattice-grammar` (208), `lattice-config` (143), `lattice-core` (133) suites green,
  workspace + GPUI build clean.

**Depends on:** CB.0.

## CB.2 — TUI native backend ✅

**Landed:** `crates/lattice-ui-tui/src/clipboard.rs`.

- `Osc52Clipboard` — write-only, zero link deps, always compiled in. `read()` always
  `None` (most terminals block OSC52 read-back for security) — falls back to the
  in-memory register, as documented. `write()` base64-encodes and emits
  `ESC ] 52 ; c ; <b64> BEL` directly to `stdout`, fire-and-forget, never panics on a
  broken pipe.
- `ArboardClipboard` (behind the new `lattice-ui-tui` **`system-clipboard`** feature,
  default off — mirrors `lattice-ui-gpui`'s `window` optional-dep exactly, because
  `arboard` pulls X11/Wayland link libs the default `cargo test --workspace` CI job
  doesn't install). `lattice-cli` gained a matching `clipboard` feature
  (`cargo build --features clipboard`) wiring `lattice-ui-tui/system-clipboard`.
  Without the feature, yank still reaches the real clipboard via OSC52 write — only
  *read* (paste-from-another-app) and native Wayland/X11 write need the feature.
- **The CB.2 obligation is resolved, not deferred:** `ArboardClipboard::read()` bounds
  the OS round-trip with `tokio::time::timeout(30ms, spawn_blocking(...))`, driven via
  the existing `lattice_runtime::block_on` sync-to-async bridge (the same escape hatch
  `Editor::apply_edit_blocking` et al. already use from the actor thread) — so a hung
  display server degrades to `None` (register fallback) instead of stalling dispatch.
  `write()` is genuinely fire-and-forget via `lattice_runtime::spawn_task` (no bound
  needed — nothing awaits the result).
- `TuiClipboard::detect()` composes the two: prefers OSC52 under SSH (`$SSH_TTY` /
  `$SSH_CONNECTION`) even when `system-clipboard` is compiled in and arboard *could*
  connect — over `ssh -X`, arboard would reach the **forwarded** X server's clipboard,
  not the user's local machine clipboard, so OSC52 (which tunnels through the terminal
  emulator directly) is the semantically correct backend there. The SSH-detection
  logic is split into a pure `detect_with(bool)` so it's unit-testable without mutating
  process-global env vars (which would race parallel tests).
- Bound at TUI boot: `App::new` (`lattice-ui-tui/src/app/boot.rs`) overrides CB.0's
  default `FakeClipboard` via `Arc::get_mut(&mut editor.services)` immediately after
  `Editor::boot` returns (the registry is still a freshly-frozen, uniquely-owned Arc at
  that point — `Arc::get_mut` is guaranteed to succeed, verified by a dedicated
  regression test plus the full 1538-test TUI suite passing unchanged).
- Tests: `Osc52Clipboard` read-always-None + write-never-panics; `detect_with`'s
  SSH-preference rule (both with and without the feature); a boot-level regression
  test pinning that `App::new` leaves a working `ClipboardHandle` registered. All green
  in both feature configurations; full workspace + GPUI build clean; 2650 tests total
  across host/grammar/config/core/TUI (up from 2645 at CB.1, +5 new).

**Depends on:** CB.1.

## CB.3 — Terminal-mode yank/paste ✅

**Scope correction found during implementation:** terminal Visual-mode yank
**already existed** host-side in `Editor::run_terminal_invocation`
(`lattice-host/src/dispatch.rs`) before this slice — it got the `explicit_yank: true`
treatment in CB.1 already. What design §6 called "mode-owned handlers" doesn't map
onto a `terminal-mode`-owned keymap layer the way it first sounded: `lattice-terminal`
has no keymap of its own (`p`/`P`/motions/operators/visual-yank for terminal buffers
all dispatch through the SAME generic vim `Action`/`CommandInvocation` grammar every
Document buffer uses); `run_terminal_invocation` is the pre-existing, registered
[`InvocationRunnerFn`] extension point that intercepts terminal-active invocations
BEFORE the generic gate — and it's necessarily host-resident because its signature is
`fn(&mut Editor, CommandInvocation) -> bool` (moving it into `lattice-terminal` would
mean `lattice-terminal` depending on `lattice-host`, inverting the existing dependency
direction — out of scope for a clipboard slice). CB.3 therefore extends this EXISTING
extension point, following its established yank precedent, rather than introducing a
new architectural pattern.

**Landed:**

- **Gap found:** `p`/`P` (`CommandKind::Action`, bound to `Action::PasteAfter/Before`)
  were NOT intercepted by `run_terminal_invocation` — its own tail-classification match
  returns `false` for `Action`-kind commands (by design, for things like `:`, `/`, `K`
  hover, LSP nav that genuinely should dispatch centrally), which meant terminal-buffer
  paste fell through to the generic `Editor::do_paste` — splicing into `self.document`,
  the wrong target (a terminal has no document; it's a PTY). Confirmed via trace, not
  assumed.
- Added an explicit intercept in `run_terminal_invocation`: on `paste_after`/
  `paste_before`, resolve the register/clipboard payload via the EXISTING
  `Editor::read_register` (so terminal paste gets the CB.1 live-clipboard preference
  for free), bracketed-paste-wrap it when the running program enabled DEC private mode
  2004, and write it via the existing `do_terminal_input` (PTY, not a document `Edit`).
  Terminal-Visual has no paste (no "paste over a read-only scrollback selection"
  concept in vim), so this only applies to the non-Visual path.
- New published primitive: `SharedTerm::bracketed_paste()` (`lattice-terminal/src/
  reader.rs`) — `inner` (the alacritty `Term`) is crate-private, so this is the one new
  accessor `lattice-terminal` needed to publish; the host never reaches into `Term`
  directly. Reads `TermMode::BRACKETED_PASTE`.
- Extracted the wrap-or-passthrough logic into a pure free function
  `terminal_paste_payload(content, bracketed) -> Vec<u8>` so it's unit-testable without
  a PTY.
- **Testing boundary, documented not silently skipped:** this codebase does not unit-
  test real-PTY-spawning code anywhere (`lattice-terminal`'s `buffer.rs` / `handle.rs`
  / `spawner.rs` carry zero tests; only the pure VT-state-machine logic, via
  `SharedTerm::fixture`, is tested) — so the full `run_terminal_invocation` paste path
  (spawn → dispatch → real PTY write) is NOT covered by an automated test, matching
  that existing boundary rather than introducing a new one. What IS covered: the
  `terminal_paste_payload` wrap logic (2 tests) and the `bracketed_paste()` accessor
  against the real DEC-2004 escape sequences via `SharedTerm::fixture` (2 tests).
  Manual verification via the `run` skill is the practical check for the PTY-touching
  glue, consistent with how the rest of this crate handles PTY-adjacent code.
- Full regression: 2697 tests across host/terminal/grammar/config/core/TUI (6 suites,
  up from 2650/5 suites at CB.2 — the jump includes lattice-terminal's full existing
  suite entering the combined run for the first time), workspace + GPUI build clean.

**Depends on:** CB.1 (register seam). Independent of CB.2 (works with any backend).

## CB.4 — GPUI backend parity ✅

**Fork-1 deviation, user-confirmed (2026-07-03): shared `arboard`, NOT gpui-native.**
Implementing gpui-native surfaced a thread-model conflict invisible when Fork 1 was
decided: gpui's clipboard is reachable only via `&App` on the main thread (`AsyncApp`
holds `Weak<AppCell>` = `Rc`/`RefCell`, not `Send`), but `lattice_core::Clipboard` is
`Send + Sync` with a **synchronous `read` on the editor actor thread**
(`Editor::read_register`, on the blocking render→actor path). A main-thread-only gpui
context can't serve that synchronous cross-thread read except from a stale cache + new
per-frame/focus polling (a paramount-#1 hazard). Presented options A (shared arboard) /
B (true gpui-native + cache) / C (hybrid: gpui write + arboard read); user chose **A**.
See design §7 for the full mapping.

**Landed:**

- Moved the native backend to `lattice-host/src/clipboard.rs` (`ArboardClipboard`)
  behind a new `lattice-host/system-clipboard` feature — host has `tokio` +
  `lattice_runtime::block_on` for the bounded read and is depended on by BOTH peers, so
  the load-bearing bounded-read logic (paramount #1) exists exactly once and can't drift.
  `Osc52Clipboard` stays TUI-local (writing escape codes to stdout is terminal-specific).
- TUI (`lattice-ui-tui/system-clipboard`) now forwards to `lattice-host/system-clipboard`
  and imports `lattice_host::clipboard::ArboardClipboard` — its `arboard` direct dep is
  dropped; `TuiClipboard::detect` composition (SSH→OSC52 preference) is unchanged.
- GPUI: `GpuiApp::new` overrides CB.0's `FakeClipboard` with the shared
  `ArboardClipboard` via the same `Arc::get_mut(&mut editor.services)` seam as the TUI.
  Gated on a new `lattice-ui-gpui/system-clipboard` feature that its **`window` feature
  pulls** (a real GUI build always links display libs, so arboard is always available and
  a GUI always wants a working clipboard). No OSC52 fallback for GPUI (a GUI is never a
  headless terminal). `--features gui` on `lattice-cli` transitively enables it.
- Feature-matrix builds all clean: gpui default (no clipboard), gpui `system-clipboard`
  alone (clipboard override compiles without the gpui window link — good for CI), gpui
  `window` (full macOS display link), cli `clipboard`, cli `gui`.
- Tests: a GPUI boot regression guard (`new_leaves_a_working_clipboard_handle`) pinning
  that boot always leaves a resolvable, panic-free `ClipboardHandle` (the
  environment-independent invariant; the arboard override itself needs a real display,
  exercised on-device + via the `window` build). Host's `ArboardClipboard` module builds
  + the existing 7 host clipboard tests pass under `--features system-clipboard`. Full
  regression: 2725 tests across host/terminal/grammar/config/core/TUI/GPUI (7 suites),
  clippy clean under the feature.

**Depends on:** CB.0 (trait), CB.1 (semantics).

## CB.5 — Bench + docs + hardening ✅

**Landed:**

- **Bench** (`crates/lattice-host/benches/clipboard_yank.rs`): times `store_yank` in
  three configs against the default `FakeClipboard`. Result — register-only
  (clipboard=false) ≈ **100 ns**, mirror-on (clipboard=true) ≈ **120 ns**,
  `"+`-register (always mirrors) ≈ **136 ns**. The clipboard mirror adds ~20–36 ns
  (an option read + `services.get` + `write`), O(1) and imperceptible against the
  8.3 ms one-frame budget (paramount #1). The bench doc-comment is explicit that it
  does NOT (and structurally can't, through the synchronous `FakeClipboard`) bench a
  *real* backend's non-blocking write — that property lives in `ArboardClipboard::write`
  (`spawn_task`) + the bounded read timeout, reviewed at those sites.
- **Graceful degradation** — audited end to end, all paths degrade without panic:
  `store_yank`'s mirror is `if let Some(cb) = services.get() { cb.write() }` (skip if
  absent); `read_register` falls back to the in-memory entry when the clipboard is
  empty/unavailable; `ArboardClipboard::{read,write}` swallow timeout/lock/FFI errors →
  `None` / no-op; `ArboardClipboard::new()` returns `None` on no display. Added a
  one-shot `tracing::debug!` at both the TUI `detect_with` and GPUI-boot fallbacks when
  the native backend was expected (feature on, not SSH) but arboard init failed — so a
  user wondering why paste-from-another-app doesn't work can diagnose via
  `--log-level debug`, per the standing "diagnostic → `debug!`" rule.
- **Docs** — design fragment flipped to ✅; this slice plan ticked per slice.

**Depends on:** CB.2, CB.3, CB.4.

---

## Post-CB.5 follow-ups (landed)

- **Native clipboard on-by-default where it is free (refines CB.2).** On macOS /
  Windows `arboard` links the always-present system frameworks (AppKit `NSPasteboard` /
  Win32), so a plain `cargo run` should just have a working OS clipboard rather than the
  OSC52 write-only fallback. `lattice-cli` now enables `lattice-ui-tui/system-clipboard`
  for those targets via a `cfg(any(target_os = "macos", target_os = "windows"))`-gated
  dependency; Linux stays opt-in via `--features clipboard` (X11/Wayland link libs break
  headless CI). `--features clipboard` is a no-op on macOS/Windows. See
  `docs/dev/architecture/clipboard.md`.
- **External/OS bracketed-paste → PTY routing (completes CB.3).** `Editor::do_paste_text`
  (the `Action::PasteText` handler for an OS/terminal-emulator bracketed paste, distinct
  from vim `p`/`P`) now routes to the PTY via `do_terminal_input` when a terminal buffer
  is focused and the modal isn't `Command`/`Search`, wrapping in DEC-2004 markers when the
  program requested them — the external-paste peer of CB.3's `p`/`P` register paste. The
  `Command`/`Search` guard keeps a paste into the `:` line from leaking into the PTY.
  Tests: `paste_text_into_command_line_while_terminal_focused_edits_command_line_not_pty`
  and `paste_text_in_terminal_normal_mode_routes_to_pty_not_document` (both PTY-free,
  matching CB.3's real-PTY testing boundary).

---

## Risk / sequencing notes

- **CB.2 dependency risk** is the main one: `arboard` must stay optional so headless CI
  and TUI-only builds don't pull X11/Wayland. If that proves awkward, ship OSC52-only
  for the TUI and treat native as a follow-up (design §8 keeps OSC52 as the fallback
  anyway).
- **CB.1 is the behavior switch** (default yank starts touching the clipboard). It lands
  green with `FakeClipboard` so it's fully testable before any real backend exists.
- **CB.3 is independent** of the backend slices — it can land in parallel with CB.2/CB.4
  since it only needs the CB.1 register seam.
- Non-blocking discipline (paramount #1) is verified in CB.2 (real I/O) and CB.5 (bench),
  not assumed.
