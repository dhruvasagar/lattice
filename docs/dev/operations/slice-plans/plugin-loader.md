# Phase 8 — Editor-side plugin loading (the plugin loader / manager)

> **Slice plan.** Sequencing, slice IDs, dependencies, status icons.
> Design contracts live in the design fragments — [`../../architecture/plugin-host.md`](../../architecture/plugin-host.md)
> (the exercised-trait → WIT-mirror spine, capability/fuel/crash model, per-seam
> rationale) and [`../../architecture/boot-composition.md`](../../architecture/boot-composition.md)
> (the Phase-A primitives → Phase-B `install(boot)` subsystem contract).
> Global ledger: [`../implementation.md`](../implementation.md) (Phase 8 row).

## What this phase is

Phase 7 shipped the **plugin host runtime** (`lattice-plugin-host`): the wasmtime
Component-Model engine, the `wit/` API package, the capability/fuel/crash-isolation
model, and every extension seam — each exercised end-to-end by guest fixtures. The
runtime is **not wired into the editor**: `lattice-host` depends only on the
wasmtime-free `lattice-plugin-api` catalog, never on `lattice-plugin-host` (the
introspection catalog `:describe-plugin-api` is all that is user-reachable today).
The empty-state proof point is the literal string the host returns:
`"No plugins are loaded. (The plugin loader is wired in at Phase 8.)"`
(`lattice-host/src/dispatch.rs:27697`).

**Phase 8 wires the finished runtime into the running editor.** It introduces the
`lattice-plugin-host` dependency into the editor graph for the first time, stands
the host up at boot, discovers plugins on disk, loads them, drains each seam's
contribution into the native registries, exposes the user-facing load/unload/reload
surface, and closes the one hot-path-sensitive gap (WASM decorations → per-buffer
cache → renderer).

**Not this phase:** Phase **8b** (bundled first-party plugins — fuzzy-finder, git,
grep, etc. as shipped components) and the full **modes-as-components repackaging**
(shipping built-in major/minor modes as WASM). Those consume the loader this phase
builds. `init.rs`-as-WASM config **is** in scope (it is the plugin lifecycle world's
first real consumer) but sequenced late, since it consumes the reload seam.

## Decomposition & dependency DAG

```
        ┌──────────────────────────────────────────────┐
        │  PL8.A  Host boot-wiring (root enabler)      │
        │  dep edge + subsystem install + service      │
        └───────────────┬──────────────────────────────┘
          ┌─────────────┼───────────────┬───────────────┐
          ▼             ▼               ▼               ▼
   PL8.B Discovery  PL8.C Load       PL8.E Decoration  (PL8.A also
   (scan dir,       ex-command       cache + renderer   unblocks all
    manifests,      surface          read-from-cache    below)
    load orchestr.) (:plugin-load…)  (hot-path)
          │             │
          └──────┬──────┘
                 ▼
        PL8.D  init.rs-as-WASM config  ──consumes──▶ reload seam (PH7.12)
                 │
                 ▼
        PL8.F  Intern-leak reclamation (rides with first reload consumer)
                 │
        ┌────────┴─────────┐
        ▼                  ▼
   PL8.G Modes-as-      PL8.H Plugin-manager
   components (bundled  UI / reload+health
   major/minor as WASM) surface (buffer-backed)
```

`PL8.A` is the root; everything else is dead code until the host is instantiated in
the editor process. `B`, `C`, `E` are independent children of `A`. `D` needs the
load path (`C`) and the reload seam. `G`/`H` are the later "as-components" +
management surface, and shade into Phase 8b.

---

## Key architectural decision (confirm before PL8.A) — where the loader lives

The loader is a cohesive subsystem: discovery (scan dir, parse `PluginManifest`
TOML), load orchestration (`compile → instantiate_plugin → activate`, then
`spawn_*` each seam, drive the actors on the runtime pool, drain each contribution
into its native registry, assemble the `PluginTeardown` token), the user surface
(`:plugin-load` / `:plugin-unload` / `:plugin-reload`), and the loaded-plugin state
(handles + teardown tokens + health, surviving past boot as a service). Three homes:

### (a) Inline in `lattice-host` (dispatch.rs + editor_boot.rs)
> **UX (higher court):** neutral — orchestration runs off the keystroke path.
> **Paramount goals:** protects #2 (extensibility) by the shortest wiring; sacrifices nothing at runtime.
> **Heuristic #1 (long-term fit):** *worst* long-term fit — it grows `Editor::` methods and a `LoadPlugin`/reload effect arm inside the host's dispatch match, exactly the accretion the mode-ownership acid test forbids.
> **Standing-rule check (mode ownership):** **fails** — handler bodies + loaded-plugin state would sit on the host, not the owning subsystem. This is the half-migration failure mode codified in CLAUDE.md.

