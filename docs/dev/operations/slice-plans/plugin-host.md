# Slice plan — Plugin Host (Phase 7)

**Status:** 🚧 in progress (2026-07-01) — PH7.0 ✅, PH7.1 ✅ (7.1a + 7.1b), PH7.2 ✅; PH7.3 🚧 (a–d; PH7.3a ✅, PH7.3b next). Design fragment:
[`../../architecture/plugin-host.md`](../../architecture/plugin-host.md). Spec:
`design.md` §5.5 / §9 / §13. This plan sequences *Phase 7 proper* (per the locked scope):
the host runtime, capability model, the WIT interface set mirroring exercised native seams,
and the `fuzzy-finder` validation plugin.

**Phase-7 exit criterion (design.md §13):** *"a WASM plugin replicates the file picker
without host changes; CI enforces overhead budgets."* Slices **PH7.0–PH7.5** deliver that
exit. Slices **PH7.6–PH7.12** harden the WIT against the rest of the exercised seams so the
interface is sized correctly *before* any ABI-freeze (design.md §14 mitigation) — they are
in-scope-for-Phase-7 as designed, but land after the exit gate is green.

**Dependencies:** none blocking — the exercised trait seams all ship today (Phases 4–6). New
external deps: `wasmtime` (Component Model + WASI preview2), `wit-bindgen`, `wasmtime-wasi`.

**Every slice ships four artefacts** (CLAUDE.md heuristic #5): architecture note (this
plan + fragment), bench coverage, test coverage (happy path **and** failure modes), graceful
error handling. A slice is not done until all four are green.

---

## Critical path — the Phase-7 exit

### PH7.0 — Crate scaffold + WIT skeleton + CI wiring ✅ (2026-07-01)
Created `lattice-plugin-host` crate; added `wasmtime` 46 / `wasmtime-wasi` 46 / `wit-bindgen`
0.58 to `[workspace.dependencies]` (the latter two declared now, first consumed at PH7.2 /
by `init.rs` respectively); created root `wit/` with the `plugin` lifecycle world (`activate`
/ `deactivate`) + empty stub files for the other §5 interfaces; wired
`wasmtime::component::bindgen!` (host) against the `plugin` world; `PluginHost` compiles +
instantiates a component and calls its lifecycle exports.
- **Touches:** new `crates/lattice-plugin-host/`, new root `wit/`, workspace `Cargo.toml`.
- **Exit:** ✅ a hand-written no-op component instantiates and its `activate()` runs; the
  existing `cargo test --workspace` CI job is the "CI builds it" gate.
- **Artefacts:** doc = this slice + `benchmarks.md` Plugin-host section; bench = instantiation
  smoke (`benches/instantiate.rs`, ~1.5 µs warm / ~180 µs cold, provisional off-box); test =
  load+activate+deactivate+drop, reuse-across-Stores (`tests/instantiate.rs`); error =
  malformed bytes **and** bare-core-module → typed `PluginHostError::Compile`, no panic.
