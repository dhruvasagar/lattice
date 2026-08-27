# `wit/` ownership and ABI skew — slice plan

> Design: [`../../../../architecture/wit-ownership.md`](../../../../architecture/wit-ownership.md).

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred.

**Status:** ✅ complete (2026-08-27). WT.1–WT.4 all landed; nothing deferred.

Two things are explicitly *not* in scope and never were — multi-version ABI
support and publishing `lattice-wit` to crates.io, both gated on 1.0. See
"What this does NOT do" at the foot of this file for why neither is an open
slice.

## Why now

Three WIT changes in one day left the user's `init.wasm` and org's artifact
both unloadable, with nothing said anywhere: the editor opened, the file
opened, and org was simply absent — no language, no highlighting, no syntax
folds, no chords.

The knot is that `require("org")` lives **inside** `init.wasm`, so the thing
that rebuilds stale plugins was itself a stale plugin. Both had to be rebuilt
by hand, and the second could only be reached after the first was fixed.

## Sequencing

```
  WT.1  `lattice-wit` — the API package as a zero-dependency crate
  WT.2  scaffolds depend on it; `wit/` becomes generated output
  WT.3  ABI fingerprint in `.build-stamp`; a mismatch is STALE, not a load failure
  WT.4  `lattice --wit-sync` (repair) + a failed load reaches the user
```

WT.1 and WT.2 stop the drift at its source. WT.3 makes what is already on disk
self-heal. WT.4 is the repair path for the case the others cannot reach — a
dead `init.wasm`, which cannot rebuild anything, including itself.

| Slice | Description | Status |
|---|---|---|
| WT.1 | `lattice-wit`: embedded `wit/` + `write_to` + ABI fingerprint | ✅ |
| WT.2 | org migrates; `wit/` gitignored generated output | ✅ |
| WT.2b | The build service refreshes `wit/` before cargo | ✅ |
| WT.3 | Fingerprint in `.build-stamp`; ABI mismatch ⇒ rebuild from source | ✅ |
| WT.4 | `lattice --wit-sync`; a failed load reaches `*messages*` / `:plugins` | ✅ |

