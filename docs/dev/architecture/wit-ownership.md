# Who owns `wit/`, and what happens when a plugin is behind it

**Status:** built (WT.1–WT.4; see the
[slice plan](../operations/slice-plans/archive/wit-ownership.md) for sequencing and what
each slice actually did). Extends [`plugin-host.md`](plugin-host.md) §12 (the
ABI-freeze deferral) and [`plugin-manager.md`](plugin-manager.md) (build +
staging).

The recommendation in §6 changed during the build — the mechanism is §3(c), not
§3(a), for a reason worth reading before touching any of this.

## 1. What went wrong

Three WIT changes landed in one day. Afterwards:

- the user's `init.wasm` (built two days earlier) failed to instantiate, so
  the `require("org")` inside it never ran;
- because `require` never ran, org's stale artifact was never rebuilt;
- org therefore never loaded, so `.org` files had no language, no
  highlighting, no syntax folds and no org chords;
- **and nothing said any of this.** The editor opened, the file opened, and
  everything was simply absent.

Both components had to be rebuilt by hand, and the second one could only be
reached after the first was fixed — the thing that repairs stale plugins was
itself a stale plugin.

Three separate defects, and they compound:

1. **`wit/` is copied once and never refreshed.** `write_wit_package` runs at
   scaffold time (`lattice --scaffold-init`, `lattice --scaffold-plugin`). After
   that the plugin's `wit/` is a fork that nothing updates.
   → answered by §3(c): the build service rewrites it before every build.
2. **A stale artifact is not rebuilt.** `build_plugin` rebuilds when the
   *source* changed. A source that did not change but was built against a
   different ABI looks current.
   → answered by §4: the stamp records the ABI too, and a mismatch is stale.
3. **An instantiate failure is invisible where it matters.** It is logged; it
   does not reach the user who is looking at an uncoloured file.
   → answered by §4: `warn!` to `*messages*`, and a *Failed to load* section in
   `:plugins`. The init-boot arm that reported this at `debug!` — conflating "no
   init.rs" with "init.rs is broken" — was the specific line at fault.

## 2. Who owns it

