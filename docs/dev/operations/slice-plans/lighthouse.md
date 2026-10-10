# Lighthouse — slice plan

> **Slice plan.** Sequencing, slice IDs, dependencies, status icons.
> Design contract: [`../../architecture/lighthouse.md`](../../architecture/lighthouse.md).
> Follows Phase 8b (core plugins); sequenced AFTER the trivial-first core plugin
> (`auto-pair`) that de-risked the packaging/load pipeline.

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred · ❌ dropped.
Every non-trivial slice ships the four artefacts (doc + bench-where-perf-relevant
+ test incl. failure modes + graceful error handling).

**Status: 🚧 LH.0 ✅ (four job/registration seams + output buffers); LH.1 next.** Re-planned 2026-10-10 against the current
host: the seams are request → addressed-event (design §3.0), the progress buffer
is the plugin's, and lighthouse is a **core plugin** (`plugins/lighthouse/`).

## Sequencing

**LH.0 → LH.1 → LH.2.** The host-services extension (LH.0) is the blocker — the
plugin (LH.1) cannot download, unpack, spawn, or register a server without it.
LH.0 is **general** plugin-host surface (every future networked/subprocess plugin
uses it), so it lives in `lattice-plugin-host`, not the lighthouse crate.

```
LH.0 host seams ───────► LH.1 lighthouse plugin ──► LH.2 core-plugin staging
 (download / extract /      (registry + install +      (ships out of the box)
  spawn / register-server)   *lsp-install:* buffer +
                             :lsp-servers view)
```

Within LH.0 the four seams are independent of each other; the order below is the
order lighthouse's install path needs them.

## Slices

### LH.0 — host-services extension (the prerequisite)

General host capabilities, capability re-checked host-side at each (the
`walk_within_grant` precedent). Every long-running one returns an id and reports
through events addressed to the requesting plugin (design §3.0).

#### LH.0.1 — host jobs + `http-download` (net:http)  ✅
The job substrate (`job.rs`): `PendingJob` → `JobGuard`, a process-wide table
for cancel-by-id, coalesced progress; `Event::{JobProgress, JobFinished}`
mirrored as the `job-progress` / `job-finished` arms in `types.wit` and
addressed in `event_task.rs` exactly as `FilesChanged` is; `cancel-job(id)`.
On it, `http-download(url, sha256, dest) -> result<u64, string>`: the host
streams to `<dest>.part` on the job's thread, hashing as it goes; only a SHA
match renames into place. Gates: URL host (and every redirect hop) ∈
`net:http:<host>`; `dest` within `fs:write`. Policy: https (http to loopback
only), bounded redirects / size / time. **Exit:** a plugin with both grants
downloads a file and hears `job-finished` **without a keypress**; a wrong SHA, a
cancel and a size overrun each leave no file; an ungranted host, an ungranted
redirect hop and an ungranted `dest` are each refused by name; another plugin
subscribed to the same kinds hears nothing.

**Landed.** `job.rs` (7 unit tests), `download_host.rs` (22, against a loopback
server), `tests/download_seam.rs` (5, through the events fixture guest). What
the plan did not have:

- **Generic job events, not `download-*` arms.** Written per-seam first, then
  changed before commit: every arm added to `event` is an ABI break, so
  per-seam arms would cost a generation per seam. LH.0.2 / LH.0.3 now add no
  arms for progress or completion.
- **The plugin API moved to `0.2.0`** — adding arms to `event` breaks every
  guest that matches on it. `cargo xtask bump-plugin-api` had missed the WIT
  embedded in Rust source (the scaffold templates, one fixture) and now covers
  it. Publishing the three crates and bumping `lattice-org-plugin`'s pin is
  **LH.3**, held to the end on purpose.
- **Cancel-by-id is process-wide, scoped by plugin id** — the instance that
  cancels is not the one that started the job.
- **A job requested inside `register-events` is held** until the subscriptions
  are wired. It has a test only because the fixture lingers in
  `register-events`; without that the race is never lost and a host with no
  hold passes (it did, the first time).
- Not done: a stall timeout (the client offers only a total, set at 30 min), so
  a cancel cannot interrupt a connection that has gone silent mid-read.

#### LH.0.2 — `extract-archive` + `set-executable`  ✅
Host-side unpack of a downloaded archive into an `fs:write`-granted
destination, as a host job: `gz` (single file) and `tar.gz`. Entries escaping
the destination (`..`, absolute, symlink out, written through a symlink) fail
the job; the executable bit is preserved and no other mode bit is. **Exit:**
each format unpacks; a traversal entry aborts with nothing written outside the
destination and nothing partial inside it.

