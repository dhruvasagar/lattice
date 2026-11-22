# Clipboard on WSL (CW) — slice plan

Design: [`docs/dev/architecture/clipboard.md`](../../architecture/clipboard.md) §4
(the WSL backend) and §3 (the asynchronous paste). The CB plan this builds on
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
| CW.1 | ✅ | `WslClipboard` (lattice-host, not feature-gated): writes through `win32yank.exe`, else `powershell.exe`, on one ordered worker thread; reads through `arboard` when compiled in. Both peers prefer it inside WSL; SSH still wins in the TUI. |
| CW.2 | 📝 | Asynchronous paste: a paste that needs the clipboard reads it off-thread and applies when the text lands (`InboundBus`), so a backend slower than 30 ms (a Windows helper, a cold WSLg bridge) pastes the right text instead of timing out to the register. Then `WslClipboard` reads through its helper. |

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

### CW.2 — asynchronous paste 📝

- `Editor::read_register` stops calling `Clipboard::read` on the actor thread
  for a paste. The paste reads on a blocking task and delivers the text through
  the inbound primitive, which applies the paste where the cursor was.
- Open: what a paste does if the user types before the text lands (apply at
  the recorded position, or drop it), and how `.` repeat and macros, which
  record the invocation and not the text, see an asynchronous paste.
- Then `WslClipboard::read` reads through the helper, and the TUI release build
  pastes from Windows.
