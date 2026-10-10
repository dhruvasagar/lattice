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

**`*lsp-install:<server>*` is a plugin output buffer** (§3.5): lighthouse
chooses its name, opens it (`effect.open-synthetic-buffer`), and writes every
line and the headerline; the host's generic `plugin-output-mode` is what puts
them on screen. The name follows `*lsp-log*` (dash) rather than `*lsp:<lang>:<root>*`
(colon) on purpose — the colon form is parsed as a server-instance buffer by
`lattice_lsp::buffer_names`, and an install is not one. Status rides the
headerline (the async-buffer-status rule).

The **managed install tree** is `<data-dir>/lsp/<name>/<version>/`, inside the
plugin's own data directory (`~/.config/lattice/plugins/lighthouse/data/` by
default; §3.6) — versioned so an update is atomic (install new, flip the
registration, GC old) and a bad version rolls back.

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
process-wide, scoped to the plugin — the chord that cancels runs on the grammar
seam and the job it stops was usually started from the events seam.

**"The plugin" is not a plugin id.** Each seam instance has its own host-issued
id, so a grammar instance and an events instance of one plugin are two numbers.
Jobs are therefore addressed, and cancel is scoped, by a **job owner**: one
number per plugin *name*, stamped on every instance of it
(`PluginHost::job_owner`). LH.0.1–LH.0.3 used the starting instance's id, which
made the paragraph above false in exactly the case it describes: a job started
from an ex-command reported to an id no event actor holds, and its outcome was
dropped. Every test of the time started and heard its job on one instance,
where the two numbers coincide. LH.0.7 fixed it, with tests that keep them
apart.

**One event vocabulary for every kind of job**, not a pair of arms per seam:

```wit
// arms of `event` (types.wit)
job-progress(event-job-progress),   // { id, done, total: option<u64> }
job-output(event-job-output),       // { id, lines: list<string> }
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

### 3.2 `extract-archive` and `set-executable`

```wit
enum archive-format { gz, tar-gz }

/// Unpack the archive file `src` to `dest`. A job; `job-progress` counts bytes
/// of `src` consumed.
extract-archive: func(src: string, dest: string, format: archive-format)
	-> result<u64, string>;

/// Mark the file `path` executable. Immediate.
set-executable: func(path: string) -> result<_, string>;
```

The unpack is host-side for §3.0's reasons plus one more: fuel. Inflating a
release archive in the guest is CPU-bound work inside a fuel-metered call.
A job like any other (§3.0), gated on `fs:read` over the source and `fs:write`
over the destination.

**An archive is untrusted input.** The SHA a registry pins says the bytes are
the ones somebody reviewed, not that they are benign. Every entry is confined
to the destination three ways, because each alone has a known way round it: the
path must be relative with no `..`; nothing is written *through* a symlink (an
archive can ship `a -> /etc` and then the lexically innocent `a/passwd`); and a
symlink's own target must stay inside. Hard links, devices and FIFOs fail the
job by name rather than being skipped — a silently dropped entry is a
half-installed server. Output size and entry count are bounded, on what comes
*out*: a few kilobytes of gzip can describe gigabytes.

**All or nothing**, the download's rule again: work in a sibling `<dest>.part`,
rename only when the whole archive has been read, remove the part on any
failure. A `tar-gz` destination must not already exist — an update unpacks
beside the old version and switches (§2), it does not merge into it.

A file comes out executable or not, as the archive says; no other mode bit is
honoured. A bare `gz` carries no mode at all, and a guest cannot `chmod` (WASI
has none), hence `set-executable` — also what a directly downloaded binary
needs.

**Formats: `gz` and `tar-gz`.** `zip` is deferred: it is a second, heavier
dependency, and on the platforms lattice builds for the first registry's
pre-built servers ship as one of the other two. It becomes necessary with a
Windows registry entry or a zip-only server (clangd).

### 3.3 `proc:spawn` — `spawn-process`

```wit
/// Run `command` with `args` in `cwd`. A job: returns its id at once; what the
/// process writes arrives as `job-output`, its exit as `job-finished`.
/// Capability-gated on `proc:spawn`, which is BUNDLED-PLUGINS-ONLY (arbitrary
/// spawn ≈ full trust; `capability.rs` withholds it from user-installed plugins).
spawn-process: func(command: string, args: list<string>, cwd: string)
	-> result<u64, string>;
```

Used only for the **package-manager install recipes** (`npm i -g`,
`pip install`, `go install`) where no pre-built binary exists. The *preferred*
path is a pre-built binary download (§3.1) — no toolchain, no arbitrary
execution.

**No shell.** `command` is a program and `args` reach it as given; nothing is
split or expanded, so a registry string cannot become a second command. A
caller that wants a shell runs `sh` and says so.

**Output is batched lines**, stdout and stderr interleaved: a quiet interval's
worth per `job-output`, bounded in size, nothing dropped, all of it before the
`job-finished`. A non-zero exit is an ordinary outcome — an `err` naming the
status, with the output that explains it already delivered.

**A cancel kills the tree, not the child.** `npm` is a script that starts
`node`, which starts more; killing only the direct child ends the job just as
promptly and leaves processes nobody owns. The child leads its own process
group and the group is signalled.

There is **no separate task surface.** The July draft had `start-task` /
`push-output` / `finalize` so the host could own a streaming buffer on the
plugin's behalf. With output arriving as events, the plugin appends to a buffer
it owns (§2) and the three calls have nothing left to do.

### 3.4 `register-server` — mutate the supervisor from WIT

```wit
record server-config {
	id: string, command: string, args: list<string>,
	env: list<tuple<string, string>>, root-markers: list<string>,
	file-patterns: list<string>, language-id: string,
	initialization-options: option<string>,   // JSON text
}

