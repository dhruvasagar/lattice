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
| CB.2 | 📝 | TUI backend: `arboard` native + OSC52 fallback |
| CB.3 | 📝 | Terminal-mode yank/paste (PTY routing, mode-owned) |
| CB.4 | 📝 | GPUI backend parity (gpui native clipboard) |
| CB.5 | 📝 | Bench + docs + graceful-degradation hardening |

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

## CB.2 — TUI native backend

- `arboard`-backed `Clipboard` impl; **feature-gated / optional dep** so the TUI-only
  and headless CI builds don't hard-require X11/Wayland libs (mirror gpui optional-dep;
  see design §9.1).
- OSC52 write fallback when no display (`$SSH_TTY` / arboard init fails); OSC52 read
  unsupported → in-memory fallback (documented).
- Bind the real backend at TUI boot (replacing `FakeClipboard`).
- `read` only ever invoked under `spawn_blocking`; `write` fire-and-forget. Verify no
  clipboard call on the synchronous keystroke path — **specifically**,
  `Editor::read_register`'s synchronous `cb.read()` (CB.1, `dispatch.rs`) sits on the
  blocking render→actor RPC path; it's correct today only because `FakeClipboard` is
  instant. Before binding `arboard` here, either make that call bounded/cached or move
  it off the synchronous path (see the CB.2-obligation comment on `read_register`).
- Test (gated, real backend where available): roundtrip; headless → OSC52 write path
  exercised without asserting a real read.

**Depends on:** CB.1.

## CB.3 — Terminal-mode yank/paste (mode-owned)

Implements §6 without a host `BufferKind` branch.

- In `terminal-mode` handlers: paste routes register/clipboard text to `do_terminal_input`
  bracketed-paste-wrapped (not a document `Edit`); yank copies terminal selection /
  scrollback range to clipboard + unnamed register (read-only source).
- Host exposes only generic primitives (register read, `ClipboardHandle`,
  `do_terminal_input`); the mode owns chords + handler bodies (acid test: zero new
  `Editor::` methods, zero new host `Action` variants).
- Test: paste into a terminal buffer writes bracketed-paste bytes to the PTY (fake PTY),
  document unchanged; yank from a terminal selection populates the clipboard.

**Depends on:** CB.1 (register seam). Independent of CB.2 (works with any backend).

## CB.4 — GPUI backend parity

- gpui-clipboard-backed `Clipboard` impl bound at GPUI boot behind the same
  `ClipboardHandle`.
- GPUI is feature-gated — verify with `cargo build -p lattice-ui-gpui --features window`
  (a plain `-p lattice-cli` build won't compile it; see CLAUDE.md).
- Audit: `grep -rn "ClipboardHandle\|read_from_clipboard" crates/lattice-ui-gpui/` non-empty.

**Depends on:** CB.0 (trait), CB.1 (semantics).

## CB.5 — Bench + docs + hardening

- Per-keystroke bench: yank enqueues a clipboard write and returns — assert the yank
  dispatch stays off the blocking path (no regression in keystroke→glyph).
- Graceful degradation everywhere: backend error / no display → in-memory register,
  `debug!` + skip, never panic on the hot path.
- Flip design status → ✅; update `implementation.md`; tick this plan per slice
  (`feedback_update_slice_docs_per_slice`).

**Depends on:** CB.2, CB.3, CB.4.

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
