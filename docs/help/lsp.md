# Language Server Protocol (LSP)

LSP integration brings IDE features — go-to-definition, hover,
diagnostics, completion, rename, code actions, formatting,
inlay hints, and more — into lattice through a clean wire
protocol. Every modern language ships an LSP server; lattice
talks to them all without per-language plugins.

The architecture is **non-negotiable on latency**: every LSP
request is asynchronous, every response arrives via the
snapshot model (§5.6.8), and a slow or crashed server cannot
stall the editor. The four paramount goals (CLAUDE.md) apply
unchanged: performance, extensibility, modal editing,
asynchronicity.

> **Status:** Phase 4.1 (foundation) **complete**.
> Phase 4.2 (navigation) **9/12 + 1 partial**: hover (`K`),
> definition family (`gd` / `gD` / `gy` / `gI`), references
> (`gr`), document outline (`:lsp-symbols`), workspace
> symbols (`:lsp-workspace-symbol`), completion picker bridge
> (`:complete`).
> Phase 4.3 (edits) **3/9 shipped**: formatting (`:format` /
> `:format-range`), signatureHelp (`:signature-help` +
> Insert-mode trigger-char autopilot).
> All multi-result LSP lookups + `:diagnostics` route through
> a unified vertico picker; tag stack `<C-t>` pops drill-down
> chains. Per-feature status tracked in
> [`../lsp-features.md`](../lsp-features.md).

---

## What LSP gives you

| Feature               | What it does                                                                                                                                                                                       |
|-----------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| **Diagnostics**       | Errors, warnings, hints rendered as gutter glyphs + inline underlines, plus a `:diagnostics` buffer for the full list.                                                                             |
| **Hover**             | Press `K` on a symbol to see its type signature, doc comment, or runtime info in a popup.                                                                                                          |
| **Go-to-definition**  | `gd` jumps to where a symbol is defined; `gD` for declaration; `gy` for type definition; `gI` for implementations. The position-history ring (§5.1.1) records each jump so `<C-o>` walks back.     |
| **Find references**   | `gr` opens a buffer-backed list of every reference; navigate it with `j` / `k` and `<CR>` to jump.                                                                                                 |
| **Outline**           | `:outline` opens the document's symbol tree as a buffer pane. Standard motions (`j` / `k` / `gg` / `G` / search) all work.                                                                         |
| **Workspace symbols** | `<Space>s` (or `:wsym`) opens a fuzzy picker over every symbol in the workspace.                                                                                                                   |
| **Completion**        | Triggered automatically in Insert mode; lattice's completion pipeline merges LSP suggestions with grammar-driven completions. Snippet expansion, label details, and lazy resolution all supported. |
| **Signature help**    | Function-call popup with parameter highlighting. Triggers automatically after `(` or `,` in supported languages.                                                                                   |
| **Code actions**      | `<Space>a` opens a picker of available quick fixes / refactors. Multi-file edits apply atomically.                                                                                                 |
| **Rename**            | `<F2>` renames the symbol under cursor across every file in the workspace.                                                                                                                         |
| **Format**            | `=G` formats the buffer; `=` operator formats motions / objects; `:format` formats the active selection.                                                                                           |
| **Inlay hints**       | Type annotations, parameter names, and lifetime hints rendered as ghost text inside the buffer. Toggle with `:set inlayhints` or `<Space>tih`.                                                     |
| **Semantic tokens**   | LSP-aware syntax highlighting that augments tree-sitter (e.g. distinguishes mutable vs immutable bindings, type vs value namespaces).                                                              |
| **Folding**           | LSP-provided fold ranges merge into the fold engine — see [`folding.md`](folding.md).                                                                                                              |

The full feature matrix (every LSP 3.17 capability + status)
lives in [`../lsp-features.md`](../lsp-features.md).

---

## Setup

### Installing servers

