# Lighthouse — slice plan

> **Slice plan.** Sequencing, slice IDs, dependencies, status icons.
> Design contract: [`../../architecture/lighthouse.md`](../../architecture/lighthouse.md).
> Follows Phase 8b (core plugins); sequenced AFTER the trivial-first core plugin
> (`auto-pair`) that de-risked the packaging/load pipeline.

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred · ❌ dropped.
Every non-trivial slice ships the four artefacts (doc + bench-where-perf-relevant
+ test incl. failure modes + graceful error handling).

**Status: 🚧 LH.0 ✅ (LH.0.1–LH.0.7); LH.1.1 ✅; LH.1.2 next.** Re-planned 2026-10-10 against the current
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
- **Cancel-by-id is process-wide, scoped to the plugin** — the instance that
  cancels is not the one that started the job. (Scoped by the wrong number
  until LH.0.7.)
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

#### LH.0.6 — `host-platform` + `data-dir`  ✅
Carved at the start of LH.1.1: a guest cannot tell what machine it is on, and
cannot name its own data directory to a host-side seam. Design §3.6, which
also **moves the install tree** into the plugin's data directory and records
why. **Exit:** a guest with no `fs:` capability marks a file in its data dir
executable by host path; one directory up is refused.

**Landed.** Two functions and a record, additive inside 0.2.0.
`CapabilityGrant` gains `data_dir`, set by the host where it mounts the
directory (never by a manifest, and not for an unsafe plugin id);
`grant_permits_walk` / `_write` accept paths under it, so every seam built on
them — download, extract, set-executable, read, walk, watch — follows without
a line of its own. 4 unit tests on the reach (inside; outside, sibling and
`..`; symlink out; a read-only `fs` grant stays read-only) and
`tests/host_info_seam.rs` with a real guest.

#### LH.0.7 — jobs report to the plugin, not the seam instance  ✅
A defect in LH.0.1–LH.0.3, found reading the loader before writing LH.1: job
events were addressed to the *instance* id of the store that started the job,
and each seam instance has its own. A job started from an ex-command (grammar
instance) was addressed to an id the events instance does not have — `ok(id)`,
then silence. `cancel-job` from the other instance missed the same way.

**Landed.** `PluginHost::job_owner(name)` — one number per plugin name —
stamped on both stores' emit context; jobs carry it, `cancel-job` is scoped by
it, and the event actor filters on it. `tests/job_addressing.rs`: 4 tests that
first hand two other plugins their owner numbers so this plugin's differs from
its instance id. **Seen red:** with the old comparison restored, three of the
four fail — including the fixture's own job, which had only ever passed because
the two numbers were both `0`.

### LH.1 — the lighthouse plugin  🚧
The core WASM Component plugin consuming LH.0. Crate `plugins/lighthouse/`.

#### LH.1.1 — the crate, the registry, and `:lsp-install`  ✅
Re-carved when it started: the July carving ended this slice at "the core
produces a tree" with no command to run it, which could only have been tested
against a fake. `:lsp-install` and its output buffer moved here from LH.1.2 so
the slice ends at something the real host can be made to do. **Exit:**
`:lsp-install <server>` produces a versioned install tree and reports each step
in `*lsp-install:<server>*`; a download that fails its SHA-256 ends in a
reported failure with no partial tree.

**Landed.** `plugins/lighthouse/` and the `lighthouse-plugin` world (grammar +
events).

- `registry.rs` — `registry.toml`, compiled in, with **rust-analyzer
  2026-10-05** for linux and macOS on x86_64 and aarch64 (digests as GitHub
  publishes them; linux-x86_64 downloaded and hashed when pinned). Everything
  is validated on parse, by server and field: names and versions are one path
  component, `binary` stays inside the tree, the digest is mandatory, URLs are
  https. A user's `registry.toml` in the data directory is laid over it — add
  a server, or replace a bundled one by name; a broken overlay costs only
  itself. 13 tests, one of which pins that every bundled download host has its
  `net:http:` line in `plugin.toml`.
- `install.rs` — the state machine, written against a `Host` trait so every
  failure branch runs under `cargo test`. Work happens under
  `<version>.partial/` and `<version>.download` and one rename puts the tree
  in place, so an installed directory existing *means* the install finished.
  Scratch names left by an editor exit are swept at startup. 17 tests.
- `lib.rs` — the adapter, and one decision: **the command does no work.** It
  validates, publishes a `lighthouse.request` event and opens the buffer; the
  events instance installs. So a job is started and stepped by one instance
  (its in-flight table is plain memory), and its `job-finished` is queued
  behind the call that started it. It also keeps WASI file calls off the
  grammar instance, where they cannot be driven — found by the first run of
  the end-to-end test, which panicked on exactly that.
- `tests/lighthouse_install.rs` (in `lattice-plugin-host`) — 5 tests through
  the shipped component as the loader stands it up, installing a script served
  from loopback: the happy path down to running the installed binary; a digest
  mismatch; an unreachable server and a successful retry; an unknown name; the
  startup sweep.

**Not done here, on purpose:** package-manager `recipe` installs (no registry
entry needs one yet — `spawn-process` is ready for it; LH.1.2b below), and a
server-name completion for the argument.

**Gap, pre-existing and now larger:** a plugin's own unit tests (30 here) run
with `cargo test` in the plugin's directory and are **not run by CI or by
`scripts/precommit.sh`** — true of `project`'s too. CI compiles the plugin and
runs the end-to-end test; the fake-host suite is by hand.

#### LH.1.2 — registration, `:lsp-uninstall`, `:lsp-update`  📝
What makes an installed server *used*. On a successful install,
`register-server` a config whose `command` is the managed binary (by host
path, from `data-dir`); at startup, re-register everything recorded as
installed, so a server survives a restart. `:lsp-uninstall <server>` removes
the tree and the registration. `:lsp-update <server>` / `:lsp-update-all`
install the registry's pin when it differs from what is installed, then drop
the old version's tree — install-new → verify → flip registration → GC old.
Registrations live on the events instance (they last as long as the instance
that made them), so uninstall is a request event like install. **Exit:**
`:lsp-install rust-analyzer` on a machine without it → a `.rs` buffer opened
afterwards runs the managed binary with no `PATH` entry, and still does after
a restart; `:lsp-uninstall` reverses it.

#### LH.1.2b — package-manager `recipe` installs  ⛔
A registry entry that names a command to run (`npm install --prefix …`)
instead of a URL, through `spawn-process`, its output streamed into the same
buffer. Deferred until a server that needs it is added to the registry: the
seam exists and is tested (LH.0.3), and an install path with no entry that
exercises it is untested code that looks finished.

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
