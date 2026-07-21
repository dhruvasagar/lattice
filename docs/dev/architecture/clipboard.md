# System clipboard integration (CB)

**Status:** ✅ complete (2026-07-03). CB.0–CB.5 all landed: the `Clipboard` trait +
shared native `arboard` backend + OSC52 fallback, the `clipboard` bool option with
yank-only sync, terminal PTY paste, and both renderer peers wired. Yank/paste use the
OS clipboard as the default target across all buffer kinds incl. terminal. Slice plan:
`docs/dev/operations/slice-plans/archive/clipboard.md` (CB series). The yank-ring / kill-ring
picker (`yank-ring.md`, to be written) builds on the register layer this defines.

## 1. Current state (verified 2026-07-03)

The **internal register core works**. A yank→paste roundtrip through the real
dispatch pipeline succeeds:

```
operator:yank motion:word-forward  → UnnamedRegister { content: "hello ", Charwise }
p                                  → buffer "hello worldhello "
```

`store_yank` (`lattice-host/src/dispatch.rs:15032`) populates the unnamed register +
`"0`; `do_paste` (`:18210`) / `read_register` (`:18316`) read it back. Not broken.

**What is missing — the OS clipboard is entirely unwired:**

- `Register::System` (`"+` / `"*`, `lattice-grammar/src/register.rs:13`) stores into an
  **in-memory `HashMap`** (`store_yank`'s `other =>` arm), never the OS clipboard.
  `read_register(System)` reads the same in-memory slot.
- **No clipboard backend exists** — no `arboard` / `copypasta` / OSC52 in the dep tree;
  gpui's native clipboard is unused. The only "clipboard" code is terminal
  *bracketed-paste* handling (`lattice-ui-tui/src/runtime.rs:122`), unrelated.
- **Default yank never reaches the clipboard.** So a clipboard workflow (yank in
  lattice → paste in another app, or the reverse) does nothing — the reported
  "can't yank/paste anything."
- **Terminal buffers:** `do_paste` applies a document `Edit`. For a terminal (claude)
  buffer that is wrong — paste must write to the PTY stdin (`do_terminal_input`,
  `:19231`) bracketed-paste-wrapped; yank must read the terminal selection/scrollback.

## 2. Vision

Yank copies to the system clipboard by default; paste reads it by default; both work in
every buffer kind, terminals included. Vim-pure register behavior stays one option away.

## 3. Paramount-goal alignment

| Goal | This feature |
|---|---|
| #1 perf | **Load-bearing.** Clipboard read/write is an OS round-trip (X11/Wayland can be slow); it MUST run off the UI/keystroke thread via `spawn_blocking`. Yank enqueues a clipboard write and returns; paste that needs the clipboard resolves it without blocking the render/keystroke path. A slow clipboard can never stall a keystroke. |
| #2 extensibility | The register layer stays the single seam; the clipboard is a backend behind it, so a future WIT/plugin register source composes the same way. |
| #3 grammar | Neutral — `"+` / `"*` already parse (`register.rs`); this only gives them real backing. |
| #4 async | Clipboard I/O is async by construction (host clipboard service on the runtime), matching the three-layer model. |

**UX (higher court):** clipboard-as-default is a deliberate deviation from Vim
(which is opt-in via `set clipboard=unnamedplus`) toward the Helix/Zed/VSCode default.
Honored per the user's explicit ask, but behind a `clipboard` **boolean** (default
`true`) so muscle-memory vim users set `clipboard=false` for pure registers. lattice
rejects vim's `unnamed`/`unnamedplus` string names as crude/non-obvious (§5); the
boolean is the self-documenting UX. No perf cost (§3 #1).

## 4. Backend

A host **clipboard service** (registered per the ServiceRegistry Arc/TypeId rule as
`ClipboardHandle = Arc<dyn Clipboard>`) with:

```rust
pub trait Clipboard: Send + Sync {
    fn read(&self) -> Option<String>;      // called on spawn_blocking
    fn write(&self, text: String);         // fire-and-forget; backend may spawn_blocking internally
}
```

- **TUI native:** `arboard` (macOS / X11 / Wayland / Windows) — robust read *and* write.
  **On by default on macOS / Windows** (a plain `cargo run` gets a working OS clipboard):
  `arboard` links the always-present system frameworks there (AppKit `NSPasteboard` /
  Win32), so there is no extra system-lib or headless concern. `lattice-cli` enables
  `lattice-ui-tui/system-clipboard` for those targets via a `cfg(target_os)`-gated
  dependency. **On Linux it stays opt-in** (`cargo run --features clipboard`) because
  `arboard` there pulls X11/Wayland link libs that break headless CI/build images.
- **TUI headless / SSH fallback:** OSC52 escape write when there's no display
  (`$SSH_TTY` set / `arboard` init fails / Linux build without the `clipboard` feature).
  OSC52 read is unreliable across terminals, so read falls back to the in-memory register
  there (documented degradation).
- **GPUI peer:** gpui's own `read_from_clipboard` / `write_to_clipboard` (gpui 0.2.2),
  wrapped in the same trait so the host stays renderer-neutral.
- **Test/CI:** an in-memory `FakeClipboard` so headless CI exercises the roundtrip
  without a real display.

**Threading:** the trait's `write` is fire-and-forget (backend spawns its own blocking
task); `read` is only ever called from a `spawn_blocking` context in the paste path.
No clipboard call sits on the render or synchronous-keystroke path.

