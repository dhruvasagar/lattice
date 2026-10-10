# Lighthouse — slice plan

> **Slice plan.** Sequencing, slice IDs, dependencies, status icons.
> Design contract: [`../../architecture/lighthouse.md`](../../architecture/lighthouse.md).
> Follows Phase 8b (core plugins); sequenced AFTER the trivial-first core plugin
> (`auto-pair`) that de-risked the packaging/load pipeline.

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred · ❌ dropped.
Every non-trivial slice ships the four artefacts (doc + bench-where-perf-relevant
+ test incl. failure modes + graceful error handling).

**Status: 🚧 LH.0.1 in progress.** Re-planned 2026-10-10 against the current
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

#### LH.0.1 — `http-download` / `cancel-download` (net:http)  🚧
`http-download(url, sha256, dest) -> result<u64, string>` +
`cancel-download(id)` in `host-services.wit`; `download-progress` /
`download-finished` event kinds + arms in `types.wit`, mirrored as
`lattice_protocol::Event::{DownloadProgress, DownloadFinished}` and addressed in
`event_task.rs` exactly as `FilesChanged` is. The host streams to `<dest>.part`
on its own thread, hashing as it goes; only a SHA match renames into place.
Gates: URL host (and every redirect hop) ∈ `net:http:<host>`; `dest` within
`fs:write`. Policy: https (http to loopback only), bounded redirects / size /
timeouts. **Exit:** a plugin with both grants downloads a file and hears
`download-finished` **without a keypress**; a wrong SHA, a cancel and a size
overrun each leave no file; an ungranted host, an ungranted redirect hop and an
ungranted `dest` are each refused by name; another plugin subscribed to the same
kinds hears nothing. Test: unit tests against a loopback server + a fixture
guest through the events seam. No bench (I/O); `perf_ratchet` stays green.

#### LH.0.2 — `extract-archive`  📝
Host-side unpack of a downloaded archive into an `fs:write`-granted directory,
same id + addressed-event shape. `gz` (single file), `tar.gz`, `zip`. Entries
escaping the destination (`..`, absolute, symlink out) are refused; the
executable bit is preserved. **Exit:** each format unpacks; a traversal entry
aborts with nothing written outside the destination. Decide at slice start
whether extraction is its own call or a `dest` mode of `http-download` — a
separate call keeps "downloaded and verified" observable on its own.

#### LH.0.3 — `spawn-process` (proc:spawn)  📝
`spawn-process(command, args, cwd) -> result<u64, string>` gated on `proc:spawn`
(**bundled-only** — `capability.rs` already withholds it from `UserInstalled`);
stdout/stderr lines and the exit status arrive as addressed events, coalesced.
Killed when the owning instance drops. **Exit:** a bundled plugin spawns a
subprocess and hears its output and exit; a user-installed plugin is denied; a
non-zero exit is an ordinary outcome, never a panic. Check `lattice-compilation`
/ shell-command for a reusable line-reader before writing a third.

#### LH.0.4 — `register-server` / `unregister-server`  📝
A `server-config` WIT record mirroring `lattice_lsp::config::ServerConfig`;
`register-server` mutates the native `LspSupervisor`'s config map,
`unregister-server(token)` reverses it, and plugin unload reverses it too (the
teardown-token pattern). **Open at slice start:** how `lattice-plugin-host`
reaches the supervisor — it has no `lattice-lsp` dependency today, so this is a
host-wired service/trait rather than a direct call. **Exit:** register → a
matching buffer open spawns that server → unregister → it does not.

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
(`effect.open-synthetic-buffer`), appends a line per event, and keeps status in
the headerline; a failure's message lands in the buffer. On success,
`register-server` a config whose `command` is the managed binary. Update is
install-new → verify → flip registration → GC old. **Verify at slice start:**
that a plugin can append to its own read-only synthetic buffer and set its
headerline from an event handler today; either gap is a small generic seam, not
a lighthouse special case. **Exit:** `:lsp-install rust-analyzer` on a machine
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

## Notes

- **Dropped from the July plan:** ❌ the `start-task` / `push-output` /
  `finalize` task surface — superseded by event-delivered output into a
  plugin-owned buffer (design §3.3). ❌ blocking `http-fetch -> list<u8>` —
  superseded by `http-download` (design §3.0).
- **Deferred:** a general `:plugin-install` reuses LH.0.1 + LH.0.2 — lighthouse
  proves the shape, so the plugin-manager slice is a thin follow-on.
- **Cross-renderer:** the progress buffer and the manager view are Documents
  (renderer-agnostic); no per-renderer work beyond the buffer substrate.
