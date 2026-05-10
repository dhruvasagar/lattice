# Modes

Lattice's mode system has two orthogonal axes:

- **Modal state** — Normal / Insert / Visual / Operator-Pending /
  Command / Search / Replace. The vim-grammar state machine
  driven by keys. Buffer-local but transient. See
  [`modal-editing`](help:modal-editing) for the grammar; this
  page is about the *other* axis.
- **Major + minor modes** — the editor's *behaviour layers*. A
  buffer always has zero or one **major mode** (its content
  identity: `rust-mode`, `markdown-mode`, `text-mode`,
  `help-mode`, `file-tree-mode`, `oil-mode`, `lsp-log-mode`,
  ...) plus any number of **minor modes** (capability layers:
  `lsp-mode`, `line-numbers-mode`, `wrap-mode`,
  `current-line-highlight-mode`, ...).

Major and minor modes contribute typed options
(`ReadOnly = true`, `Wrap = false`, ...) and per-buffer state.
The contributions layer through the option resolver to produce
a single `ResolvedOptions` snapshot the renderer reads. Modes
also subscribe to events (`MajorEntered`, `BufferOpened`,
`CursorMoved`, ...) to react to the editor's lifecycle.

> Modes are *the* extensibility unit. Every keystroke, render
> decision, and option lookup goes through the mode stack. When
> a feature feels native — line numbers, LSP integration, hover
> popups — it's a mode.

---

## Quick reference

| Command                            | Effect                                                          |
|------------------------------------|-----------------------------------------------------------------|
| `:<mode-name>`                     | Toggle a registered mode on the active buffer                   |
| `:list-modes`                      | List every registered mode, marking the active ones with `*`    |
| `:describe-mode <name>`            | Show one mode's metadata (kind, contributed options, caps)      |
| `:describe-option-resolution NAME` | Show which layer provides the resolved value for `NAME`         |
| `:customize <name>`                | Open the customize buffer for a group or mode                   |
| `:apropos -mode$`                  | Search for everything ending in `-mode` (mode catalogue)        |

`<Tab>` after `:` enumerates every command, including every
mode's auto-generated toggle.

---

## The full mode catalogue

### Major modes (one active per buffer)

| Mode                  | Owns                                                                                                |
|-----------------------|-----------------------------------------------------------------------------------------------------|
| `text-mode`           | Plain text. The fallback major when no language matches.                                            |
| `rust-mode`           | Rust source — tree-sitter highlight, fold provider, language config.                                |
| `python-mode`         | Python source.                                                                                      |
| `javascript-mode`     | JavaScript / TypeScript source.                                                                     |
| `markdown-mode`       | Markdown — heading-aware folds, fenced-block highlight, link parsing for help / hover surfaces.     |
| `help-mode`           | (Composed with markdown-mode.) Read-only + link-follow + `:apropos` integration.                    |
| `file-tree-mode`      | The file-tree buffer kind — directory listing, expand/collapse, icon rendering.                     |
| `oil-mode`            | The oil-style editable directory listing.                                                           |
| `lsp-log-mode`        | The per-server log buffer (`*lsp:<server>*`).                                                        |
| `lsp-trace-log-mode`  | The per-server JSON-RPC wire trace.                                                                  |
| `lsp-server-log-mode` | The per-server stderr feed.                                                                          |

### Minor modes

#### LSP umbrella + sub-modes

| Mode                     | Gates                                                            |
|--------------------------|------------------------------------------------------------------|
| `lsp-mode`               | Umbrella — every other LSP sub-mode requires this active.        |
| `lsp-completion-mode`    | LSP completion source + insert popup contributor + `:complete`.  |
| `lsp-diagnostics-mode`   | Inline + gutter diagnostic paint + `]d`/`[d`.                    |
| `lsp-hover-mode`         | `K` hover popup.                                                 |
| `lsp-signature-mode`     | Auto signature help on `(` / `,`.                                |
| `lsp-format-mode`        | `:lsp-format` / `:lsp-format-range` + on-type-formatting.        |
| `lsp-rename-mode`        | `:lsp-rename` + workspaceEdit apply.                             |
| `lsp-symbols-mode`       | `:lsp-symbols`, `:lsp-workspace-symbol`.                         |
| `lsp-code-action-mode`   | `:lsp-code-action`.                                              |
| `lsp-nav-mode`           | `gd` / `gD` / `gy` / `gI` definition family + `gr` references.   |

See [`lsp-mode`](help:lsp-mode) for the full sub-mode contract.

