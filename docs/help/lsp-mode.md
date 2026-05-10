# `lsp-mode`

`lsp-mode` is the per-buffer gate that controls whether LSP
features run for a given buffer. When the mode is *active*,
LSP traffic flows: requests are issued, diagnostics are
applied, document sync runs as you type, the modeline shows
`[lsp:<server>]`. When the mode is *inactive*, every LSP
signal is suppressed for that buffer -- requests bail,
diagnostics don't render, no didChange goes to the server.
The buffer stays open in the editor; only LSP tracking ends.

Server connections persist if other buffers are still
attached. Toggling `lsp-mode` off in one buffer doesn't shut
the server down.

## Toggle

```
:lsp-mode      " toggle on / off (auto-generated per :help modes)
```

The `:lsp-mode` ex-command is auto-generated like every
mode's toggle command; see [modes](help:modes) for the
broader convention.

## Auto-activation

When you open a file whose path matches a configured server
(per the LSP registry, e.g. `*.rs` for rust-analyzer),
`lsp-mode` activates automatically. The hook fires on
`MajorEntered` events for languages with a server config and
respects the user's per-major opt-out (future:
`:set rust-mode.lsp = false`).

If you open a file with no configured server, `lsp-mode`
stays inactive. You can run `:lsp-mode` to activate it
manually -- useful for standalone-server scenarios (snippets,
scratch buffers experimenting with a server).

## What's gated when `lsp-mode` is off

| Feature                           | When off                                     |
|-----------------------------------|----------------------------------------------|
| `K` hover, `gd`/`gD`/`gy`/`gI`    | Echo "lsp-mode disabled"; no request fires   |
| `:lsp-rename`, `:lsp-format`, ... | Same echo; no request fires                  |
| Insert-mode completion (LSP)      | Silent; no candidates merged                 |
| `:complete`                       | Echo + bail                                  |
| Signature help / on-type-format   | Silent; no popup / no autoformat             |
| Document sync (`didChange`)       | Suppressed at publish; server's view freezes |
| Diagnostics (gutter, underline)   | Hidden in the renderer; layer keeps storing  |
| Modeline `[lsp:<server>]` segment | Hidden                                       |

The diagnostics layer continues to receive data from the
server -- toggling back on shows current state immediately,
without a re-fetch round-trip. The server-side document
state, however, *is* freed via `textDocument/didClose` on
deactivate; the next toggle on sends `textDocument/didOpen`
with the buffer's current content.

This matches the wire-level protocol nvim's
`vim.lsp.buf_detach_client` and emacs `lsp-mode` / `eglot`
follow: detach = `didClose`. Server resources for that URI
are reclaimed; re-attach pays one parse round to rebuild.

## Major-mode swaps don't touch `lsp-mode`

Running `:text-mode` on a buffer that had `lsp-mode` active
leaves `lsp-mode` active. State is owned per-mode in typed
`BufferLocals`; major swaps can't corrupt or clear minor
state. To turn LSP off after a major swap, run `:lsp-mode`
explicitly. (See [modes](help:modes) for the design rationale
-- this is the deliberate departure from emacs's
`kill-all-local-variables` footgun.)

## Programmatic API

For hooks / config code:

- `App::lsp_mode_enabled_for(buffer_id) -> bool` -- the
  shared gate accessor every LSP request entry point reads.
- `App::activate_mode_by_id(buffer_id, LspMode::mode_id())`
  / `deactivate_mode_by_id(...)` -- explicit programmatic
  toggle.

## Events

`lsp-mode` lifecycle is observable on the editor's typed
event bus:

- `lattice_lsp::LspBufferAttached { id, path }` -- fires when
  the mode activates (after the supervisor has queued
  didOpen).
- `lattice_lsp::LspBufferDetached { id, path }` -- fires when
  the mode deactivates (after didClose).

Subscribe via `EventBus::subscribe_typed::<T>(tx)`. See
[`:describe-events`](event:lsp.buffer-attached) for the full
LSP event catalogue.

## See also

- [modes](help:modes) -- the broader major / minor mode
  system this lives inside.
- [lsp](help:lsp) -- the LSP feature surface (servers,
  capabilities, configuration).
