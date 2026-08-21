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

### PL8.B — On-disk discovery + load orchestration  ✅
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

> **Progress.** A seam can only register at runtime once its native registry is
> runtime-mutable, so PL8.B lands as a **registry-conversion → drain** pair per
> seam (the enabler makes the registry an `ArcSwap` handle + registers it as a
> boot service; the drain consumes it in the loader):
>
> | Seam | Registry conversion (enabler) | Drain (consumer) |
> |------|-------------------------------|------------------|
> | Discovery (`discover` + `discover_and_load`) | — | ✅ `e32aff53` |
> | Picker | ✅ B1 `31dbd577` (`PickerRegistryHandle`) | ✅ `drain_picker` `e32aff53` |
> | Config (already interior-mutable) | — | ✅ `drain_config` `a4001d9c` |
> | Events (`EventBus`, already shared) | — | ✅ `drain_events` `a4001d9c` |
> | Mode | ✅ B2 `aede19af` (`ModeRegistryHandle`) | ✅ `drain_mode` |
> | Grammar | ✅ B3a `0b6baded` (Box→Arc spec closures) + B3b `14fe3ce8` (`CommandRegistryHandle`) | ✅ `drain_grammar` |
> | Completion | ✅ option A (rides a loader-owned universal carrier mode's `completion_sources()`, not a standalone registry) | ✅ `drain_completion` |
>
> **`drain_grammar` (landed).** The grammar seam is the **synchronous** one (the
> PH7.7 fork): `instantiate_grammar_plugin` drives the guest's `register-grammar`
> and returns a `GrammarContributionSet` whose specs each carry a sync trampoline;
> the command registry itself owns the guest `Store` (inside the boxed
> trampolines), so there is **no** actor `run()` loop — the dispatcher fires the
> trampoline on keystroke off a wait-free `.load()` snapshot (the `DocumentActor`
> read path B3b put in place). Registration is **load → clone → register → store**
> (*not* `rcu`, because `register_all` consumes the set — the specs are non-`Clone`;
> B3a made `CommandRegistry: Clone` so the snapshot clone is cheap). `LoaderServices`
> gains a `command_registry: Option<CommandRegistryHandle>` captured from the boot
> service (`editor_boot.rs:1500`); a missing handle degrades the grammar seam to a
> logged skip (`NotWired("grammar")`), never a boot abort. Tests:
> `grammar_drain.rs` — (1) a discovered grammar plugin registers `down-n` /
> `to-cursor` / `fails` with unforgeable `SourceLayer::Plugin` provenance and a
> loaded motion dispatches through `execute_motion_only` off the snapshot (line
> 1 + count 3 → line 4); (2) a loader with no wired command registry skips the
> plugin without panicking. No new bench: the drain is load-time (off the
> keystroke path); the hot-path guard is B3b's `.load()` + the existing
> `plugin-host/tests/perf_ratchet.rs::grammar_round_trip_stays_within_ceiling`.
>
> **`drain_mode` (landed).** A mode plugin's minor modes register into B2's
> runtime-mutable `ModeRegistryHandle`, each mode's declared keymap binding
> landing in its own gated `MinorMode` layer. A registered mode is **declarative
> data** (id / kind / activation policy / capabilities + keymap bindings to
> *existing* commands), so after `spawn_mode_plugin` copies it into the registry
> the guest `Store` drops — no actor task, no live callback, nothing to keep
> alive (teardown, PL8.C, removes modes + layers by `plugin_id`). Registration
> RCUs the mode registry **load → clone → spawn → store** (not `rcu`:
> `spawn_mode_plugin` takes `&mut ModeRegistry` across its async `register-modes`,
> so a local owned snapshot clone keeps the borrow sound; B2 made `ModeRegistry`
> an `ArcSwap` handle + `Clone`). `LoaderServices` gains `mode_registry:
> Option<ModeRegistryHandle>` + `keymap: Option<KeymapHandle>`; the keymap handle
> is now a boot service (`editor_boot.rs`, next to the command-registry service),
> and the drain reads a `CommandRegistry` snapshot for bind-time command
> resolution. `spawn_mode_plugin` now returns `(PluginId, Vec<ModeId>)` (was
> `Vec<ModeId>`) so the loader records provenance + teardown-by-id, consistent
> with the other seams. A missing service degrades the modes seam to a logged
> skip (`NotWired("modes")`), never a boot abort. Tests: `mode_drain.rs` — (1) a
> discovered mode plugin registers `git-blame-mode` + `lsp-lens-mode` (the
> mis-suffixed `not-suffixed` rejected by the `-mode` gate), its `<C-s>` binding
> resolves only when its mode is active, and provenance is recorded; (2) a loader
> with no wired mode registry skips the plugin without panicking. No new bench:
> mode registration is load-time declarative work, off the keystroke path.
>
> **`drain_completion` (landed, option A — confirmed with Dhruva).** Completion
> is mode-attached across the whole editor (the aggregator
> `recompute_active_completion_sources_for` walks the mode registry calling
> `completion_sources()`; LSP rides the LSP mode, snippets ride the snippet
> mode), so a plugin source rides a **mode**, not a parallel registry. Since
> `ModeRegistry` stores `Arc<dyn DynMode>` (immutable post-registration), the
> loader owns a tiny `PluginCompletionMode` (universal minor) that carries the
> wrapped source and registers it — no post-hoc mutation, no join to a separately
> declared mode, no plugin-host API change. `impl AsyncCompletionSource for
> WasmCompletionSource` (plugin-host) is the missing adapter: `produce_async`
> runs the async guest `generate` on the source's actor (spawned on the
> multi-thread runtime, off the keystroke path) and pushes candidates to the
> sink; matching / ranking / annotation stay native, so paramount #1 holds. The
> source is wrapped as a `CompletionSourceContribution` (`Async` kind, default
> priority 100 — below LSP 200 / snippets 150; a per-plugin priority override is
> future work) and the carrier mode is RCU-registered into B2's
> `ModeRegistryHandle`. **Zero host edits** — mode-registry / bus / runtime were
> already boot services, so the acid test holds trivially. Runtime-visibility
> caveat: a universal mode contributes only once active + the source cache
> recomputed (on mode transitions); at boot, discovery precedes buffers so the
> first cache build includes it — a plugin loaded after buffers are open needs a
> re-activation + recompute pass that lands with PL8.C. Graceful degradation: a
> missing service → `NotWired` skip; a carrier-mode id collision aborts the actor
> + skips loudly; a spawn/connect trap → `PluginLoaderError::Host`. Tests:
> `completion_drain.rs` — (1) a discovered completion plugin registers the
> universal carrier mode whose `completion_sources()` surfaces the `keywords`
> source as an async contribution at priority 100, with provenance recorded; (2)
> a loader with no wired mode registry skips it without panicking. The
> async-produce path itself is proven by `plugin-host/tests/completion_source.rs`.
> This closes the PL8.B seam drains (picker / config / events / grammar / modes /
> completion); decorations are the separate hot-path slice PL8.E.

### PL8.C — User-facing load/unload/reload ex-commands  ✅
- `:plugin-load <path>`, `:plugin-unload <id|name>`, `:plugin-reload <id|name>`.
- Unload = `PluginTeardown::unload(&mut TeardownRegistries)` (reverses every surface);
  reload = unload + re-run the load orchestration (fresh `Store` → fresh, untripped
  `Quarantine`).
- **Exit:** load/unload/reload work interactively; teardown removes every contributed
  surface (assert counts via `TeardownReport`).