lattice does not bundle servers; install the one(s) you need
through your language's standard channel. The defaults expect
the canonical binary on `PATH`:

| Language                | Binary                       | Install                                                |
|-------------------------|------------------------------|--------------------------------------------------------|
| Rust                    | `rust-analyzer`              | `rustup component add rust-analyzer`                   |
| Python                  | `pyright-langserver`         | `npm install -g pyright`                               |
| Go                      | `gopls`                      | `go install golang.org/x/tools/gopls@latest`           |
| TypeScript / JavaScript | `typescript-language-server` | `npm install -g typescript-language-server typescript` |
| C / C++                 | `clangd`                     | `apt install clangd` / `brew install llvm`             |
| Lua                     | `lua-language-server`        | `brew install lua-language-server`                     |

Other servers can be configured manually (see
**Configuration** below).

### What lattice does on file open

When you open a buffer, lattice:

1. **Detects the language** from the file extension (and a
   future content-pattern fallback for shebang scripts).
2. **Resolves the workspace root** by walking up from the
   buffer's directory looking for marker files (`.git`,
   `Cargo.toml`, `pyproject.toml`, `go.mod`, `package.json`,
   `compile_commands.json`, ...). Falls back to the buffer's
   directory if nothing matches.
3. **Spawns the server** if no actor is already running for
   `(workspace, language)`. The server's `initialize` /
   `initialized` handshake completes before the buffer is
   considered "attached".
4. **Sends `didOpen`** with the buffer's text and language id.
   The server can now report diagnostics and answer queries.

If the server isn't on `PATH`, lattice tells you in the
modeline. The buffer still opens — LSP failure never blocks
editing.

---

## Navigation

The 4.2 navigation surface is wired through one consistent
shape: `K` for hover, `g`-prefixed chords for jumps, `:lsp-*`
ex commands for outlines / workspace search. Multi-result
lookups + `:diagnostics` open a vertico picker
(type-to-filter, `<CR>` to jump, `<Esc>` to dismiss);
single-result jumps go directly per vim convention.

### Hover

| Keystroke   | Meaning                                                                                    |
|-------------|--------------------------------------------------------------------------------------------|
| `K`         | Show LSP hover documentation in a tooltip popup near the cursor (markdown-rendered).       |
| `K` (again) | Move focus into the popup. Full vim grammar inside (search, motions, scroll); `<Esc>`/`q` returns. |

### Jumps

All four use the same dispatch path; differs only in which
LSP method runs server-side. Single result jumps directly;
multi-result opens the picker. **Two persistence shapes
record where you've been:**

- **Jump list** (`<C-o>` back, `<C-i>` forward) -- chronological
  history of every "big jump". Every LSP nav, picker accept,
  search submit, `n`/`N` repeat, and `gg`/`G`-style motion
  pushes onto it.
- **Tag stack** (`<C-t>` to pop) -- LIFO chain of `gd` / `gD` /
  `gy` / `gI` drill-downs only. Distinct from the jump list:
  the user's mental model for `<C-t>` is "undo the drill-down
  chain", not "step through every cursor jump". The two have
  different lengths and different push semantics.

`<C-o>` and `<C-t>` coexist deliberately: walk a chain of
five `gd` calls, then `<C-t>` pops one drill-down at a time
back to the original symbol; `<C-o>` walks every cursor
position you've visited along the way (which may include
references-picker rows, diagnostics jumps, etc.).

| Chord | LSP method                  | What it asks                                              |
|-------|-----------------------------|-----------------------------------------------------------|
| `gd`  | `textDocument/definition`   | Where is the symbol defined?                              |
| `gD`  | `textDocument/declaration`  | Where is the forward declaration / `extern`?              |
| `gy`  | `textDocument/typeDefinition` | Where is the *type* of the expression defined?         |
| `gI`  | `textDocument/implementation` | Where are the implementations of this trait / interface? Lowercase `gi` is reserved for vim's "go to last insert position". |

