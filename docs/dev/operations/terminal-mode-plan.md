# Terminal Mode — implementation plan

Slice-level breakdown for the terminal-mode feature. The
authoritative design + architecture is in
[`../architecture/terminal-mode.md`](../architecture/terminal-mode.md);
this doc is the implementation roadmap with concrete sub-tasks,
acceptance gates, and tracker entries per slice.

User-facing help: [`../../user/terminal.md`](../../user/terminal.md).

---

## Substrate confirmation

- **VT parser + grid:** `alacritty_terminal` ≥ 0.24 (pinned).
- **PTY:** `portable-pty` ≥ 0.8.
- **No FFI; no extra toolchain.** Pure Rust workspace addition.
- **Future libghostty swap** preserved via a thin trait
  abstraction (deferred design — only if sixel / kitty
  graphics demand surfaces).

## Slice T1 — Core PTY + buffer kind + render

**Goal:** A `:terminal` command spawns a shell, the buffer
renders its output, no interaction yet (typing doesn't go to
the PTY — that's T2). The user can OBSERVE a shell prompt and
any noise the shell emits at startup.

### Sub-tasks

1. **New crate** `crates/lattice-terminal/` added to the workspace.
   - `Cargo.toml` deps: `alacritty_terminal`, `portable-pty`,
     `tokio`, `arc-swap`, `parking_lot`, `lattice-core`,
     `lattice-runtime`, `tracing`.
   - Lints workspace inherited.
2. **`BufferKind::Terminal`** added to `lattice-core::buffers::BufferKind`.
   - Match-exhaustiveness fixes ripple through every existing
     `match BufferKind { ... }` site (similar to the
     `BufferKind::Oil` extension; expect 20-30 arms).
3. **`BufferData::Terminal(TerminalEntry)`** added to
   `lattice-host::buffer_registry::BufferData`.
   - `TerminalEntry { id, pty, state, scrollback, cwd, label,
     exit_status, created_at }`.
   - `buffer_registry.rs` gains `with_terminal` / `with_terminal_mut`
     accessors mirroring the existing `with_document` /
     `with_oil` family.
4. **`PtyHandle`** in `lattice-terminal::handle`.
   - `pub fn write(&self, bytes: &[u8]) -> io::Result<()>`
   - `pub fn resize(&self, rows: u16, cols: u16) -> io::Result<()>`
   - `pub fn kill(&self) -> io::Result<()>` (sends SIGKILL)
5. **Spawn** in `lattice-terminal::spawner`.
   - `spawn(cmd: &str, args: &[String], cwd: Option<&Path>) -> io::Result<(PtyHandle, ReaderTaskHandle)>`.
   - Internally: `native_pty_system().openpty`, `CommandBuilder`, fork.
6. **Reader task** in `lattice-terminal::reader`.
   - Spawned tokio task per terminal.
   - Owns `Term<EventProxy>` from alacritty_terminal.
   - Loop: read raw bytes (32 KB buf) → feed `vte::Parser` →
     `Term::advance` → throttle (16 ms window) → build
     `TerminalSnapshot` → `ArcSwap::store`.
   - On EOF: write final exit_status into TerminalEntry,
     publish final snapshot.
7. **`TerminalSnapshot`** in `lattice-terminal::snapshot`.
   - Per the architecture doc § 7 shape.
8. **Ex command `:terminal`** in `lattice-grammar::ex_commands`.
   - Registered as `ex:terminal`.
   - Optional `String` arg = command line to spawn.
   - Effect: `Effect::AppAction(AppEffect::TerminalSpawn { cmd: Option<String> })`.
9. **AppEffect + Action + ActionIds** for `TerminalSpawn`.
   - Same four-layer surface (Action / AppEffect / ActionIds /
     named CommandId `action:terminal-spawn`) as the existing
     `Tab*`, `Picker*` actions for plugin parity.
10. **Editor handler** `do_terminal_spawn(&mut self, cmd: Option<String>)`.
    - Resolves shell from `terminal.shell` option (default
      `$SHELL` then `/bin/sh`).
    - Resolves cwd from `terminal.cwd` option (`document` →
      parent of editor.document.path; `cwd` → process cwd).
    - Calls `lattice_terminal::spawn(...)`.
    - Inserts `BufferEntry { kind: Terminal, data: Terminal(entry) }`
      into the buffer registry.
    - Activates the new buffer in the active pane (or per
      `terminal.display` preference).
11. **TUI renderer** — paint terminal buffer as a grid.
    - New pane-render provider for `BufferKind::Terminal` (uses
      the M.4 pane-render registry pattern).
    - Reads `terminal_entry.state.load()` → translates cells
      to `ratatui::Style` spans.
12. **GPUI renderer** — same.
    - New paint branch in `window.rs` for the Terminal kind.
    - Each cell becomes a styled `div().child(c)` in a flex row.
13. **`Terminal` option group + `terminal.shell` / `terminal.cwd` / `terminal.display` typed options**
    - Macro-registered via `options!`.
    - Validators for the enum members.

### Acceptance gates (T1)

- `cargo check --workspace` clean.
- `cargo test --workspace` green (existing 1900+ tests untouched).
- `lattice some_file.rs` then `:terminal` opens a buffer titled
  `[shell]`; the user sees a shell prompt.
- Output from background commands (e.g. `:terminal sleep 1 && echo
  done`) appears asynchronously.
- `:ls` lists the terminal buffer alongside documents.
- `:bd!` on the terminal kills the child and closes the buffer.

### Out of scope for T1

- Typing into the PTY (T2).
- Scrollback navigation (T3).
- Resize handling (T4 polish).
- Mouse passthrough (T4).
- Process-exit UX (T4).

---

## Slice T2 — Input + Terminal-Insert mode

**Goal:** User can type into the shell. Two sub-states wired:
Normal-in-terminal (vim grammar in scrollback view) and
Terminal-Insert (keystrokes encoded → PTY).

### Sub-tasks

1. **Modal sub-state** in `lattice-core::ModalState`.
   - Either add a new `ModalState::TerminalInsert` variant, OR
     model as a minor mode (`TerminalInsertMode`) layered on
     Normal. Lean toward minor-mode (matches existing
     `HoverMode` / `CompletionPopupMode` precedent — minor
     modes are the substrate for buffer-attached transient
     overlays).
2. **Input translation** in `lattice-host::input::translate`.
   - Branch on `active_buffer == Terminal`.
   - Sub-branch: `if minor_mode == TerminalInsert` → encode
     keystroke to ANSI bytes; produce
     `Action::TerminalInput(bytes)`.
   - Else (Normal-in-terminal): standard motion / operator
     translation (the existing vim grammar dispatches against
     the scrollback view).
3. **`encode::key_to_ansi`** in `lattice-terminal::encode`.
   - Full mapping table per architecture doc § 9.
   - Mode-aware (DECPAM / DECCKM).
   - Returns `Vec<u8>`.
4. **`Action::TerminalInput(Vec<u8>)`** + handler.
   - `editor.do_terminal_input(bytes)`: looks up active
     terminal entry, calls `pty.write(&bytes)`.
5. **Mode entry/exit chords**.
   - `i` / `a` / `I` / `A` (in Normal-in-terminal) → enter
     `TerminalInsert` minor mode.
   - `<C-\><C-n>` (in TerminalInsert) → exit minor mode.
   - `<Esc>` (if `terminal.esc_exits`) → exit.
6. **`<C-w>` exemption**.
   - In TerminalInsert: `<C-w>` is INSIDE the encoder path
     (becomes `\x17`, passed to shell — shell's WERASE).
   - In Normal-in-terminal: `<C-w>` is the window-motion
     prefix (existing chord trie path).
   - The translate-layer's branch is the seam.
7. **Mode-line indicator**.
   - `-- TERMINAL --` for Normal-in-terminal.
   - `-- TERMINAL-INSERT --` for TerminalInsert.

### Acceptance gates (T2)

- `:terminal`, `i`, then typing produces shell output.
- `<Esc>` → cursor stays in same pane, mode-line shows
  `-- TERMINAL --`, vim motions work in the scrollback view.
- `<C-\><C-n>` works as alternative escape from
  TerminalInsert.
- `<C-w>` in TerminalInsert deletes the previous shell-word
  (bash/zsh's WERASE).
- `<C-w>j` in Normal-in-terminal navigates panes.
- Running `htop` or any program that uses arrow keys works
  (cursor-key mode handled).
- `<C-c>` in TerminalInsert sends SIGINT (kills running
  process; doesn't quit Lattice).

---

## Slice T3 — Scrollback + nav + copy

**Goal:** Read history without reaching for the mouse.

### Sub-tasks

1. **Scrollback view exposed**.
   - `TerminalEntry::scrollback_view()` returns `ScrollbackView`
     with `total_rows` and `viewport_row`.
2. **Vim motion adapter** for the scrollback grid.
   - `j` / `k` adjust `viewport_row`.
   - `gg` → top of scrollback. `G` → bottom (live).
   - `/` / `?` search across cells (regex on UTF-8 cell text).
   - `n` / `N` next / previous match.
3. **Visual mode** over cells.
   - Charwise / linewise / blockwise selection inside the cell
     grid.
   - `y` copies selected cells as text to the unnamed register.
   - The shell's selection model isn't exposed — we copy
     OUR cell text.
4. **`<C-o>` / `<C-i>`** jump list integration.
   - Cross-buffer jumps that land in a Terminal buffer set
     `viewport_row` to the recorded position.

### Acceptance gates (T3)

- `:terminal`, scroll a long log into the buffer, exit to
  Normal-in-terminal, `/pattern<CR>` finds matches.
- `yy` in Visual mode copies a row's text; `:reg` shows it.
- `gg` / `G` work as expected.

---

## Slice T4 — Polish

**Goal:** Production-ready UX.

### Sub-tasks

1. **Resize handling**.
   - On pane geometry change: `PtyHandle::resize(rows, cols)`.
   - alacritty_terminal handles reflow internally.
2. **Process exit UX**.
   - `terminal.exit_on_process_exit` honored.
   - When `false`: append `[Process exited with code N]` line
     to the snapshot.
3. **Configurable options** all wired and validated.
   - `terminal.scrollback_lines` change → resize the
     scrollback ring.
   - `terminal.refresh_hz` change → resize the throttle window.
4. **Mouse passthrough**.
   - When the program enables a mouse mode and
     `terminal.mouse_passthrough != "off"`, mouse events encode
     to xterm mouse protocol bytes → PTY.
5. **`<C-w>T`** — move current terminal to new tab.
6. **`:tabterminal`** — open terminal in new tab directly.
7. **`:bd!` confirms** the child is killed before the buffer
   is removed.

### Acceptance gates (T4)

- Resizing the Lattice window resizes the embedded terminal
  child's reported size (verifiable via `stty size` inside the
  terminal).
- Mouse click in `htop` selects rows (via passthrough).
- `terminal.scrollback_lines = 50000` then heavy output: ring
  bounded; oldest rows drop.

---

## Slice T5 — Plugin surface (deferred to Phase 7)

**Goal:** Plugins can spawn their own terminals.

WIT shape sketched in the architecture doc § 13. Implementation
gates on the Phase 7 plugin-host work. Tracked here so the
v1 terminal mode lays compatible foundations.

---

## Cross-slice concerns

### Performance gates

- Per-keystroke encode latency: ≤ 200 ns p99 (criterion
  bench `key_encode`).
- 1 MB output parse: ≤ 8 ms (criterion bench `parse_burst_1mb`).
- Snapshot build for 200×60 grid: ≤ 200 µs.
- Per-frame publish: ≤ 16 ms (already inside the 60 Hz
  throttle window).

Gates added to `lattice-terminal/benches/terminal.rs` and
checked in CI.

### Test gates

Per the design doc § 15:

- Encoder round-trip table tests.
- State machine fixture tests (canned escape sequences).
- PTY lifecycle test (`echo hi`).
- Modal-transition property test.
- Scrollback ring bound test.

Integration tests in `lattice-host`:
- `:terminal` spawn.
- `<C-s>` on file-picker landing into a split alongside a
  terminal.
- `<C-w>T` moving terminal to new tab.
- `:bd!` killing child.

### Documentation deliverables

- `docs/dev/architecture/terminal-mode.md` — ✅ (this slice
  preparation work).
- `docs/dev/operations/terminal-mode-plan.md` — ✅ (this doc).
- `docs/user/terminal.md` — ✅ (companion).
- `docs/dev/architecture/design.md` §5.B — added once T1 ships
  (terse canonical text mirroring the architecture doc).
- `docs/dev/operations/implementation.md` Phase row — added
  once T1 ships (new "Terminal" row).
- `docs/dev/operations/benchmarks.md` — extended with the
  terminal bench results after T4.

### Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| alacritty_terminal API churn | Medium | Pin to a known-good version; thin wrapper crate so upgrades happen in one file. |
| portable-pty Windows ConPTY quirks | Medium | Defer Windows polish to T4; Unix-first development. |
| Output flood DoS (cat /dev/urandom) | Low | Render throttling + scrollback bound; PTY's OS buffer back-pressures the child. |
| Mouse passthrough conflicts with picker / Lattice mouse handlers | Low | `terminal.mouse_passthrough = off` default-conservative until tested. |
| Concurrent terminals × LSP × tree-sitter exhaust CPU | Low | Each terminal is its own task; tokio multi-thread scales; if it bites, add a `terminal.max_concurrent` cap. |

---

## Tracker

Each slice gets its own commit (or small commit chain). Track
progress here:

- [ ] T1 — Core PTY + buffer kind + render
- [ ] T2 — Input + Terminal-Insert mode
- [ ] T3 — Scrollback + nav + copy
- [ ] T4 — Polish
- [ ] T5 — Plugin surface (Phase 7+)

Mark as `[X]` and commit hash once landed.
