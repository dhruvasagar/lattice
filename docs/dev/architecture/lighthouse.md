# Lighthouse — the LSP server manager (core plugin)

> **Design fragment.** Contracts, data model, rationale, rejected alternatives,
> paramount-goal alignment. Slice sequencing lives in
> [`../operations/slice-plans/lighthouse.md`](../operations/slice-plans/lighthouse.md)
> (LH.0 host seams → LH.1 plugin → LH.2 bundling). Sibling fragments:
> [`plugin-host.md`](plugin-host.md) (the seam spine + capability model),
> [`lsp-architecture.md`](lsp-architecture.md) (the supervisor/actor/client the
> installed servers plug into).
>
> **Status: 🚧 in progress.** The first non-trivial **core plugin** — it ships
> through the pipeline `auto-pair` proved (`plugins/<name>/`, staged by
> `cargo xtask build-core-plugins`, discovered at boot as `TrustTier::Bundled`).
> Lighthouse's real cost is the **host-services extension it forces** (§3), not
> the plugin itself.
>
> **Revised 2026-10-10** against the host as it now stands. The July draft
> specified blocking calls (`http-fetch -> list<u8>`, a three-call task surface
> owning a host buffer); neither survives contact with the two-linker reality.
> §3.0 records why, and §6 keeps the superseded shapes as rejected alternatives.

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
  managed tree; register its `ServerConfig`. Returns at once: the install runs
  in the background and its progress — and its error, if it fails — streams
  live into `*lsp-install:<server>*`.
- `:lsp-update <server>` / `:lsp-update-all` — install a newer pinned version;
  keep the old until the new verifies.
- `:lsp-uninstall <server>` — remove the tree + unregister the `ServerConfig`.
- `:lsp-servers` — a buffer listing every registry server, its installed version
  (if any), and health. (The everything-is-a-buffer manager surface, the
  `:plugins` view precedent.)

**`*lsp-install:<server>*` is the plugin's buffer**, not the host's: lighthouse
opens it (`effect.open-synthetic-buffer`), owns its mode, and appends a line per
host event. The name follows `*lsp-log*` (dash) rather than `*lsp:<lang>:<root>*`
(colon) on purpose — the colon form is parsed as a server-instance buffer by
`lattice_lsp::buffer_names`, and an install is not one. Status rides the
headerline (the async-buffer-status rule).

The **managed install tree** is `${XDG_DATA_HOME}/lattice/lsp/<name>/<version>/`
— versioned so an update is atomic (install new, flip the registration, GC old)
and a bad version rolls back.

## 3. The host-services extension it forces (the real work) — LOAD-BEARING

Lighthouse is small; the **host seams it needs are not built**. They are
**general** plugin-host capabilities — every future plugin that touches the
network, an archive or a subprocess uses them — so they land in
`lattice-plugin-host` ([`plugin-host.md`](plugin-host.md)) knowing nothing about
LSP, and lighthouse *consumes* them.

Each runs **host-side with full host authority** (the host process is not
sandboxed), so — like `walk_within_grant` — the capability grant is re-checked at
the seam, not delegated to WASI.

### 3.0 The shape every long-running seam takes: a host job

A long-running host-service starts a **job**: it **returns an id immediately**
and reports through **events addressed to the plugin that asked** — the `watch` → `files-changed`
shape (OR.2), not a call that blocks until the work is done. Three facts about
the host force this, and each was verified against source rather than assumed:

- **`host-services` is wired on BOTH linkers**, including the synchronous one
  the grammar seam runs on the dispatch thread. A component's import set is fixed
  for the whole artefact, and `:lsp-install` is a grammar-seam ex-command — so a
  call that blocks for the length of a download blocks the editor. "Async-linker
  only" is not a shape the Component Model offers.
- **The per-call budget is wall clock** (`PluginBudget::epoch_deadline`, 5 s by
  default, host time included since OA.0b). A download inside one guest call
  traps.
