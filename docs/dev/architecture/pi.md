# Pi — the minimal agent harness, in a terminal buffer

Pi (pi.dev) is a minimal, extensible coding-agent harness by Earendil Inc.,
distributed as an npm package (`@earendil-works/pi-coding-agent`) and
installable via `curl -fsSL https://pi.dev/install.sh | sh`. Lattice drives
Pi's native TUI in a terminal buffer with a `pi-mode` marker minor — the same
**terminal topology** as `:opencode` and `:claude`.

Slice plan (the *when* / *in what order*):
[`../operations/slice-plans/pi.md`](../operations/slice-plans/pi.md).
This fragment owns *what* and *why*.

## 1. Why Pi

Pi is structurally similar to opencode — it is a terminal-native agent with
its own TUI (readline, `/` commands, model switching, history, session tree,
extension system). Lattice's job is one thing: host the terminal and let the
agent's UI shine. Reimplementing any of that makes lattice a worse Pi client,
not a better editor.

Pi differs from opencode in one meaningful way: it exposes a **JSON-RPC mode**
(`pi --mode rpc`) with a full command/event protocol over stdin/stdout. This
is the same shape as opencode's ACP adapter, and it opens the door to the
same IDE-native conversation buffer (prompt input, streaming display, diff
review). The v1 integration uses the terminal topology; the RPC adapter is
future work noted in §6.

## 2. Shape

```
           lattice-ai/src/pi/           (new module, no feature gate)
   ┌──────────────────────────────────────────────────┐
   │  register_pi_ex_commands(boot.commands_mut())      │
   │    :pi  ──► Effect::SpawnTerminal {                │
   │                cmd_line: "pi",                      │
   │                env: [],                             │
   │                activate_minor: "pi-mode"             │
   │            }                                        │
   │                                                     │
   │  register_pi_modes(boot.modes_mut())                │
   │    PiMode (minor, Manual)                           │
   └──────────────────────────────────────────────────┘
            │
            ▼
   crate::pip::install(boot)   ← one line in crate::install
```

The module is always compiled (no cargo feature gate) — a terminal spawn plus
a marker minor mode carries negligible weight, and making it unconditional
means `:pi` is available regardless of the `acp`/`mcp` transport features, the
same policy as `:opencode`.

## 3. Ex-command: `:pi`

Registered in `lattice-ai::pi::commands`:

| Field | Value |
|---|---|
| Name | `pi` |
| Latency class | `Reflex` (no I/O before the effect) |
| `accepts_bang` | `false` |
| `accepts_range` | `false` |
| `parse_args` | `parse_no_args` (no arguments) |
| `apply` | Returns `Effect::SpawnTerminal { cmd_line: Some("pi"), env: vec![], activate_minor: Some("pi-mode") }` |

### Why `parse_no_args`

Pi has its own `/` commands and config surface. Passing arguments through the
ex-command line would duplicate that surface and require a separate parser.
The v1 contract is: `:pi` launches the agent, and all subsequent interaction
goes through the agent's own TUI.

The dash-separated form `:pi` (one word, no hyphen) follows the precedent
set by `:opencode` and `:claude` — the agent's natural CLI name.

## 4. `pi-mode` — the minor mode

A Manual-activation marker minor over `terminal-mode`, exactly matching
`opencode-mode` and `claude-code-mode`:

| Property | Value |
|---|---|
| id | `pi-mode` |
| kind | `Minor` |
| activation policy | `Manual` |
| guard | `()` (no per-buffer resources) |
| keymap | `Keymap::default()` (none yet) |
| `on_activate` | No-op (returns `Ok(())`) |

The mode exists as the buffer's *identity* — it lets `:ls`, `:b pi`,
`modeline` logic, and buffer predicates distinguish "this is a Pi terminal"
without `BufferKind` branching. It is also the **seam** for future
lattice-native integration:

- RPC conversation buffer (§6) would register per-session subscriptions here.
- A headerline status row showing the model / thinking level / context usage
  would be contributed by the mode's lifecycle.

## 5. Wiring

### 5.1 Module structure

```
crates/lattice-ai/src/pi/
  mod.rs           — module root, pub mod, install(boot)
  commands.rs      — register_pi_ex_commands, tests
  modes.rs         — PiMode, register_pi_modes, tests
```

### 5.2 `crate::pi::install(boot)`

```rust
pub fn install(boot: &mut impl SubsystemBoot) {
    commands::register_pi_ex_commands(boot.commands_mut());
    modes::register_pi_modes(boot.modes_mut());
}
```

### 5.3 Crate-root wiring (one line)

In `crates/lattice-ai/src/install.rs`, alongside the opencode install:

```rust
pub fn install(boot: &mut impl SubsystemBoot) {
    let logger = install_ai_log(boot);

    crate::opencode::install(boot);   // :opencode
    crate::pi::install(boot);         // :pi         <-- added

    #[cfg(feature = "acp")]
    crate::acp::install::install(boot, &logger);

    #[cfg(feature = "mcp")]
    crate::mcp::install::install(boot);

    boot.register_service::<AiLogger>(logger);
}
```

No host changes needed (`editor_boot.rs`, `dispatch.rs`) — `Effect::SpawnTerminal`
dispatch is generic and already handles `activate_minor` by looking up the
mode in the `ModeRegistry`.

### 5.4 Install script resolution

Pi's install script writes a `pi` binary to the user's path. Lattice delegates
resolution to the terminal emulator's `$PATH` — `Effect::SpawnTerminal` with
`cmd_line: Some("pi")` shells out via the PTY, so `which pi` / `command -v pi`
is the shell's problem, not lattice's. This matches how `:claude` and
`:opencode` resolve.

## 6. Future: RPC adapter (RPC.1 — deferred)

Pi's `--mode rpc` exposes a JSON-LR protocol over stdin/stdout (§2 terminal
spawn, actually, but with JSONL framing rather than a raw TUI). This is the
same substrate as opencode's ACP adapter and would unlock:

- A conversation buffer (lattice-owned, like `:opencode-acp`).
- Streaming token display in the buffer.
- Extension UI protocol handling (confirm/select dialogs as lattice popups).
- Tool execution display and edit review via `EditorAccess`.
- Session tree browsing and forking as a lattice buffer.

The shape is nearly identical to the ACP adapter: spawn `pi --mode rpc`,
drive it with JSON-LR commands, consume event JSON-LR lines from stdout,
route extension UI requests to lattice's popup/picker infrastructure.

**Deferred because** (1) Pi's RPC protocol is richer than opencode's ACP and
the conversation buffer cost is real, (2) the terminal topology is immediately
useful with zero new UI work, and (3) the ACP adapter already exists as a
reference implementation when the time comes. The v1 `pi-mode` is the seam
this future adapter would hook into — the mode's `on_activate` would register
the per-buffer RPC session, and the mode's keymap would contribute
`:pi-send`, `:pi-abort`, `:pi-switch-model` chords.

## 7. Alignment with the paramount goals

- **#1 Performance.** Zero UI-thread work. `:pi` emits a `Effect::SpawnTerminal`
  and returns — no parsing, no I/O, no allocation beyond the effect payload.
  The terminal runs in its own task.
- **#2 Extensibility.** `pi-mode` is the same seam pattern as `opencode-mode`.
  A future RPC adapter lives in the same module, behind the same mode id.
- **#3 Modal editing.** The buffer's major mode is `terminal-mode` — all
  standard terminal vim grammar works (`i` to enter, `jj`/`<Esc>` to leave,
  buffer navigation, yank/put to system clipboard). `pi-mode` layers zero
  chords in v1.
- **#4 Asynchronicity.** `Effect::SpawnTerminal` dispatch is async-safe; the
  terminal is a PTY task, the mode activation is a `ModeRegistry` lookup.

## 8. Rejected alternatives

- **RPC-only (no terminal topology).** Rejected. The RPC adapter is real work
  and the terminal topology is free: one ex-command registration, one mode
  registration, one line in the crate install. Starting with the RPC adapter
  would mean shipping nothing while building a conversation UI we may not
  need (Pi's own TUI is excellent). Terminal-first, RPC-later.

- **Feature-gate behind `acp` or a new `pi` feature.** Rejected. The same
  reasoning as `:opencode`: a terminal spawn + marker mode is ~50 lines of
  code with zero dependencies beyond the ones already in `lattice-ai`. Adding
  a cargo feature creates a compile-time choice where none is needed.

- **Custom install/version detection (`which pi` check in the apply
  closure).** Rejected. The apply closure runs on the editor thread and must
  not do I/O. If `pi` is not installed, the PTY spawn fails and the terminal
  buffer reports the error — the same UX as `:claude` without a Claude install
  or `:opencode` without opencode. The user runs `curl -fsSL
  https://pi.dev/install.sh | sh` once and it works.

- **Ex-command arguments passed to pi CLI.** Rejected. Pi has its own `/`
  command surface, and forwarding arguments like `:pi --provider openai`
  would require lattice to know about pi's CLI flags — a coupling we avoid
  for all other terminal-hosted agents. The user configures pi via
  `~/.pi/config.json` or feeds it `/` commands once the terminal is open.