- **Decisions (locked with Dhruva 2026-07-01):**
  - *No-op component = hand-written WAT assembled in-test via the `wat` crate* (not a real
    `wit-bindgen` guest). No `wasm32-wasip2` target or separate guest build enters CI for the
    scaffold; that toolchain arrives with the real Rust guest at PH7.4. Matches the slice's own
    "hand-written no-op component" wording (heuristic #1: lightest honest tool, toolchain lands
    when a real consumer needs it).
  - *The `plugin` lifecycle world's first consumer is `init.rs`* (the user's config compiled to
    WASM), **not** `fuzzy-finder`. The no-op component is the degenerate `init.rs`; PH7.4's
    `fuzzy-finder` validates the picker seam. Reflected in `wit/plugin.wit` + the `Cargo.toml`
    dep rationale. *Sequencing resolved (Dhruva 2026-07-01): framing only* — `init.rs` is the
    primary motivating consumer, but its load path stays post-Phase-7 per fragment §12
    (consumes PH7.12's reload seam); Phase-7 sequencing is unchanged and `fuzzy-finder` (PH7.4)
    remains the exit gate.
  - *No WASI view / async ABI / fuel in the scaffold* — deferred to the slices that own them
    (WASI → PH7.2; async + Store-per-plugin tasks + fuel + module cache → PH7.1). The scaffold
    is synchronous and import-free; the `PluginHostError::Engine` variant is reserved for
    PH7.1's custom engine config.

### PH7.1 — Host runtime core (Store-per-plugin, async ABI, fuel/epoch, AOT cache, lazy) ✅ (2026-07-01)
Split into two green-able steps (the slice bundled too much for one landing): **PH7.1a** async
runtime core + fuel/epoch trapping; **PH7.1b** on-disk module cache + lazy instantiation.
- **Depends:** PH7.0.
- **Exit (whole PH7.1):** ✅ two plugins run CPU work on two cores in parallel; a fuel-exhausting
  plugin traps cleanly and the other keeps running; second launch reuses the cached module.

#### PH7.1a — Async runtime core + fuel/epoch ✅ (2026-07-01)
Canonical async ABI (`bindgen!` `exports: { default: async }`; wasmtime 46 makes async always
available, so `Config::async_support` is a no-op and not called); `consume_fuel` +
`epoch_interruption`; a background epoch-ticker thread bumps the engine epoch (1ms) so
wall-clock deadlines fire. Each lifecycle call re-arms two hard budgets — **fuel** (work cap)
and **epoch** (wall-clock) — and either, on exhaustion, traps cleanly into a typed
`PluginHostError::Trap { kind: Fuel | Epoch | Other }`. The lib owns no runtime (methods are
`async fn`; `tokio` is a dev-dep only), so the caller's multi-thread pool drives plugin work —
never the `current_thread` actor.
- **Exit:** ✅ two `busy` plugins overlap on the pool (parallel wall-clock < 1.8× single);
  a `spin` plugin fuel-traps as `TrapKind::Fuel` while a concurrent no-op stays `Ok`
  (isolation); plugin work runs off the actor thread.
- **Artefacts:** bench = instantiation smoke updated for async (~1.6µs warm / ~300µs cold,
  provisional off-box); test = `tests/runtime.rs` (parallel + fuel-trap isolation +
  off-actor-thread) + `tests/instantiate.rs` (async round-trip); fixtures `busy.wat` / `spin.wat`;
  error = fuel/epoch/other traps → typed `Trap`, host stays live.
- **Decisions (locked Dhruva 2026-07-01):** fuel = hard *trap* cap (deterministic in tests);
  epoch = wall-clock *trap* deadline (default epoch behaviour, no async-yield) — the async-yield
  path is reserved for when host I/O calls land (they `await` and release the thread). CPU-bound
  wasm doesn't yield, so a runaway is bounded by fuel/epoch trapping, not cooperative yield.

#### PH7.1b — On-disk module cache + lazy instantiation ✅ (2026-07-01)
On-disk AOT cache under `<user-cache>/lattice/plugin-cache/` (via `dirs::cache_dir()` —
XDG/Application-Support/LocalAppData) so a second launch reuses the cached module.
`PluginHost::with_cache_dir` for hermetic tests; `cache_hits()` / `cache_misses()` accessors.
Lazy instantiation is *structural*: `compile` loads/caches a `Component` without instantiating;
the `Store` + instance are created only by an explicit `instantiate` (which the contribution
model, PH7.3+, will trigger on first invocation).
- **Depends:** PH7.1a.
- **Exit:** ✅ a fresh host over the same cache dir reuses the cached module (`cache_hits() == 1`,
  no recompile).
- **Artefacts:** bench = `load_50_plugins_warm_cache` (~20ms/50 off-box, under the 30ms budget)
  + `instantiate_50_plugins` (~76µs); test = `tests/cache.rs` — second-launch reuse,
  distinct-components-don't-collide, compile-doesn't-run-guest-code (lazy); error =
  `PluginHostError::Cache` on cache-init failure, never a panic.
- **Decision (locked Dhruva 2026-07-01):** use **wasmtime's built-in cache** (`Config::cache` +
  `CacheConfig::with_directory`), NOT the fragment §3 manual `sha256(...)` key +
  `Component::deserialize`. wasmtime owns keying/invalidation (bytes + compiler config + target +
  wasmtime version) and needs **no `unsafe`**, keeping the workspace `unsafe_code = "deny"` gate
  intact (paramount-#2/security + heuristic #1: less code, upstream-maintained invalidation).
  Fragment §3's manual-key text is superseded — see the note there.

### PH7.2 — Capability & security model ✅ (2026-07-01)
Manifest parsing (declared `fs:*`/`net:*`/`proc:*` + editor `CapabilitySet`); build each
Store's WASI view from its grant; per-plugin data dir mount; trust tiers (bundled pre-grant
vs user-install consent); host-issued `SourceLayer::Plugin(id)` provenance stamping.
- **Depends:** PH7.1.
- **Exit:** ✅ (host-layer) a plugin's WASI view is built from exactly its grant — a no-`fs:write`
  plugin's `Store` preopens only its data dir, so a path outside the grant is unreachable at the
  WASI layer (WASI has no ambient authority); `LoadedPlugin::source_layer()` stamps `Plugin(id)`
  from a host-issued, per-instance, guest-unforgeable id. **The guest-level end-to-end write-denied
  proof is deferred to PH7.4** — it needs the real `wasm32-wasip2` guest (the toolchain PH7.0
  deferred to that slice); PH7.2 proves the model at the host layer (grant computation, grant→preopen
  mapping, provenance issuance), the WASI OS-enforcement itself resting on wasmtime's tested guarantee.
