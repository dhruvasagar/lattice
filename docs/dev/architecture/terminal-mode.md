# Terminal Mode (developer reference)

This document is the implementer-side companion to the
forthcoming `design.md` §5.B (canonical text lands once T1
ships). User-facing help lives in
[`../../user/terminal.md`](../../user/terminal.md). The
planning document with slice-level breakdown lives in
[`../operations/terminal-mode-plan.md`](../operations/terminal-mode-plan.md).

---

## 1. Goals

Terminal-mode brings a PTY-backed shell into lattice as a
first-class buffer, mirroring vim's `:terminal` and emacs
vterm. Three paramount-goal commitments shape the design:

- **Performance (goal #1).** PTY I/O, VT parsing, and grid
  state-machine updates run on dedicated tokio tasks. The UI
  thread never touches the PTY, never parses an escape
  sequence, never blocks on the child process. Render-state
  publish uses the existing ArcSwap pattern.
- **Modal (goal #3).** The vim modal grammar is preserved.
  Terminal buffers have two sub-states: **Normal-in-terminal**
  (vim grammar applies to the scrollback) and **Terminal-Insert**
  (keystrokes encode to the PTY). `<C-\><C-n>` is the
  always-on escape hatch.
- **Asynchronicity (goal #4).** Each terminal owns its own
  tokio task chain. Many terminals running concurrently in
  splits / tabs scale with `num_cpus` workers.

## 2. Substrate choice

| Choice | Decision |
|---|---|
| **VT parser + grid state machine** | [`alacritty_terminal`](https://docs.rs/alacritty_terminal) — pure Rust, battle-tested (alacritty, Zed, neovide), typed escape-handler hooks for future extensibility, no FFI / no extra toolchain. |
| **PTY** | [`portable-pty`](https://docs.rs/portable-pty) — cross-platform (Unix, macOS, Windows ConPTY), spawns the child, returns Read+Write handles. |
| **Future swap** | A thin trait abstraction over the parser+grid keeps the door open for [`libghostty`](https://ghostty.org/docs/help/libghostty) if sixel / kitty graphics protocol demand justifies the FFI complexity. Out of scope for v1. |

**`alacritty_terminal` is the state-machine library, NOT the
alacritty application.** It does not read alacritty's config; it
doesn't ship a renderer; it doesn't own a font. The renderer
chrome (font family / size / window decorations) is Lattice's
via the existing `ui.font_*` options. The shell's dotfiles
(`.bashrc` / `.zshrc` / etc.) are read by the shell process
Lattice spawns — same as any other terminal — so user shell
configuration carries over.

## 3. Crate layout

```
crates/lattice-terminal/
├── Cargo.toml
└── src/
    ├── lib.rs            -- public re-exports
    ├── handle.rs         -- PtyHandle: writer + resize + kill
    ├── spawner.rs        -- spawn_child (portable-pty wrapper)
    ├── snapshot.rs       -- TerminalSnapshot (renderer-facing)
    ├── cell.rs           -- Cell + CellAttrs + TerminalColor
    ├── scrollback.rs     -- bounded ring of historical rows
    ├── encode.rs         -- key → ANSI input encoding
    ├── reader.rs         -- async task: PTY bytes → state machine → publish
    └── state.rs          -- alacritty_terminal Term<EventProxy> wrapper
```

Dependencies (`Cargo.toml`):

```toml
[dependencies]
alacritty_terminal = "0.24"
portable-pty = "0.8"
tokio = { workspace = true }
arc-swap = { workspace = true }
parking_lot = { workspace = true }
lattice-core = { path = "../lattice-core" }
lattice-runtime = { path = "../lattice-runtime" }
tracing = { workspace = true }
```

## 4. Buffer kind integration

New variants slot into the existing typed buffer machinery:

```rust
// lattice-core::buffers
pub enum BufferKind {
    Document,
    Help,
    FileTree,
    Oil,
    Terminal,   // NEW
}
```

```rust
// lattice-host::buffer_registry
pub enum BufferData {
    Document(DocumentEntry),
    Help(HelpEntry),
    FileTree(FileTreeEntry),
    Oil(OilEntry),
    Terminal(TerminalEntry),   // NEW
}

pub struct TerminalEntry {
    pub id: BufferId,
    pub pty: Arc<PtyHandle>,
    pub state: Arc<ArcSwap<TerminalSnapshot>>,
    pub scrollback: Arc<parking_lot::Mutex<Scrollback>>,
    pub cwd: Option<PathBuf>,
    pub label: String,
    pub exit_status: Option<std::process::ExitStatus>,
    pub created_at: std::time::SystemTime,
}
```

Terminal buffers participate uniformly in:
- `:bnext` / `:bprev` / `:ls` / `:b N` / `:bd` (buffer registry walk)
- `gt` / `gT` / `{N}gt` (tabs)
- Pane operations (split / vsplit / close / navigate)
- Position history (jump list records cross-buffer position)

Terminal buffers do NOT participate in:
- Tree-sitter highlighting (no rope; cells carry style)
- LSP attach (no language)
- Folds (no semantic structure)
- `:write` / `:e` (no on-disk representation)
- Undo (the PTY child owns history; bash has its own undo)

## 5. Modal sub-states

A pane whose active buffer is `BufferKind::Terminal` has two
sub-states. Mode-line shows the current one explicitly.

### 5.1 Normal-in-terminal (default on entry)

- Mode-line: `-- TERMINAL --`
- Vim grammar applies to the **scrollback view** (NOT the PTY).
- Keystrokes operate on the cell grid as if it were a read-only
  buffer:
  - Motions (`j` / `k` / `gg` / `G` / `0` / `$` / `/`)
  - Operators (yank only — `y` copies cells as text)
  - Visual mode (charwise / linewise / blockwise — select cells)
  - `<C-o>` / `<C-i>` jump list integration
- Window motions work (`<C-w>` is the prefix).
- `i` / `a` / `I` / `A` enter Terminal-Insert.

### 5.2 Terminal-Insert

- Mode-line: `-- TERMINAL-INSERT --`
- Keystrokes encoded as ANSI input sequences → PTY stdin.
- `<C-w>` is **passed through** to the shell (shell's WERASE
  — delete word backward). Window navigation requires
  exiting Insert first.
- `<C-\><C-n>` always exits to Normal-in-terminal (vim
  convention). This is the escape hatch when `<C-w>` is
  shell-captured.
- `<Esc>` also exits by default; configurable via
  `terminal.esc_exits` for users whose shell binds Esc.
- Mouse passes through when the program (htop, tmux, etc.)
  has enabled a mouse mode and
  `terminal.mouse_passthrough != "off"`.

### 5.3 Transition table

| From | Key | To |
|---|---|---|
| Normal-in-terminal | `i` / `a` / `I` / `A` | Terminal-Insert |
| Terminal-Insert | `<C-\><C-n>` | Normal-in-terminal |
| Terminal-Insert | `<Esc>` (if `terminal.esc_exits`) | Normal-in-terminal |
| Terminal-Insert | `<C-w>` | shell (NOT to window mgr) |
| Normal-in-terminal | `<C-w>w` / `<C-w>j` / etc. | window navigation (standard) |

## 6. Async architecture

```
                ┌────────────────────────┐
                │   user keystroke       │
                └───────────┬────────────┘
                            ▼
                   translate (input.rs)
                            │
                ┌───────────┴────────────┐
                │ sub-state branch       │
                └─┬──────────────────┬───┘
                  │                  │
   Terminal-Insert│                  │Normal-in-terminal
                  ▼                  ▼
        Action::TerminalInput    Action::* (motions, etc.)
                  │                  │
                  ▼                  ▼
          encode::key_to_ansi    scrollback nav
                  │                  │
                  ▼                  │
        PtyHandle::write(bytes)      │
                  │                  │
                  ▼                  │
           PTY child stdin           │
                                     │
                 ┌───────────────────┘
                 │
                 ▼
           publish RenderState
                 │
                 ▼
          renderer paints

Independently, the read loop runs:

           PTY child stdout
                 │
                 ▼
   tokio task (reader.rs):
     - read raw bytes (32 KB chunks)
     - feed to vte parser
     - update Term<EventProxy>
     - throttle: batch into max-60Hz frames
     - on each frame: build TerminalSnapshot
     - ArcSwap publish into state cell
                 │
                 ▼
        next paint reads new snapshot
```

One tokio task per terminal buffer. The reader task owns the
`Term<EventProxy>` state machine; only it writes to the
`Arc<ArcSwap<TerminalSnapshot>>`. The writer side
(`PtyHandle::write`) is fire-and-forget — host code calls it
synchronously without await; the OS buffers writes to the PTY.

### 6.1 Render throttling

Many programs (cargo build, npm install, anything verbose)
spew thousands of lines per second. Naive "publish every
parsed chunk" would dominate the render thread. Strategy:

- Reader task batches into time windows (default 16ms = 60 Hz).
- Within a window: accumulate parsed bytes; rebuild snapshot
  once at window close.
- Snapshot rebuild is bounded by terminal size (200×60 = 12k
  cells) + cursor — not by output volume.

Window size configurable via `terminal.refresh_hz` (default 60).

## 7. TerminalSnapshot — the renderer contract

```rust
pub struct TerminalSnapshot {
    pub cells: Arc<[Cell]>,         // rows * cols, row-major
    pub rows: u16,
    pub cols: u16,
    pub cursor_row: u16,
    pub cursor_col: u16,
    pub cursor_visible: bool,
    pub cursor_shape: CursorShape,
    pub title: Option<String>,      // OSC 0/2
    pub alt_screen: bool,           // true while program is on the alt buffer
    pub seq: u64,                   // monotonic frame counter
}

pub struct Cell {
    pub ch: char,
    pub fg: TerminalColor,
    pub bg: TerminalColor,
    pub attrs: CellAttrs,
}

pub enum TerminalColor {
    Default,
    Named(NamedColor),     // 16 ANSI palette names
    Indexed(u8),           // 256-color
    Rgb(u8, u8, u8),       // truecolor
}

pub struct CellAttrs {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
    pub dim: bool,
    pub strikethrough: bool,
    pub blink: bool,
}
```

The render hot path:

1. Renderer reads `terminal_entry.state.load()` → `Arc<TerminalSnapshot>`.
2. Translates each `Cell` to renderer-native styling.
3. Paints into the pane area as a fixed-pitch grid.

`Cell`'s `TerminalColor` variants follow Lattice's existing
host `Color` enum — TUI adapter degrades `Rgb` to nearest
256/16-color; GPUI consumes `Rgb` directly.

## 8. PTY lifecycle

### 8.1 Spawn

`portable_pty::native_pty_system().openpty(...)` returns a pair
of `MasterPty` / `SlavePty`. Lattice keeps the master (read +
write); the slave is dup'd into the child's stdin/stdout/stderr
via `portable_pty::CommandBuilder`.

Spawn cwd: parent directory of the active document's path
(`editor.document.path().and_then(|p| p.parent())`), falling
back to process cwd. Configurable via `terminal.cwd`
(`document` / `cwd`).

Shell command: `terminal.shell` option, defaulting to:
1. `SHELL` environment variable if set.
2. `/bin/sh` (Unix) / `cmd.exe` (Windows) as fallback.

### 8.2 Resize

When the pane geometry changes (window resize, split, tab
switch), the renderer computes new (rows, cols) and calls
`PtyHandle::resize(rows, cols)`. portable-pty issues a
SIGWINCH-equivalent to the child. `alacritty_terminal::Term::resize`
reflows the grid. Wrapped lines re-flow correctly because
alacritty_terminal maintains soft-wrap line metadata.

### 8.3 Process exit

When the child exits:
- Reader task sees EOF on the master.
- Reader writes `exit_status` into the `TerminalEntry`.
- Publishes a final snapshot with the exit message appended.
- Behavior controlled by `terminal.exit_on_process_exit`:
  - `false` (default) — buffer stays open for review; user
    closes with `:bd!`.
  - `true` — buffer auto-closes on exit; pane reverts to
    previous buffer.

### 8.4 Force close

`:bd!` on a terminal buffer: send SIGKILL to the child via
`PtyHandle::kill`, drop the master PTY, remove the buffer
entry from the registry.

## 9. Input encoding

`encode::key_to_ansi(key, mods) -> Vec<u8>` translates a
`KeyChord` to the bytes the program-side terminal expects:

| Chord | Encoded |
|---|---|
| printable char `c` | `c` as UTF-8 |
| `<C-c>` | `\x03` (SIGINT) |
| `<C-d>` | `\x04` (EOF) |
| `<C-z>` | `\x1a` (SIGTSTP) |
| `<CR>` | `\r` |
| `<Esc>` (when not exiting) | `\x1b` |
| `<Tab>` | `\t` |
| `<BS>` | `\x7f` (DEL — modern shells expect this) |
| `<Up>` / `<Down>` / `<Left>` / `<Right>` | CSI cursor-key (mode-dependent: `\x1bOA` in application mode, `\x1b[A` in normal) |
| `<Home>` / `<End>` / `<PageUp>` / `<PageDown>` / `<F1>` … | standard CSI / SS3 codes |
| `<C-letter>` | `\x01` … `\x1a` (Ctrl-A … Ctrl-Z) |
| `<A-c>` | `\x1bc` (Alt = Esc prefix) |

Application keypad / cursor key modes (`DECPAM` / `DECCKM`)
are tracked by alacritty_terminal — the encoder reads
`Term::mode()` to pick the right escape variant.

## 10. Scrollback

Each `TerminalEntry` owns a bounded scrollback ring (default
10,000 rows; `terminal.scrollback_lines`). alacritty_terminal
maintains the live scrollback; we expose it via:

```rust
impl TerminalEntry {
    pub fn scrollback_view(&self) -> ScrollbackView<'_>;
}

pub struct ScrollbackView<'a> {
    pub total_rows: u32,
    pub viewport_row: u32,  // 0 = bottom (live); N = N rows up
}
```

Normal-in-terminal motions adjust `viewport_row`. When the
user returns to the bottom of the scrollback and switches to
Terminal-Insert, new output appears at the live bottom — same
UX as `tmux` / `screen` / `kitty`.

## 11. Configuration

New `Terminal` option group (registered via `labeled_enum!` /
`options!` macros):

| Key | Type | Default | Description |
|---|---|---|---|
| `terminal.shell` | string | `$SHELL` else `/bin/sh` | Command to spawn |
| `terminal.scrollback_lines` | i64 | `10000` | Max scrollback ring size |
| `terminal.display` | enum: `active-pane` / `split` / `vsplit` / `tab` | `active-pane` | Where `:terminal` lands |
| `terminal.enter_insert_on_open` | bool | `true` | Auto-enter Insert on `:terminal` |
| `terminal.exit_on_process_exit` | bool | `false` | Auto-close buffer on child exit |
| `terminal.refresh_hz` | i64 | `60` | Render throttle ceiling |
| `terminal.mouse_passthrough` | enum: `auto` / `on` / `off` | `auto` | Send mouse events to the program |
| `terminal.cwd` | enum: `document` / `cwd` | `document` | Spawn cwd source |
| `terminal.esc_exits` | bool | `true` | `<Esc>` exits Terminal-Insert |

All live-editable via `:set terminal.<key>=<value>`. Changes
apply to new terminals; existing terminals keep their boot-time
config.

## 12. Ex commands + keymap

| Surface | Behavior |
|---|---|
| `:terminal` / `:term` | Open shell in active pane (or per `terminal.display`) |
| `:terminal {cmd}` | Spawn `{cmd}` instead of `$SHELL` |
| `<C-w>T` | Move current terminal to a new tab |
| `<C-\><C-n>` | Exit Terminal-Insert (any-mode escape hatch) |
| `:tnew` | Alias of `:terminal` (vim parity) |
| `:tabterminal` | Open terminal in a new tab directly |

The `:bd` / `:bd!` / `:bnext` / `:bprev` / `:b` commands
operate on terminal buffers without modification.

## 13. Plugin surface (Phase 7)

Out of scope for the v1 terminal mode but designed for:

```wit
// crates/lattice-plugin-host/wit/terminal.wit (forthcoming)
interface terminal {
    record spawn-request {
        argv: list<string>,
        cwd: option<string>,
        env: list<tuple<string, string>>,
        label: option<string>,
    }
    record spawn-handle {
        id: u32,
        writer: writer-handle,
    }

    spawn: func(req: spawn-request) -> result<spawn-handle, string>
    write: func(id: u32, bytes: list<u8>) -> result<_, string>
    subscribe-output: func(id: u32) -> stream<list<u8>>
}
```

Use cases that drive the WIT shape:
- REPL plugins (`*scratch:rust*`, Python repl, sql repl) —
  spawn their own preferred process under a
  `BufferKind::Terminal`.
- Test runner plugins — pipe `cargo test` / `pytest` into a
  labeled terminal.
- DAP integration (Phase 7+) — the debugger's REPL goes through
  this surface.

## 14. Performance posture

Bench targets (`benches/terminal.rs`):

- `parse_burst_1mb` — 1 MB of `cat /tmp/big.log` output → ≤ 8 ms
  parse (alacritty_terminal upstream benchmark; we just stay
  within its envelope).
- `snapshot_build_200x60` — assemble a snapshot for a 200×60
  grid → ≤ 200 µs.
- `key_encode` — single keystroke encode → ≤ 200 ns (in-loop
  allocation via small thread-local buffer).

CI gate (added to `lattice-terminal/benches/terminal.rs`).

## 15. Testing strategy

| Layer | Tests |
|---|---|
| Encoder | round-trip table tests for every chord variant + mode permutation |
| State machine | feed canned escape-sequence fixtures, assert snapshot |
| PTY lifecycle | spawn `/bin/echo hi`, drain output, assert exit_status |
| Modal transitions | property test: any sequence of Insert / Normal-in-terminal toggles is internally consistent |
| Scrollback | bounded ring discards oldest correctly under load |

Integration tests in `lattice-host` exercise `:terminal`
spawn, `<C-s>` opening a terminal in a split, `<C-w>T`
moving to new tab.

## 16. Open questions

| Q | Status |
|---|---|
| sixel / kitty graphics protocol support | deferred; future libghostty swap |
| Live shell reload (`:source` style) | n/a — the shell owns its own dotfile state |
| Per-terminal env var overrides (`:terminal --env=...`) | deferred; T4 polish if requested |
| Terminal buffer save-and-restore across sessions | tied to §15:27 (session save / restore) — deferred per design |

## 17. Slice plan summary

See [`../operations/terminal-mode-plan.md`](../operations/terminal-mode-plan.md)
for the full slice-by-slice breakdown.

- **T1** — Core PTY + buffer kind + render (no input yet)
- **T2** — Input + Terminal-Insert mode
- **T3** — Scrollback + nav + copy
- **T4** — Polish (resize / exit UX / options)
- **T5** — Plugin surface (Phase 7+)
