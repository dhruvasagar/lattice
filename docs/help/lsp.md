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

> **Status:** Phase 4 is in progress. Foundation (4.1) is
> partially shipped: wire layer, per-server actor with
> capability handshake, document synchronisation, and
> diagnostics routing. Per-feature status is tracked in
> [`../lsp-features.md`](../lsp-features.md). The keystrokes
> below describe the planned v1 surface.

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

## Diagnostics

When the server reports problems, lattice renders them in
three places:

- **Gutter** — a glyph in the line-number column for every
  line with diagnostics (severity-coloured: red `■` for error,
  yellow `▲` for warning, blue `●` for info, grey `·` for
  hint). The most severe wins on lines with multiple.
- **Inline** — squiggly underline under the affected range
  (utf-8 / utf-16 column converted from the server's units).
- **`:diagnostics` buffer** — opens a new pane listing every
  diagnostic in the workspace with file / line / message.
  Standard motions navigate; `<CR>` jumps to the diagnostic.

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

### One server, multiple workspaces

Open files from two unrelated Cargo workspaces. lattice spawns
two `rust-analyzer` actors, one per workspace root. Each
maintains its own indexed view; cross-workspace navigation is
not yet supported (multi-root workspace folders are post-1.0).

### Two servers, one buffer

A `.cpp` file with both `clangd` (semantics) and a custom
linter bridge (style) attached. Each is its own actor; both
publish diagnostics; the renderer merges them per-line, sorted
by severity. Code actions from each appear in the picker
prefixed with their `server_id`.

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
