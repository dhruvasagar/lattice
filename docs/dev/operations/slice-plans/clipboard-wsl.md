# Clipboard on WSL (CW) — slice plan

Design: [`docs/dev/architecture/clipboard.md`](../../architecture/clipboard.md) §4
(the WSL backend) and §3 (why a paste reads synchronously). The CB plan this builds on
is finished and archived: [`archive/clipboard.md`](archive/clipboard.md).

## Why

Reported 2026-11-19: on Windows under WSL the clipboard worked in neither
the TUI (in tmux, in Windows Terminal) nor the GUI. Measured on that machine:

| Path | Result |
|---|---|
| OSC52 through tmux → Windows | never arrives |
| OSC52 written to the terminal directly | never arrives |
| X11 clipboard (WSLg) → Windows | arrives within 0.3 s |
| Windows → X11 clipboard (WSLg) read | 132 ms cold, 5 ms warm |
| `win32yank.exe -o` | 36–150 ms |
| `powershell.exe Get-Clipboard` | ≈320 ms |

So a TUI yank never reached Windows (OSC52 is its only write path on a Linux
build), the TUI could never paste from Windows (no read path), and the GUI's
first paste after a Windows copy timed out against the 30 ms synchronous
read and pasted the register instead.

## Slices

| Slice | Status | What |
|---|---|---|
| CW.1 | ✅ | `WslClipboard` (lattice-host, not feature-gated): writes through `win32yank.exe`, else `powershell.exe`, on one ordered worker thread; reads through `arboard` when compiled in (replaced in CW.2). Both peers prefer it inside WSL; SSH still wins in the TUI. |
| CW.2 | ✅ | Reads: `Clipboard::try_read` tells a failed read from an empty clipboard; a paste that cannot read says so instead of pasting the register; `arboard`'s bound 30 ms → 500 ms; `WslClipboard` reads through its helper (1 s bound), and a read while our write is in flight returns that write. |

### CW.1 — Windows clipboard writes ✅

- `crates/lattice-host/src/clipboard.rs`: `WslTool::find` (win32yank → PowerShell
  on `PATH` → PowerShell's install path), `WslClipboard::{detect, with_tool,
  with_reader}`. A hung helper is killed after 5 s; a failed copy warns once,
  then logs at debug.
- `crates/lattice-ui-tui/src/clipboard.rs`: `TuiClipboard::Wsl`, chosen after
  the SSH check.
- `crates/lattice-ui-gpui/src/lib.rs`: WSL first, reads through `arboard`.
- Tests: helper choice (order, the install-path fallback, never `clip.exe`),
  reads delegate to the reader, a missing helper is an error, writes reach a
  stand-in helper unchanged and in order, and the TUI picks WSL when the probe
  finds it. No test touches the real clipboard.
- Not covered: paste from Windows in a TUI build without `system-clipboard`
  (the Linux release build). That is CW.2.

### CW.2 — synchronous, bounded, reported reads ✅

The plan was an asynchronous paste: read off the actor, apply when the text
lands. Dropped on review (2026-11-22): text arriving after keys typed later is
a confusing paste, and keeping `.`, macros and typed-ahead keys in order would
mean holding input until the read returned — the same wait, with more
machinery. The paste stays synchronous; what was wrong was the 30 ms bound
quietly pasting the register when it ran out.

- `crates/lattice-core/src/clipboard.rs`: `ClipboardRead { Text, NoText,
  Failed }` and `Clipboard::try_read` (required); `read` is a provided
  shorthand.
- `crates/lattice-host/src/dispatch.rs`: `read_register_checked` returns the
  failure; `register_for_put` echoes it for `p`/`P`/`gp`/`]p`, Visual `p`,
  `:put` and terminal-buffer paste; `<C-r>` and `do_paste_from_clipboard`
  (Cmd+V) echo it too. A snippet's `$CLIPBOARD` still falls back to the `+`
  register.
- `crates/lattice-host/src/clipboard.rs`: `ArboardClipboard` bound 500 ms,
  `ContentNotAvailable` → `NoText`; `WslClipboard` reads with
  `win32yank -o --lf` / `Get-Clipboard -Raw`, CRLF → LF, and returns a pending
  write while it is in flight. The `with_reader` composition from CW.1 is gone:
  WSLg's copy lags a helper's write, so it pasted the old clipboard after `yy`.
- Tests (stand-in helper, never the real clipboard): a read returns Windows'
  text with LF line ends; an empty clipboard is `NoText`; a read during an
  in-flight write returns the write, then Windows again once it lands; a hung
  helper fails within the bound; a missing helper fails rather than reading
  empty.
- Not done: a clipboard cache refreshed on focus, which would make a paste
  instant on every backend. Not planned; it would need focus events in both
  peers and can paste stale text when the clipboard changes while lattice keeps
  focus.