### (b) Extend `lattice-plugin-host` with `install(boot)` + orchestration
> **UX:** neutral.
> **Paramount goals:** protects #2; risks blurring the runtime's substrate-neutrality.
> **Heuristic #1:** mediocre fit — folds editor-integration concerns (XDG discovery dirs, the `:plugin-load` ex-command, native-registry drain, `SubsystemBoot` wiring) into the *runtime* crate whose job is "own the wasmtime engine + seams." The runtime should stay callable by a headless test harness or a future non-editor host without dragging in ex-command/discovery policy.
> **Standing-rule check:** passes ownership, but at the cost of conflating two responsibilities in one crate (against "many small focused files / organize by feature").

### (c) New `lattice-plugin-loader` crate — the subsystem that composes runtime + registries  ⭐ recommended
> **UX:** neutral.
> **Paramount goals:** protects #2 (the editor-integration story gets a real home) and #4 (owns actor-drive on the runtime handle); sacrifices nothing.
> **Heuristic #1 (long-term fit):** **best** — the runtime crate stays "engine + seams"; the loader crate owns discovery + orchestration + the user surface + loaded-plugin state, depending on `lattice-plugin-host` + the registry crates (all already in plugin-host's graph). Clean, single-purpose, independently testable — the design-for-isolation shape.
> **Heuristic #2 (paramount, not other editors):** anchored on extensibility (#2) + the mode-ownership acid test, not on "consistent with X".
> **Heuristic #3 (third option):** this *is* the third option beyond the two obvious homes (host-inline / runtime-crate).
> **Standing-rule check (mode ownership):** **passes cleanly** — the acid test ("a new subsystem adds one `install` line + zero `Editor::` methods + zero host `Action` variants") is met: `lattice_plugin_loader::install(&mut boot)` is the single edit to `editor_boot.rs`; the `:plugin-load` handler body and loaded-plugin state live in the loader crate; contributions reach registries through the existing `SubsystemBoot` seams.

**Recommend (c)** because it protects heuristic #1 (genuinely-better long-term fit:
runtime substrate and editor-integration orchestration are distinct responsibilities
with distinct consumers) and satisfies the mode-ownership acid test that (a) fails
and (b) strains.

**Secondary decision — effect-handler placement.** The `:plugin-load` ex-command
(registered in `lattice-grammar/src/ex_commands.rs`, aliased in
`lattice-host/src/excommand.rs`, per the `:describe-plugin-api` precedent) emits an
`Effect`. Today subsystem effects (`DescribePluginApi`, …) are handled in the host's
`dispatch.rs` match. The mode-ownership rule pushes handler bodies into the owning
crate via the action-handler registry. **Recommendation:** the ex-command *parse
front-end* stays in `lattice-grammar` (grammar owns command surface), but the load
*action* is registered by the loader as an `ActionId` closure bound in
`lattice_plugin_loader::install` — so the body lives in the loader crate, not a new
host `dispatch.rs` arm. Confirm this shape at PL8.C; if the action-registry closure
path is not yet ergonomic for ex-command-triggered effects, fall back to a single
thin host effect arm that delegates to `service::<PluginLoaderHandle>()` and log the
debt (do **not** grow orchestration in the host).

---

## Slices

Status icons: ✅ done · 🚧 in progress · 📝 planned. Every non-trivial slice ships the
four artefacts (doc + bench-where-perf-relevant + test incl. failure modes + graceful
error handling).

### PL8.A — Host boot-wiring: the editor instantiates a WASM component  ✅
The root enabler. Introduces `lattice-plugin-host` into the editor graph and proves
the boot → load → lifecycle spine end-to-end with a fixture, loading *no* real
plugins yet.

- New crate `crates/lattice-plugin-loader/` depending on `lattice-plugin-host` +
  the registry crates; `lattice-host/Cargo.toml` gains a path dep on
  `lattice-plugin-loader` (transitively pulling `lattice-plugin-host`).
- `pub fn install(boot: &mut impl SubsystemBoot)` — constructs `PluginHost::new()`
  (or `with_dirs` in tests), wraps it in a `PluginLoader` with interior mutability
  for the loaded-plugin set, registers `PluginLoaderHandle = Arc<PluginLoader>` as a
  service (alias convention per the `ServiceRegistry` TypeId pitfall). One line added
  to the Phase-B install list (`editor_boot.rs` ~:534-575).
