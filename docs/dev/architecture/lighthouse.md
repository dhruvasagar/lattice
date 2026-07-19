# Lighthouse — the LSP server manager (bundled plugin)

> **Design fragment.** Contracts, data model, rationale, rejected alternatives,
> paramount-goal alignment. Slice sequencing lives in
> [`../operations/slice-plans/lighthouse.md`](../operations/slice-plans/lighthouse.md)
> (LH.0 host seams → LH.1 plugin → LH.2 bundling). Sibling fragments:
> [`plugin-host.md`](plugin-host.md) (the seam spine + capability model),
> [`lsp-architecture.md`](lsp-architecture.md) (the supervisor/actor/client the
> installed servers plug into).
>
> **Status: 📝 designed, not built.** Captured for future reference. The first
> non-trivial bundled 8b plugin; deliberately sequenced AFTER a trivial bundled
> plugin (`auto-pair`) that de-risks the packaging/load pipeline with zero new
> host surface. Lighthouse's real cost is the **host-services extension it forces**
> (§3), not the plugin itself.

## 1. Why

Editor-quality out of the box means language servers *just work* — but today
lattice ships a hardcoded curated list (`lattice_lsp::config::builtin_servers()`:
rust-analyzer / pyright / gopls) whose `command` is resolved from `PATH`. The
user must install every server themselves, by hand, with the right version, on
`PATH`. That is the single biggest "install friction" gap between lattice and a
batteries-included editor.

**Lighthouse** closes it: install / update / uninstall language servers into a
lattice-managed tree, from a bundled registry of common servers, with
SHA-pinned downloads — and register the installed server so the native LSP
subsystem (§`lsp-architecture.md`) spawns it with no `PATH` dependency.

It is a **bundled WASM Component plugin**, not native, by deliberate choice
(§5): design.md §5.5.6 names it *"the first non-trivial bundled plugin we build,
validating that the WIT surface is sized correctly."* Building lighthouse as a
plugin forces the host's plugin-API surface to be large enough for a *real*
workload — network, subprocess, long-running streaming tasks, and mutating a
native subsystem from WIT — which a trivial plugin never exercises.

## 2. What lighthouse does (user surface)

- `:lsp-install <server>` — fetch + verify + install the named server into the
  managed tree; register its `ServerConfig`; progress streams into a
  buffer-backed view.
- `:lsp-update <server>` / `:lsp-update-all` — install a newer pinned version;
  keep the old until the new verifies.
- `:lsp-uninstall <server>` — remove the tree + unregister the `ServerConfig`.
- `:lsp-servers` — a buffer listing every registry server, its installed version
  (if any), and health. (The everything-is-a-buffer manager surface, the
  `:plugins` view precedent.)

The **managed install tree** is `${XDG_DATA_HOME}/lattice/lsp/<name>/<version>/`
— versioned so an update is atomic (install new, flip the registration, GC old)
and a bad version rolls back.

## 3. The host-services extension it forces (the real work) — LOAD-BEARING

Lighthouse is small; the **four host seams it needs are not built** (only the
`fs:walk` host-service is wired today). These are **general** plugin-host
capabilities — every future plugin that touches the network / a subprocess / a
long task uses them — so they land as a host-services extension
([`plugin-host.md`](plugin-host.md)), and lighthouse *consumes* them. They map
directly to design.md §5.5.6's first five WIT prerequisites.

Each runs **host-side with full host authority** (the host process is not
sandboxed), so — like `walk_within_grant` — the capability grant is re-checked at
the seam, not delegated to WASI.

### 3.1 `net:http` host-service — `http-fetch`

```wit
/// Fetch `url` (GET) → the response bytes. Capability-gated: the URL's host must
/// be in one of the plugin's granted `net:http:<host>` prefixes, else `err`. A
/// gated host-service, NOT a raw `wasi:http` socket — the host owns the client,
/// so redirects / TLS / timeouts are host policy, and the grant bounds reach.
http-fetch: func(url: string) -> result<list<u8>, string>;
```

Async-linker import (off the keystroke path). Bounded response size; streaming
variant deferred until a real streaming consumer needs it (the `walk`-vs-stream
precedent). Lighthouse fetches a pinned binary/archive URL, then verifies SHA
before touching the filesystem.

### 3.2 `proc:spawn` host-service — `spawn-process`

```wit
/// Run `command` with `args` in `cwd`, streaming stdout/stderr, → exit status.
/// Capability-gated on `proc:spawn`, which is BUNDLED-PLUGINS-ONLY (arbitrary
/// spawn ≈ full trust; user-installed plugins are denied it at grant time,
/// `capability.rs`). Output streams through the long-running-task surface (§3.3).
spawn-process: func(command: string, args: list<string>, cwd: string)
    -> result<process-exit, string>;
```

Used only for the **package-manager install recipes** (`npm i -g`,
`pip install`, `go install`) where no pre-built binary exists. The *preferred*
path is a pre-built binary fetch (§3.1) — no toolchain, no arbitrary execution.