## 5. Semantics — the `clipboard` boolean

Vim's `clipboard=unnamed,unnamedplus` string names are crude and non-self-documenting;
lattice uses a plain **boolean** instead. This is a deliberate, named UX deviation from
vim (paramount #3): the *behavior* — default yank targets the clipboard — stays
opt-outable, it's just spelled clearly.

New typed option `clipboard` (**bool**, default `true`):

- `true` — **yank targets the clipboard by default.** The yank operator (`y`, `yy`,
  Visual `y`) mirrors to the unnamed register **and** the system clipboard; paste of
  the unnamed register reads the clipboard, falling back to the in-memory register when
  it is empty/unavailable. **Delete / change / `x` do NOT touch the clipboard** — they
  stay in the registers. This *yank-only* rule is the deliberate improvement over vim's
  `unnamedplus`, whose chief UX wart is that an incidental `x`/`dd` silently clobbers
  the system clipboard. (Fork-2 resolution: yank-only sync, boolean-gated.)
- `false` — registers only; nothing syncs to the clipboard implicitly.

**Independent of the boolean, `"+` / `"*` always map to the system clipboard** — the
explicit, manual push/pull, available regardless of the default (the way to hit the
clipboard when `clipboard=false`, and redundant-but-harmless when `true`). All other
registers (`"a`–`"z`, `"0`–`"9`, unnamed) stay in-memory as today — **registers are
fully supported**, the clipboard is an additional mirror, not a replacement.

`store_yank` mirrors to the clipboard service on a **yank** when `clipboard=true`, and
always when the target register is `System`. `read_register` prefers the live clipboard
for the unnamed register under `clipboard=true`, and always for `System`; it falls back
to the in-memory entry when the clipboard is empty/unavailable.

## 6. Terminal buffers

Terminal yank/paste is NOT a `BufferKind` branch in the host `do_paste` — it routes
through `Editor::run_terminal_invocation`, the pre-existing registered
`InvocationRunnerFn` extension point that intercepts terminal-active invocations before
the generic vim-grammar `Action` gate:

- **Paste into a terminal:** route the register/clipboard text to the PTY via
  `do_terminal_input`, wrapped in bracketed paste (`\x1b[200~` … `\x1b[201~`) when the
  child enabled it (DEC private mode 2004) — never a document `Edit`.
- **Yank from a terminal:** copy the terminal selection (or scrollback range) to the
  clipboard + unnamed register; the terminal buffer is a read-only yank *source*.
  (Pre-existing before this doc; CB.1 added the clipboard mirror.)

**Correction to the mode-ownership framing (CB.3 implementation finding):**
`lattice-terminal` has no keymap layer of its own — `p`/`P`, motions, operators, and
visual-yank for terminal buffers dispatch through the SAME generic vim grammar every
Document buffer uses; `run_terminal_invocation` intercepts them BEFORE the central gate.
It is necessarily host-resident (`fn(&mut Editor, CommandInvocation) -> bool` needs the
concrete `Editor` type; moving it into `lattice-terminal` would invert the crate's
current dependency on `lattice-host`, which doesn't exist today). "Terminal-mode owns
its surface" here means: the runner is the mode-scoped extension point (registered
against terminal's `ModeId`), and `lattice-terminal` publishes the one primitive the
host needs (`SharedTerm::bracketed_paste()`, since the alacritty `Term` handle is
crate-private) — not that the handler bodies live in the `lattice-terminal` crate.

## 7. Cross-renderer parity

TUI and GPUI move in lockstep behind the same `ClipboardHandle`, both overriding CB.0's
default `FakeClipboard` at boot; no renderer reads the clipboard directly.

**The native backend is shared, not per-peer (CB.4 finding).** Fork 1 named
"gpui-native" for the GPUI peer, but gpui's clipboard is reachable only through `&App`
on the main thread (`AsyncApp` holds `Weak<AppCell>` = `Rc`/`RefCell`, not `Send`), and
the `Clipboard` trait is `Send + Sync` with a **synchronous `read` on the editor actor
thread** (`Editor::read_register`). A main-thread-only gpui context cannot satisfy that
synchronous cross-thread read except via a stale main-thread-refreshed cache. `arboard`
is `Send + Sync` and reads synchronously (bounded, §4), so it is the sound resolution —
and since the GPUI peer always links display libs (its `window` feature), `arboard` is
always available in a real GUI build with no extra system-dep cost.

So a **single** `ArboardClipboard` (in `lattice-host`, behind `system-clipboard`) serves
both peers — the load-bearing bounded-read (paramount #1) exists once, can't drift. The
TUI composes it with its own OSC52 write-only fallback (`Osc52Clipboard`, TUI-specific
since it writes terminal escape codes to stdout) for headless / SSH; the GPUI peer uses
`ArboardClipboard` directly (a GUI is never a headless terminal, so no OSC52 fallback).
The trade-off accepted: on Wayland, arboard's own connection is marginally less reliable
for *writes* than a window-owned selection would be — acceptable given arboard 3.x's
`wl-clipboard-rs` handling, and far outweighed by keeping the synchronous-read contract
and one shared impl (heuristic #1).

## 8. Rejected alternatives

- **OSC52-only backend.** Works over SSH with no native dep, but read-back is blocked
  by many terminals → unreliable paste. Kept only as the *fallback write* path (§4).
- **Shell out to `pbcopy`/`xclip`/`wl-copy`.** Per-op process spawn + platform
  detection; fragile, slower, more failure modes than `arboard`. Rejected.
- **Default yank stays vim-pure.** Contradicts the explicit requirement; the `clipboard`
  option still offers it as opt-out.

## 9. Open questions (resolve during slicing)

1. **arboard on Wayland/Linux CI:** arboard pulls X11/Wayland libs; confirm it's
   optional/feature-gated so headless CI and the TUI-only build don't hard-require them
   (mirror the gpui optional-dep pattern). Fallback = OSC52 + in-memory.
2. **Clipboard ownership on exit (X11):** arboard on X11 needs a live process to serve
   the selection; decide whether to flush to a clipboard manager on quit or accept X11's
   lose-on-exit semantics (OSC52 write sidesteps it).
3. **Large yanks / images:** cap text size for OSC52 (base64 escape length limits);
   images/other MIME are out of scope (text only for v1).
4. ~~`clipboard` option shape~~ — **resolved:** a plain `bool` (default `true`), NOT
   vim's `unnamed`/`unnamedplus` strings (rejected as crude / non-self-documenting).
   `true` = yank-only clipboard sync; `false` = pure registers; `"+`/`"*` are always the
   explicit manual clipboard registers regardless.

## 10. Deliverables (heuristic #5)

Four artefacts: this design fragment, the slice plan, tests (roundtrip via
`FakeClipboard`; `clipboard=false` opt-out; `x`/`dd` under `clipboard=true` leave the
clipboard untouched; terminal paste routes to PTY not the doc), a per-keystroke bench
proving yank stays off the blocking path, and graceful degradation
(no backend / clipboard error → in-memory register, log + skip, never panic).

## 11. Relationship to the yank ring

The kill-ring / yank-ring picker (future `yank-ring.md`) extends the **register layer**,
not the clipboard backend. Its picker unifies **two sources into one history view** (per
the user's design intent):

- **yank history** — a bounded, capped ring of recent yanks (vim's `"0`–`"9` numbered
  ring grown into a longer history), and
- **named registers** — the live `"a`–`"z` contents (and `"+` when populated),

so from one list the user picks either a past yank or an explicitly-stashed register.
This doc's `store_yank` / `read_register` seam is where the ring hooks in; the clipboard
stays orthogonal (the ring holds internal history; under `clipboard=true` the clipboard
mirror still applies to the newest yank at the top of the ring).
