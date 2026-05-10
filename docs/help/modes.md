# Modes

Lattice's mode system has two orthogonal axes:

- **Modal state** — Normal / Insert / Visual / Operator-Pending /
  Command / Search / Replace. The vim-grammar state machine.
  Driven by keys, transitions are buffer-local but transient.
  Not what this page is about.
- **Major + minor modes** — the editor's *behaviour layers*. A
  buffer always has zero or one *major mode* (its content
  identity: `rust-mode`, `markdown-mode`, `text-mode`,
  `help-mode`, `file-tree-mode`, `oil-mode`, `lsp-log-mode`,
  ...) plus any number of *minor modes* (capability layers:
  `lsp-mode`, `hover-mode`, `line-numbers-mode`, ...). This
  page covers that.

Major and minor modes contribute typed options
(`ReadOnly = true`, `wrap = false`, ...) and per-buffer state.
The contributions layer through the option resolver to produce
a single `ResolvedOptions` snapshot the renderer reads. Modes
can also subscribe to events (`MajorEntered`, `BufferOpened`,
`CursorMoved`, ...) to react to the editor's lifecycle.

## Toggling a mode -- `:<mode-name>`

Every registered mode is reachable as an ex-command whose
keyword is the mode's id, with no `:enable` / `:disable` /
`:activate` prefix:

```
:rust-mode         " activate rust-mode (or reload if already active)
:markdown-mode     " swap the major from rust to markdown
:lsp-mode          " toggle the lsp-mode minor (active <-> inactive)
:hover-mode        " toggle the hover-mode minor
:line-numbers-mode " toggle line numbers (M.7+)
```

The toggle command auto-generates per mode at boot, so adding
a mode (built-in or future plugin) gives you `:<that-name>`
free.

### Toggle semantics

- **Minor mode** -- the command is a true toggle. Active →
  deactivate (the mode's `on_deactivate` runs, its option
  contributions roll back, its owned `BufferLocals` are
  cleaned up). Inactive → activate.
- **Major mode** -- activating swaps. If you run
  `:markdown-mode` on a buffer whose current major is
  `rust-mode`, rust-mode deactivates first, then markdown-mode
  activates. Running `:rust-mode` on a buffer that already has
  rust-mode triggers a *reload* (deactivate + re-activate;
  useful for picking up a config change to its options).

### Major-mode swaps don't touch minors

If you have `lsp-mode` active on a buffer and you run
`:text-mode` (swapping the major from rust-mode to text-mode),
**lsp-mode stays active**. The minors live on their own; their
state is owned per-mode in typed `BufferLocals`, so a major
swap can't corrupt or clear it.

This differs from emacs's default (`kill-all-local-variables`
on major change wipes most minors). Lattice's typed-ownership
model means we don't need that footgun. If you want a minor
gone after a major swap, run its toggle:

```
:rust-mode          " was rust-mode + lsp-mode
:text-mode          " now text-mode + lsp-mode (lsp-mode kept)
:lsp-mode           " toggle lsp-mode off; now plain text-mode
```

### Auto-activation hooks

Some minors *want* to follow the major. `lsp-mode` is the
canonical example -- when you enter a language major that has
a configured server, you usually want LSP on. That's
configured via the event-bus hook system: a subscription on
`MajorEntered { mode: rust-mode }` activates `lsp-mode`.
Exiting the major doesn't auto-deactivate (that's the
trade-off; explicit toggle if you want it gone).

User config can override per-major:

```toml
# Future: ~/.config/lattice/init.rs (typed-WASM config)
# disables lsp-mode auto-activation on python files
```

(See [lsp](help:lsp) for the LSP-specific bits.)

## What modes are registered

`:apropos -mode$` lists every mode. Tab-completion on
`:` shows them inline as you type. Future:
`:list-modes` (M.8) shows the full catalogue with descriptions.

## How modes interact with options

Each mode declares which typed options it contributes via
`overrides! { ReadOnly = true, wrap = false }`. The resolver
chains contributions in priority order:

1. `:setlocal foo=bar` (per-buffer explicit override)
2. minor mode contributions (most-recently-activated wins)
3. major mode contributions
4. `:set foo=bar` (global)
5. registered defaults

Toggling a mode triggers a cache refresh for the buffer; the
renderer reads the resulting `ResolvedOptions` next frame
without recomputing per-option per-frame.

Inspect resolution with `:describe-option <name>` -- shows
which layer each value came from.

## Programmatic API

For hooks / config / plugin code:

- `App::activate_mode_by_id(buffer_id, ModeId)` -- activate
  without going through the toggle.
- `App::deactivate_mode_by_id(buffer_id, ModeId)` -- explicit
  deactivate.
- `App::toggle_mode_by_name(name)` -- the same toggle the
  ex-command uses, callable from code.

These are the seams subsequent slices wire auto-activation
hooks into.

## Where to read more

- [lsp](help:lsp) -- the `lsp-mode` umbrella + per-feature LSP
  sub-modes (M.6).
- [completion](help:completion) -- insert-mode completion is a
  minor (`completion-mode`) in the longer term.
- `docs/mode-architecture.md` (developer-facing reference) --
  the full design spec including capability gates, conflict
  policy, and the migration plan.