### Lists

| Keystroke / command          | Picker over                                                          |
|------------------------------|----------------------------------------------------------------------|
| `gr`                         | `textDocument/references` -- every call site of the symbol under cursor (`include_declaration: true`). |
| `:lsp-symbols`               | `textDocument/documentSymbol` -- the active document's outline; nested DocumentSymbol responses keep their hierarchy as picker indent. |
| `:lsp-workspace-symbol [q]`  | `workspace/symbol` -- workspace-scoped symbols matching `q` (server-side substring filter). Empty `q` returns the server's idea of "every workspace symbol". Fans out across **every running server**, not just the active buffer's. |
| `:diagnostics`               | Every workspace diagnostic across attached servers; severity in marginalia.   |
| `:complete`                  | LSP completion items at the cursor (`textDocument/completion`). Plain-text insert; snippet expansion + lazy `completionItem/resolve` queued behind buffer-level Insert-mode completion. |

### Edits

| Command                | Effect                                                                                              |
|------------------------|-----------------------------------------------------------------------------------------------------|
| `:format` (`:fmt`)     | `textDocument/formatting` -- whole buffer; highest-priority server with the provider; one undo unit.|
| `:format-range`        | `textDocument/rangeFormatting` over the active Visual selection (or whole buffer when not in Visual). |
| `:signature-help`      | `textDocument/signatureHelp` -- popup with the active signature + parameter highlight. Auto-fires in Insert mode on server-advertised trigger characters (typically `(` and `,`). |

Each picker entry encodes `path:line:col` in its candidate
text; accept dispatch parses through `jump_to_file_line_col`
which pushes the pre-jump cursor onto position history. So
the same `<C-o>` dance works regardless of whether you took
a single-result `gd` jump or picked one of N references.

---

## Diagnostics

When the server reports problems, lattice renders them in
three places:

- **Gutter** — a glyph in the line-number column for every
  line with diagnostics (severity-coloured: red `■` for error,
  yellow `▲` for warning, blue `●` for info, grey `·` for
  hint). The most severe wins on lines with multiple.