### 3.3 Long-running-task surface — `start-task` / `push-output` / `finalize`

A three-call shape so a plugin-driven install streams its stdout into a
buffer-backed view without blocking the renderer (design.md §5.5.6 #5):

```wit
start-task: func(title: string) -> task-id;
push-output: func(task: task-id, line: string);
finalize:    func(task: task-id, outcome: task-outcome);
```

The host owns the `*lsp-install:<server>*` buffer + its headerline progress
(the async-buffer-status-in-headerline standing rule); the plugin just pushes
lines. Reuses the synthetic-buffer streaming substrate the LSP-log / plugin-trace
views already prove.

### 3.4 `lsp-register-server` seam — mutate the supervisor from WIT

```wit
/// Register (or replace) a ServerConfig from a plugin. `command` points at the
/// managed install tree; the native LspSupervisor then spawns it on the next
/// matching buffer open. Returns a token the plugin drops (or `unregister`s) on
/// uninstall — the teardown-token pattern.
register-server: func(config: server-config) -> result<server-token, string>;
unregister-server: func(token: server-token);
```

`server-config` mirrors `lattice_lsp::config::ServerConfig` (name / command /
args / env / root-markers / file-patterns / language-id / init-options) as a WIT
record — **the WIT type design.md §5.5.6 #1 calls the first blocker.** The
supervisor already keys servers by config; the seam is a capability-gated
mutation of that map (the grammar/config registry-mutation precedent).

## 4. The bundled server registry

A `registry.toml` compiled into the plugin: per server, per platform
(`os`-`arch`), the pinned version, download URL, SHA-256, and either a
`binary` path inside the archive or a `recipe` (package-manager command). SHA
pinning is mandatory — a mismatch aborts the install (supply-chain integrity).
Adding a server is a registry edit, not code. The registry is the plugin's data;
the host never interprets it.

## 5. Paramount-goal alignment

- **#2 Extensibility.** Lighthouse is the WIT-surface validator: it forces the
  net / proc / task / supervisor-mutation seams to exist and be sized right, which
  is exactly why design.md nominates it first among non-trivial plugins. Every
  later plugin reuses those seams.
- **#1 Performance.** Downloads / installs / subprocesses are async host-services
  off the keystroke path; progress is a buffer-backed streaming view (O(viewport)
  to render), never UI-thread work.
- **#4 Asynchronicity.** Each install is a spawned task; nothing blocks the actor
  or the renderer.
- **UX (higher court).** Zero-friction "it just works" server install, with a
  transparent, cancellable, buffer-backed progress trace — no opaque hangs.
- **Security.** `net:http` is host-scoped (only the registry's download hosts);
  `proc:spawn` is bundled-only (lighthouse ships pre-granted; a user-installed
  plugin can never reach it); SHA-pinning bounds supply-chain risk; the managed
  tree is the only `fs:write` grant.

## 6. Rejected alternatives

- **A native server manager (not a plugin).** Rejected: it would not dogfood the
  plugin host, and — the whole point — would not *validate the WIT surface*. The
  net / proc / task / supervisor-mutation seams are needed by the ecosystem
  regardless; building them for a native manager and again for plugins is the
  duplication heuristic #1 forbids. Lighthouse-as-plugin builds them once.
- **Bundling server binaries into the editor.** Rejected: multi-hundred-MB
  binary, no per-server versioning, no updates without an editor release.
- **Source-build everything** (`cargo install rust-analyzer`). Rejected as the
  *primary* path: needs a toolchain the user may not have, is slow, and runs
  arbitrary build scripts. Pre-built binary + SHA is primary; source/pkg-manager
  recipes are the bundled-only fallback (§3.2).
- **Raw `wasi:http` sockets.** Rejected: unbounded ambient network reach defeats
  the capability model; the gated `http-fetch` host-service keeps the host owning
  the client and the grant bounding the reach.

## 7. Slices (future)

The build order puts the host-services extension first (it is the blocker), then
the plugin:

1. **Host §3.1–§3.4** — `http-fetch`, `spawn-process` + the task surface,
   `register-server`/`unregister-server`; capability re-checks; the WIT records;
   async-seam instrumentation + a bench where perf-relevant. (Host-runtime work,
   `lattice-plugin-host`.)
2. **The `lighthouse` plugin** — the registry, the install/update/uninstall
   commands, the managed tree, the `:lsp-servers` buffer view, and the
   `ServerConfig` registration. (`plugins/lighthouse/`.)
3. **Bundling** — ship compiled-in / in `core-plugins/`, pre-granted
   (`net:http:<registry-hosts>`, `proc:spawn`, `fs:write:<managed-tree>`).

Deferred: a general `:plugin-install` (third-party plugin manager) reuses the
same `http-fetch` + SHA + managed-tree machinery — lighthouse proves the shape.