> **Landed — option A (loader self-registers), confirmed with Dhruva.** The
> ex-command→loader wiring was the design fork. The slice plan's *preferred*
> action-handler path was **blocked** (`ActionContext` carries no ex-command
> args), so the choice was between (A) the loader self-registering its
> ex-commands and (B) the documented thin-host-effect-arm fallback. **(A)** won on
> the mode-ownership standing rule: plugin load/unload is *loader-internal* (not
> editor state), so it needn't round-trip the host `Effect` vocabulary. The
> resolver tries `id_by_name` before `expand_alias`, so **plain** command names
> (`plugin-load`, not `ex:plugin-load`) resolve directly — **zero host code**: no
> host `Effect` variant, no `Editor::` method, no `expand_alias` entry.
>
> **C.1 (teardown foundation).** Every drain now accumulates a `PluginTeardown`
> bundle on its `LoadedRecord` (modes + completion carrier
> mode→`modes`; picker→`picker_sources`; config→`config_options`;
> events→`subscriptions`; commands need no token — unload runs
> `CommandRegistry::unregister_plugin(id)` unconditionally, reversing grammar
> contributions + `:<mode>` toggle ex-commands by `SourceLayer::Plugin(id)`
> provenance). `LoadedRecord` gains `source_dir` (reload) + `teardown`;
> the old `picker_sources` field folds in. `PluginLoader::unload(target)` (sync —
> resolves by manifest id or numeric id, aborts actor tasks, RCU-reverses the
> ArcSwap registries + refs the Arc-shared ones through `run_teardown`), `reload`
> (= capture dir → unload → `load_path`), `load_path` (`:plugin-load` entry via
> `discovery::discover_one`). New errors: `Discovery` / `NotLoaded` /
> `NotReloadable`. Tests: `unload_reload.rs` (picker + grammar reversal counts,
> reload from disk, unknown-target).
>
> **C.2 (ex-command surface).** New `ex_commands` module: the loader
> self-registers the three commands into the runtime-mutable command registry at
> `install` (load→clone→register→store), each `apply` closure capturing
> `Arc<PluginLoader>`. `unload` is synchronous so its `apply` does the work +
> echoes the result immediately; `load` / `reload` are async so their `apply`
> spawns on the loader's runtime and echoes "loading…", completion via
> `tracing::info!`/`warn!` (→ `*messages*`). A benign registry↔loader Arc cycle
> (both app-lifetime services). Tests: `ex_command_surface.rs` (plain-name
> registration, usage hints, sync unload, async load-poll) + boot pin
> `plugin_lifecycle_ex_commands_registered_at_boot` (end-to-end on a real boot).
> Also fixed a **boot-ordering bug** en route (`install` seated before the
> command-registry + keymap services it now depends on — grammar/mode plugins
> silently `NotWired` in the real editor; guarded by
> `plugin_loader_captures_every_drain_service`).

### PL8.E — WASM decorations: producer → per-buffer cache → renderer  ✅ (hot path — paramount #1)
The one UX-vigilant slice, and the **last un-drained seam**. The producer
(`WasmDecorationSource::gutter_decorations`, async) + `spawn_decoration_source`
exist in the runtime; there is **no** per-buffer cache on the host and the
renderers read only the native sync `Mode::gutter_decorations` trait.

**Assessment (done 2026-07-16): pattern-composition, NOT a fresh design fork —
build it, don't re-review.** It composes three established patterns:
- **cache + publish** ← `DiffSignRenderState` / `VirtualRowsRenderState` (a
  per-buffer sub-state published into the `ArcSwap<RenderState>` snapshot;
  `render_state.rs` + `publish_render_state()` chokepoint in `dispatch.rs`);
- **off-render-path async producer → Arc-backed per-buffer cache** ← the LSP async
  caches (`lsp_inlay_hints_cache` / `lsp_semantic_tokens_cache`: a spawned request
  task writes directly into the per-buffer cache via `PerBufferCacheExt::insert_for`,
  and `run_tick_pending` publishes on the cache-version flip);
- **loader registers the producer** ← the picker seam (`drain_picker` RCU-registers
  into a host-owned registry service).

Ready to build: the `decorations-guest` fixture is **built**
(`tests/fixtures/decorations-guest/target/.../decorations_guest.wasm`, 51.5K);
`GutterDecoration` is the SAME enum (`Diff { line, kind }` / `Severity { line,
level }`) both renderers already paint at the `mode.gutter_decorations` partition
(TUI `render.rs`, GPUI `window.rs:1818`).

**Decomposition:**
- **PL8.E.1 — host foundation** ✅: a per-buffer `Vec<GutterDecoration>` cache
  (Arc-backed, LSP-cache shape) + a `RenderState` field for it + a
  `WasmDecorationProviderRegistry` service (alias `Arc<…>`, ServiceRegistry
  TypeId rule) the loader registers producers into; the off-render-path drive —
  on trigger (edit / scroll / diagnostic change) spawn the producer on the
  runtime, the spawned task writes the result into the cache (`insert_for`), and
  `run_tick_pending` publishes; on producer `Err`, keep the prior snapshot (no
  flicker). NO per-frame WASM — the renderer reads only the cache.
- **PL8.E.2 — loader `drain_decorations`** ✅: `spawn_decoration_source`, register
  the producer into the `WasmDecorationProviderRegistry`, record the teardown
  token (unregister on unload — extend `PluginTeardown` with a decoration
  surface). Wire the `PluginSeam::Decorations` arm.
- **PL8.E.3 — renderer merge (lockstep)** ✅: both renderers merge the cached
  WASM decorations into the same partition they already walk for
  `mode.gutter_decorations` (TUI + GPUI **in the same patch**, cross-renderer
  rule); end-of-slice `grep -rn "decoration cache" crates/lattice-ui-gpui/` parity
  audit.