- **Inline** — squiggly underline under the affected range
  (utf-8 / utf-16 column converted from the server's units).
- **`:diagnostics` picker** — opens a vertico picker over
  every workspace diagnostic. Severity glyph (`[E]/[W]/[I]/[H]`)
  rides in the marginalia; type to filter; `<CR>` jumps; `<Esc>`
  dismisses.

### Diagnostic navigation

| Keystroke / command | Meaning                                                                                  |
|---------------------|------------------------------------------------------------------------------------------|
| `]d`                | Jump to the next diagnostic in the active buffer (wraps to next file in `:diagnostics`). |
| `[d`                | Jump to the previous diagnostic.                                                         |
| `:diagnostics`      | Open the workspace diagnostics buffer.                                                   |
| `:cnext` / `:cprev` | Walk the diagnostic list (alias of `]d` / `[d` for users coming from vim quickfix).      |
| `<Space>e`          | Show the diagnostic at the cursor in a popup (full message + related-info if any).       |
| `:diag-clear`       | Drop the renderer's overlay for the active buffer (server may republish).                |

Diagnostics are version-tracked — if the server publishes
diagnostics for a stale doc version (because an edit raced
with the publish), lattice drops the stale ones rather than
overwriting fresher state.

---

## Configuration

> Most users never need to configure anything: the defaults
> match how each server's docs say to invoke it. Override only
> when you need a non-standard binary path or initialization
> options.

### Where config lives

LSP configuration is part of lattice's options system (§5.12).
Until the typed-options layer ships, `lsp.toml` at the
workspace root provides overrides:

```toml
# lsp.toml at workspace root
[server.rust]
binary = "/opt/rust-analyzer-nightly/rust-analyzer"
args = []
root_markers = ["Cargo.toml", "rust-project.json"]
file_patterns = ["*.rs"]

[server.rust.initialization_options]
checkOnSave.command = "clippy"
cargo.features = ["all"]

[server.python]
binary = "pyright-langserver"
args = ["--stdio"]
```

The keys mirror `ServerConfig` (see
[`../lsp-architecture.md`](../lsp-architecture.md) for the
schema).

### Per-language overrides

A user-level `~/.config/lattice/lsp.toml` is merged on top of
the workspace file. Workspace wins on key collision.

### Disabling LSP for a buffer

`:set nolsp` (per-buffer) detaches the server for that buffer
without affecting others. Re-attach with `:set lsp`.

---

## Performance discipline

Every commitment in §8.2 holds with LSP attached:

- **No request blocks the UI.** Every `ServerHandle::request` /
  `notify` returns immediately; the editor's input pipeline
  never awaits a server response synchronously.
- **`didChange` is debounced.** Every keystroke commits one
  edit but doesn't emit `didChange` until ~50ms of idle. A
  fast typist won't generate one wire message per keystroke.
- **Diagnostics are version-gated.** Stale publishes are
  silently dropped.
- **Cancellation is real.** A pending request can be cancelled
  via `$/cancelRequest` + the `Pending` resolves locally;
  superseding a stale completion request is a one-line API.
- **Crashed servers don't take the editor down.** The actor
  detects pipe close, the supervisor restarts with backoff,
  and pending requests resolve with a clear `ActorGone` error.

The benches in `crates/lattice-lsp/benches/lsp.rs` measure
the wire-layer cost (framing, encode/decode, position
conversion) — all sit in the nanosecond-to-microsecond range,
well below human-perceptible latency.

---

## Debugging & logs

LSP integration ships with a layered logging surface modelled
after emacs's `*lsp-log*` / `*<server> stderr*` buffers, on
top of lattice's everything-is-a-buffer foundation.

### Log buffers

| Buffer | Contents | Open with |
|---|---|---|
| `*lsp*` | Subsystem-wide events: server spawn / handshake / crash / restart, supervisor decisions. | `:lsp-log` |
| `*lsp:<server-id>*` | Per-server: stderr lines, `window/logMessage` and `window/showMessage` notifications at the server-requested severity, lifecycle events, decode failures, capability gating decisions. | `:lsp-log <server>` |
| `*lsp:<server-id>:trace*` | Full JSON-RPC wire trace -- every inbound (`←`) and outbound (`→`) message, body truncated at ~240 chars. Off by default. | `:lsp-trace <server>` toggles + opens. |

Each buffer is read-only, auto-scrolls to tail when you're at
the bottom, and keeps a bounded ring (10 000 records / server
by default; oldest evicts first).

### Log levels

Five levels, matching `tracing` / `RUST_LOG` conventions:

| Level | What it captures |
|---|---|
| `error` | Unrecoverable failures (server died, framing rejected, decode totally broken). |
| `warn` | Recoverable problems: stderr lines, unexpected protocol behaviour, malformed payloads dropped. |
| `info` | **Default.** Lifecycle milestones (handshake done, server attached). |
| `debug` | Per-message detail: `publishDiagnostics` summaries, `$/progress` events, telemetry. |
| `trace` | JSON-RPC wire trace. Only emitted when trace mode is on for the server (otherwise short-circuited cheaply -- ~9 ns/call when off). |

The default min level is `info`; per-server overrides allowed.

### Commands

| Command | Effect |
|---|---|
| `:lsp-log` | Open `*lsp*`. |
| `:lsp-log <server>` | Open `*lsp:<server>*` (e.g. `:lsp-log rust`). |
| `:lsp-trace <server>` | Toggle JSON-RPC trace + open the trace buffer. |
| `:lsp-status` | Show every running server (id, root, pid, uptime, restart count). |
| `:lsp-restart <server>` | Force-restart a server; re-issues `didOpen` for every attached buffer. |
| `:lsp-log-level <level>` | Subsystem-wide default min level. |
| `:lsp-log-level <server> <level>` | Per-server min level override. |
| `:lsp-log-clear [server]` | Drop the buffer's records. |

### Configuration

`lsp.toml` (workspace) + `~/.config/lattice/lsp.toml` (user)
accept these logging keys:

```toml
[lsp]
log_level    = "info"   # subsystem-wide default
log_capacity = 10000    # records per ring (per server + global)

[server.rust]
log_level = "debug"     # override for this server
trace_io  = true        # turn on JSON-RPC trace at startup
```

Every key is optional; defaults are conservative
(`info` / `10000` / no trace).

### Tracing-crate fall-through

Every record `LspLogger` emits also fires a `tracing::*` event
at the matching level. Two consequences:

- Users who run `RUST_LOG=lattice_lsp=debug ./lattice` see the
  same events on stderr as in the buffer views.
- Custom `tracing_subscriber` setups (JSON logs, OpenTelemetry,
  ...) work without further wiring.

The two paths are independent: in-memory rings survive even
when no `tracing` subscriber is installed; `tracing` users see
the same events whether or not buffer views are open.

### Debugging workflows

**"Server isn't returning hover."**

1. `:lsp-log rust` -- look for handshake errors or "server
   request unhandled" entries.
2. `:lsp-status` shows the server's advertised capability set.
   If `hoverProvider` isn't there, the server doesn't support it.
3. `:lsp-trace rust` -- watch for
   `→ Request method=textDocument/hover` then the matching
   `← Response id=...`. A request without a response means the
   server is stuck.

**"Diagnostics are stale."**

1. `:lsp-trace rust` then look for
   `← Notification method=textDocument/publishDiagnostics`.
   Compare its `version` field against lattice's `DocSync`
   version -- stale publishes are correctly dropped.
2. `*lsp:rust*` shows `publishDiagnostics: N diag(s) for <uri>`
   summaries -- spot when the server stopped reporting.

**"Server keeps crashing."**

1. `*lsp:rust*` has stderr lines (warn-level) and lifecycle
   `read_loop terminating` errors. Server stderr captures the
   panic / exit reason verbatim.
2. `:lsp-status` shows restart counts.
3. The supervisor restarts with exponential backoff (100ms →
   5s) and re-issues `didOpen` for every attached buffer.

**"Editor feels slow when LSP attached."**

1. The §8.2 commitment is that LSP plumbing never blocks the
   UI. If it does, that's a bug -- file an issue with
   `cargo bench -p lattice-lsp` numbers.
2. `*lsp:rust*` may show a flood of `$/progress` -- a runaway
   indexer.
3. Trace mode itself adds ~100 ns per traced message.
   Negligible at editor pace, perceptible only at indexer
   bursts.

---

## Troubleshooting

### "rust-analyzer: command not found" in the modeline

The server binary isn't on `PATH`. Either install it through
your language's normal channel, or set an absolute path in
`lsp.toml`:

```toml
[server.rust]
binary = "/full/path/to/rust-analyzer"
```

### Server starts but no diagnostics appear

1. Check the server's stderr in the log — lattice emits each
   line as a tracing warning with the `server_id` field.
   `RUST_LOG=lattice_lsp=debug` surfaces them at the terminal.
2. Confirm the workspace root matches what the server expects.
   For rust-analyzer, this is the directory containing
   `Cargo.toml`. If lattice resolved the wrong root, edit
   `lsp.toml` to set `root_markers` explicitly.
3. Some servers need warmup time on first open (rust-analyzer
   indexes the workspace). Wait a few seconds.

### Diagnostics from a stale doc version

This shouldn't happen — lattice version-gates publishes — but
if you see ghost diagnostics that don't match the current
buffer, run `:diag-clear` to drop the overlay. The server
should republish on the next idle tick.

### Server crashed mid-session

The supervisor restarts with exponential backoff (100ms,
200ms, 400ms, ..., max 5s). After restart, lattice re-issues
`didOpen` for every previously-open buffer the server cared
about. If the crash is reproducible, file an issue with the
server's stderr output.

### `:lsp-status`

Shows every running server with PID, workspace root, attached
buffer count, and uptime. Useful for confirming the server
you expect is the one actually attached.

---

## Power-user flows

### Per-buffer navigation vs workspace quickfix

Three navigation surfaces, each with a clear scope:

- **`]d` / `[d`** -- walk diagnostics in the **active buffer
  only**. Wraps top/bottom. Same diagnostic list two panes can
  share; the cursor "where am I in the walk" lives on the
  pane, so two panes on the same file walk independently.
- **`:diagnostics`** -- opens the **workspace-wide** list as a
  buffer (every URI with diagnostics, sorted alphabetically;
  diagnostics within each URI sorted by line then column).
- **`:cnext` / `:cprev`** -- walk **the `:diagnostics`
  buffer's list**, vim-quickfix-style. Hits the bottom →
  wraps to top. Jumps across files. The cursor on the
  `:diagnostics` buffer is the iteration state, so opening
  it explicitly and using `]d` over there does the same
  thing.
- **`:diagnostics buffer`** -- a filtered view: only the
  active buffer's diagnostics. Useful when you want
  `:cnext`-style navigation but limited to one file.

When `]d` says "no more diagnostics in this buffer" and
`:cnext` would jump to a different file, the model is
deliberate: per-buffer navigation never surprises you by
switching files; workspace navigation does so on purpose.

### One buffer, multiple servers

A `.cpp` file with both `clangd` (semantics) and a custom
linter bridge (style) attached. Each is its own actor; both
publish diagnostics. lattice merges per feature:

| Feature         | What happens with two servers                                                                                                |
|-----------------|------------------------------------------------------------------------------------------------------------------------------|
| Diagnostics     | Both lists shown in the gutter / inline overlay / `:diagnostics` buffer. The most severe wins per-line for the gutter glyph. |
| Hover           | Both responses concatenated, each section labelled with its `server_id`.                                                     |
| Goto-definition | Higher-priority server's response wins. If empty, falls through to the next.                                                 |
| References      | Union of all servers' results, deduped.                                                                                      |
| Code actions    | Picker shows entries from each server, prefixed `[server-id]` so you can tell them apart.                                    |
| Formatting      | Single winner: highest-priority server with `documentFormattingProvider`. Avoids two formatters fighting.                    |
| Rename          | `WorkspaceEdit`s from each server merged; conflicts (same range edited by two servers) resolve to the higher-priority one.   |

Server priority is configurable in `lsp.toml`:

```toml
[server.rust]
priority = 100  # default; higher wins ties

[server.clippy-bridge]
priority = 50
```

A crashed server affects only its own diagnostics + its own
running task; other servers attached to the same buffer keep
working.

### One server, multiple workspaces

Open files from two unrelated Cargo workspaces. lattice spawns
two `rust-analyzer` actors, one per workspace root. Each
maintains its own indexed view; cross-workspace navigation is
not yet supported (multi-root workspace folders are post-1.0).

### Toggling features off

`:set nodiagnostics`, `:set noinlayhints`, `:set nosemtokens`
disable specific feature renderers without detaching the
server (so go-to-definition still works). Useful for visual
decluttering on screens.

---

## Related

- **[`folding.md`](folding.md)** — fold engine; LSP fold ranges
  merge in via `FoldMethod::Lsp` (4.4).
- **[`../lsp-features.md`](../lsp-features.md)** — feature
  matrix.
- **[`../lsp-architecture.md`](../lsp-architecture.md)** —
  developer-facing architecture.
- **[`../DESIGN.md#54-lsp-subsystem`](../DESIGN.md)** §5.4 —
  canonical design.