**Landed.** `extract_host.rs` (21 unit tests, archives built in-test including
hand-written hostile headers) + `tests/extract_seam.rs` (3, through the fixture
guest). Decided at slice start, as planned: extraction is its **own call**, not
a mode of `http-download`, so "downloaded and verified" stays observable alone.
Added beyond the plan: **`set-executable`** — a guest has no `chmod`, and a bare
`.gz` (rust-analyzer's format) or a raw binary carries no mode. New dependency:
`tar` (default features off); `flate2` was already in the lock file.

#### LH.0.2b — `zip`  ⛔
Deferred until a registry entry needs it: a Windows build of any server, or a
zip-only one (clangd). A second, heavier dependency for no server in the first
registry on the platforms lattice builds for. Additive when it lands — a new
case on `archive-format`, which **is** an ABI change to that enum, so batch it
with the next generation.

#### LH.0.3 — `spawn-process` (proc:spawn)  ✅
`spawn-process(command, args, cwd) -> result<u64, string>` gated on `proc:spawn`
(**bundled-only** — `capability.rs` already withholds it from `UserInstalled`);
a host job whose output arrives as the `job-output` arm (batched lines, stdout
and stderr interleaved) and whose exit is `job-finished`. **Exit:** a bundled
plugin spawns a subprocess and hears its output and exit; a user-installed
plugin is denied; a non-zero exit is an ordinary outcome, never a panic.

**Landed.** `process_host.rs` (10 unit tests) + `tests/spawn_seam.rs` (3,
through the fixture guest at both trust tiers). `job-output` is the one new
`event` arm — inside the unpublished 0.2.0, so no further bump. No shell: argv
is passed as given. A cancel signals the child's **process group**; the first
version of that test passed with the group kill removed (the job ends promptly
either way), so it now asserts the grandchild is gone. `lattice-compilation`'s
runner was checked and not reused: it is `:compile`'s own (shell cmdline,
error parsing, `unsafe` libc), not a plain "run argv, stream lines".

#### LH.0.4 — `register-server` / `unregister-server`  ✅
A `server-config` WIT record mirroring `lattice_lsp::config::ServerConfig`;
`register-server` adds it to the running `LspSupervisor`, `unregister-server`
reverses it, and so does the plugin instance going away. **Exit:** register → a
matching buffer open runs that server → unregister → it does not.

**Landed.** Three layers, each tested where it lives:

- `lattice-lsp`: the supervisor could only be configured **before** it was
  spawned — the handle exposed no way to add a config. It now keeps boot-time
  configs and runtime registrations apart (`RegisterConfig` / `UnregisterConfig`
  commands, fire-and-forget). 7 tests, one of which runs a marker script as the
  "server" to prove the registered binary is the one reached for.
- `lattice-mode`: `LanguageServerRegistrar` + `LanguageServerSpec`, the answer
  to the question this slice opened with. `lattice-lsp` implements and registers
  it from its own `install`; **`lattice-host` is untouched**.
- `lattice-plugin-host`: the seam, gated on **`proc:spawn`** (not an LSP
  capability — registering a command is spawning it one buffer-open later).
  `tests/register_server_seam.rs`, 4 tests against a recording registrar, both
  trust tiers. `WiredSeams::language_servers` pins the boot order.

Semantics settled here: a registration **shadows** same-`id` configs rather than
adding beside them; the newest registration for an id wins; nothing is started
or restarted by registering.

#### LH.0.5 — plugin output buffers (`output-append` / `-status` / `-reset`)  ✅
Carved at the start of LH.1, when its "verify at slice start" check failed: an
events handler returns nothing, so a plugin could open a synthetic buffer and
never write to it. Design §3.5. **Exit:** a line a plugin writes shows in a
`plugin-output-mode` buffer of that name with no keypress, whether written
before or after the buffer was opened.

**Landed.** The native streaming-buffer shape (`*compilation*`, `*messages*`,
the LSP logs), with the plugin given the producer's end:

- `lattice-plugin-host::output` — the store (per-name ring + status), the typed
  `PluginOutputPushed` event, and `Tail`, the seed/tail join. 16 unit tests,
  including the bounds and the reload case.