- **PL8.E.4 — fixture test + bench** ✅: a `decorations-guest`-driven e2e (a
  plugin's gutter marks reach the cache + paint) + a keystroke→glyph bench / the
  `lattice-plugin-host/tests/no_per_frame_wasm_guard.rs` invariant proving no
  per-frame WASM; a trapping producer keeps the last-good snapshot (zero flicker
  test).

**Exit:** a decoration-producing plugin's gutter marks paint in both renderers;
keystroke→glyph latency shows no per-frame WASM (the `no_per_frame_wasm_guard`
invariant holds); a trapping producer keeps the last-good snapshot with zero
flicker.

> **Landed 2026-07-16.** Built as pattern-composition (no design fork), E.1–E.4.
>
> **Home decision (deviation from the plan's `WasmDecorationProviderRegistry`
> name).** The producer trait + registry live in **`lattice-mode`**
> (`decoration_source.rs`): `AsyncGutterDecorationSource` (the native async
> producer trait) + `GutterDecorationSourceRegistry` +
> `GutterDecorationSourceRegistryHandle = Arc<ArcSwap<…>>`. Named *generically*
> (not `Wasm…`) because it holds native trait objects — the picker precedent
> (`PickerRegistry`/`PickerSourceGenerator`), where the WASM source is one
> implementor. This keeps `lattice-host` **and the renderers** free of any
> `lattice-plugin-host` dependency: `impl AsyncGutterDecorationSource for
> WasmDecorationSource` lives in plugin-host, the loader hands a trait object
> across the seam, and the renderer reads only cached `lattice_mode::GutterDecoration`s
> — the same indirection completion (`AsyncCompletionSource`) uses. The
> `no_per_frame_wasm_guard` invariant (renderers must not name plugin-host) holds
> structurally.
>
> **E.1 — host foundation.** `WasmGutterDecorationCache { document_version,
> decorations }` + a cohesive `WasmDecorationState` bundle on `Editor` (one
> field: cache + registry handle + paint `generation: AtomicU64` + single-flight
> `pending` + `last_registry_epoch`) — so the boot struct literal grows by one
> line (`WasmDecorationState::with_registry`). `RenderState` gains a
> `wasm_gutter_decorations` `PerBufferCache` clone (published in
> `build_render_state`). `Editor::maybe_refresh_wasm_decorations` (in
> `wasm_decorations.rs`, called from `run_tick_pending` next to the LSP pumps) is
> modelled on `maybe_request_inlay_hint`: version + **registry-epoch** gated
> (a `:plugin-load`ed producer repaints without an edit; an unloaded one's marks
> clear), single-flight, spawns producers on `spawn_on_lsp_runtime` (off the
> actor thread), each writing the merged result via `insert_for`, bumping
> `generation`, and firing `async_landed`. `compute_paint_revision` folds
> `generation` so an off-keystroke decoration arrival repaints the gutter. Boot
> registers the registry as a service (`editor_boot.rs`, next to the picker
> registry) + clones it onto the editor.
>
> **E.2 — loader `drain_decorations`.** RCU-registers the plugin's
> `WasmDecorationSource` (wrapped as `Arc<dyn AsyncGutterDecorationSource>`) into
> the registry (load → clone → register → store, like the picker seam); the
> producer actor runs on the runtime (off keystroke). `PluginTeardown` grew a
> `decoration_sources: Vec<u64>` surface (+ `TeardownReport.decoration_sources` +
> `TeardownRegistries.decorations`) reversed via
> `GutterDecorationSourceRegistry::unregister`. `LoaderServices` +
> `WiredSeams`/`all()` + `install` capture gain `decoration_registry`; the boot
> pin `plugin_loader_captures_every_drain_service` now asserts it wired. The
> `PluginSeam` match in `load_discovered` is now **exhaustive** (decorations was
> the last un-drained seam; a new variant must add its drain — compiler-enforced).
>
> **E.3 — renderer merge (lockstep).** Both peers merge
> `rs.wasm_gutter_decorations.get_for(buffer_id).decorations` into the SAME
> `(diff_map, sev_map)` partition they already build from `Mode::gutter_decorations`
> — TUI `render.rs`, GPUI `window.rs`, same patch. Plugin marks paint through the
> identical glyph/style/tint mapping downstream; no renderer knows the marks came
> from WASM. Parity audit: `grep -rn "wasm_gutter_decorations" crates/lattice-ui-gpui/`
> non-empty.
>
> **E.4 — tests.** `lattice-plugin-loader/tests/decoration_drain.rs` (the
> `decorations-guest` fixture loads, its producer registers + is callable —
> Diff/Change@0, Severity/Error@1, Diff/Add@last — and unload reverses it,
> `report.decoration_sources == 1`; a no-registry loader skips it, not fatal).
> `lattice-host/tests/wasm_decoration_cache.rs` (a native stub producer: the
> refresh writes the cache off-thread + bumps `generation` + fires `async_landed`,
> is single-flight/version-gated, and an **erroring producer keeps the prior
> snapshot** — zero flicker). No new bench: the perf-critical property (no
> per-frame WASM) is structural — the renderer reads a native cache and cannot
> reach the producer — and pinned by the existing `no_per_frame_wasm_guard` +
> `keystroke_publish_ratchet`. Graceful degradation throughout: `NotWired` skip,
> `Err`→keep-prior (no clear), all-error→no write, missing-handle teardown no-op.

### PL8.D — `init.rs`-as-WASM user config  ✅
`init.rs` is **just another plugin**, loaded from `<config>/lattice/init/`
(manifest'd dir — reuses `discover_one` verbatim) instead of
`<data>/lattice/plugins/`. Its config kinds map to seams: commands→`grammar`,
options→`config`, autocmds→`events`, minor modes→`modes`, and — the one gap —
plain user keybinds→a **new `keymap` seam**. Two design forks were resolved with
Dhruva:

- **Not a unified "init world."** The host's deliberate sync/async **linker
  split** (`grammar_linker` = `add_to_linker_sync` for the keystroke hot-path;
  everything else `add_to_linker_async`) means one component instance reaches
  only one import table. So init.rs is a **multi-seam plugin** — one instance per
  declared seam, each against its correct linker — reusing the PL8.B drain
  machinery. This is forced by the architecture, not a preference.
- **Keymap gap → a new `keymap` seam** (Dhruva's choice over reuse-a-user-mode /
  defer). User global bindings have a natural home: `KeymapLayer::User` (already
  exists), gated by `KeymapCapability::User` — non-`Builtin` per the standing
  rule, sitting above the builtin vim grammar.

The `init/plugin.toml` is a normal `PluginManifest`: `id = "init"`, `provides =
["keymap", "grammar", "config", "events", ...]` (the load-bearing seam
declaration), broad `editor_capabilities` (trusted user config), optional
`capabilities` / `doc`.

**Decomposition:**

- **PL8.D.1 — the `keymap` seam.** ✅ New `wit/keymap.wit` (a `keymap` register
  interface — `register-binding(mode, chords, command-name)` — + a
  `keymap-plugin` world importing it + exporting `register-keymap`), host
  `bindgen!` + async-linker import wiring registering into `KeymapLayer::User`
  (capability-gated `KeymapCapability::User`), `PluginHost::spawn_keymap_plugin`
  returning the bindings for teardown, and `"keymap"` added to `PluginSeam` +
  the manifest `provides` vocabulary. Fixture guest + host round-trip test.
  Registration-only (async linker, one-shot at load); binding *resolution* at
  keystroke stays native (`KeymapHandle`) — no hot-path WASM.
- **PL8.D.2 — loader `drain_keymap`.** ✅ `drain_keymap` drives
  `spawn_keymap_plugin` (direct registration like config — the shared
  interior-mutable `KeymapHandle`, no RCU/actor) and records the bindings as
  teardown tokens. `PluginTeardown` grew a `keymap_bindings` surface (+
  `TeardownReport.keymap_bindings`) reversed via a new
  `KeymapHandle::try_unbind_chord_string` (the symmetric string-unbind counterpart
  to `try_bind_chord_string`) — needed for both `:plugin-unload` consistency
  *and* `:reload-config` correctness (a binding removed from init.rs must clear,
  which re-binding alone can't do). Per-binding (not wholesale User-layer clear)
  because the User layer is shared. Test `keymap_drain.rs`: a discovered keymap
  plugin binds `<C-s>`→`ex:write` into `KeymapLayer::User`, unload unbinds it
  (`report.keymap_bindings == 1`, binding gone).
- **PL8.D.3 — boot-load init + `:reload-config`.** ✅ `install` loads
  `<config>/lattice/init/` (`default_init_dir`, `dirs::config_dir()`) via
  `load_path` with the `Bundled` tier (trusted user config), OFF the boot thread,
  AFTER the native builtins register (install is seated late in boot) so user
  keymaps / commands / options layer on top. An absent init dir (the common case)
  is a benign debug skip. The loader self-registers `:reload-config` (option A,
  the 4th loader-owned ex-command) = `reload("init")` — the `init` manifest id is
  the convention. Boot pin extended (`reload-config` registered at boot); test
  `init_config.rs`: init loads from a config dir under `Bundled`, its keybinding
  lands, and `:reload-config` re-instantiates from disk with **no binding
  accumulation** (the old binding unbound + re-bound once — validates D.2's
  per-binding teardown).
- **PL8.D.4 — init-artifact auto-reload watcher.** ✅ `watch::spawn_init_watcher`
  watches `<config>/lattice/init/` with `notify` and calls the new
  `PluginLoader::sync_init` (load-or-reload) on every settled change (300ms
  debounce coalesces a build's event burst). So `cargo build` → the editor picks
  up the new `init.wasm` with no manual `:reload-config`. Watches the *artifact*,
  not the Rust source (saving `init.rs` doesn't change `init.wasm`). A broken
  rebuild leaves `init` unloaded (logged); the next good build's write heals it
  (`sync_init` loads when absent, reloads when present). A watcher that can't be
  created disables auto-reload with a warning, never fails boot. Tests: `sync_init`
  (deterministic load-then-reload, no binding accumulation) + `init_watch.rs`
  (real `notify` integration — rewriting `init.wasm` triggers a reload, observed
  via a counting provenance sink).

**Exit ✅:** a user `init.rs` (multi-seam plugin) that registers a keybind /
command / option / autocmd takes effect at boot (loaded from
`<config>/lattice/init/` with the `Bundled` tier, after builtins), survives
`:reload-config` (fresh Store, old contributions reversed), and auto-reloads when
its artifact is rebuilt. PL8.D complete.

> **Follow-on (future slice): `:plugin-build`.** Today the auto-reload watches the
> compiled `init.wasm`; the user rebuilds externally (`cargo build`). A
> `:plugin-build` ex-command that compiles a plugin's / init's Rust source →
> `wasm32-wasip2` artifact from inside the editor (the watcher then reloads it)
> would close the edit→build→reload loop entirely. It's also the flagship first
> consumer of an event-handler-callable API (per the settled event-handler
> principle — see the status snapshot), and wants build output surfaced in
> `*messages*` / a compilation buffer. Deferred.

### PL8.F — Intern-leak reclamation  ✅
- The interner leak (Low–Medium): plugin option `name`/`doc` were `Box::leak`ed to
  `&'static str` in `config_host::build_and_register` (documented there, PH7.12b.2
  decision C), plus the same `Box::leak` intern in `boundary_picker.rs`
  (`PickerSourceSpec` + `ArgSpec`) and `boundary_grammar.rs` (`SurfaceForm::Delimiter.hint`).
  Harmless for load-once, but repeated `:plugin-reload` / `:reload-config` (real
  as of PL8.C/D) grew the leaked strings unbounded.
- **Exit ✅:** a reload-loop test (`config_reload_leak.rs` — reloads the
  `config-guest` fixture 12×) shows the live option footprint bounded (stays at 3,
  not 3·N), and a final unload reclaims every option.

> **Landed 2026-07-16 (combined single-slice sweep, confirmed with Dhruva).** The
> durable `Cow<'static, str>` fix, across all three seams in one commit:
>
> - **Config** (`lattice-config`): `Option<T>.name`/`.doc` → `Cow<'static, str>`;
>   `name()` → `-> &str`; `new`/`builder` + the `ErasedOption` trait take/return
>   accordingly; `try_register` owns the name up front (borrow-after-move). The 96
>   `options!`-macro options + ~58 literal call sites compiled unchanged (the
>   `impl Into<Cow<'static, str>>` constructors coerce `&'static str` literals).
> - **Grammar** (`lattice-grammar`): the shared `ArgSpec.{name,doc,prompt,completion}`
>   → `Cow`; `SurfaceForm::Delimiter.hint` → `Cow` (dropping `Copy` — the one
>   `Copy`-reliance break was `excommand.rs`'s by-value `if let` + the
>   `WrongSurfaceForm.hint` carried field, both now `Cow`/by-ref). The ~55 `ArgSpec {…}`
>   struct-literals took a scripted `.into()` sweep (scoped to `ArgSpec` blocks;
>   the compiler enumerated the multi-line-string + `SurfaceForm` stragglers).
> - **Picker** (`lattice-picker`): `PickerSourceSpec.{id,doc,args_hint}` → `Cow`;
>   the `PickerRegistry` HashMap key → `Cow<'static,str>` (lookups still take
>   `&str` via `Cow: Borrow<str>`); `iter`/`ids` return `&str` (was `.copied()`).
> - **Leak removal**: `boundary_picker.rs` (`ArgSpec`/`PickerSourceSpec::from_wit`)
>   and `boundary_grammar.rs` (`Delimiter.hint`) drop `intern`/`Box::leak` — the
>   plugin's WIT strings cross as `Cow::Owned` and free with the registry entry on
>   `unregister` / `unregister_plugin`. No `unsafe`. Both `intern` fns deleted.
>
> Downstream consumers (`lattice-host` dispatch/excommand, `lattice-ui-tui` tests)
> took mechanical `.as_deref()` / guard-binding fixes. Green: config (160) /
> grammar (221) / picker (62) / plugin-host (160) / loader (26, +`config_reload_leak`) /
> host (794) suites, workspace `--tests` check, GPUI `--features window`, clippy
> clean. No bench: the change is load/unload-time (off the keystroke path).

### PL8.G — Modes-as-components (bundled major/minor as WASM)  ✅ (shades into 8b)
- Ship a built-in mode as a WASM component through `spawn_mode_plugin` to validate the
  full mode seam end-to-end (the design's §5.8.3 "as components" goal). Built-in modes
  stay native by default; this proves the extension path.
- **Exit:** one mode loads as a component, registers its `MinorMode` keymap layer, and
  passes the mode-ownership acid test from the guest side.

> **Landed 2026-07-16 — option (B), confirmed with Dhruva (real-dispatch
> end-to-end through a booted editor).** The subject is the **emacs-keys leader
> tribute** itself — a real native builtin minor mode
> (`lattice-mode/src/emacs_keys_mode.rs`, the very "EmacsKeysMode template" the
> mode-host docs cite) whose entire behavior IS a keymap layer of
> leader→existing-command bindings, so it is fully expressible through the
> declarative mode seam (no seam extension — `try_bind_chord_string` already
> parses the multi-chord `<C-x>` sequences via `parse_chord_sequence`).
>
> **Fixture** `tests/fixtures/emacs-keys-guest` (built by `build.rs` →
> `EMACS_KEYS_GUEST_WASM`, ~50K): a `modes-plugin`-world component declaring ONE
> minor mode `emacs-keys-plugin-mode` (`Universal`) via the canonical WIT
> `register-mode`, binding two **component-exclusive** leader chords to existing
> commands — `<C-x>e` → `action:split-pane-horizontal`, `<C-x>w` → `ex:write`.
>
> **Why a distinct id + distinct chords (not `emacs-keys-mode` verbatim):** the
> native mode is still registered (foundation mode) — "modes stay native by
> default" — so a component re-declaring `emacs-keys-mode` would hit the
> registry's duplicate-id gate. A distinct id is the realistic case (a plugin
> shipping its own leader mode). Distinct chords make the dispatch proof
> unambiguous: active mode layers MERGE into one composite trie at lookup
> (`lookup_with_context`'s `merge_over`), so a chord the native `<C-x>` layer
> lacks (`<C-x>e`) can only resolve via the component's layer.
>
> **Test** `lattice-host/tests/emacs_keys_as_component.rs` (mirrors the native
> `emacs_keys_dispatch.rs` harness): boots a real `Editor`, wires a
> tempdir-`PluginHost` loader to the editor's LIVE `mode_registry` / `keymap` /
> command registry, loads the component, activates it (publish `MajorEntered` +
> drain), then dispatches `<C-x>e` through `Editor::dispatch_chord` and asserts it
> resolves `action:split-pane-horizontal` AND grows the pane tree 1→2 — a
> component-shipped mode driving a real editor action, indistinguishable from
> native at the dispatch layer. Also asserts ownership/gating (the `<C-x>e`
> binding resolves only when `emacs-keys-plugin-mode` is active, never globally)
> and the graceful-skip path (no wired mode registry → `NotWired("modes")` skip,
> editor uncorrupted).
>
> **Mode-ownership acid test, guest side:** the only production change is
> `build.rs` registering the new fixture — **zero** `Editor::` methods, **zero**
> host `Action` variants. The plugin mode reaches dispatch through the same
> generic path builtins use. No bench: registration/activation are off the
> keystroke path and chord *resolution* stays native (no hot-path WASM), pinned
> by the existing `no_per_frame_wasm_guard`.

### PL8.H — Plugin-manager surface (reload + health, buffer-backed)  ✅ (shades into 8b)
- A `:plugins` buffer-backed view (everything-is-a-buffer) listing loaded plugins,
  health (quarantined? via `Event::PluginCrashed`), capabilities granted/denied, with
  reload/unload actions. Async status → headerline per the standing rule.
- **Exit:** the manager view lists plugins + health; a crashed plugin shows quarantined;
  reload/unload from the view work.

**Substrate assessment (2026-07-17).** Today `PluginMeta { name, doc }` carries no
health, no capabilities; `:list-plugins` is a read-only `HelpBuffer` (PI.4). The
exit *requires* in-view reload/unload, so a read-only enriched list does NOT
satisfy it — the view must be interactive. Two new things are needed: (1) a
loader **status data model** (the load-time capability grant/denied outcome is
computed then dropped; no health/quarantine set is tracked), and (2) an
**interactive buffer-backed view** owning its mode + keymap + handlers.

**Home decision — option (b), confirmed with Dhruva: a new `lattice-plugin-manager`
crate** modelled on `oil` (a mode-owned interactive synthetic buffer). The
dependency direction is fixed (`host → loader`; the loader cannot reach host
synthetic-buffer/Document machinery without a cycle), so the view registers
through the generic host boot primitive (the `oil` `register_*_modes(boot.…)`
shape) and reads loader status via the `PluginLoaderHandle` service. Rejected:
(a) a host module (`dashboard`/`tutor` precedent) — fails the mode-ownership acid
test (keymap + handler bodies land on the host); (c) fold the view into the loader
crate — drags wasmtime into a pure-UI view and conflates the runtime-composing
loader with a view concern. (b) passes the acid test: zero new `Editor::` methods,
zero host `Action` variants; the view owns its full surface in its own crate.

**Decomposition:**

- **PL8.H.1 — loader status data model** ✅: retain per-plugin capability
  `{granted, denied}` at load (today `build_plugin_wasi`'s `outcome` is dropped
  after the `denied` warn); subscribe to `Event::PluginCrashed` to track a
  quarantined/health set; expose a wasmtime-free `plugin_status()` query
  (id · name · tier · caps granted/denied · health) on the loader — the loader
  already owns the data + the `unload`/`reload` APIs.

  > **Landed 2026-07-17.** New `status.rs` (`PluginHealth { Healthy | Quarantined
  > { func, kind } }` + `PluginStatus { id, name, tier, granted, denied, health }`
  > — structured, not pre-formatted, so the view owns presentation). `LoadedRecord`
  > gained `tier` / `granted` / `denied` / `health`; `load_discovered` computes the
  > grant once via the pure `lattice_plugin_host::grant(manifest, tier)` (the same
  > outcome each seam spawn computes internally — `granted = requested − denied`).
  > `PluginLoader::plugin_status()` clones a stable snapshot under the loaded-set
  > lock; `mark_quarantined(id, func, kind)` flips health (unknown id = benign
  > no-op); `subscribe_health(self: &Arc<Self>)` subscribes a `PluginCrashed`
  > `Channel` sink drained on the runtime (`Weak<Self>`, off keystroke path),
  > wired in `install`. `lattice-protocol` promoted dev-dep → dep (Event/EventKind).
  > Tests (`plugin_status.rs`, 5): caps denied@`UserInstalled` / granted@`Bundled`;
  > `mark_quarantined` flips + ignores unknown ids; the full `PluginCrashed`-event
  > → quarantined path through `subscribe_health`; unload drops from the set. Green:
  > full loader suite + clippy clean + host builds. No bench (off keystroke path).
- **PL8.H.2 — buffer-backed `:plugins` view (read render)** ✅: new
  `lattice-plugin-manager` crate; a synthetic Document rendering the status table
  (name / health / caps); `:plugins` ex-command opens it.

  > **Landed 2026-07-17.** New crate `lattice-plugin-manager` — a pure PROVIDER
  > crate (zero host code). `render.rs`: `render_status(&[PluginStatus]) -> String`
  > (adaptive-width table: NAME / HEALTH / TIER / CAPABILITIES; granted caps in
  > wire form + `(denied: …)`; a quarantined row trails `[trap: <kind> in <func>]`;
  > explicit empty state). `mode.rs`: `PluginManagerMode` (major, `ReadOnly` +
  > `NoFile` via `Mode::options`), `on_activate` resolves `PluginLoaderHandle` +
  > `BufferStoreHandle` via `ctx.service()`, renders the snapshot, and writes it
  > **off the actor thread** (full-range `Edit::replace`, spawned) — plus a
  > `PluginCrashed` `Channel` subscription that re-renders health live (the LSP-log
  > precedent; `Subscription` guard unsubscribes on deactivate). `lib.rs`:
  > `install(boot)` registers the mode via `boot.modes_mut()` + the `:plugins`
  > ex-command via `boot.commands_mut()` (seated in the Phase-B list before the
  > freeze; needs no loader-install ordering — the mode resolves the loader service
  > at *activation*). `:plugins` returns the generic
  > `Effect::OpenSyntheticBuffer { name: "*plugins*", mode_id: "plugins-mode" }`;
  > the host's existing `open_synthetic_buffer` ensures the buffer + activates the
  > mode → content projects. **Acid test: one install line in `editor_boot.rs`,
  > zero `Editor::` methods, zero host `Action` variants.** Tests: `render` (4 —
  > empty state, per-row health/tier, granted+denied caps, empty-grant dash) +
  > `lattice-host/tests/plugins_manager_view.rs` (2 — `:plugins` registered at
  > boot; opening `*plugins*` activates `plugins-mode`). Green: crate + clippy
  > clean + host builds. No bench (open/render off the keystroke path).
- **PL8.H.3 — interactivity** ✅: the view's gated `MajorMode(plugins-mode)` keymap
  layer (`r`→reload, `x`→unload, `K`/`<CR>`→`:describe-plugin`, `gr`→refresh) whose
  handler bodies call the loader's existing `unload`/`reload` APIs; the view
  re-renders on the resulting status change.

  > **Landed 2026-07-17.** New `actions.rs`: the four `action:plugins-*` command
  > names + `register_actions` (dead-body, so the keymap `cmd:` resolves; the
  > `repl-mode` precedent) + the handler closures. Each reads the loader + buffer
  > store from the `ActionContext` service registry (nothing captured at
  > registration; a missing service → no-op, never a panic) and maps the cursor
  > line → plugin by index (`cursor.line − HEADER_LINES`, render order ==
  > `plugin_status()` order — pinned by `header_occupies_exactly_three_lines`).
  > `unload` (sync) tears down + re-renders; `reload` spawns the async
  > unload+re-instantiate then re-renders (off the actor thread); `describe`
  > returns `Effect::DescribePlugin`; `refresh` re-renders. `PluginManagerMode`
  > gains `keymap()` (5 entries — `K`/`<CR>` both describe) + `action_handlers()`;
  > the host's `keymap_mode_contributions` + `register_mode_action_handlers` walks
  > pick them up automatically (pushed under `MajorMode(plugins-mode)`, gated to
  > the buffer). `install` calls `actions::register_actions(boot.commands_mut())`.
  > **Acid test still holds: zero new `Editor::` methods, zero host `Action`
  > variants.** Tests: `actions` unit (the 4 commands register) +
  > `plugins_manager_view.rs` `plugins_mode_chords_override_in_its_layer` (the
  > `action:plugins-*` commands register at boot; `x` binds to
  > `action:plugins-unload` in the plugins-mode layer, proving the `cmd:` literals
  > resolve + the keymap pushed; and `x` is NOT globally overridden — the gate
  > holds). Green: crate (7) + host (3) + clippy clean. No bench (off keystroke
  > path).

No bench (registration / status / actions are all off the keystroke path).

---

## Sequencing summary

**PL8.A → {B, C, E} → D → F → {G, H}.** Land A first (foundation, thin behavior, real
dependency de-risk). Then B+C give the real load path + user surface; E closes the
hot-path decoration gap on its own UX-vigilant slice. D brings `init.rs`. F rides D.
G/H are the "as-components" + management surface that bridge into Phase 8b.

---

## Status snapshot — 2026-07-16 (session handoff)

Branch `phase-8-plugin-loader`, **~22 commits ahead of `main`**, green throughout.

**Done:** PL8.A ✅ · PL8.B ✅ (all 6 seam drains: picker / config / events /
grammar / modes / completion, + the boot-ordering fix seating `install` after the
command-registry + keymap services) · PL8.C ✅ (`:plugin-load` / `-unload` /
`-reload` + `PluginTeardown` lifecycle, option A — loader self-registers, zero
host code) · PL8.D ✅ (D.1 keymap seam → `KeymapLayer::User`; D.2 `drain_keymap` +
keymap teardown surface; D.3 boot-load `init.rs` from `<config>/lattice/init/` +
`:reload-config`; D.4 init-artifact auto-reload watcher via `notify`) · PL8.E ✅
(WASM decorations — the last un-drained seam + the hot-path slice: producer trait
+ registry in `lattice-mode`, host per-buffer cache + off-thread refresh, loader
`drain_decorations` + teardown, both renderers merge at the gutter partition in
lockstep; renderers stay free of plugin-host).

**Done (cont.):** PL8.E ✅ (WASM decorations — the last un-drained seam + the
hot-path slice): producer trait + registry in `lattice-mode`
(`AsyncGutterDecorationSource` / `GutterDecorationSourceRegistry`, generic-named
per the picker precedent); host per-buffer cache + `maybe_refresh_wasm_decorations`
drive (registry-epoch + version gated, off-thread, `generation`→`compute_paint_revision`);
loader `drain_decorations` + decoration teardown surface; both renderers merge the
cache at the existing gutter partition (lockstep). Renderers stay free of
plugin-host (guard holds). All seam drains now exhaustive.

**Done (cont.):** PL8.F ✅ (intern-leak reclamation — the durable `Cow<'static, str>`
sweep across config / grammar / picker + boundary leak removal; `config_reload_leak`
proves the footprint stays bounded across 12 reloads).

**Done (cont.):** PL8.G ✅ (modes-as-components — the emacs-keys leader tribute
shipped as a WASM component through the mode seam; loads into a real booted
editor's live registries, activates, and its `<C-x>e` leader chord dispatches
`action:split-pane-horizontal` through the real dispatcher (pane 1→2),
indistinguishable from native; zero host `Editor::`/`Action` additions — the
mode-ownership acid test passing from the guest side).

**Done (cont.):** PL8.H ✅ (plugin-manager surface — the new `lattice-plugin-manager`
provider crate: H.1 loader status data model (caps granted/denied + health via a
`PluginCrashed` subscription); H.2 the buffer-backed `:plugins` view (major mode
projects the status table off-thread, opened via the generic
`Effect::OpenSyntheticBuffer`); H.3 the `MajorMode(plugins-mode)` keymap layer +
`r`/`x`/`K`/`<CR>`/`gr` handlers calling the loader's `unload`/`reload`. Zero host
`Editor::` methods, zero host `Action` variants — one install line).

**PL8 (Phase 8) is complete.** Next: **Phase 8b** (bundled first-party plugins as
components) + the settled **plugin observability** stack (Layers 0–2, its own
design fragment — see the settled-decisions note above).

**Design decisions settled this session (don't re-litigate):**
- **Tier-2 "event→command" is resolved as a principle, NOT `:autocmd`/`SubscriptionTarget::Invocation`.**
  Event handlers act by calling the **underlying API** (WIT imports for plugins /
  `init.rs`, native imports for internal handlers) using the `Event` payload's
  context — never by invoking `:` command strings and never by returning effects.
  `:` commands are thin front-ends over those APIs. No declarative `:autocmd`. No
  `buffer N` targeted-dispatch (defer buffer-registry exposure until a real use
  case). See the memory `event-handlers-call-apis-not-commands`. The WASM
  `on-event` already delivers the full typed `Event`; expanding the imported-API
  surface is incremental + per-use-case (flagship: plugin build/reload, gated on
  `:plugin-build`).
- **`:plugin-build`** (compile source → `wasm32-wasip2` from inside the editor,
  the watcher then reloads) is a future slice — sequence after the first concrete
  API-exposure use case; wants build output in `*messages*` / a compilation buffer.
- **Plugin observability — settled 2026-07-17 (own design fragment + slices,
  AFTER PL8.H.2/.3).** A layered stack anchored on the insight that *the host owns
  the whole Component-Model boundary*, so the richest **language-agnostic** signal
  (independent of the guest's source language — Rust/Zig/Go/AS) is instrumenting
  our side of every WIT import/export. The Emacs-debugger analogy is corrected:
  we don't own the guest runtime, so a step-debugger isn't the model — for a
  message-passing architecture the right granularity is the **boundary interaction
  trace + crash forensics**. Layers: **0** lifecycle+health+crash-isolation (built:
  `PluginCrashed`/quarantine/H.1 status; the `:plugins` view is its surface);
  **1** host-boundary trace (every call: name/args-summary/duration/fuel-delta/
  result-or-trap), config-gated verbosity, routed to a plugin-trace buffer; **2** a
  `wasi:logging`-style host import so guests emit their own structured logs,
  host-captured. **Layer 3** (wasmtime DWARF/gdb source-level step-debug) is
  **deferred** as a documented author-only escape hatch — not core. Decisions:
  boundary-trace-first (portable, no guest cooperation); verbosity a typed option;
  **surface = one shared underlying stream + per-plugin filtered views** (drill-in
  from the manager view). **Hard constraint (paramount #1 + #4) — tracing is NEVER
  on the editor hot path (Dhruva, 2026-07-17):** trace records are **streamed via
  the event bus, exactly like LSP logs** (`LspLogPushed` → off-thread drain →
  synthetic buffer, the `MessagesLayer`/lsp-trace precedent). Nothing is formatted
  or written on the UI/actor thread — the drain side (off-thread) does all
  formatting + buffer append. Even the grammar/sync-trampoline seam (keystroke hot
  path, typed call < 500ns p99) never synchronously traces: emission is at most a
  cheap non-blocking enqueue behind a per-plugin atomic gate (zero-alloc /
  zero-arg-format when off), and by default the hot-path seam carries only
  lifecycle/crash signal, not per-call traces. Async seams
  (picker/completion/decorations/events) are already off-thread and stream richly.
  Full design fragment (`docs/dev/architecture/plugin-observability.md`) to be
  written when PL8.H.2/.3 land.

**Uncommitted in the working tree (intentionally):**
- `docs/user/init.md` (+ its `docs/user/README.md` index entry) — the user-facing
  `init.rs` guide, **held uncommitted** until the real event-handler-action APIs
  land, then finalize against actual APIs. Its events section must be corrected to
  "handlers call APIs via imports" (not the effect-return framing it currently
  sketches) per the settled principle above.
- A separate `:cd` / `:pwd` feature (11 files, Dhruva's parallel work) — leave
  untouched; stage commits **explicitly** (never `git add -A`) so it isn't swept in.

---

## Plugin manager redesign — sources, build-on-boot, core-plugin shipping (PM.x)

> **Design contract:** [`../../architecture/plugin-manager.md`](../../architecture/plugin-manager.md)
> (the two roots, the source/build/require model, the settled decisions). This
> redesign extends the finished PL8 loader (+ the `:plugins` view) with the two
> things it lacked: **where a plugin comes from** (a git/local source) and **how a
> missing artifact is produced** (build-on-boot). The load / unload / reload /
> discovery machinery is unchanged — PM adds a resolve→build→cache layer in front.

**Status: 🚧 core track ✅ (PM.1 · PM.2 · PM.3 · PM.4) · user track PM.5 ✅ · PM.6 ✅ · PM.7 ✅ (a + b) · PM.8 ✅ (a + b). **User track complete.**

Two tracks. The **core track (PM.1–PM.4) ships auto-pair out of the box** — no
build service, just a second (prebuilt, shipped) plugin root discovered at boot.
The **user track (PM.5–PM.8)** is the use-package `require`+build layer on top.

### Core track — auto-pair out of the box (reframes AP.4)

#### PM.1 — the runtime root: search path + boot discovery  ✅
A `runtime_root()` search path — `$LATTICE_RUNTIME` → compile-time install prefix
(`LATTICE_INSTALL_PREFIX/share/lattice`, a packager build env) → `<exe-dir>/
../share/lattice` (relocatable) → `<workspace>/runtime` (dev) — resolving the FIRST
existing dir (design §7). Boot (`install.rs`) discovers `<runtime>/plugins/` at
`TrustTier::Bundled` **in addition to** `~/.config/lattice/plugins/`
(`UserInstalled`), reusing the existing `discover_and_load`. **Exit:** a plugin dir
staged under a runtime-root candidate is discovered + loaded at Bundled tier at
boot; an absent runtime root is a benign skip (like the user dir); the search-path
resolution is unit-tested (env override wins; missing dirs fall through). No build,
no network. Bench: n/a (discovery is off the boot thread, already async).

#### PM.2 — `xtask build-core-plugins` staging  ✅
A workspace `xtask` (and the release CI step) that runs the `wasm32-wasip2`
component build for each `plugins/<name>/` and stages `plugin.toml` + `<name>.wasm`
into the dev runtime root (`<workspace>/runtime/plugins/<name>/`), so
`cargo run` finds core plugins without a hand-copy. **Exit:** `cargo xtask
build-core-plugins` produces `runtime/plugins/auto-pair/{plugin.toml, auto-pair.wasm}`;
a dev editor discovers it via PM.1.

**Delivered (2026-07-20).** New `xtask` crate (workspace member, std-only) +
`cargo xtask` alias (`.cargo/config.toml`). `build-core-plugins` builds each
`CORE_PLUGINS` entry (`auto-pair`) to a `wasm32-wasip2` component in a **clean
env** (the `build.rs` `build_guest` precedent — inherited workspace `RUSTFLAGS`/
target/wrappers removed) and stages `<name>.wasm` + `plugin.toml` into
`runtime/plugins/<name>/`. `/runtime` is gitignored (regenerated build output).
Verified: `cargo xtask build-core-plugins` stages
`runtime/plugins/auto-pair/{auto-pair.wasm (138K component), plugin.toml (id +
editor_capabilities)}`; the dev-fallback search path (`<exe>/../../runtime/plugins`
for `target/<profile>/lattice`) resolves that dir, so a `cargo run` editor
discovers it via PM.1.

#### PM.3 — manifest `default_mode` + the `<plugin>.enabled` gate  ✅
Manifest gains `default_mode: option<string>` (the mode a plugin enables by
default). On load, the manager auto-registers a bool option `<plugin-id>.enabled`
(default `true`) and, gated by it, enables the declared mode via the CI.4
`ModeEnablementRequested` path; a `:set <id>.enabled=false` (an `OptionChanged`)
disables it. Host learns the mode-id ONLY from the manifest (mode-ownership holds).
**Exit:** a discovered plugin declaring `default_mode` has its mode active on load
with no init.rs; toggling `<id>.enabled` activates/deactivates it live; the option
is `:describe-option`-visible. General mechanism (core or user plugin).

**Delivered (2026-07-20).** `PluginManifest.default_mode: Option<String>` (+ raw
manifest `#[serde(default)]`). The loader's `apply_default_mode_gate` (in
`load_discovered`, after `PluginLoaded`) registers `<id>.enabled` via the now-`pub`
`config_host::register_plugin_option` (Boolean, default `"true"` — idempotent,
reuses the tested collision/parse logic), reads it, and publishes
`ModeEnablementRequested { mode, enabled }`. `LoadedRecord` carries `default_mode`
so `subscribe_mode_gates` (mirrors `subscribe_health`, wired in `install`) maps a
`<id>.enabled` `OptionChanged` back to the mode and re-requests. Host never names a
mode-id — the manifest does (mode-ownership). **Tests:**
`lattice-plugin-loader/tests/mode_gate.rs` — load-time enable (option registered
default true + `ModeEnablementRequested{enabled:true}`), live toggle off→on via
`OptionChanged`, and no-`default_mode`⇒no-gate. General for core or user plugins.

#### PM.4 — auto-pair as the first core plugin (AP.4)  ✅
auto-pair's `plugin.toml` declares `default_mode = "auto-pair-mode"`; PM.2 stages
it into the runtime root; PM.1 discovers it; PM.3's `auto-pair.enabled` (default
true) enables the mode. **Exit:** a fresh editor (no user config) auto-pairs out of
the box; `:plugins` lists auto-pair (`source = bundled`); `:set
auto-pair.enabled=false` turns it off (plugin stays loaded, mode deactivates);
`:set auto-pair.style=manual` flips the style live. Closes the auto-pair epic.

**Delivered (2026-07-20).** `plugins/auto-pair/plugin.toml` declares
`default_mode = "auto-pair-mode"` (the module doc updated: the mode enables via
the gate, not init.rs). `cargo xtask build-core-plugins` re-stages it into
`runtime/plugins/auto-pair/`. The composition is proven by: the shipped-manifest
regression guard (`manifest.rs::shipped_auto_pair_manifest_declares_the_gate` —
the real file declares `default_mode` + `tree-sitter`) and `mode_gate.rs` (the
modes seam registers `auto-pair-mode`, the gate registers `auto-pair.enabled`
default-true + requests enablement, and the toggle deactivates/reactivates live).
So a `cargo run` editor — after one `cargo xtask build-core-plugins` — discovers
auto-pair from the runtime root (PM.1) at `Bundled` tier and auto-pairs out of the
box with no user config, `:set auto-pair.enabled=false` turning it off. **The
auto-pair epic (AP.0.1 → AP.4) is complete**, and it is the first plugin to ride
the whole stack: the grammar `document`/tree-sitter seams, the multi-seam host,
the mode-ownership + config gate, and now the core-plugin shipping path.

### User track — use-package (`require` + build)

#### PM.5 — the build service (source dir → cached wasm)  ✅ (2026-08-21)
`crates/lattice-plugin-loader/src/build.rs`. `build_plugin(builder, source_dir,
name, user_root, pinned) -> BuildOutcome`, blocking (callers use
`spawn_blocking`).

**Exit met.** A local source builds once and caches; a warm re-resolve is a pure
load that invokes no toolchain at all — so a machine with no Rust still boots
every already-built plugin. A broken source logs + skips.

Four outcomes, and `StaleKept` is the one worth naming: when a *stale* rebuild
fails but a previous artifact exists, the old artifact keeps loading. Pushing a
broken revision costs you the new code, not the editor you had.

Two decisions the design left open:
- **The stamp counts files as well as max mtime.** mtimes only move forward, so
  a *deletion* would otherwise leave a stale artifact looking current.
- **The stamp is written last, only on full success.** A stamp that outlived its
  artifact would suppress the very rebuild that fixes a half-staged install.

The toolchain sits behind a `ComponentBuilder` trait — the interesting behaviour
is the caching and failure logic, and none of it should be untestable on a
machine that cannot compile a component. `CargoComponentBuilder` scrubs the same
inherited env (`RUSTFLAGS`/target/rustc-wrapper) as the plugin-host `build.rs`
and `cargo xtask build-core-plugins`; this is the third site to need it.

*Tests:* 13. *Bench:* n/a (off-thread, and dominated by cargo).

#### PM.6 — source resolver (`Local` → `Git` → `Prebuilt`)  ✅ (2026-08-21)
`crates/lattice-plugin-loader/src/resolve.rs`. `resolve(...) -> Resolved`, where
`Resolved` is `Source(dir)` **or** `Artifact(path)` — an enum, not a path,
because `Prebuilt` deliberately does not end in a build and collapsing the two
answers would force it to invent a source tree it does not have.

**Exit met.** All three kinds resolve; a re-resolve at an unchanged Git rev
issues `rev-parse` and nothing else — the warm-boot rule applied to the network.

Decisions taken during the build:
- **Git is a subprocess, not `gix`.** `gix` is in the workspace with read-only
  features only; enabling `blocking-network-client` would pull TLS and a network
  stack into a crate that has neither, to replace a binary every developer using
  a Git source already has. Same bargain PM.5 strikes with `cargo`.
- **A pinned rev clones full; an unpinned one clones `--depth 1`.** A shallow
  clone may not contain the pinned commit, so pinning opts out of the cheap path.
- **`Prebuilt` added `ureq`** (blocking, rustls) rather than waiting for
  lighthouse LH.0.1's `http-fetch`. Decided by Dhruva. When that capability-gated
  host service lands, **this is the call site to unify** — do not mint a third
  HTTP path.
- **A prebuilt gets a synthesised manifest with no capabilities**, and an
  existing manifest is never overwritten. A downloaded binary is the least-known
  code the editor runs; a plugin needing more ships a real `plugin.toml` through
  `Local`/`Git`, where the user can read what it asked for first.

*Tests:* 12 — network and toolchain behind `Fetcher` / `GitRunner` traits, plus
one end-to-end against a **real local git repository** (a fake can only prove the
shape of the interaction, not that the commands are right).

#### PM.7a — the `require` seam + install pipeline  ✅ (2026-08-21)

`wit/plugin-manager.wit` (interface + the `plugin-manager-plugin` world),
`lattice-plugin-host/src/plugin_manager_host.rs` (sink, name gate,
`spawn_plugin_manager_plugin`), and
`lattice-plugin-loader/src/pipeline.rs` (`install_required` / `install_all`).

**`require` records and returns** — no resolve, no clone, no build, no
download inside the guest call. A `require` that resolved inline would put a
clone and a compile on the boot path (paramount goal #1) and hang a cold first
boot on a network the machine may not have. Pinned by a test that fails *the
way it would really fail*: the fixture names `example.invalid`, so an inline
resolver times out on DNS instead of returning promptly against an untouched
cache.

Decisions taken during the build:
- **Its own world**, not an import added to an existing one. A plugin that
  contributes a grammar has no business holding the capability to install
  software; worlds keep that true by construction.
- **An unsafe `name` returns `false`, it does not trap.** The name becomes a
  directory under two roots, so `../../.ssh` is a traversal write with the
  editor's authority. Same gate the untrusted `manifest.id` already gets. A
  rejection costs that entry, not the whole config.
- **Types are mirrored across the boundary**, not shared: the dependency runs
  `loader → host`, and a WIT-facing type reshaped by a loader refactor would
  break a public API on an internal edit.

*Tests:* 2 end-to-end through a real `wasm32-wasip2` guest + 4 unit (name gate,
drain-once) + 7 pipeline (failure policy, ordering) = **13**.

#### PM.7b — init.rs built by the service + boot wiring  ✅ (2026-08-21)

The open question the scoping note flagged — `load_path` does not retain the
compiled `Component`, so keep it or compile init twice? — turned out to be a
false choice. **`plugin-manager` became a `PluginSeam`**, so the drain happens
inside `load_discovered`'s existing per-seam dispatch and reuses the component
that function already compiled. No retained state, no second spawn.

It is its own seam rather than folded into `config` because declaring software
to install is a strictly larger authority than setting an option, and
`provides` is where a user reading a manifest can see the difference.

**init.rs is built by the PM.5 service.** Its source dir is a cargo project
like any other, so an edited init.rs rebuilds on the next boot and an unchanged
one is a pure load — which removes the "run cargo by hand first" step §6 called
out. A dir holding a hand-built `init.wasm` and no `Cargo.toml` still loads
as-is, so existing setups do not break; a build failure is a skip that leaves
the user an editor and their previous build.

**Ordering:** required plugins install *before* the on-disk scan, so a plugin
just staged into the user root is discovered this boot rather than the next.

*Tests:* 3 end-to-end + 2 conversion. One harness trap recorded in the commit:
the first cut read `PLUGIN_MANAGER_GUEST_WASM` via `option_env!`, which the
host crate's build.rs exports to its own compilation and not the loader's — so
both real tests passed by silently skipping. They resolve the fixture by path
now, as `init_config.rs` already did.

#### PM.8 — `:plugins` source/build columns + rebuild chord  ✅ (2026-08-21)

Landed as **PM.8a** (columns) then **PM.8b** (chord + in-flight states). The
ordering was forced rather than chosen: the rebuild chord cannot re-clone a git
plugin without its url and rev, and on the next boot the plugin loads from the
on-disk scan where nothing has seen a `require` — so persisting the source had
to come first.

**PM.8a — SOURCE + BUILD columns.** A `.source` marker travels with the
artifact, beside PM.5's `.build-stamp`; together they answer the view's two
questions (where from, is it current). `cargo xtask build-core-plugins` writes
one too, so a core plugin reads `bundled`.
- **Persisted, not derived.** In-memory source would render `local` for a git
  plugin on the second boot — confidently wrong, worse than blank.
- **`Unknown` is a state, not a fallback to `Local`.** A hand-installed plugin
  reads `—`; claiming a source we do not know would put a wrong path in the
  column and offer a rebuild that cannot succeed.
- **`BuildState` is recomputed per snapshot**, because the interesting
  transition happens *while the editor runs*: edit a local plugin, see `stale`
  without restarting.
- Columns sit between TIER and CAPABILITIES because CAPABILITIES is the only
  variable-length cell (it trails `(denied: …)`), so anything after it would
  not align. A test pins that.

**PM.8b — `b` rebuilds.** Distinct from `r` (reload) on purpose: reload
re-instantiates what is on disk, rebuild produces it first, and collapsing them
would make a casually-pressed `r` occasionally take minutes. The stamp is
dropped before building so the build is unconditional — the user asked, the
staleness check does not get a vote. A failed rebuild never unloads a working
plugin (PM.5's `StaleKept` keeps the artifact and the reload comes from it).
- `building…` / `build-failed` are the only build states **not** derived from
  disk, and are deliberately not persisted: a build interrupted by a crash is
  not still running after a restart, and a failure since fixed should not greet
  the user next boot. In-flight wins over the disk answer — reporting `cached`
  during a rebuild would say the keypress did nothing.
- The count is duplicated as a **lock-free atomic** because the headerline's
  `version()` is polled every tick and the trait forbids blocking there; it is
  recounted under the same lock the map is mutated under, so the two cannot
  disagree.
- Progress goes in the **headerline**, hidden while idle
  (async-buffer-status-in-headerline).
- A non-buildable row (bundled / prebuilt / unknown) echoes why rather than
  starting a build that cannot work — PM.8a's marker earning its keep.

*Tests:* 13 (PM.8a) + 4 (PM.8b).

> **Known gap, not part of PM.8.** `enable_mode` is carried faithfully from
> WIT through to `RequiredSpec` and tested, but nothing consumes it yet: the
> CI.5 `on-plugin-loaded` → `enable-mode` desugaring is unwired, so a
> `require` with `enable-mode` installs and loads the plugin without enabling
> its mode. Small, and the natural next follow-up.

### Sequencing

**PM.1 → PM.2 → PM.3 → PM.4** (core track, delivers AP.4) first — it ships
auto-pair out of the box with no build service. Then **PM.5 → PM.6 → PM.7 → PM.8**
(user track) layers the use-package `require`+build surface on top. PM.3 is the one
shared piece the user track also uses (a user plugin declaring `default_mode`).