/// Tell the editor about a language server. Returns a token.
register-server: func(config: server-config) -> result<u64, string>;
unregister-server: func(token: u64);
```

`server-config` mirrors `lattice_lsp::config::ServerConfig` field for field.

**Gated on `proc:spawn`, not on an LSP capability.** `command` is a program the
editor will run, unsandboxed, the next time a matching buffer opens — so
registering a server *is* spawning, one buffer-open later, and a plugin that
could do the one could do the other. Bundled plugins only, like §3.3.

**A registration shadows; it does not add.** While registered, the config
replaces every server the editor already had under the same `id`: a managed
rust-analyzer supersedes the `PATH` lookup instead of running beside it and
doubling every diagnostic. A second registration for the same `id` shadows the
first, which is how an update switches versions and how a failed one rolls back
(§2). Unregistering restores exactly what was shadowed, because the supervisor
keeps boot-time configs and runtime registrations apart and recomputes the
effective list on each change.

**Nothing is started or restarted.** The config applies from the next matching
buffer open; a server already running keeps the program it was started with
until it is restarted. Registration is a fire-and-forget message to the
supervisor's task — it can arrive on the dispatch thread — visible on the next
snapshot.

**A registration lives as long as the plugin instance that made it** and is
withdrawn when that instance drops, so an unloaded server manager leaves no
server pointing into an install tree nobody manages. The teardown-token
pattern, with the guard on the `Store` as a watch's is.

**How the plugin host reaches the supervisor without depending on it:** a
`LanguageServerRegistrar` trait in `lattice-mode`, which both sides already
depend on. `lattice-lsp` implements it over its supervisor handle and registers
it as a service from its own `install`; the plugin loader looks the service up
and hands it to the host. `lattice-host` is not involved, and this is the only
piece of lighthouse that touches `lattice-lsp`.

### 3.5 Plugin output buffers — `output-append` / `output-status` / `output-reset`

The four seams above let a plugin start work and hear how it went. None lets it
*show* that. A job's events arrive in `on-event`, which returns nothing: it
cannot return an `open-synthetic-buffer` effect, and no host function writes to
a buffer. The plan assumed otherwise; the gap was found when LH.1 started.

Native streaming buffers already have one shape, and this is it:

```
producer ──► store (bounded ring) ──► typed event on the bus ──► the mode that
                                                                 owns the buffer