#### Display modes (M.7)

| Mode                              | Contributes                                                       |
|-----------------------------------|-------------------------------------------------------------------|
| `line-numbers-mode`               | `Number = true`                                                   |
| `relative-line-numbers-mode`      | `RelativeNumber = true` + `Number = true` (vim's rnu-implies-nu)  |
| `wrap-mode`                       | `Wrap = true`                                                     |
| `read-only-mode`                  | `ReadOnly = true` (the user-typed pathway)                        |
| `whitespace-show-mode`            | `Whitespace = true` (`:set list`)                                 |
| `current-line-highlight-mode`     | `CursorLine = true` (`:set cursorline`)                           |

`:set NAME=VALUE` and `:<mode-name>` are two surfaces on the
same state. Setting the option flips the mode's activation;
toggling the mode flips the option's resolved value (one-way
ratchet — the typed-option layer keeps the user's last `:set`
across mode toggles, so flipping the mode doesn't silently
rewrite a global default). See
[`options`](help:options) for the layer model.

#### Other minors

| Mode                  | Owns                                                                                       |
|-----------------------|--------------------------------------------------------------------------------------------|
| `hover-mode`          | The hover popup buffer (composed with `markdown-mode`).                                    |

---

## Toggling — `:<mode-name>`

Every registered mode is reachable as an ex-command whose
keyword is the mode's id. **No** `:enable` / `:disable` /
`:activate` prefix:

```
:rust-mode               " activate rust-mode (or reload if already active)
:markdown-mode           " swap the major from rust to markdown
:lsp-mode                " toggle the lsp-mode umbrella
:line-numbers-mode       " toggle line numbers
:lsp-hover-mode          " toggle just the hover sub-mode
```

The toggle command is auto-generated per mode at boot. Adding a
mode (built-in or future plugin) gives you `:<that-name>` for
free, without manual registration.

### Toggle semantics

- **Minor mode** — a true toggle. Active → deactivate (the
  mode's `on_deactivate` runs, option contributions roll back,
  owned `BufferLocals` are cleaned up). Inactive → activate.
- **Major mode** — activating *swaps*. Running `:markdown-mode`
  on a buffer whose current major is `rust-mode` deactivates
  rust-mode first, then activates markdown-mode. Running
  `:rust-mode` on a buffer that already has rust-mode triggers
  a **reload** (deactivate + re-activate; useful when you
  changed something the major reads at activation).

### Major swaps don't touch minors

If you have `lsp-mode` active on a buffer and you run
`:text-mode` (swapping the major), **lsp-mode stays active**.
The minors live on their own; their state is owned per-mode in
typed `BufferLocals`, so a major swap can't corrupt or clear it.

This is the deliberate departure from emacs's
`kill-all-local-variables` footgun. Lattice's typed-ownership
model means we don't need it. If you want a minor gone after a
major swap, run its toggle explicitly:

```
:rust-mode          " was rust-mode + lsp-mode
:text-mode          " now text-mode + lsp-mode (lsp-mode kept)
:lsp-mode           " toggle lsp-mode off; now plain text-mode
```

### Auto-activation

Some minors follow the major automatically. `lsp-mode` is the
canonical example — when you enter a language major that has a
configured server, you usually want LSP on. That's wired via
the `MajorEntered` event hook: a subscription activates
`lsp-mode` on entry.

**Auto-activation is one-way.** Exiting a major doesn't
auto-deactivate the auto-activated minor. The trade-off keeps
state predictable across edits + swaps; an explicit `:lsp-mode`
toggle removes it when you actually want it gone.

### Cascade activation (`lsp-mode`)

The `lsp-mode` umbrella has a special property: activating it
cascades activation of every LSP sub-mode (`lsp-hover-mode`,
`lsp-completion-mode`, ...). Deactivating cascades them off.
Between those endpoints, you can disable one sub-mode
individually while the umbrella stays on:

```
:lsp-mode               " activate umbrella + cascade on all 9 sub-modes
:lsp-completion-mode    " disable just LSP completion (others still active)
```

Re-toggling `:lsp-mode` (off → on) is the **reset gesture**:
every sub-mode flips back to active, including ones you'd
disabled individually.

---

## How modes interact with options

Each mode declares which typed options it contributes via
`Mode::options()`:

```rust
impl Mode for HelpMode {
    fn options(&self) -> OptionOverrideSet {
        overrides! {
            lattice_config::ReadOnly = true,
            lattice_config::Wrap = true,
        }
    }
    // ...
}
```