- **What landed:** `manifest.rs` (`Capability` enum + `PluginManifest` TOML round-trip);
  `capability.rs` (`TrustTier`, `grant()->GrantOutcome{grant,denied}`, `CapabilityGrant::preopens`,
  `build_wasi_ctx`); `lib.rs` (`PluginState: WasiView`, `p2::add_to_linker_async`, `PluginId`
  allocator, `PluginHost::{with_dirs,instantiate_plugin}`, `LoadedPlugin::{id,source_layer,grant,
  denied_capabilities,data_dir}`). New deps: `wasmtime-wasi 46`, `serde`, `toml`, `lattice-grammar`,
  `lattice-mode`, `tracing`.
- **Decisions (locked with Dhruva 2026-07-01):**
  - *Enforcement proof depth:* host-layer at PH7.2 + guest-level e2e at PH7.4 (respects the PH7.0
    toolchain deferral; heuristic #1 — the WASI OS-enforcement is upstream's, we test our mapping).
  - *Manifest form:* committed TOML format **and** typed `PluginManifest` (fragment §6 "ships a
    manifest"); trust tier stays a host-supplied input, never a self-declared manifest field.
  - *WASI enforcement scope = filesystem only.* `net:http`/`proc:spawn` ride the grant as metadata
    for the capability-gated `host-services` seam (PH7.3+); wiring raw WASI sockets/subprocess for a
    `net`/`proc` grant would be *broader* than intended, so `build_wasi_ctx` leaves them disabled.
    (Refines fragment §6's "`wasi:filesystem`/`wasi:http` view" — see the §6 note there.)
- **Artefacts:** bench = n/a (correctness slice); test = 8 unit (`manifest`/`capability` mods:
  parse matrix + malformed rejection + grant/deny + preopen specs + graceful skip) + 6 integration
  (`tests/capability.rs`: grant load + data-dir mount, no-grant reaches no fs, proc-spawn tier
  matrix + surfaced denial, missing-prefix degrades, provenance uniqueness + `Plugin(id)` stamp,
  editor caps); error = denied capability → `LoadedPlugin::denied_capabilities()` + plugin loads
  degraded, missing/uncreatable data-dir or bad prefix → `warn!` + skip, never a panic or failed load.

### PH7.3 — Boundary primitives (the crux — §4 of the fragment) 🚧
The reusable adapter machinery every seam consumes: owned-snapshot projection (borrows →
owned records + `document` resource handle with slice callbacks); callback-id ↔ guest-export
dispatch; the `effect` WIT variant mirroring the closed `Effect` enum + back-mapping; explicit
WIT records for the non-serde candidate fields; `Result<_, string>` error convention; the
async result-carrier adapter (guest returns batches; host owns `Future`/stream).
- **Depends:** PH7.1.
- **Exit:** round-trip tests native→WIT→native for `Effect`, `Args`, `RawCandidate`,
  `PickerAcceptOutcome`; a document resource handle serves `get-text-range` zero-copy.
- **Artefacts:** bench = typed host-call overhead (< 500ns p99 — the headline gate); test =
  exhaustive enum round-trip + boundary projection; error = malformed WIT payload → rejected.
- **Decomposed (2026-07-01)** into four independently-landable sub-slices (the slice bundled the
  whole of §4; each reuses the conventions from PH7.3a):

  #### PH7.3a — Boundary conventions + small-type round-trips ✅ (2026-07-01)
  The `WitBoundary` adapter trait (`to_wit`/`from_wit -> Result<_, String>` — the WIT
  `result<_,string>` convention in one place); a shared `wit/types.wit` `types` interface for the
  owned mirror records; `bindgen!` wiring (the `plugin` world `use types.{…}` so the host gets
  generated Rust mirrors — types `use`d in the world surface at the crate root, transitively-
  referenced payload records at `crate::lattice::plugin_host::types::*`); round-trips for `Args`,
  `RawCandidate`, `PickerAcceptOutcome`.
  - **Landed:** `wit/types.wit` (`args`/`arg-value` + `candidate-kind`/`candidate-data` (+ payload
    records) + `raw-candidate` + `picker-accept-outcome` (+ `jump-target`/`location`/`command-ref`/
    `lsp-code-action-ref`)); `boundary.rs` (`WitBoundary` + impls for `Args`/`ArgValue`,
    `CandidateKind`/`CandidateData`/`RawCandidate`, `PickerAcceptOutcome`); `benches/boundary.rs`
    (conversion microbench, ~21–47ns off-box). 7 boundary tests green; deps `lattice-picker` +
    `lattice-completion` added.
  - **Typed-error (not lossy) boundaries:** nested `ArgValue::Invocation` (§4.1) and
    `CandidateData::Command` (recursive `SourceLocation`, §4.4) cross only with the command /
    provenance mirror — until then a `WitBoundary` `Err`. Non-serde `RawCandidate` fields
    (`accept_action`/`annotations`/`display_spans`) do NOT cross (crossable core =
    `text`/`display`/`source`/`kind`/`data`); a non-UTF-8 path is a typed error, never lossy.
  - **Deferred:** the headline **< 500ns p99 end-to-end typed-call bench** lands at PH7.3d, where an
    actual guest↔host call exists to measure (the marshalling component is benched here at PH7.3a).

  #### PH7.3b — The `Effect` variant mirror 🚧
  Mirror the **whole** ~105-variant closed `Effect` enum (LOCKED with Dhruva 2026-07-01: whole-enum,
  not a subset — `Effect` is pure data, no `dyn`/`tokio`/`lsp_types`; §4.4 rejects any partial/opaque
  `effect` seam, §14 flags WIT-design-wrong as the highest risk).
  - **Recursion re-plan (LOCKED with Dhruva 2026-07-01):** `Effect` is *recursive* — `Many(Vec<Effect>)`,
    `Global { body: Box<CommandInvocation> }` (and `CommandInvocation → Args → ArgValue::Invocation`),
    plus `AppAction(AppEffect)` pulls in a peer-sized ~100-variant enum. WIT cannot express recursive
    value types, so a literal 1:1 mirror is impossible for those arms. Resolution (Option A, chosen
    over reviving the guest-producible subset): the boundary crosses **`list<effect>`** — `Many` is
    associative composition, flattened non-lossily (`to_wit` flattens, `from_wit` rebuilds `Many` when
    len>1); `Global` + `AppAction` + any `CommandInvocation`-carrying arm cross as typed `WitBoundary`
    errors until their mirrors land (the `ArgValue::Invocation` precedent). No opaque blob — every
    representable arm stays typed.
  - **Staged:** **PH7.3b1a** = the ~12 nested payload record mirrors (`Position`/`Range`/`Edit`/
    `EditKind`/`EditDelta`/`AppliedEdit`/`Register`/`ModalState`(+`VisualKind`/`SearchDirection`)/
    `Selection`(+`VisualMode`)/`SelectionSet`) + their round-trips. Adds
    `SelectionSet::from_parts(Vec<Selection>, usize)` to `lattice-protocol` (the boundary reconstruction
    counterpart to `all()`/`primary_index()`; only `single()`/`cursor_at_origin()` existed). **PH7.3b1b**
    = the ~95-case `effect` variant using those records + the `list<effect>` flatten + exhaustive
    round-trip + typed-error arms. **PH7.3b2** = the `AppEffect` mirror (unblocks `AppAction`).

  #### PH7.3c — `document` resource handle + owned-snapshot projection 📝
  Borrows→owned records (§4.2); the `buffer` WIT `document` resource; `get-text-range` zero-copy
  slice callback so bulk rope text never rides a snapshot.

  #### PH7.3d — Callback-id ↔ guest-export dispatch + async result carrier 📝
  The trampoline (`command_id → guest_export_ref`, §4.1) + the async result-carrier adapter (guest
  returns batches; host owns the `Future`/`mpsc`, §4.3).

### PH7.4 — Picker-source WIT seam + `fuzzy-finder` (⭐ the exit) 📝
The `picker-source` WIT interface mirroring `PickerSourceGenerator`; host adapter wrapping a
WASM component as `Arc<dyn PickerSourceGenerator>` and registering via the `SubsystemBoot`
`install(boot)` seam → `PickerRegistry::register_generator`; the `fuzzy-finder` plugin
(`plugins/fuzzy-finder/`) replicating the `files` source (walk via host-services fs; `Stream`
result; `OpenFile` outcome). Unregister the native `files` source; register the WASM one.
- **Depends:** PH7.2, PH7.3.
- **Exit:** **the native `files` picker is replaced by the WASM one with ZERO host changes;
  every existing picker test + the overhead benches pass.** ← Phase-7 exit criterion.
- **Artefacts:** doc = fragment §9; bench = picker filter per item (< 2μs p99) with the WASM
  source; test = fuzzy-finder end-to-end through `Editor::boot`, parity with native picker
  tests, MRU-for-free; error = source init trap → picker stays closed + echo.

### PH7.5 — Perf gates + CI ratchet 📝
Land every §7 budget row as a CI-gated criterion bench with a ratchet (bar only moves down);
cold-start budget; the no-per-frame-WASM guard (assert the renderer never calls a plugin on
the UI tick).
- **Depends:** PH7.4.
- **Exit:** CI fails on any budget regression ≥ threshold; the picker-replication path is
  under all budgets.
- **Artefacts:** the benches themselves; test = a regression tripwire; doc = benchmarks.md
  rows added.

> **Phase-7 exit reached at PH7.5.** The remaining slices harden the WIT.

---

## Exercised-seam hardening (post-exit, pre-freeze)

Each mirrors one more exercised trait so the WIT is sized against the full set before any
ABI-freeze (design.md §14). All reuse PH7.3 primitives; each is independently landable.

### PH7.6 — Completion-source WIT seam 📝
Mirror `Candidate{Generator,Matcher,Ranker,Annotator}`; host wraps as `Arc<dyn>` via the
`insert_*` seam (not the generic `register_*`); reuse the `CandidateData::Extension` payload
hatch for plugin candidate data. **Depends:** PH7.3.

### PH7.7 — Grammar-extension WIT seam (paramount #3) 📝
Mirror `register_{motion,text_object,operator,ex_command}`; the six closure seams become
guest exports called by command id (§4.1); `parse_args` + `apply` are two exports for
ex-commands; host constructs the native spec with a trampoline `apply`. Validate with a
`tree-sitter-motions`-style test plugin (a motion + a text object). **Depends:** PH7.3.
**Gate:** grammar round-trip < 5μs p99.

### PH7.8 — Event/hook WIT seam 📝
Add `SubscriptionTarget::Plugin { plugin, handler }` delivery (host owns the mpsc; each
`Event` serialized and delivered to the guest `on-event` export as a separate task); `:autocmd`
from a plugin desugars to `subscribe`. **Depends:** PH7.3. (Before-class veto/mutation stays
out — the native bus is observation-only in v1.)

### PH7.9 — Decoration + UI-contribution WIT seam 📝
Mirror `Mode::gutter_decorations` → per-line `GutterDecoration` data (no draw calls); status/
gutter segments, popups, notifications, sprite sets (design.md §9.4 `ui`). Host builds the
decoration snapshot off the render path. **Depends:** PH7.3. **Gate:** segment update < 50μs p99.

### PH7.10 — Config/options WIT seam 📝
Plugin declares typed options (name + type_label + default + doc); host registers via
`register_with_typeid` into the same `ConfigRegistry`; values round-trip as strings; changes
publish `OptionChanged`. `:set`/`:describe-option`/`:customize` treat them uniformly.
**Depends:** PH7.3.

### PH7.11 — Modes declaration WIT seam 📝
`modes` WIT mirroring the `Mode` trait method set; host registers `Arc<dyn DynMode>` into
`ModeRegistry`; keymap contributions land at `KeymapLayer::MinorMode(id)` only (the
`KeymapCapability` write-gate); Guard-`Drop` teardown. (Bundled modes-as-components shipping
is Phase 8 — this slice lands only the declaration WIT + registration path.) **Depends:**
PH7.7, PH7.9.

### PH7.12 — Crash isolation + lifecycle hardening + four-artefact close 📝
Trap → `PluginCrashed` event + quarantine; graceful degradation audit across every seam;
reload/hot-swap seam (teardown + re-instantiate) for the deferred `init.rs`/plugin-manager
consumers; fuzz malformed components/payloads/timing. **Depends:** all above.

---

## Out of scope (deferred — own fragments + slice plans)

- **Modes-as-components (bundled major/minor modes)** — Phase 8. PH7.11 lands only the
  declaration WIT + registration path.
- **Bundled-plugin manager & LSP server manager** — Phase 8 (design.md §5.5.6); the LSP
  manager is the lighthouse that validates WIT sizing.
- **Config-as-WASM (`init.rs`) + `lattice-config-api` crate** — post-Phase-7 (design.md
  §5.12.2–5.12.4); consumes PH7.12's reload seam. `lattice-config-api` does not exist yet.
- **Live-eval (`*scratch:rust*` → `rustc` → dynamic load)** — Phase 10 (design.md §10).
- **ABI-freeze / SemVer versioning policy** — post-1.0, after ≥3 real plugins exercise the
  WIT (design.md §14).
- **Before-class event veto/mutation** — the native bus is observation-only in v1.