```

`*compilation*`, `*messages*`, the LSP logs and `*plugin-trace*` are all this.
The producer never holds a buffer; the mode, in `on_activate`, seeds from the
store, subscribes, and drains off-thread into its own document.

So a plugin gets the producer's end, and nothing else:

```wit
output-append: func(name: string, lines: list<string>) -> result<_, string>;
output-status: func(name: string, state: output-state, text: string) -> result<_, string>;
output-reset:  func(name: string) -> result<_, string>;
```

- **The store** (`lattice-plugin-host::output`) keeps, per buffer name, the last
  10 000 lines and one headerline status, and publishes `PluginOutputPushed`
  (a typed bus event, not an arm of the WIT `event` variant — nothing crosses
  back to a guest, so the ABI's event vocabulary is untouched).
- **The mode** (`plugin-output-mode`, in `lattice-plugin-trace` beside the trace
  view it is a twin of) is read-only, tails the event for the buffer whose name
  it was activated on, and renders the status as a headerline.
- **The plugin** opens the buffer with the ordinary `open-synthetic-buffer`
  effect naming that mode, from whichever command should show it.

Writing and opening are independent, in either order. Lines written before the
buffer is opened are in the ring and seed it; a buffer opened first fills when
the first line comes. That is why there is a store and not only an event: an
event alone reaches only a buffer that is already open, and an install is
started by the same command whose effect opens the buffer.

**Seed and tail join exactly.** The mode subscribes before it snapshots, so no
line falls between them — and one can be in both. Each line has a position
(`epoch` = which page of the buffer, changed by a reset; `seq` = line within
it), and the view drops precisely the overlap. A trace view tolerates a repeated
record; an install log that says "downloading" twice is wrong.

**Ownership is by plugin name**, which a reload keeps and a host-issued id does
not. The first plugin to write a name owns it; another plugin's write is an
`err` naming the owner. An unload drops the plugin's buffers. Epochs are
store-wide and only rise, so a view left open across that reload sees the first
new line as a new page rather than as lines it already showed.

**Bounds.** 32 buffers per plugin (the names are plugin-chosen and may embed
user input), 1024 lines per call, 4096 characters per line, names in the
`*name*` form. Past a bound the call is an `err` or the excess is dropped with a
marker line; nothing grows without limit.

**No capability.** A plugin can already log and already open synthetic buffers;
text in a buffer of its own is no new reach.

**The wake.** The drain writes off-thread, then sends on an `InboundBus` whose
`send` *is* the wake — after the write, not at publish time, so the repaint
never precedes the text. A headerline-only change (a percentage ticking) takes
the same path; it edits no text, so nothing else would repaint it.

### 3.6 What a plugin knows about its host — `host-platform` / `data-dir`

Two facts a guest could not obtain, both needed before the first byte is
downloaded.

```wit
host-platform: func() -> platform;       // { os, arch }
data-dir:      func() -> option<string>; // host path of the guest's /data
```

**`host-platform`.** A guest is `wasm32` wherever it runs. The registry is keyed
on `os`-`arch`, and the value has to be the machine the binary will run on.

**`data-dir`, and the reach that comes with it.** The seams above act on the
host's behalf and take host paths; `/data` means nothing to them, and the
guest had no way to say where `/data` really is. Returning the path is half of
it. The other half: those seams check paths against the manifest's `fs:`
grants, where the data directory never appeared — it is a WASI mount, not a
grant. So the capability grant now carries the data directory, and the shared
path checks (`grant_permits_read` / `grant_permits_write`) accept anything
under it. This grants nothing new: the guest can already create, overwrite and
delete everything in that directory through WASI. It only lets the host do, on
the plugin's behalf, what the plugin may do itself.

Lighthouse therefore needs **no `fs:` capability at all**. That is the reason
the install tree lives in the data directory rather than
`${XDG_DATA_HOME}/lattice/lsp/`, as this fragment first had it:

> **UX (higher court):** the first plan is worse. `install.sh` puts the bundled
> plugins under `~/.local/share/lattice` and upgrades by replacing that
> directory; plugin stores kept there were deleted on every reinstall until
> they moved (plugin-host, 2026-09-22). Servers beside them invite the same
> loss — a re-download of every server after each editor upgrade.
> **Paramount goals:** protects #2 (least privilege: the manifest asks for the
> network and nothing on disk); sacrifices nothing at runtime.
> **Heuristic #1:** yes on merit — a plugin's whole home is already one
> directory by decision, and built artefacts already live there.
> **Heuristic #2:** anchored on the capability model and the data-loss record,
> not on where another editor keeps servers.
> **Heuristic #3:** the third option was a `fs-grants` call returning the
> manifest's own writable prefixes; it keeps a second home for the plugin and
> still needs a per-OS path in a static manifest.
> **Heuristic #6:** no new crate.
> **Mode ownership:** untouched.

The cost, stated: server binaries sit under a *config* directory, which is not
where a filesystem-hierarchy purist expects hundreds of megabytes. Anyone who
syncs `~/.config` should exclude `lattice/plugins/*/data/`.

The effect write-gate (`EffectAuthorizer`, which vets file-writing *effects* a
grammar action returns) is deliberately not widened: it has no caller that
needs it, and a reach nobody uses is only surface.

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
  actor; what the plugin writes about them reaches the screen through an
  `InboundBus` wake (§3.5) — without a keypress, by construction.
- **UX (higher court).** Zero-friction "it just works" server install, with a
  transparent, cancellable, buffer-backed progress trace — no opaque hangs.
- **Mode ownership.** The commands, the `:lsp-servers` view and their chords
  live in the plugin, as do the name and every line of
  `*lsp-install:<server>*`. That buffer's *mode* is the generic
  `plugin-output-mode`, which owns its whole surface in its own crate — the
  drain, the headerline, the read-only gate. Nothing is split with the host:
  zero `Editor::` methods, zero host `Action` variants.
- **Security.** `net:http` is host-scoped (only the registry's download hosts,
  redirect hops included);
  `proc:spawn` is bundled-only (lighthouse ships pre-granted; a user-installed
  plugin can never reach it); SHA-pinning bounds supply-chain risk; there is no
  `fs:` grant — the managed tree is inside the plugin's own data directory
  (§3.6).

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
  Rejected: it welds a job to a buffer, so the host decides what an install
  looks like. What landed keeps them apart — jobs know nothing of buffers
  (§3.0), an output buffer knows nothing of jobs (§3.5), and the plugin decides
  which lines of which job go where. (This entry first claimed a plugin "can
  already open and write a synthetic buffer"; it could open one and never
  write to it, which is why §3.5 exists.)
- **A multibuffer view over a log file the plugin writes**, refreshed with
  `refresh-view`. Needed no host work. Rejected on UX: every refresh rebuilds
  the whole view, so a busy install restyles the viewport many times a second —
  a pixel change to content nobody edited.
- **Letting `on-event` return effects.** Rejected: it changes an export every
  events plugin implements, turns an observation-only seam into a mutation
  path, and still needs an append effect that does not exist.
- **The mode tailing job events directly**, with no `output-*` calls. Rejected:
  an install is several jobs plus steps that are not jobs (verify, register),
  and the lines between them are the plugin's to write. The host would have to
  format progress it does not understand.
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
