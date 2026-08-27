# `wit/` ownership and ABI skew — slice plan

> Design: [`../../architecture/wit-ownership.md`](../../architecture/wit-ownership.md).

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred.

**Status:** 🚧 in progress (2026-08-27).

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
  WT.4  `lattice wit sync` (repair) + a failed load reaches the user
```

WT.1 and WT.2 stop the drift at its source. WT.3 makes what is already on disk
self-heal. WT.4 is the repair path for the case the others cannot reach — a
dead `init.wasm`, which cannot rebuild anything, including itself.

| Slice | Description | Status |
|---|---|---|
| WT.1 | `lattice-wit`: embedded `wit/` + `write_to` + ABI fingerprint | ✅ |
| WT.2 | org migrates; `wit/` gitignored generated output | ✅ |
| WT.2b | Scaffolds — **blocked on a decision**, see below | ⛔ |
| WT.3 | Fingerprint in `.build-stamp`; ABI mismatch ⇒ rebuild from source | 📝 |
| WT.4 | `lattice wit sync`; a failed instantiate reaches `*messages*` / `:plugins` | 📝 |

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

## WT.2b — the scaffolds ⛔ blocked on a decision

`lattice --init` and `lattice --new-plugin` should emit the same `build.rs`
line and build-dependency. They cannot yet, and the reason is worth stating
rather than working around.

**A scaffold has nowhere to point the dependency.** `lattice-wit` is
unpublished (§"What this does NOT do"), so a generated `Cargo.toml` can name it
only by a path into a lattice checkout the user may not have, or by a git rev
that may not match the binary that generated the scaffold — reintroducing the
skew from the other side.

Worse, the two scaffolds want *different* answers:

- **`~/.config/lattice/init`** targets the editor the user runs. There, "the
  `lattice` on `PATH`" is not a weakness, it is the correct source — which
  argues for the WT.4 `lattice wit sync` path, invoked from the scaffold's
  `build.rs`.
- **A plugin repo** is built by CI and by other people, where a pinned crate is
  right and an ambient binary is not.

So the honest options are: emit `lattice wit sync` for init and a crate dep for
plugins (two mechanisms, each correct in its context); or keep both on the
one-shot copy until `lattice-wit` is published and then move both. Needs a
decision, so it is not being guessed at mid-plan. `write_wit_package` stays
until then and the scaffolds keep working exactly as they do.

## WT.3 — the fingerprint reaches the stamp 📝

`.build-stamp` records what the artifact was built *from*; it must also record
what it was built *against*. `BuildState` gains the case it is missing:

| State | Today | After |
|---|---|---|
| stamp matches, source unchanged | `Cached` | `Cached` |
| source changed | `Stale` ⇒ rebuild | `Stale` ⇒ rebuild |
| **ABI differs** | `Cached` ⇒ load, fail, silently | `Stale` ⇒ rebuild |
| ABI differs, no source | `Cached` ⇒ load, fail, silently | refuse, naming both |

Tests: an artifact stamped with a different ABI is `Stale` even when its source
is untouched; a matching one stays `Cached`; a stamp from a lattice predating
the field is `NotBuilt` rather than a false `Cached`.

## WT.4 — repair, and saying so 📝

Two halves, both about the failure being *reachable by a user*:

- **`lattice wit sync [dir]`** — rewrite the `wit/` of `~/.config/lattice/init`
  and every local plugin source from the embedded package. This is the only
  thing that unsticks a dead `init.wasm`, which cannot rebuild itself and which
  holds the `require` that would have rebuilt everything else.
- **A failed instantiate reaches `*messages*` and `:plugins`**, naming the
  plugin and the reason. One-shot and user-actionable, so `info!`-class, not
  `debug!` — the "LSP server attached" bar. A silently-absent plugin is
  indistinguishable from one that was never installed, which is exactly why the
  reported failure took a debugging session rather than a glance.

WT.4's message is the one that would have saved the session that produced the
design fragment, so it is not last by importance.

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