Every slice ships four artefacts (CLAUDE.md heuristic #5): doc, bench where a
hot path is touched (none here — build time, load time, and an echo), tests
covering the failure mode as well as the happy path, graceful error handling.
One slice, one commit, `scripts/precommit.sh <crate>` before each.

---

## WT.1 — `lattice-wit` ✅

The `wit/` package as a crate: `FILES` (name, contents), `write_to(dir)`, and
`ABI_FINGERPRINT`.

**Heuristic #6 — the dependency surface this crate carves out.** It must depend
on **nothing**. That is the whole mechanism: a plugin needs the API definition
at build time, and today the only way to get it is a checkout of lattice. A
crate that pulled in any editor crate would defeat its own purpose, because the
plugin would then be building the editor to compile against its API. What
breaks without it: nothing can obtain the WIT except by copying it, which is
the drift this plan exists to end. `lattice-cli`'s own embedding collapses into
it rather than being a second copy.

The **fingerprint is defined here**, not at either consumer, so the builder and
the loader cannot disagree about what "the ABI" is. Hand-rolled FNV-1a over the
sorted `(name, contents)` pairs — a change detector, not a security boundary,
and the zero-dependency rule is why it is not sha2. An adversarial collision is
not in this threat model; an accidental one is not reachable.

Excludes the same four world-only fixture files `lattice-cli` already excludes
(`auto-pair`, `init-fixture`, `multiseam-fixture`, `trampoline-fixture`) — a
plugin has no use for another plugin's world or the test fixtures'.

Tests: `FILES` is non-empty and contains the load-bearing package files;
`write_to` produces a directory that a `wit_bindgen` package resolve accepts;
the fingerprint is stable across calls and changes when a file's contents
change; the fixture worlds are absent.

## WT.2 — org migrates ✅

org's `build.rs` writes the package from the `lattice-wit` build-dependency;
its 31 vendored files are gone and `wit/` is gitignored. It is the only real
consumer, and a mechanism with no consumer is not proven.

**Verified from a genuinely empty `wit/`.** The first attempt appeared to work
and had not: `write_to` deliberately leaves files it does not own alone, so the
stale vendored copy was still satisfying the resolve. Deleting the directory
and rebuilding is what proved it — and also proved org needs none of the four
worlds `lattice-wit` excludes.

## WT.2b — the build service refreshes `wit/` ✅

This slice was blocked, and the block turned out to be the wrong question.

**The block.** A scaffold has nowhere to point a build-dependency: `lattice-wit`
is unpublished (§"What this does NOT do"), so a generated `Cargo.toml` could
name it only by a path into a checkout the user may not have, or by a git rev
that may not match the binary that generated the scaffold — the same skew,
arriving from the other side. And the two scaffolds wanted different answers:
`~/.config/lattice/init` targets the editor you run, where the ambient binary is
the *correct* source; a plugin repo is built by CI and by other people, where a
pinned crate is right and an ambient binary is not.

**What unblocked it** was noticing that `build_plugin`
(`crates/lattice-plugin-loader/src/build.rs`) is the single path by which the
editor builds every local source — init via `build_init_if_needed`, every
scaffolded plugin, every `require`d git or local tree — and that it shells out
to cargo itself. Both framings above asked what the scaffold's manifest should
say. The better question was who runs the compiler, and the answer is us.

So the refresh lives there: `lattice-plugin-loader` takes the zero-dependency
`lattice-wit`, and `refresh_wit_package` writes the canonical package into the
source as `build_plugin`'s first act. The component is compiled against the API
of the process about to instantiate it — not the `lattice` on `PATH`, which is
what design §3(b) was rightly rejected for, but the loading process itself.

**`write_to` became content-preserving, and that is load-bearing.** The refresh
runs *before* the staleness check, and staleness is mtime-based over a tree that
includes `wit/`. An unconditional write would move every mtime forward on every
boot, so every source would read as edited, so every plugin would rebuild from
cold on every start — precisely inverting the requirement the build cache exists
for. `refreshing_the_wit_package_does_not_invalidate_the_cache` is the test that
catches its loss from the side that pays for it.

**The scaffolds' copy stays, demoted to a seed.** `wit_bindgen::generate!` needs
the files on disk to expand, so without it rust-analyzer resolves nothing in a
freshly scaffolded `src/lib.rs` until the editor has built it once. Nothing
downstream depends on it staying current any more.

**Out-of-tree repos keep their build-dependency**, and it wins — `build.rs` runs
after the refresh, so a pin the repo declares overrides the ambient one. That
precedence is the right way round; WT.3 is what makes the resulting mismatch
legible rather than silent.

Cost, stated plainly: two lattice builds sharing one config home will
alternately rebuild each other's plugins once WT.3 lands. That is new thrash,
and it is correct thrash — each editor loads a component that works, where today
one of them silently loads nothing.

Tests: the package lands in a source that had no `wit/` at all (the fresh-clone
shape WT.2 gave org); a drifted file is repaired before the build; a warm boot
with an untouched source stays `Cached` and does not invoke the toolchain; and
in `lattice-wit`, an identical file is not rewritten while a drifted one is.

## WT.3 — the fingerprint reaches the stamp ✅

`.build-stamp` records what the artifact was built *from*; it now also records
what it was built *against*. The stamp becomes a two-field `Stamp` — two
prefixed lines, so it stays greppable by a human staring at a broken install and
so a later field costs no second format break:

```text
abi:1c4e9f2a70b3d581
source:mtime:1756…:files:12
```

| State | Before | Now |
|---|---|---|
| stamp matches, source unchanged | `Cached` | `Cached` |
| source changed | `Stale` ⇒ rebuild | `Stale` ⇒ rebuild |
| **ABI differs** | `Cached` ⇒ load, fail, silently | `Stale` ⇒ rebuild |
| ABI differs, pinned (no rebuild allowed) | `Cached`, silently | `Cached` + a named `warn!` |
| stamp predates the field | `Cached` on the source alone | `NotBuilt` ⇒ rebuild |

**The last row of the original table said "refuse, naming both". It is wrong,
and the slice does not do it.** The fingerprint hashes the whole package, so it
moves when *any* file changes — including files the plugin never imports. A
mismatch therefore means "this may not load", not "this cannot load", and
refusing on it would stop plugins that work perfectly well. A pin exists
precisely to say *keep this build*, so the honest response to a coarse signal is
to load the artifact and put the skew on record: `warn!` naming both
fingerprints, one-shot and user-actionable. If the component then does fail to
instantiate, WT.4 names that failure and this line is already there explaining
it. Refusing would have traded a silent failure for a confident wrong one.

An unparseable stamp is `NotBuilt` rather than a match, in both the build
service and `:plugins`. That is the conservative direction and it matters:
reading a legacy stamp as agreement would keep exactly the artifacts most likely
to be skewed — the ones built before anyone was tracking the ABI.

Tests: an untouched source whose stamp carries a foreign ABI rebuilds, records
the ABI it actually built against, and *settles* — the boot after does not
rebuild again; a legacy source-only stamp is rebuilt rather than trusted; a
pinned artifact with a skewed ABI still loads and the pin is honoured; the stamp
round-trips and both truncated forms fail to parse.

## WT.4 — repair, and saying so ✅

Two halves, both about the failure being *reachable by a user*.

### `lattice --wit-sync [DIR]`

Spelled as a flag, not the `lattice --wit-sync` subcommand the plan drafted: this
CLI is flag-shaped throughout (`--scaffold-init`, `--scaffold-plugin NAME`) and
has a positional `FILE` argument that a bare `wit` subcommand would be
ambiguous against. Same command, spelled the way the surface is already spelled.

With no `DIR` it sweeps `~/.config/lattice/init` plus every immediate child of
the plugins dir; with one it syncs that directory alone, consulting no config
home, so it works against a checkout anywhere.

**Why it survives WT.2b covering every build.** `init.wasm` holds the `require`
that installs and rebuilds everything else. When `init.wasm` itself will not
instantiate, nothing runs, so nothing rebuilds — including `init.wasm`. The
thing that repairs stale plugins was itself the stale plugin, and that is the
knot the reported failure actually tied. This is the one command that cuts it,
and it needs nothing to load, boot, or build first.

**It deliberately does not build.** A user whose install is broken wants the two
steps separate: repair the API definition, then watch the build succeed or fail
on its own terms. Folding a build in would bury the second failure inside the
first. A directory that is not a cargo project is skipped *by name* in the
report rather than silently — sweeping the plugins dir is a convenience, and the
one folder a user cared about must not disappear into a success total.

### A failed load reaches the user

Two surfaces, because they answer different questions — the log says a thing
went wrong just now; `:plugins` answers "why is org not here?" asked ten minutes
later.

**`*messages*`:** the boot path's `Err` arm for `init.rs` was a single `debug!`
reading *"no user init.rs loaded"*, and **that line was the silent failure**. It
covered two unrelated situations: the user has no `init.rs` (normal,
uninteresting) and the user has one that would not load (the most consequential
thing that can happen at boot, since `init.rs` holds the `require` for
everything else). A `plugin.toml` in the init dir tells them apart — if one is
there the user meant to have config, so its absence is a failure they must be
told about, at `warn!`, naming the repair.

**`:plugins`:** a new `FailedLoad { name, dir, error }` set, rendered as a
trailing section. Trailing because the interactivity layer maps
`cursor.line - HEADER_LINES` into the loaded list, so a section inserted above or
between rows would put `u` / `r` / `b` on the wrong plugin — and a failed entry
has no host id to act on anyway. Held separately from `PluginStatus` rather than
as another `PluginHealth` variant: a failed load has no id, no granted
capabilities, no tier that was ever applied, and inventing all three to fit the
shape would produce rows with no plugin behind them.

In memory, not on disk — a load failure is a fact about *this* boot against
*this* editor, and persisting it would mean showing a user an error about a
plugin they have since rebuilt. Recording replaces rather than appends, and a
successful load clears: a stale "failed" row is a worse lie than no row.

Tests: a broken plugin is recorded with name, directory and reason; retrying it
three times leaves one row; a genuine successful load clears it (driven through
a real load with the `modes-guest` fixture rather than a test-only hook, because
a hook proves the bookkeeping and leaves the wiring untested); a path that is
not a plugin is reported to the caller but files nothing; `:plugins` renders the
section after every loaded row, omits it entirely when nothing failed, and shows
it even when *nothing* loaded — which is the combination the reported failure
actually produced.

---

## What this does NOT do

- **Multi-version support.** A host serving `@0.1` and `@0.2` simultaneously is
  the post-1.0 story (`plugin-host.md` §12: the WIT is unstable until ≥3 real
  plugins have exercised it; SemVer only post-1.0). Pre-1.0 the answer to "how
  does an older plugin work" is *it does not* — by policy — so this plan makes
  that loud and self-healing rather than pretending otherwise.
- **Publishing to crates.io.** `lattice-wit` is a path dependency while the
  editor is unreleased. Publishing is what makes an out-of-tree plugin able to
  pin it, and it waits for the same 1.0 gate.