**Lattice owns `wit/`. It is the canonical API** (design.md: "WIT is the
canonical API"). Every copy in a plugin tree is a **cache of that**, not a
fork — but nothing in the system says so, and nothing enforces it.

The copies exist for a real reason: `wit_bindgen::generate!` resolves its
`path` at macro-expansion time, so the files must be on disk beside the crate
being compiled. That is a build-time need, which is the hint the design should
take — **a build-time need is met by the build, not by the user.**

## 3. Options for keeping the copy current

### (a) A `lattice-wit` crate the plugin depends on, synced by `build.rs`

Lattice publishes its `wit/` as a crate (the files embedded, exactly as
`lattice-cli`'s `build.rs` already embeds them for scaffolding). A plugin adds
it as a build-dependency and a five-line `build.rs` writes the files into its
own `wit/`:

```rust
// plugin build.rs
fn main() { lattice_wit::write_to("wit").unwrap(); }
```

`wit/` becomes generated, gitignored output. The plugin's Cargo.toml pins which
ABI it targets, and upgrading is a dependency bump that cargo resolves and
records in the lockfile.

> **UX (higher court):** the failure the user hit becomes impossible to reach
> by accident — a rebuild always builds against the ABI the manifest pins,
> and the plugin manager already rebuilds a local source whose files changed.
> **Paramount goals:** protects #2 — the plugin API becomes a *versioned
> dependency* rather than a folder someone remembered to copy. Nothing at #1.
> **Heuristic #1 (long-term fit):** this is the genuinely-better design and
> not merely the smaller one: the ABI a plugin targets is a fact about the
> plugin, and a dependency version is exactly how that fact is normally
> expressed. It also makes the post-1.0 SemVer story (§5) fall out — pinning
> `lattice-wit = "0.2"` is how a plugin says which API it wants.
> **Heuristic #2 (paramount, not other editors):** anchored on the
> canonical-API goal, not on how another editor vendors headers.
> **Heuristic #3 (third option):** (b), (c) and (d) below.
> **Heuristic #6 (crate boundary):** a new crate, and the dependency surface
> it carves out is the point — `lattice-wit` must depend on *nothing*, so a
> plugin can target the API without pulling the editor in. Today a plugin
> that wants the WIT has no way to get it that does not involve a checkout of
> lattice. That is the thing that breaks without it.

### (b) The editor exports it: `lattice --wit-sync [DIR]`

The binary already embeds the files. A subcommand rewrites the `wit/` of a
given directory (defaulting to `~/.config/lattice/init` and every local plugin
source), and the plugin manager runs it before building a local source.

> **UX (higher court):** fixes the drift, and gives a user a one-line repair
> for an install that is already broken — which (a) does not, because a broken
> `init.wasm` cannot rebuild itself.
> **Paramount goals:** protects #2. Nothing at #1.
> **Heuristic #1:** weaker than (a) as the *primary* mechanism — it ties the
> plugin's ABI to whichever `lattice` binary is on `PATH` rather than to
> anything the plugin declares, so two editors on one machine silently
> disagree. Strong as a *repair* path, which is a different job.
> **Heuristic #2:** —
> **Heuristic #3:** complements (a) rather than competing.

### (c) The build service refreshes it, immediately before cargo

The editor already stands between a plugin's source and the compiler.
`build_plugin` is the single path by which every local source is built —
`~/.config/lattice/init`, every scaffolded plugin, every `require`d git or local
tree — and it shells out to cargo itself. So it can write the canonical package
into the source as its first act, and the component is then compiled against the
API of the process that is about to instantiate it.

`lattice-plugin-loader` takes `lattice-wit` (zero dependencies) as a normal
dependency; the refresh happens *before* the staleness check, and `write_to`
leaves a file already holding the right bytes untouched so a warm boot moves no
mtime and the build cache survives.

> **UX (higher court):** the reported failure becomes unreachable for every
> source the editor builds, with no `Cargo.toml` line to point anywhere, no
> `PATH` lookup, and nothing waiting on a crates.io release.
> **Paramount goals:** protects #2 — the ABI a component is compiled against is
> the ABI of the host that will load it, structurally rather than by anyone
> remembering. Nothing at #1: one directory write in front of a cargo
> invocation that costs seconds to minutes, already on `spawn_blocking`.
> **Heuristic #1 (long-term fit):** the genuinely-better design, and not merely
> the available one. (b) was rejected above for tying the plugin's ABI to
> whichever `lattice` is on `PATH`; that objection does not reach here, because
> this is not an ambient binary — it is the loading process itself, in the same
> boot.
> **Heuristic #2:** anchored on the canonical-API goal.
> **Heuristic #3:** this is the option (a)/(b)/(d) missed. Both of those asked
> what the scaffold's manifest should say; this asks who runs the compiler, and
> the answer turns out to be *us*.
> **Heuristic #6:** no new crate — an existing one takes a zero-dep dependency.

**What it costs, stated plainly.** Two lattice builds sharing one config home
will alternately rebuild each other's plugins, because each reads the other's
ABI fingerprint as stale once §4 lands. That is new, and it is thrash — but it
is *correct* thrash: each editor loads a component that works, where today one
of them silently loads nothing. A hand-run `cargo build` in a source directory
still uses whatever `wit/` is there; (b)'s `lattice --wit-sync` is the manual peer
for that case, and §4 makes the resulting artifact stale rather than quietly
broken.

**It composes with (a) rather than replacing it.** A repo built out-of-tree — by
CI, by another person, with no editor in the loop — still wants a declared pin,
and org has one. When both are present the build-dependency *wins*, because
`build.rs` runs after the refresh. That precedence is the right way round: a pin
the repo declares should override an ambient one, and §4 is what makes the
resulting mismatch legible instead of silent.

### (d) Status quo plus a documented `cp`

> **UX (higher court):** this is what produced the reported failure. It loses
> on UX before any other consideration.
> **Heuristic #1:** keeping an inferior mechanism because the change is
> bigger is exactly what the heuristic forbids.

## 4. Detecting the skew, and repairing it

Syncing at build time fixes *future* builds. It does nothing for an artifact
already on disk, so the loader needs to notice.

**Record an ABI fingerprint at build time and compare it at load.** The
`.build-stamp` already records what the artifact was built *from*; it should
also record what it was built *against* — a hash over the `wit/` package. The
loader then has three cases rather than one:

| State | Today | Proposed |
|---|---|---|
| stamp matches, source unchanged | load | load |
| source changed | rebuild | rebuild |
| **ABI hash differs** | **load, fail, silently** | rebuild from source |
| ABI differs, no rebuild allowed (pinned) | load, fail, silently | load, naming both versions |

That last row was drafted as *refuse*, and refusing is wrong. The fingerprint
hashes the whole package, so it moves when any file changes — including files
the plugin never imports. A mismatch means "this may not load", not "this
cannot", and a pin exists precisely to say *keep this build*. The honest
response to a coarse signal is to load the artifact and put the skew on record
where the user can act on it. Trading a silent failure for a confident wrong one
is not an improvement.

`plugin-host.md` §3 originally proposed a `wit_revision` in the AOT cache key
before wasmtime's built-in cache superseded it. The idea was right; it belongs
here, where it answers a question wasmtime's cache does not.

**And the failure must surface.** A plugin that fails to instantiate is a
user-visible event — the same class as "LSP server attached", not a `debug!`.
It reaches `*messages*` with the plugin named and the reason, and `:plugins`
carries a trailing *Failed to load* section giving the name, the directory, and
the error. A silently-absent plugin is indistinguishable from one that was never
installed, which is precisely why this took a debugging session to find rather
than a glance.

Both surfaces, not one, because they answer different questions: the log says a
thing went wrong just now; `:plugins` answers *"why is org not here?"* asked ten
minutes later, by which time the log has scrolled.

The failure set is held apart from `PluginStatus` rather than as another
`PluginHealth` variant. A failed load has no host-issued id, no granted
capabilities, and no tier that was ever applied; fabricating all three to fit the
shape would put rows in `:plugins` with no plugin behind them, and the view's
row→plugin index mapping would then point at nothing.

## 5. "How does a plugin built against an older `wit/` work?"

**Pre-1.0, it does not — and that is a standing decision, not an oversight.**
`plugin-host.md` §12: *"the WIT is unstable until ≥3 real plugins have
exercised it. SemVer only post-1.0."* (design.md §15 Q7.)

So the question to answer now is not *how do we keep it working* but *how does
its breaking stop being silent and manual* — which is §4.

What changes at 1.0 is worth writing down while the reasoning is fresh:

- **The package version becomes meaningful.** `lattice:plugin-host@0.1.0` has
  never been bumped through any of the changes that broke components. Post-1.0
  a breaking change bumps it, and `lattice-wit = "1.2"` is how a plugin says
  what it targets.
- **The host can offer more than one.** The Component Model allows a host to
  provide several versions of an interface simultaneously; a plugin built
  against `@1.0` keeps working while the host also serves `@1.1`, until
  support is dropped on a stated policy. That is the real compatibility story
  and it costs a bindgen world and a boundary conversion per supported
  version — which is why it waits for a stable shape rather than paying that
  cost against an ABI still changing weekly.
- **Additivity does not help.** Worth stating so nobody plans around it:
  records are structural in the Component Model, so *adding a field* to a
  record that appears in a crossed signature is a breaking change. There is no
  "just be additive" escape; `transient-context` gaining `args` broke every
  built component exactly that way.

## 6. Recommendation

**(c) as the mechanism, (a) for out-of-tree repos, (b) as the repair, §4 as the
safety net.** They answer different questions and the failure needed all four:

- **(c)** stops the drift for every source the editor builds, which is every
  source the reported failure involved. Chosen by heuristic #1: it expresses the
  coupling that actually matters — a component is compiled against the host that
  will load it — at the layer that already owns the compiler invocation, and it
  needs no publish gate to be true today.
- **(a)** remains right where no editor is in the loop: a repo built by CI or by
  a stranger declares the ABI it targets, and a declared pin overrides the
  ambient refresh. It is also what becomes a version pin the day `lattice-wit`
  publishes.
- **(b)** repairs an install that is *already* broken, including the case
  neither (a) nor (c) can reach: a dead `init.wasm` that cannot rebuild
  anything, which is the knot the reported failure actually tied.
- **§4** makes the remaining breakage loud and self-healing where a source
  exists, and refuses honestly where one does not.

The original recommendation here was (a)-as-mechanism, written before it was
noticed that `build_plugin` is the single compiler-invocation path for every
local source. (a) is not wrong — it is the correct answer to a different
question, and it keeps that role above.

## 7. Paramount-goal alignment

**#2 Extensibility.** The goal at stake throughout. A plugin API that a plugin
cannot reliably obtain, and whose skew is silent, is an extensibility surface
in name only.

**#1 Performance.** Nothing here is on any hot path: a build-time copy, a hash
comparison at load, a message on failure.

## 8. Slice sketch

| Slice | What |
|---|---|
| WT.1 | `lattice-wit` crate (embedded files + `write_to`), zero dependencies |
| WT.2 | An out-of-tree plugin (org) takes the build-dependency; its `wit/` becomes gitignored generated output |
| WT.2b | The build service refreshes `wit/` before every cargo invocation (§3c); the scaffolds' copy becomes a seed |
| WT.3 | ABI fingerprint in `.build-stamp`; loader treats a mismatch as stale and rebuilds from source |
| WT.4 | `lattice --wit-sync` for repair; a failed instantiate reaches `*messages*` and `:plugins` |

WT.4's message is the one that would have saved the session that produced this
fragment, so it is not the tail of the list by importance.