- **The bytes have nowhere good to go.** A 40 MB archive returned as `list<u8>`
  is copied into guest memory, and the guest then cannot write it from the
  grammar seam at all (the sync WASI filesystem shim panics there — `read-file`'s
  doc comment).

So the host does the I/O off-thread and streams to disk; the guest holds an id.
Delivery rides the plugin's own event actor, which is what makes a result reach
the screen **without a keypress**. Ids are host-global, because the instance
that starts a job (grammar seam) is not the instance that hears about it (events
seam) — they are separate `Store`s.

A job is owned by the `PluginState` that started it and is cancelled when that
state drops (unload, quarantine): mechanism lives where its lifetime matches,
with no teardown wiring to forget. Cancel **by id** is separate and
process-wide, scoped by plugin id — the chord that cancels runs on the grammar
seam and the job it stops was usually started from the events seam.

**One event vocabulary for every kind of job**, not a pair of arms per seam:

```wit
// arms of `event` (types.wit)
job-progress(event-job-progress),   // { id, done, total: option<u64> }
job-finished(event-job-finished),   // { id, outcome: result<_, string> }

// host-services
cancel-job: func(id: u64);
```

An arm added to the `event` variant is an ABI break — every guest that matches
on it stops compiling, and the package version must move (`0.1 → 0.2` was spent
on exactly this). Per-seam arms would spend a generation per seam. A plugin
keys its state by id and already knows what it started, so the kind would tell
it nothing; a seam says what its `done`/`total` units are (bytes, for a
download). Exactly one `job-finished` per id, a cancelled job included
(`err("cancelled")`), so a guest drives a state machine off it without a
timeout. A job requested from inside `register-events` is held until the
plugin's subscriptions are on the bus, for the same reason.

### 3.1 `net:http` — `http-download`

```wit
/// Download `url` (GET) to the file `dest`, verified against `sha256` (hex).
/// A job: returns its id at once; `job-progress` counts bytes received.
http-download: func(url: string, sha256: string, dest: string)
	-> result<u64, string>;
```

Gated twice, both re-checked host-side, both refused **synchronously** so a
plugin author sees a manifest problem at the call rather than as a late event:

- the URL's host must match a granted `net:http:<host>` entry — and so must
  **every redirect hop**. The host follows redirects itself and re-checks each
  one; a release URL that bounces to a CDN needs the CDN granted too, by name.
  A redirect is otherwise a way to turn one granted host into any host.
- `dest` must lie within an `fs:write` grant.

Host policy, not guest policy: `https` only (plain `http` solely to a loopback
address, which is what makes the seam testable), bounded redirects, bounded
size, connect/read timeouts.

**The SHA check is structural.** The body streams to `<dest>.part` while being
hashed; only a match renames it into place, and every other exit — mismatch,
cancel, short read, size cap — deletes the part file. "A tampered download
leaves no partial install" is therefore a property of the seam, not of each
plugin's discipline. The host hashes; the expected value is the guest's data and
the host never learns where it came from.

Progress is **coalesced** (a quiet interval between `job-progress`
deliveries), so a fast link is a handful of guest calls rather than one per
chunk. No bytes-returning `http-fetch` is offered: nothing needs one yet, and a
small bounded variant is additive when something does.

### 3.2 `extract-archive`

The unpack is host-side for §3.0's reasons plus one more: fuel. Inflating a
release archive in the guest is CPU-bound work inside a fuel-metered call.
A job like any other (§3.0), gated on `fs:write` over the destination,
with entries that escape it (`..`, absolute paths, symlinks out) refused.
Formats are the ones the registry needs (`gz` single-file, `tar.gz`, `zip`).

### 3.3 `proc:spawn` — `spawn-process`

```wit
/// Run `command` with `args` in `cwd`. A job: returns its id at once; output
/// lines and the exit status arrive as addressed events.
/// Capability-gated on `proc:spawn`, which is BUNDLED-PLUGINS-ONLY (arbitrary
/// spawn ≈ full trust; `capability.rs` withholds it from user-installed plugins).
spawn-process: func(command: string, args: list<string>, cwd: string)
	-> result<u64, string>;
```

