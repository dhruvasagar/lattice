# Plugin manager — declarative sources, build-on-boot, use-package for WASM

> **Design fragment.** Contracts, data model, rationale, rejected alternatives,
> paramount-goal alignment. Slice sequencing lives in the slice plan
> ([`../operations/slice-plans/plugin-loader.md`](../operations/slice-plans/plugin-loader.md)).
> Sibling fragments: [`plugin-host.md`](plugin-host.md) (the seam host + capability
> model), [`config-and-init.md`](config-and-init.md) (init.rs lifecycle + the
> `on-plugin-loaded` deferred-config pattern the enablement reuses),
> [`plugin-auto-pair.md`](plugin-auto-pair.md) (the first consumer).
>
> **Status: 📝 designed, not built.** A redesign of the existing loader
> (`lattice-plugin-loader`) + `:plugins` view (`lattice-plugin-manager`) to add
> what they lack: **where a plugin comes from** (a git/local *source*) and **how a
> missing artifact is produced** (*build-on-boot*). The load / unload / reload /
> discovery machinery and the interactive `:plugins` view are unchanged — this
> adds a resolve→build→cache layer *in front of* them, plus a `require`
> declaration API (use-package for WASM).

## 1. Why

Today a plugin must already exist as a built `.wasm` under
`~/.config/lattice/plugins/<name>/`, placed there by hand. There is no notion of
*where it came from* or *how to (re)produce it*. The user wants the emacs
`use-package` / `lazy.nvim` model: **declare a plugin and its source (a git repo
or a local directory); the editor builds it on first boot if the compiled
artifact is missing, and just loads it when it's present.** Plugins ship and
version independently of the editor binary.

**Explicitly rejected: `include_bytes!` embedding.** Compiling plugin bytes into
the lattice binary couples plugin releases to editor releases and bloats the
binary — plugins ship separately (§11). The artifact cache is the filesystem
(`~/.config/lattice/plugins/<name>/<name>.wasm`), not the binary.

## 2. What exists vs. what's new

