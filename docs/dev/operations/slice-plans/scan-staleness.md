# The boot scan checks staleness — slice plan (SS)

> Design: [`../../architecture/plugin-manager.md`](../../architecture/plugin-manager.md) §5b.
> Builds on [`archive/wit-ownership.md`](archive/wit-ownership.md) (WT.3 put the
> ABI fingerprint in the stamp; this teaches a third caller to read it).

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred.

**Status:** 📝 planned (2026-08-27). Specced, not started — parked behind org
work by choice, not blocked.

## Why

Three load paths, two of which check staleness. `init.rs` does
(`build_init_if_needed`) and every `require`d plugin does (`install_all`); the
boot scan of `~/.config/lattice/plugins/` does not — it loads whatever `.wasm` is
staged and never reads the stamp.

The case that matters is not the tidy one. **When `init.rs` fails to load,
`require` never runs, so nothing is checked at all**, and the scan loads every
stale artifact in turn. That is precisely the reported failure: a dead
`init.wasm` took org down with it, and org's own artifact was never reconsidered.
Had the scan checked stamps, org would have rebuilt against the current ABI on
the next boot with no `--wit-sync` and no hand-holding.

## The decision already made

Design §5b option **(c)**: read the stamp and build from a source that is
**already on disk**, without going through the resolver. `resolve_git` fetches
unless a rev is pinned and checked out, so any option that re-resolves turns a
pure-load boot into a network-dependent one — disqualifying, and the reason
(a) and (b) were rejected on merit rather than on size.

## Slices

| Slice | Description | Status |
|---|---|---|
| SS.1 | Decide how a scanned plugin expresses `pinned` | 📝 |
| SS.2 | `scan_build_state`: stamp check from the on-disk source, no resolver | 📝 |
| SS.3 | `discover_and_load` builds a stale scanned plugin before loading it | 📝 |
| SS.4 | The no-local-source case reports rather than builds | 📝 |

### SS.1 — pinning, first, because it gates the rest 📝

`pinned` lives on `RequiredSpec` and a scanned plugin has no spec. Two ways out:
a marker file beside the artifact, or pinning stays a `require`-only feature and
the scan always rebuilds a stale source.

**This is first deliberately.** "The editor rebuilt the artifact I deliberately
froze" is a worse failure than the silent staleness being fixed, and deciding it
after the build path exists means deciding it under pressure to keep what was
already written. Pick it cold.

### SS.2 — the check 📝

A function over `(artifact_dir, source_record)` answering *current / stale /
unknowable*, using `Stamp::parse` + `Stamp::current(source_dir)`. Pure, no
network, no resolver, no `&mut` — unit-testable without a toolchain the way
`build_plugin`'s cache logic already is.

`unknowable` is a real third answer, not an error: a `Bundled` source, a
`Prebuilt` one, a legacy stamp, or a source directory that is gone.

### SS.3 — the build 📝

In `discover_and_load`, between discovery and load: on `stale`, run
`build_plugin` on `spawn_blocking` and load the result. Failure keeps today's
behaviour — `StaleKept` loads the previous artifact, and the plugin still comes
up. A rebuild that fails must never cost the user a plugin that was working
five minutes ago (§5's graceful-failure clause).

Already-loaded plugins are skipped by the existing `is_loaded` guard, so a
`require`d plugin does not get checked twice.

### SS.4 — when there is nothing to build from 📝

Report and load. `:plugins` shows `stale`, a `warn!` names the plugin and why it
could not be refreshed (design §5b option (d), retained as (c)'s fallback). Do
not fetch — that is the disqualified path, and doing it "just this once for the
broken case" is how the network dependency arrives anyway.

## Tests worth naming now

- A scanned plugin with a matching stamp is **not** rebuilt — the warm-boot
  requirement, asserted from the third path the way `build.rs` asserts it from
  the first.
- A scanned plugin whose source changed rebuilds and loads the new artifact.
- A scanned plugin whose **ABI** differs rebuilds even with an untouched source
  — the WT.3 case, reaching the scan for the first time.
- **With `init.rs` broken, a stale scanned plugin still rebuilds and loads.**
  The headline claim; it must be driven with a genuinely failing init, not with
  init absent, because absent takes a different branch.
- A failed rebuild leaves the previous artifact loading, and says so.
- No source on disk → loaded as-is, reported stale, **no network call** —
  asserted against a git runner that panics if invoked, since "we did not fetch"
  is not observable any other way.
- A pinned artifact is not rebuilt (shape depends on SS.1).

## Not in scope

- **Re-resolving sources at boot.** Design §5b (a)/(b), rejected on the network
  cost. A user who wants a fresh checkout has `:plugin-reload` and the `b` chord
  in `:plugins`, both of which do go through the resolver because the user asked.
- **Core plugins.** They ship prebuilt and `Bundled` is not buildable; there is
  nothing to check.