The resolver chains contributions in priority order
(highest-priority-first):

1. **Modal-state** — reserved for future modal-keyed overrides.
2. **Buffer-local** (`:setlocal foo=bar`) — explicit per-buffer
   override.
3. **Active minor modes** in activation order; the most-recently
   activated minor wins ties.
4. **Major mode**.
5. **Typed-option layer** (`:set foo=bar`) — the global default.
6. **Built-in default** — the option's registered default value.

Toggling a mode triggers a cache rebuild for the buffer's
`ResolvedOptions`. The renderer reads from the cache; no
per-frame resolution.

**Debugging a value:** `:describe-option-resolution <name>`
walks every layer and shows which one provided the resolved
value, with a ⭐ on the contributing layer. Useful when you
`:set nonumber` but the gutter still renders — the resolution
view immediately surfaces the mode contribution that's
shadowing your write.

---

## Introspection

### `:list-modes`

Shows every registered mode grouped by kind:

```
# Modes (28 registered)

## majors (10)

- * text-mode
-   markdown-mode
-   rust-mode
- ... (one per registered major)

## minors (18)

- * lsp-mode
- * lsp-hover-mode
-   line-numbers-mode
- ... (one per registered minor)
```

`*` marks the modes active on the current buffer. Each row is
a clickable link — `<CR>` on `[lsp-mode](mode:lsp-mode)`
follows to `:describe-mode lsp-mode` (when the mode-link follow
ships).

### `:describe-mode <name>`

Shows one mode's metadata:

```
# mode :: lsp-mode

- kind: `minor`
- active on current buffer: yes
- contributed options: (none)
- required capabilities: (none)

Toggle with `:lsp-mode`. For options the mode contributes,
see `:describe-option <name>`.
```

For modes that contribute options, the rows list each option's
canonical name (resolve to `:describe-option NAME` for full
metadata).

### `:customize <mode-name>`

Opens an interactive form view of every option the mode
contributes. Edit values inline (`<CR>` on a row prefills
`:set NAME=current` for editing). When the mode is active on
the current buffer, each row gets a `[mode-shadow]` indicator
— the user understands that a `:set` write would be overridden
by the mode-contribution layer until the mode deactivates.

See [`options`](help:options) for the customize surface.

---

## Programmatic API

For hooks / config / plugin code:

- `App::activate_mode_by_id(buffer_id, ModeId)` — activate
  without going through the toggle.
- `App::deactivate_mode_by_id(buffer_id, ModeId)` — explicit
  deactivate.
- `App::toggle_mode_by_name(name)` — the same toggle the
  ex-command uses, callable from code.
- `App::lsp_<feature>_mode_enabled_for(buffer_id) -> bool` —
  per-sub-mode accessor for the LSP family (`lsp_hover_mode_enabled_for`,
  etc.). Used by gate sites.
- `App::active_modes` — the per-buffer mode set
  (`HashMap<BufferId, ActiveModes>`); `ActiveModes::major()` /
  `ActiveModes::minors()` / `ActiveModes::has_minor(id)`.

These are the seams plugins / future hooks wire into.

---

## Edge cases

### Activating an already-active minor

No-op. The registry's `activate_minor` returns `Ok` silently;
no `on_activate` fires twice. (Toggle would deactivate; direct
activate is idempotent.)

### Activating a major that's already active

**Reloads** — deactivates then re-activates. Used for picking
up changes a mode reads only at activation time.

### Capability requirements

A few modes declare `required_capabilities()` (e.g. a future
mode might require `BufferUri` capability). Activating a mode
on a buffer that lacks the required capabilities returns a
`MissingCapabilities` error and the toggle echoes a message —
the mode stays inactive.

### Modes vs option names

Mode ids always end in `-mode`. Option group names never end
in `-mode`. This invariant is enforced at registration time
(compile-time `const fn` byte-walk asserting the suffix).
`:customize editor` opens the editor *group*; `:customize
rust-mode` opens the rust-mode *focused view*.

---

## See also

- [`modal-editing`](help:modal-editing) — the *other* mode
  axis: Normal / Insert / Visual / etc.
- [`lsp-mode`](help:lsp-mode) — the LSP umbrella minor + nine
  sub-modes (M.6 family).
- [`options`](help:options) — typed-options model + the
  customize buffer view + the layered resolver.
- [`completion`](help:completion) — insert-mode completion
  (will become a minor mode in M.7+ wave).
- `docs/dev/architecture/mode-architecture.md` — full design
  spec including capability gates, conflict policy, migration
  plan.