Used only for the **package-manager install recipes** (`npm i -g`,
`pip install`, `go install`) where no pre-built binary exists. The *preferred*
path is a pre-built binary download (§3.1) — no toolchain, no arbitrary
execution.

There is **no separate task surface.** The July draft had `start-task` /
`push-output` / `finalize` so the host could own a streaming buffer on the
plugin's behalf. With output arriving as events, the plugin appends to a buffer
it owns (§2) and the three calls have nothing left to do.

### 3.4 `register-server` — mutate the supervisor from WIT

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
record. The supervisor already keys servers by config; the seam is a
capability-gated mutation of that map (the grammar/config registry-mutation
precedent). This is the only piece that touches `lattice-lsp`.

## 4. The bundled server registry

A `registry.toml` compiled into the plugin: per server, per platform
(`os`-`arch`), the pinned version, download URL, SHA-256, and either a
`binary` path inside the archive or a `recipe` (package-manager command). SHA
pinning is mandatory — a mismatch aborts the install (supply-chain integrity),
enforced by the download seam itself (§3.1).
Adding a server is a registry edit, not code. The registry is the plugin's data;
the host never interprets it.

## 5. Paramount-goal alignment

- **#2 Extensibility.** Lighthouse is the WIT-surface validator: it forces the
  net / proc / task / supervisor-mutation seams to exist and be sized right, which
  is exactly why design.md nominates it first among non-trivial plugins. Every
  later plugin reuses those seams.
- **#1 Performance.** Every seam returns an id and does its I/O on a host
  thread, so no call can stall the dispatch thread whichever linker it arrives
  on (§3.0); progress is a buffer-backed streaming view (O(viewport) to render),
  never UI-thread work.
- **#4 Asynchronicity.** Results are addressed events on the plugin's own event
  actor — they reach the screen without a keypress, by construction.
- **UX (higher court).** Zero-friction "it just works" server install, with a
  transparent, cancellable, buffer-backed progress trace — no opaque hangs.
- **Mode ownership.** The commands, the `*lsp-install:<server>*` buffer, the
  `:lsp-servers` view and their chords all live in the plugin. The host gains
  generic primitives only: zero `Editor::` methods, zero host `Action` variants.
- **Security.** `net:http` is host-scoped (only the registry's download hosts,
  redirect hops included);
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
- **Native, inside `lattice-lsp`.** The strongest form of the rejection above —
  `lattice-lsp` does own the LSP domain. But it would pull an HTTP / hashing /
  archive / process surface into a crate that today only talks to local server
  processes, and the same seams would be built a second time for plugins.
- **A separately installed (user-tier) plugin.** Rejected: the user would have
  to install a plugin before servers could install themselves, and `proc:spawn`
  is withheld from that tier, so the package-manager recipes could never run.
- **A blocking `http-fetch(url) -> list<u8>`** (this fragment's first draft).
  Rejected in §3.0: it blocks the dispatch thread from a grammar action, trips
  the wall-clock budget, and routes the archive through guest memory.
- **A truly async import on its own interface**, imported only by non-grammar
  worlds. Rejected: lighthouse needs the grammar seam for its ex-commands, a
  component's import set is fixed, and an import the sync linker cannot satisfy
  fails the WHOLE component at instantiation.
- **A host-owned task buffer** (`start-task` / `push-output` / `finalize`).
  Rejected: it puts a provider's buffer production in the host. A plugin can
  already open and write a synthetic buffer; it only lacked the events to drive
  one.
- **Raw `wasi:http` sockets.** Rejected: unbounded ambient network reach defeats
  the capability model; the gated `http-download` host-service keeps the host
  owning the client and the grant bounding the reach.

## 7. Slices

Sequencing and status live in the
[slice plan](../operations/slice-plans/lighthouse.md): the host seams (§3) land
first because they are the blocker, then the plugin, then staging it as a core
plugin.

Deferred: a general `:plugin-install` (third-party plugin manager) reuses the
same `http-download` + SHA + `extract-archive` machinery — lighthouse proves the
shape.