**Reused, unchanged** (`lattice-plugin-loader`): `discover` (scan the plugins
tree), `discover_one` (read one dir's `plugin.toml` + sole `.wasm`),
`load_discovered` / `load_path` (compile + instantiate the seams), `unload`,
`reload`, the health/quarantine plumbing, and the whole `:plugins` interactive
view (`lattice-plugin-manager`: reload/unload/describe chords). These operate on a
*resolved directory containing a built `.wasm`* — that contract is preserved.

**New — the manager layer that produces that directory:**

1. A **plugin source** — `Local(path)` or `Git{url, rev}` (or `Prebuilt{url}`,
   §7) — resolved to a source directory on disk.
2. A **build service** — a source dir (a `wasm32-wasip2` component cargo project)
   → the cached `<plugins>/<name>/<name>.wasm`. Builds only when the cache is
   missing or stale (§5); **off the boot thread**; graceful when the toolchain is
   absent (§7).
3. A **`require` declaration API** — init.rs (or a TOML list, §3) declares
   `(name, source, …)`; the host resolves → builds → loads → optionally enables.

The pipeline is a prefix on the existing load path:

```
declare(name, source)  →  resolve source dir  →  build if <name>.wasm missing/stale
                          →  [existing]  discover_one(cache dir)  →  load_discovered  →  enable?
```

## 3. Where the declaration lives — init.rs `require` (recommended)

**Recommendation: programmatic, in init.rs**, via an imported host function —
use-package is programmatic (conditional loading, per-plugin config), and it fits
the standing principle *logic stays code; static settings stay declarative* (the
user's `init.rs`-is-config model). The bootstrapping subtlety (§6) is real but
resolvable.

```wit
// A new `plugin-manager` seam, imported by the init world (and any config plugin).
interface plugin-manager {
    require: func(spec: plugin-spec);

    record plugin-spec {
        name: string,
        source: plugin-source,
        // use-package sugar: enable this minor mode once the plugin loads
        // (desugars to the CI.5 on-plugin-loaded → enable-mode handler; the host
        // never learns the mode-id statically — feedback_mode_owns_its_surface).
        enable-mode: option<string>,
        // pin a build: skip the rebuild-on-change check, only build if absent.
        pinned: bool,
    }
    variant plugin-source {
        local(string),          // a directory path (built in place, not copied)
        git(git-source),
        prebuilt(string),       // a URL to a ready .wasm component (§7)
    }
    record git-source { url: string, rev: option<string> }
}
```

init.rs body (the shipped default, §9):

```rust
require(PluginSpec {
    name: "auto-pair".into(),
    source: Source::Local(bundled_plugin_dir("auto-pair")), // → Git/Prebuilt when shipped
    enable_mode: Some("auto-pairs-mode".into()),
    pinned: false,
});
```

The host **records** each `require` during the init.rs `register` export (the
`register-grammar` / `register-events` precedent — no work on the export), then
drains the recorded specs after init.rs returns and runs the pipeline (§2)
off-thread. A plugin's contributions appear a frame or two after boot — the
eventual-consistency the UX contract already permits for plugin cold-start.

> **`require` is the *user*-plugin surface.** Core plugins (the ones that ship
> with lattice) are NOT `require`d — they are **discovered** from a runtime root
> and enabled by a config gate (§7). `require` exists for plugins the *user*
> declares with a source; a fresh editor with no user init.rs still gets its core
> plugins.

> **Rejected: a TOML plugin list.** `[[plugin]] name=… source=…` is simpler (no
> bootstrapping) but not programmable — no `when(cfg)`, no per-plugin setup. It
> loses the use-package expressiveness the user asked for. A TOML list *may* land
> later as declarative sugar over the same `require` for the trivial case, but the
> `require` API is canonical.

## 4. The source model + cache layout

- **`Local(path)`** — the source dir is `path` (a cargo project). Built **in
  place** (the artifact still caches under `<plugins>/<name>/`; the source tree is
  not copied). The dev + monorepo case; auto-pair uses this today (§9).
- **`Git{url, rev}`** — cloned/fetched into a **source cache**
  `~/.cache/lattice/sources/<name>/` (checked out at `rev`, default the remote
  head), then built like `Local`. A re-`require` fetches + rebuilds only when the
  resolved rev differs from the cached one (or `pinned` skips the fetch).
- **`Prebuilt{url}`** — download the `.wasm` straight into the cache; **no build,
  no toolchain** (§7).

There are **two plugin roots** (§7): the user root below (the `require`+build
cache) and the **runtime root** that ships with lattice (core plugins, prebuilt).

```
<runtime>/plugins/<name>/          # CORE root — ships with lattice, prebuilt, read-only (§7)
    plugin.toml
    <name>.wasm

~/.config/lattice/plugins/<name>/  # USER root — the require/build cache
    plugin.toml           # the manifest (from source, or synthesized for Prebuilt)
    <name>.wasm           # the built/downloaded component  ← discover_one reads this
    .build-stamp          # source rev / mtime the artifact was built from (§5)
~/.cache/lattice/sources/<name>/   # git checkouts (Git sources only)
```

Once the artifact + manifest are in either root, the **existing** discovery /
load path takes over unchanged. A plugin placed there by hand (no `require`) still
loads exactly as today — the manager layer is additive.

## 5. The build service — build only when stale, off-thread, never a boot stall

- **Input:** a source dir. **Output:** the cached `<name>.wasm`.
- **Build:** invoke the component toolchain (`cargo build --target
  wasm32-wasip2 --release` + the component step the plugin build scripts already
  use) via `spawn_blocking` — **never on the boot or actor thread** (a cold build
  is seconds-to-minutes; boot must not wait). The plugin loads when the build
  completes (eventual consistency); a build in flight shows in the `:plugins` view
  (§8).
- **Staleness:** the `.build-stamp` records the source rev (Git) or a content hash
  / max-mtime (Local) the artifact was built from. Build iff the artifact is
  missing OR the stamp differs (and not `pinned`). So a warm boot with an
  unchanged source is a pure load — no rebuild, the user's core requirement.
- **Graceful failure (four-artefact clause):** a build failure (no toolchain,
  compile error, clone failure) is a **logged skip** that surfaces in `:plugins`
  and `*messages*` — never a failed boot, never a panic. If a *stale* rebuild
  fails but a previous artifact exists, the old artifact keeps loading (a broken
  new revision doesn't take the plugin down).

## 6. Bootstrapping — init.rs is built by the same service

init.rs is itself a `wasm32-wasip2` component. Today it must be pre-built. Under
this design the **same build service** builds init.rs first (its source dir is
`~/.config/lattice/init/`, a cargo project), then loads it, then drains its
`require` specs. One build primitive, two callers (init + every plugin). This
removes the "user must run cargo by hand" step for init.rs too — the editor
rebuilds init.rs on change (the PL8.D.4 init-watcher already re-loads a rebuilt
`init.wasm`; here the rebuild becomes the editor's job).

Ordering (extends config-and-init.md §3): **discover + load core plugins (runtime
root, §7) → build+load init.rs → drain its `require`s → resolve/build/load each
user plugin → fire `plugin-loaded` → the `<plugin>.enabled` gate (§7) enables
declared modes, and init.rs's `on-plugin-loaded` handlers run.** init.rs's
subscriptions are live before any *user* plugin loads, so a deferred handler can't
miss its plugin; a core plugin's mode enables from its own gate independent of any
init.rs.

## 7. Core plugins — how they ship (the runtime root, prebuilt, discovered)

Core plugins (auto-pair, and future bundled ones) ship **with** lattice yet must
work on a fresh machine with **no toolchain and no network** on first boot. The
model — the batteries-included pattern every editor uses (VSCode's built-in
extensions, Helix's `runtime/`, nvim's `$VIMRUNTIME`) — is **two plugin roots**:

- **Runtime root — core plugins, prebuilt, read-only.** Ships with the
  distribution as `.wasm` files (NOT embedded — §11), discovered at boot at the
  `Bundled` tier (pre-granted). No build, no network, no toolchain: the artifacts
  are already there.
- **User root — `~/.config/lattice/plugins/`.** The `require`+build cache (§2–§5):
  Git/Local source builds and Prebuilt downloads land here, `UserInstalled` tier.

**Finding the runtime root — a search path, not one fragile exe-relative path**
(the reason a naive `core-plugins/`-next-to-the-exe was rejected, §11; a search
path is the standard fix):

```
$LATTICE_RUNTIME                             (explicit override)
  → <compile-time install prefix>/share/lattice   (distro packagers set the prefix at build)
  → <exe-dir>/../share/lattice                (relocatable tarball / .app bundle)
  → <workspace>/runtime                       (dev, running from target/)
```

First hit wins. Boot discovers `<runtime>/plugins/` **in addition to** the user
root — the existing discovery path, pointed at a second dir.

**How the prebuilt core wasm is *made* — at lattice-*build* time, not boot, not
embedded.** A packaging step (`cargo xtask build-core-plugins`, or release CI)
runs the same `wasm32-wasip2` component build the plugin `build.rs` already does
for each `plugins/<name>/`, staging the components into the runtime root. Dev
points the search path at the workspace's built artifacts. So a core plugin is a
file that ships beside the binary — never `include_bytes!`, never built on the
user's first boot.

**Enablement — a per-plugin config gate (decision (i), 2026-07-20).** A discovered
core plugin's minor mode does not self-activate (the CI.3 available-but-off rule
stands — a mode never forces itself on). Instead the plugin **declares its
default mode in its manifest** (`default_mode = "auto-pairs-mode"`), and the
manager auto-registers a bool option **`<plugin-id>.enabled`** (default `true`)
that gates it: on load (and on any change to the option) the manager enables /
disables the declared mode via the CI.4 `ModeEnablementRequested` path. So a fresh
editor auto-pairs out of the box, `:set auto-pair.enabled=false` turns it off, and
the **host never learns a mode-id statically** — the plugin's manifest names it
(the mode-ownership rule holds). This is the general mechanism for *any* plugin
(core or user) that declares a default mode; the init.rs `on-plugin-loaded →
enable-mode` handler (CI.5) remains for programmatic / conditional enablement of
modes a plugin did not declare as default.

**The toolchain reality — only user `Git`/`Local` sources need it.** Build-on-boot
needs `rustup` + `wasm32-wasip2`, but that's only for a *user* who declares a
source build. Core plugins are prebuilt (above), so a no-Rust user still gets
them. A user `Git`/`Local` spec with no toolchain degrades to "unavailable —
install the toolchain, or the plugin offers a `Prebuilt`" (logged, surfaced in
`:plugins`), never a hard failure; `Prebuilt{url}` (a released `.wasm`, downloaded
+ cached, no build) is the no-toolchain path for user plugins too.

## 8. The `:plugins` view — source + build status

The existing view (`lattice-plugin-manager`) gains columns/state for the new
lifecycle: **source** (`local` / `git@rev` / `prebuilt`), **build state**
(`cached` / `building…` / `build-failed` / `stale`), and a **build/rebuild chord**
(force a rebuild of the plugin under the cursor). Async-build progress surfaces via
the buffer's headerline (the async-buffer-status-in-headerline rule), not a status
line. Reload already re-instantiates; a new "rebuild" is reload + a forced build.

## 9. auto-pair as the first consumer (AP.4, reframed)

AP.4 stops being "compile auto-pair into the binary" and becomes "**auto-pair is
the first *core plugin*** (§7)." It ships as a **prebuilt** `.wasm` in the runtime
root (staged there by the `xtask`/release plugin build), is **discovered** at boot
at the `Bundled` tier, and its `plugin.toml` declares `default_mode =
"auto-pairs-mode"`. The manager auto-registers `auto-pair.enabled` (default
`true`), so a fresh editor auto-pairs out of the box with **no user init.rs, no
toolchain, no network**. No `require` is involved — auto-pair is core.

Exit (updated from the AP.4 sketch): a fresh editor auto-pairs out of the box;
`:plugins` lists auto-pair with `source = bundled`, `build = cached`; `:set
auto-pair.enabled=false` turns it off (its mode deactivates, the plugin stays
loaded); `:set auto-pair.style=manual` flips the style live. The `require`+build
path (§2–§5) is proven separately by a *user* plugin from a `Local`/`Git` source.

Because auto-pair is a discovered core plugin, the **default-init-delivery question
disappears for AP.4** — no default init.rs is needed to enable it (that was the (i)
vs (ii) choice; (i) won). A shipped default init.rs remains a separate, optional
config-and-init.md concern for *other* defaults, not a dependency of core-plugin
enablement.

## 10. Paramount-goal alignment

- **#2 Extensibility.** *The* distribution story — a plugin is a git URL, built or
  downloaded on demand, versioned independently of the editor. use-package for
  WASM. The seam host (plugin-host.md) is unchanged; this is the layer above it.
- **#1 Performance.** Builds + clones run off the boot/actor thread
  (`spawn_blocking`); a warm boot with unchanged sources is a pure load (no
  rebuild). Nothing on the keystroke path.
- **#4 Asynchronicity.** Resolve/build/load is an async pipeline; a plugin appears
  when ready (eventual consistency), boot never blocks on a cold build.
- **UX (higher court).** No embedded-binary bloat; core plugins work offline with
  no toolchain (prebuilt in the runtime root); a no-toolchain user still gets
  *user* plugins via `Prebuilt`; a broken rebuild never takes a working plugin
  down; all failures are logged skips surfaced in `:plugins`, never a failed boot.

## 11. Rejected alternatives

- **`include_bytes!` (compiled-in).** Rejected by the user: couples plugin
  releases to editor releases, bloats the binary, and plugins should ship
  separately. The FS cache (user root) + the runtime root (core) are the artifact
  stores.
- **A single hardcoded `core-plugins/`-next-to-the-exe path.** Rejected — resolving
  one fixed path relative to `current_exe()` is fragile across dev / installed /
  `.app` / packaged layouts. The runtime root (§7) is the *right* idea done right:
  a **search path** (`$LATTICE_RUNTIME` → build-time prefix → exe-relative → dev
  workspace), the standard editor mechanism, not one brittle path.
- **A TOML-only plugin list.** Rejected as the *canonical* surface (not
  programmable); may return as sugar over `require` (§3).
- **Build on the boot/actor thread.** Rejected: a cold build is seconds-to-minutes;
  it must be `spawn_blocking`, boot loads what's already cached and picks up the
  fresh build when it lands.
- **`require`-driven core plugins with a `Bundled` source (option (ii)).** Rejected
  in favour of discovery + a `<plugin>.enabled` gate (option (i), §7): it dragged
  in the default-init-delivery question and put shipped config in the loop for
  something the editor already knows shipped. Core = discovered; `require` = user.

## 12. Settled decisions

1. **Declaration surface** — init.rs `require` for *user* plugins (§3); core
   plugins are **discovered**, not required (§7). TOML list rejected as canonical.
2. **Toolchain model** — core plugins prebuilt (no toolchain); user `Git`/`Local`
   build-from-source **plus** `Prebuilt` download for the no-toolchain path (§7).
3. **Core-plugin shipping** — prebuilt `.wasm` in a runtime root found via a search
   path, staged by an `xtask`/release build; discovered at `Bundled` tier; mode
   enabled by a manifest `default_mode` + auto-registered `<plugin>.enabled`
   option (default true). (§7, §9)
4. **auto-pair** — the first core plugin: `Local` in-repo build for dev → the
   runtime root for shipping; `auto-pair.enabled` gates `auto-pairs-mode`. (§9)

## 13. Slices

See the slice plan
([`../operations/slice-plans/plugin-loader.md`](../operations/slice-plans/plugin-loader.md))
once sequenced. Two tracks — the **core-plugin track ships auto-pair out of the
box first** (no build service needed), then the **user require+build track**:

*Core track (delivers AP.4):*
- **PM.1** runtime-root search path (`$LATTICE_RUNTIME` → prefix → exe-relative →
  workspace) + boot discovery of `<runtime>/plugins/` at `Bundled` tier.
- **PM.2** the `xtask build-core-plugins` staging step (dev + release) that
  produces the prebuilt core `.wasm` into the runtime root.
- **PM.3** manifest `default_mode` + auto-registered `<plugin>.enabled` gate →
  enable/disable the declared mode via `ModeEnablementRequested`.
- **PM.4** auto-pair as the first core plugin (AP.4): manifest `default_mode`,
  `auto-pair.enabled` default true, discovered + enabled out of the box.

*User track (use-package):*
- **PM.5** build service (source dir → cached wasm, `.build-stamp`, `spawn_blocking`,
  graceful) → **PM.6** source resolver (`Local` → `Git` → `Prebuilt`) → **PM.7** the
  `require` seam + init.rs `require`-drain + init.rs-built-by-the-service
  bootstrapping → **PM.8** the `:plugins` source/build columns + rebuild chord.