- Spine proof: drive one bundled no-op fixture component through
  `compile → instantiate_plugin → activate` on `boot.runtime_handle()`; assert it
  activates and reports through `PluginMetaRegistry` (already registered empty at
  boot, `editor_boot.rs:1434`).
- **Exit:** the editor boots with the plugin host live; `service::<PluginLoaderHandle>()`
  resolves; a fixture component activates; the renderer dep-guard
  (`lattice-plugin-host/tests/no_per_frame_wasm_guard.rs`) stays green (it guards
  *renderers*, and the transitive `host → plugin-host` edge is explicitly anticipated
  there); `cargo build -p lattice-cli` (TUI) and `--features gui` both link.
- **Note:** first time wasmtime enters the editor binary — watch build-time + binary
  size; record in the slice.

> **Landed 2026-07-15.** New crate `lattice-plugin-loader` (`lib.rs` +
> `install.rs`): `PluginLoader { host: Arc<PluginHost>, loaded: Mutex<Vec<…>> }`
> with `load_component(bytes, manifest, tier)` driving `compile →
> instantiate_plugin → activate` and recording the live instance;
> `PluginLoaderHandle = Arc<PluginLoader>` registered via
> `lattice_plugin_loader::install(&mut boot)` (one line at `editor_boot.rs:576`,
> after `lattice_lsp::install`). Graceful degradation: a `PluginHost::new()`
> failure logs a `warn!` and returns — the editor boots with **no plugin
> support**, never a failed boot.
>
> **Deviation from the spine-proof bullet (deliberate, crate-boundary):** the
> proof asserts through the **loader's own loaded-set** (`loaded_count` /
> `is_loaded`), *not* `PluginMetaRegistry`. The meta registry is a `lattice-host`
> type; the loader cannot name it without a dependency cycle, and populating it
> is a *contribution drain* — genuinely PL8.B work (a `PluginMetaSink`-style
> service the host registers and the loader looks up via `service::<T>()`, or a
> host-side read of the loaded-set). PL8.A keeps the loaded-set as the source of
> truth and defers the host-side projection to PL8.B. The boot install loads no
> fixture (loading a no-op plugin into every session is wrong); the spine proof
> is a `#[tokio::test]` in the loader crate driving its own runtime.
>
> **Tests:** `lattice-plugin-loader/tests/spine.rs` — (1) the no-op
> `plugin`-world fixture (referenced in place from
> `lattice-plugin-host/tests/fixtures/noop.wat`, single source of truth) loads
> through the full lifecycle and is recorded; (2) garbage bytes fail with a typed
> `PluginLoaderError::Host` and leave the loaded-set untouched (the
> graceful-skip contract PL8.B relies on). Boot pin
> `boot_regression_pins.rs::plugin_loader_service_present_at_boot` asserts the
> handle resolves after `Editor::boot`.
>
> **wasmtime footprint (recorded per the Note):** first crossing of wasmtime into
> the editor binary — now **unconditional** (not `gui`-gated): `cargo tree -i
> wasmtime` traces `wasmtime 46.0.1 → plugin-host → plugin-loader → host →
> ui-tui → cli`, so even the TUI build links it. Debug `target/debug/lattice`
> ≈ **133 MB**; clean incremental host rebuild ≈ 47s, `--features gui` link ≈
> 73s. No bench artefact: PL8.A is boot-only (runs once, off the keystroke
> path) — the perf-relevant slice is PL8.E (the hot-path decoration cache).

### PL8.B — On-disk discovery + load orchestration  📝
- Discovery: resolve the plugins dir (`dirs`-based, `<data>/lattice/plugins/`),
  scan for components + their `PluginManifest` TOML, parse (`PluginManifest::from_toml_str`).
