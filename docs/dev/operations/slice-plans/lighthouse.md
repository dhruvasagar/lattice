# Lighthouse — slice plan

> **Slice plan.** Sequencing, slice IDs, dependencies, status icons.
> Design contract: [`../../architecture/lighthouse.md`](../../architecture/lighthouse.md).
> Follows Phase 8b (bundled reference plugins); sequenced AFTER the trivial-first
> bundled plugin (`auto-pair`) that de-risks the packaging/load pipeline.

Status icons: ✅ done · 🚧 in progress · 📝 planned. Every non-trivial slice ships
the four artefacts (doc + bench-where-perf-relevant + test incl. failure modes +
graceful error handling).

**Status: 📝 all planned.** Not started; captured alongside the design fragment.

## Sequencing

**LH.0 → LH.1 → LH.2.** The host-services extension (LH.0) is the blocker — the
plugin (LH.1) cannot fetch, spawn, stream, or register a server without it. LH.0
is **general** plugin-host surface (every future networked/subprocess plugin uses
it), so it lives in `lattice-plugin-host`, not the lighthouse crate; the design
detail is [`lighthouse.md`](../../architecture/lighthouse.md) §3 +
[`plugin-host.md`](../../architecture/plugin-host.md). LH.1 is the plugin; LH.2
bundles it.

```
LH.0 host-services ──► LH.1 lighthouse plugin ──► LH.2 bundling
 (net / proc / task /       (registry + install +      (ship pre-granted)
  register-server)           :lsp-servers view)
```

## Slices

### LH.0 — host-services extension (the prerequisite)  📝
The four unbuilt seams lighthouse forces (design fragment §3). General host
capabilities; capability re-checked host-side at each (the `walk_within_grant`
precedent). Async-linker imports (off the keystroke path).

#### LH.0.1 — `http-fetch` (net:http)  📝
`http-fetch: func(url) -> result<list<u8>, string>` in `wit/host-services.wit`;
host impl fetches GET (host owns the client — TLS/redirect/timeout policy), gated
so the URL host must be in a granted `net:http:<host>` prefix, else `err`. Bounded
response size; streaming variant deferred. **Exit:** a bundled plugin with a
`net:http:<host>` grant fetches bytes from that host; a plugin without the grant,
or for a different host, gets `err`; a non-bundled/user plugin's grant is honored
identically. Test: fixture guest fetch (gated allow + gated deny); no bench (I/O).

#### LH.0.2 — `spawn-process` + the long-running-task surface  📝
`spawn-process: func(command, args, cwd) -> result<process-exit, string>` gated on
`proc:spawn` (**bundled-only** — `capability.rs` denies it to `UserInstalled`);
output streams through `start-task` / `push-output` / `finalize`. The host owns
the `*…*` streaming buffer + its headerline progress (async-buffer-status rule),
reusing the LSP-log / plugin-trace synthetic-buffer substrate. **Exit:** a bundled
plugin spawns a subprocess and its stdout streams into a buffer live; a
user-installed plugin's `spawn-process` is denied; a non-zero exit surfaces, never
a panic. Test: fixture spawns `echo`, asserts streamed output + exit; the deny
path. No hot-path bench (off-thread); the drain follows the LspLogPushed shape.

#### LH.0.3 — `register-server` / `unregister-server`  📝
A `server-config` WIT record mirroring `lattice_lsp::config::ServerConfig` (name /
command / args / env / root-markers / file-patterns / language-id / init-options);
`register-server: func(server-config) -> result<server-token, string>` mutates the
native `LspSupervisor`'s config map (capability-gated, the grammar/config
registry-mutation precedent); `unregister-server(token)` reverses it (the
teardown-token pattern). **Exit:** a plugin registers a `ServerConfig` pointing at
an arbitrary path; opening a matching buffer spawns that server; unregister (or
plugin unload) removes it so no later buffer spawns it. Test: register → the
supervisor spawns on a matching open → unregister → it doesn't. This is the WIT
type design.md §5.5.6 #1 calls the first blocker.

### LH.1 — the lighthouse plugin  📝
The bundled WASM Component plugin consuming LH.0. Crate `plugins/lighthouse/`.

#### LH.1.1 — crate scaffold + registry + fetch/verify/install core  📝
The `plugins/lighthouse/` guest crate (`wasm32-wasip2`, `manifest.toml` requesting
`net:http:<registry-hosts>` + `proc:spawn` + `fs:write:<managed-tree>`); a
compiled-in `registry.toml` (per server × platform: pinned version, download URL,
SHA-256, `binary`-in-archive or package-manager `recipe`); the
fetch (LH.0.1) → **SHA-verify** → unpack/install into
`${XDG_DATA_HOME}/lattice/lsp/<name>/<version>/`. A SHA mismatch aborts before any
fs write. **Exit:** given a registry entry, the core downloads + verifies + lays
down a versioned install tree; a tampered SHA aborts with no partial install.
Test: a local fixture URL + known SHA (allow) + a mismatched SHA (abort).

#### LH.1.2 — install/update/uninstall commands + ServerConfig registration  📝
`:lsp-install <server>` / `:lsp-update <server>` / `:lsp-update-all` /
`:lsp-uninstall <server>`, each a `register-ex-command` (grammar seam) driving the
LH.1.1 core off-thread with progress via the LH.0.2 task surface; on install,
`register-server` (LH.0.3) a `ServerConfig` whose `command` is the managed binary;
on uninstall, `unregister-server` + GC the tree. Update is install-new →
verify → flip registration → GC old (atomic; rollback on verify fail). **Exit:**
`:lsp-install rust-analyzer` on a machine without it → the server installs and a
`.rs` buffer gets diagnostics/hover with no `PATH` entry; `:lsp-uninstall` reverses
it. Test: the command → registration → (mocked) supervisor-spawn path.

#### LH.1.3 — the `:lsp-servers` manager view  📝
A read-only buffer (everything-is-a-buffer, the `:plugins` view precedent) listing
every registry server, its installed version (if any), and health; in-view chords
(install / update / uninstall the row). Mode owns its chords + handlers
(mode-ownership rule). **Exit:** `:lsp-servers` lists the registry with
installed/available state and live-updates as an install completes.

### LH.2 — bundling  📝
Ship `lighthouse.wasm` compiled-in (`include_bytes!`) or in `core-plugins/` next
to the binary, instantiated at boot with its pre-granted capabilities
(`net:http:<registry-hosts>`, `proc:spawn`, `fs:write:<managed-tree>`) — the
bundled-plugin bootstrap (design.md §5.5.6). **Exit:** a fresh editor has
lighthouse loaded at boot (`:plugins` shows it, `:lsp-servers` works) with no user
install step.

## Notes

- **Deferred:** a general `:plugin-install` (third-party plugin manager) reuses the
  LH.0.1 `http-fetch` + SHA + managed-tree machinery — lighthouse proves the shape,
  so the plugin-manager slice is a thin follow-on, not a fresh design.
- **Cross-renderer:** LH.0.2's streaming buffer + LH.1.3's manager view are
  Documents (renderer-agnostic); no per-renderer work beyond what the buffer
  substrate already provides.