- `host-services` — three functions and one enum, additive inside 0.2.0. No
  capability. `tests/output_seam.rs`: 4 tests with a real guest (lines, status
  and reset cross in order; another plugin's buffer, a malformed name and an
  unwired host are each a named `err`).
- `lattice-plugin-trace` — `plugin-output-mode` beside the trace view: drain,
  headerline, read-only. 6 unit tests on the headerline and the batch fold.
- `lattice-plugin-loader` — builds the store, binds its publisher to the bus,
  drops a plugin's buffers on unload; `WiredSeams::plugin_output` pins the
  wiring.
- `lattice-host/tests/plugin_output_view.rs` — 7 tests on a booted editor, none
  of which presses a key before asserting. **Seen red:** with the wake removed,
  the append and status-only tests fail; with `read-only-mode` un-implied, `x`
  edits the log.

No bench: the write path is a ring push and a channel send per call, off the
keystroke path, and the per-call WASM overhead is already ratcheted.

### LH.1 — the lighthouse plugin  📝
The core WASM Component plugin consuming LH.0. Crate `plugins/lighthouse/`.

#### LH.1.1 — crate scaffold + registry + install core  📝
The guest crate (`wasm32-wasip2`, `plugin.toml` requesting
`net:http:<registry-hosts>` + `proc:spawn` + `fs:write:<managed-tree>`); a
compiled-in `registry.toml` (per server × platform: pinned version, URL,
SHA-256, archive kind + binary path, or a package-manager `recipe`); the
download → extract → lay down
`${XDG_DATA_HOME}/lattice/lsp/<name>/<version>/` state machine, driven by
events. **Exit:** given a registry entry, the core produces a versioned install
tree; a tampered SHA ends in a reported failure with no partial tree.

#### LH.1.2 — commands, the `*lsp-install:<server>*` buffer, registration  📝
`:lsp-install` / `:lsp-update` / `:lsp-update-all` / `:lsp-uninstall`; each
returns at once. The plugin opens `*lsp-install:<server>*`
(`effect.open-synthetic-buffer`, mode `plugin-output-mode`), writes a line per
event with `output-append`, and keeps status in the headerline with
`output-status`; a failure's message lands in the buffer. On success,
`register-server` a config whose `command` is the managed binary. Update is
install-new → verify → flip registration → GC old. **Exit:** `:lsp-install rust-analyzer` on a machine
without it → progress is visible live with no keypress, and a `.rs` buffer then
gets diagnostics with no `PATH` entry; `:lsp-uninstall` reverses it.

#### LH.1.3 — the `:lsp-servers` manager view  📝
A read-only buffer listing every registry server, its installed version and
health; in-view chords (install / update / uninstall the row) on the plugin's
own mode. **Exit:** `:lsp-servers` lists the registry and live-updates as an
install completes.

### LH.2 — core-plugin staging  📝
Add lighthouse to `cargo xtask build-core-plugins` so it is discovered at boot as
`TrustTier::Bundled` (the PM.1–PM.4 pipeline `auto-pair` ships through — no
`include_bytes!`). **Exit:** a fresh editor has lighthouse loaded (`:plugins`
shows it, `:lsp-servers` works) with no user install step.

### LH.3 — publish plugin API 0.2.0  📝
Publish `lattice-wit`, `lattice-plugin-sdk` and `lattice-plugin-sdk-derive` at
0.2.0 and bump `lattice-org-plugin`'s pin, per `releasing.md`. **Last, and only
once LH.1 and LH.2 are done** (decided 2026-10-10): 0.2.0 is unpublished, so
every WIT change lighthouse turns out to need lands inside it for free — LH.0.5
already did — where each one after publication is another version. Outward-facing
and irreversible; done with Dhruva, not by an agent alone. Until it lands,
`lattice-org-plugin` does not instantiate against this branch.

## Notes

- **Dropped from the July plan:** ❌ the `start-task` / `push-output` /
  `finalize` task surface — superseded by event-delivered output into a
  plugin-owned buffer (design §3.3). ❌ blocking `http-fetch -> list<u8>` —
  superseded by `http-download` (design §3.0).
- **Deferred:** a general `:plugin-install` reuses LH.0.1 + LH.0.2 — lighthouse
  proves the shape, so the plugin-manager slice is a thin follow-on.
- **Cross-renderer:** the progress buffer and the manager view are Documents
  (renderer-agnostic); no per-renderer work beyond the buffer substrate.
