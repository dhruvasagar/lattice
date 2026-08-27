# Slice plan — Pi agent integration (PI)

**Design home:** [`../../../../architecture/pi.md`](../../../../architecture/pi.md).
That fragment owns *what* and *why*; this file owns *when* and *in what order*.
Authoritative per-slice status lives in
[`../../implementation.md`](../../implementation.md).

> **Status: ✅ COMPLETE (verified against source 2026-08-22).** Both
> slices were already implemented; the icons read `📝` until this audit,
> which is exactly the drift the archiving rule warns about — the plan
> said "not started" while `crates/lattice-ai/src/pi/` had been shipping
> `:pi` for some time. Verified by reading the source, not the icons:
> `pi/{mod,commands,modes}.rs` exist, `:pi` registers an ex-command
> returning `SpawnTerminal { cmd_line: "pi", activate_minor: "pi-mode" }`,
> `crate::pi::install(boot)` is wired in `lattice-ai/src/install.rs:39`,
> and all three tests pass (`pi_spawns_terminal_and_activates_mode`,
> `id_kind_and_policy`, `registers_without_conflict`).

## Why

Pi (pi.dev) is a minimal, extensible coding-agent harness by Earendil Inc.,
trending on Hacker News and rapidly adopted. Lattice users who reach for Pi
should be able to launch it with `:pi` and have it open in a terminal buffer
with the same first-class treatment as `:claude` and `:opencode` — a marker
minor mode, buffer identification, and the seam for future deeper integration.

The build order starts with the pure module + test slice (PI.1), then wires it
into the crate root (PI.2) — each slice landing green on its own.

## Slices

### PI.1 — pi module (commands + modes + install + tests)  ✅

Create `crates/lattice-ai/src/pi/` with three files, mirroring `opencode/`:

| File | Contents |
|---|---|
| `mod.rs` | Module root, `pub mod commands; pub mod modes;`, `install(boot)` fn, `pub use modes::PiMode` |
| `commands.rs` | `register_pi_ex_commands(registry)` registering `:pi` → `Effect::SpawnTerminal { cmd_line: Some("pi"), env: vec![], activate_minor: Some("pi-mode") }` |
| `modes.rs` | `PiMode` (Manual minor, no guard, no keymap), `register_pi_modes(registry)` |

Tests in `commands.rs` verify the effect shape (SpawnTerminal with correct
cmd_line + activate_minor). Tests in `modes.rs` verify the mode identity data
(`pi-mode` string, Minor kind, Manual policy), plus round-trip through the
ModeRegistry.

- *paramount:* #2 (same seam pattern as opencode-mode for future RPC adapter);
  #3 (ex-command and mode, standard vim grammar on the terminal buffer).
- *test:* command returns SpawnTerminal / cmd_line "pi" / activate_minor
  "pi-mode"; mode id, kind, and policy; registry round-trip.
- *doc:* design §3, §4 (landed with this plan).
- *error handling:* same as opencode — no args accepted, `parse_no_args`
  rejects non-empty input. If `pi` binary is missing, the PTY spawn fails and
  the terminal reports the error; no new error path in lattice.

### PI.2 — wiring into crate install  ✅

One line in `crates/lattice-ai/src/install.rs`:

```rust
crate::pi::install(boot);
```

Placed adjacent to the opencode install (before the feature-gated ACP/MCP
sections), since pi is always-compiled like opencode. No host changes needed —
`Effect::SpawnTerminal` dispatch is generic and already handles
`activate_minor`.

- *paramount:* #4 (single async-safe line in the crate boot; zero host changes).
- *test:* existing host tests for SpawnTerminal dispatch verify the generic
  path; no new host test needed.
- *doc:* design §5.3 (wiring), calls out the adjacent-line placement.
- *error handling:* module-level — if register_pi_ex_commands fails (name
  conflict), the expect in `mod.rs::install` panics at boot, same as all
  other command registrations.

## Future slices (deferred)

| Slice | Contents | Depends on |
|---|---|---|
| PI.3 | RPC adapter (`pi --mode rpc`) — spawn with JSON-LR protocol, conversation buffer, streaming, tool display, extension UI routing | ACP adapter pattern (landed), headerline (landed) |
| PI.4 | Pi integration tests — end-to-end with a mock `pi` binary (socat echo pair) | Terminal test infrastructure |