- Orchestration in the loader: per discovered plugin, `compile → instantiate_plugin`
  with the manifest + `TrustTier::UserInstalled` (bundled → `Bundled`) + per-seam
  `PluginBudget`; `activate`; then `spawn_*` each declared seam, `tokio::spawn` the
  actor `run()` on the runtime handle, and drain the contribution into its native
  registry (`PickerRegistry::register_generator`, `GrammarContributionSet::register_all`,
  `ConfigRegistry` live-register, `ModeRegistry` + `MinorMode` keymap, `EventBus`
  subscriptions, `WasmCompletionSource` into a mode's `completion_sources()`).
- Assemble the `PluginTeardown` token from the returned per-seam tokens; store in the
  loader's loaded-set keyed by `PluginId`; populate `PluginMetaRegistry`
  (`register_plugin` / `register_plugin_name`).
- Graceful degradation: a plugin that fails to compile/instantiate/activate is
  logged + skipped (never aborts boot or other plugins); denied capabilities surface,
  never fatal.
- **Exit:** a plugin dropped in the plugins dir loads at boot and its contribution is
  live (`:list-plugins` shows it; a picker/grammar/completion contribution is
  reachable); a broken plugin is skipped with a logged reason.

### PL8.C — User-facing load/unload/reload ex-commands  📝
- `:plugin-load <path>`, `:plugin-unload <id|name>`, `:plugin-reload <id|name>` —
  parse front-end in `lattice-grammar/src/ex_commands.rs` (dashed/namespaced naming
  rule; no 1-2 letter shorts), alias in `excommand.rs`, action-handler bound in the
  loader (per the secondary decision above).
- Unload = `PluginTeardown::unload(&mut TeardownRegistries)` (reverses every surface);
  reload = unload + re-run the load orchestration (fresh `Store` → fresh, untripped
  `Quarantine`).
- **Exit:** load/unload/reload work interactively; `:list-plugins` / `:describe-plugin`
  reflect state; teardown removes every contributed surface (assert counts via
  `TeardownReport`).

### PL8.E — WASM decorations: producer → per-buffer cache → renderer  📝 (hot path — paramount #1)
The one UX-vigilant slice. The producer (`WasmDecorationSource::gutter_decorations`,
async) + `spawn_decoration_source` exist in the runtime; there is **no** per-buffer
cache on the host and the renderers read only the native sync `Mode::gutter_decorations`
trait.

- Add a per-buffer `Vec<GutterDecoration>` cache on the host (published via the
  `RenderState` snapshot mechanism, alongside `DiffSignRenderState` /
  `VirtualRowsRenderState` — **not** a per-frame read of WASM).
- Drive `WasmDecorationSource::gutter_decorations` **off the render path** on triggers
  (edit / scroll / diagnostic change), `spawn_blocking`/actor, write the result into
  the cache; on any producer `Err` keep the prior snapshot (no flicker).
- Both renderers merge the cached WASM decorations into the same partition they
  already walk (TUI `render.rs:3847`, GPUI `window.rs:1818`) — **lockstep per the
  cross-renderer rule**; end-of-slice `grep` audit for GPUI parity.
- **Exit:** a decoration-producing plugin's gutter marks paint in both renderers;
  keystroke→glyph latency bench shows no per-frame WASM on the hot path (the
  `no_per_frame_wasm_guard` invariant holds); a trapping producer keeps the last-good
  snapshot with zero flicker.

### PL8.D — `init.rs`-as-WASM user config  📝
- Load the user's `init.rs`-compiled component through `instantiate_plugin` with a
  `Bundled`-tier manifest (boot-capability set) and `activate` it during boot, after
  the native builtins register (so user keymaps/autocmds/commands layer on top).
- Consumes the reload seam (PH7.12) for `:reload-config` (unload + re-activate).
- **Exit:** a user `init.rs` that registers a keymap / autocmd / command takes effect
  at boot and survives `:reload-config`.

### PL8.F — Intern-leak reclamation  📝
- The interner leak (Low–Medium, no consumer until a reload path exists) is reclaimed
  as part of the first reload consumer (D/H). Small; lands *with* its consumer, not
  standalone.
- **Exit:** repeated reload does not grow the interner unbounded (assert via a
  reload-loop test).

### PL8.G — Modes-as-components (bundled major/minor as WASM)  📝 (shades into 8b)
- Ship a built-in mode as a WASM component through `spawn_mode_plugin` to validate the
  full mode seam end-to-end (the design's §5.8.3 "as components" goal). Built-in modes
  stay native by default; this proves the extension path.
- **Exit:** one mode loads as a component, registers its `MinorMode` keymap layer, and
  passes the mode-ownership acid test from the guest side.

### PL8.H — Plugin-manager surface (reload + health, buffer-backed)  📝 (shades into 8b)
- A `:plugins` buffer-backed view (everything-is-a-buffer) listing loaded plugins,
  health (quarantined? via `Event::PluginCrashed`), capabilities granted/denied, with
  reload/unload actions. Async status → headerline per the standing rule.
- **Exit:** the manager view lists plugins + health; a crashed plugin shows quarantined;
  reload/unload from the view work.

---

## Sequencing summary

**PL8.A → {B, C, E} → D → F → {G, H}.** Land A first (foundation, thin behavior, real
dependency de-risk). Then B+C give the real load path + user surface; E closes the
hot-path decoration gap on its own UX-vigilant slice. D brings `init.rs`. F rides D.
G/H are the "as-components" + management surface that bridge into Phase 8b.
