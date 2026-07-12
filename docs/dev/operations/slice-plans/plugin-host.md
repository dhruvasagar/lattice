# Slice plan — Plugin Host (Phase 7)

**Status:** 🚧 in progress (2026-07-03) — PH7.0 ✅, PH7.1 ✅ (7.1a + 7.1b), PH7.2 ✅, **PH7.3 ✅**
(a + b(b1a+b1b+b2) + c + d); **PH7.4 🚧** (⭐ exit, decomposed a–e): **4a ✅** (picker boundary
mirrors + marginalia + context projection), **4b ✅** (host-services `walk` seam — first
guest→host call, capability-gated), **4c ✅** (host adapter; **4c.1a ✅** picker-source world +
async bindings, **4c.1b ✅** per-plugin actor task + `PickerClient` bridge, **4c.2 ✅**
`WasmPickerSource` adapter + async-accept seam), **4d ✅ ⭐** (`fuzzy-finder` validation plugin
— the Phase-7 exit gate MET, parity + overhead benched, no cutover). **PH7.5** (CI perf gates +
ratchet) next. Design fragment:
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

### PH7.3 — Boundary primitives (the crux — §4 of the fragment) ✅ (2026-07-02)
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

  #### PH7.3b — The `Effect` variant mirror ✅ (PH7.3b1a ✅, PH7.3b1b ✅, PH7.3b2 ✅)
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
    counterpart to `all()`/`primary_index()`; only `single()`/`cursor_at_origin()` existed). **PH7.3b1b
    ✅ (2026-07-02)** = the ~101-arm `effect` variant using those records + the `list<effect>` flatten
    + exhaustive round-trip + typed-error arms. **PH7.3b2 ✅ (2026-07-02)** = the `AppEffect` mirror (unblocks `AppAction`).
  - **PH7.3b2 landed:** `wit/types.wit` — the `app-effect` variant (~110 arms) + 4 helper enums
    (`viewport-pos`/`scroll-pos`/`pane-direction`/`hscroll`) + `narrow-lines-payload`; the `effect`
    variant grew an `app-action(app-effect)` arm. `wit/plugin.wit` — world `use` of the new types.
    New module `boundary_app_effect.rs` — `WitBoundary for AppEffect` (compiler-exhaustive both ways)
    + the 4 helper-enum impls, reusing the shared `ModalState`/`VisualKind`/`SearchDirection`/`Register`
    mirrors. `boundary_effect.rs` — the `AppAction` arm now crosses (`app.to_wit()?` / `from_wit`),
    replacing its PH7.3b1b typed error. `benches/boundary.rs` — `boundary_app_effect_round_trip`
    (~13 ns off-box). `OperatorId → CommandId(u64)` crosses via `.0.raw()` / `OperatorId(CommandId::new)`.
    - **Typed-error (not lossy) arm:** `AppEffect::NarrowTrigger { range: Option<Range> }` carries the
      recursive ex-command `lattice_grammar::range::Range` (`RangeBound::Offset { base: Box<RangeBound> }`)
      + a plugin `RangeId` — an `Err` until a range mirror lands (the `Global` precedent). It propagates
      out of the `Effect::AppAction` arm, so an `AppAction(NarrowTrigger)` fails the whole effect cross.
    - Tests (5 new, 45 lib green): AppEffect payload arms (4 helpers + shared mirrors + primitives + the
      `OperatorId` round-trip), unit arms, helper enums, `NarrowTrigger` typed-error; and in
      `boundary_effect.rs`, `AppAction` now round-trips (incl. a `Many` of AppActions) while
      `AppAction(NarrowTrigger)` still errors (the `global_is_a_typed_error` test was split out).
  - **PH7.3b1b landed:** `wit/types.wit` — the `effect` variant (~101 crossable arms) + 5 helper types
    (`quit-scope`/`echo-level`/`substitute-scope`/`utf16-pos`/`lsp-request`) + 14 multi-field payload
    records (`apply-edit-payload`, `yank-payload`, …); `wit/plugin.wit` — world-level `use` of the new
    types so `bindgen!` emits them. `boundary_effect.rs` — `WitBoundary for Effect` with
    `type Wit = Vec<WitEffect>` (the `list<effect>` seam: `to_wit` flattens `Many` recursively,
    `from_wit` rebuilds `Many` when len>1, collapses len==1 to the atom, empty→`None`) + `WitBoundary`
    impls for the 5 helpers + `effect_to_wit`/`effect_from_wit` (both **compiler-exhaustive** — the real
    "every variant covered" guarantee: a new `Effect` arm cannot land without a mapping here).
    `benches/boundary.rs` — `boundary_effect_round_trip` (~92 ns off-box for a 4-arm `Many`).
    - **Typed-error (not lossy) arms:** `Global` (Box<CommandInvocation>, §4.1), `AppAction` (AppEffect,
      PH7.3b2), and defensively `Many` reaching `effect_to_wit` — each an `Err`, propagated out of the
      whole `list<effect>` even when nested inside a `Many`. No opaque blob; every representable arm typed.
    - Tests (6 new, 35 lib green): payload-arm round-trips (all 14 records + 5 helpers + path/option/list
      arms), unit-arm round-trips, helper-enum round-trips, `Many` flatten+rebuild, single/empty
      normalisation, `Global`/`AppAction` typed-error (incl. nested-in-`Many`).

  #### PH7.3c — `document` resource handle + owned-snapshot projection ✅ (2026-07-02)
  Borrows→owned records (§4.2); the `buffer` WIT `document` resource; `get-text-range` zero-copy
  slice callback so bulk rope text never rides a snapshot.
  - **Decisions (locked with Dhruva 2026-07-02):** *(A)* the `document` resource is backed by an
    `Arc<DocumentSnapshot>` — a point-in-time immutable view; edits after the handle is minted never
    shift byte ranges under the guest (chosen over a live `Arc<dyn Document>` = mutation-under-read
    hazard, or a rope-only clone = drops the metadata the snapshot already carries). *(A)* prove at the
    host layer, deferring the guest→host call through the canonical ABI to PH7.3d (the b-series
    precedent). *(C)* `get-text-range(range) -> result<string,string>` (reuses the `range` record; OOB /
    `end<start` is a typed error, mirroring `Buffer::slice`); metadata readers `line-count`/`byte-len`/
    `line`; the owned `buffer-snapshot` record carries id/path/language/cursor/selection.
  - **Landed:** `wit/buffer.wit` — the `document` resource + `buffer-snapshot` record (fills the empty
    stub). `wit/plugin.wit` — `use buffer.{buffer-snapshot}` (emits the record mirror the projection
    targets). New `buffer.rs` — `DocumentResource` (the `Arc<DocumentSnapshot>` backing +
    `get_text_range` slice / `line_count` / `byte_len` / `line_at`, native-typed + unit-tested) +
    `project_buffer_snapshot(&ActiveBufferSnapshot) -> Result<BufferSnapshot, String>` (one-way borrows→
    owned projection; non-UTF-8 path = typed error). Dep: +lattice-runtime (`DocumentSnapshot`).
    `benches/boundary.rs` — `document_get_text_range_one_line` (~400 ns to slice one line out of a
    10k-line buffer: O(log n) locate + O(slice) copy, NOT O(document) — the "zero-copy at the slice
    level" claim, §9.6).
  - **Deferred to PH7.4 (bindgen constraint, not a scope cut):** the resource's host `HostDocument`
    trait impl + `with`-mapping (`document` → `DocumentResource`) + `add_to_linker`. bindgen only binds a
    `with` entry for a resource that a *world function signature* references, and no signature takes a
    `document` until the picker-source `init(ctx)` seam (PH7.4). (Originally slated for 3d, but 3d proved
    the generic trampoline via the fixture world, which doesn't reference `document`.) `use buffer.{buffer-snapshot}` emits the
    record mirror but does NOT satisfy that check; forcing it now (a premature guest fn, or stuffing the
    handle into the snapshot record — which would couple the projection to a `Store` and break its
    unit-testability) was rejected on heuristic #1. `DocumentResource` is the ready-to-`with`-map backing.
  - **Tests (7, 47 lib green):** `get_text_range` slices the requested span; OOB + `end<start` typed
    errors; metadata readers match the buffer (`Buffer::line` strips the trailing newline); snapshot
    backing is immutable under a later edit (decision A); projection projects metadata-not-text +
    handles absent optionals. Existing instantiate tests unaffected by the world change.

  #### PH7.3d — Callback-id ↔ guest-export dispatch + async result carrier ✅ (2026-07-02)
  The trampoline (§4.1) + the async result-carrier adapter (guest returns batches; host owns the
  loop, §4.3), proven **end-to-end against a real guest**.
  - **Decision (locked with Dhruva 2026-07-02, option A):** 3d's substance — a guest-export call
    through the canonical ABI + the `<500ns` bench — cannot be stubbed (unlike a/b/c, there's no
    host-only layer left; the projection is 3c, the effect back-map is 3b). So bring a **minimal
    `wasm32-wasip2` fixture guest** forward (the toolchain PH7.0 deferred to PH7.4) rather than defer
    again (B, leaves §14's highest risk — ABI-design-wrong — unvalidated two more slices) or merge into
    a PH7.4 mega-slice (C). The fixture is the permanent ABI regression test.
  - **Landed:** `wit/trampoline-fixture.wit` — a test-only world exporting `apply-effect(args) ->
    list<effect>` (§4.1 shape) + `next-batch() -> list<string>` (§4.3 shape). `tests/fixtures/
    trampoline-guest/` — a standalone-workspace guest crate (own `target/`, gitignored) built with
    `wit-bindgen` to a `wasm32-wasip2` **component**. `build.rs` — builds the guest at host-compile time
    (env-scrubbed nested cargo + pinned `--target-dir` so leaked `CARGO_ENCODED_RUSTFLAGS` /
    `CARGO_TARGET_DIR` can't break the wasm build), hands the path via `TRAMPOLINE_GUEST_WASM`, and
    **degrades gracefully** (empty var → test/bench skip) when the target is absent. `src/trampoline.rs`
    — the generic `collect_batches` §4.3 carrier (host owns the loop; empty batch = exhausted) as real
    lib machinery PH7.4's picker Stream reuses, + a `#[cfg(test)]` fixture module whose **second
    `bindgen!` reuses the host's generated `types`** (`with:` → so the guest-returned `Effect` is the
    SAME Rust type `WitBoundary::from_wit` consumes) driving real guest calls. `benches/trampoline.rs`
    — the deferred `<500ns` headline: `trampoline_apply_effect_warm_call` ~437 ns median (a real warm
    guest↔host typed call), retiring §14's highest risk with a real number.
  - **Toolchain:** `rustup target add wasm32-wasip2` (Rust 1.94 emits a component directly — no
    `wasm-tools`/`cargo-component` needed). CI must add the target for the fixture test + perf gate to
    run (they skip otherwise).
  - **Tests (5, 52 lib green):** `collect_batches` aggregate + error-propagation (no guest); and 3
    fixture tests through a REAL component — `apply-effect` round-trips an `Echo` payload arm (data
    flows in), rebuilds `Many` from a `list<effect>`, and the `next-batch` carrier aggregates
    `["a","b","c"]` host-side.
  - **NB — the PH7.3c `document` resource wiring lands at PH7.4, not here.** 3d proved the *generic*
    trampoline/carrier mechanism via the fixture world; the `document` resource's `HostDocument` impl +
    `with`-mapping + `add_to_linker` still need a *world function signature* that references a `document`
    handle, which first appears in the picker-source `init(ctx)` seam (PH7.4). `DocumentResource` is the
    ready backing.

### PH7.4 — Picker-source WIT seam + `fuzzy-finder` (⭐ the exit) 🚧
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

- **Decomposed (2026-07-03)** into five independently-landable sub-slices. The carve
  incorporates the picker-API scope Dhruva locked at the start of PH7.4: **(1)** expose *the
  API, not sources* — one generator contract (`spec`/`init`/`accept`) any source (native or
  plugin) satisfies, never per-source WIT; **(2)** plugins both *create* new sources AND
  *utilize* existing ones (guest→host open-picker); **(3)** plugins *define + populate
  marginalia columns* (the whole `Annotation` enum crosses — un-defers the PH7.3a exclusion).
  Marginalia decision (locked): whole enum crosses, host lays out `AnnotationColumns` (a
  render-consumed projection stays host-side per the substrate-vs-consumer rule).

  #### PH7.4a — Picker boundary type mirrors + projection ✅ (2026-07-03)
  The plugin-facing API's data types + round-trips (the PH7.3a "conventions + small
  round-trips" character, extended to the picker seam). Host-layer only — no guest, no
  host-services, no adapter, no cutover.
  - **Landed:** `wit/types.wit` — the marginalia mirrors (`annotation` variant + `key-chord`/
    `key-kind`/`special-key` + `annotation-segment`/`-custom`/`-styled`), `annotations` added
    to `raw-candidate`; the source-API records (`arg-kind`/`arg-default`/`arg-spec`,
    `picker-source-spec`); `routing-payload` (+ `resolve-diff`/`lsp-instance`/`show-message-action`
    payloads; reuses `location`/`jump-target`/`command-ref`) + `open-target`; the owned
    `picker-context` projection (`active-buffer-snapshot`/`buffer-entry`/`position-entry`/
    `position-source`/`symbol-location`). `wit/plugin.wit` — world-level `use` of the new types.
    New `boundary_picker.rs` — `WitBoundary` for `KeyChord`/`SpecialKey`/`Annotation`/`ArgKind`/
    `ArgDefault`/`ArgSpec`/`PickerSourceSpec`/`OpenTarget`/`RoutingPayload` (all compiler-
    exhaustive both ways) + `project_picker_context` (one-way borrows→owned, the
    `project_buffer_snapshot` precedent). `boundary.rs` — `RawCandidate` now round-trips
    `annotations`. `benches/boundary.rs` — a marginalia-carrying candidate + a `RoutingPayload`
    round-trip.
  - **The `&'static str` seam (finding):** native `PickerSourceSpec`/`ArgSpec` hold `&'static str`
    (compile-time source ids); a WASM plugin supplies owned runtime strings, so the adapter
    **interns** them (`Box::leak`, no `unsafe`) in `from_wit`. Bounded by loaded-source count —
    each spec leaked once at registration; **unbounded re-registration (hot reload) is a PH7.12
    concern.**
  - **Deferred to PH7.4c (with the guest world):** the `document` resource wiring (the PH7.3c
    `HostDocument` impl + `with`-map + `add_to_linker`) — it needs a *world function signature*
    referencing `document`, which first appears in the `init(ctx)` guest export (4c). The
    active buffer's rope text + `syntax_highlights` ride that resource, not the context
    projection; a fuzzy-finder needs neither. `DocumentResource` (PH7.3c) is the ready backing.
  - **Utilize-an-existing-picker (finding):** the `effect` mirror already carries
    `open-picker-payload` (`Effect::OpenPicker { source, args }`), so a plugin command handler
    emitting that effect *already* opens an existing picker — the "utilize" path is largely free
    via the effect vocabulary. PH7.4e only needs to formalise/validate it.
  - **Tests (8, 60 lib green):** key-chord char/special/mods round-trip + `f(0)` typed error;
    every `Annotation` variant (incl. `Styled` perms cell + `Keybinding`); arg-spec +
    picker-source-spec through intern; open-target; representative routing-payload arms;
    non-UTF-8 path typed error.

  #### PH7.4b — `host-services` fs-walk seam ✅ (2026-07-11)
  The first guest→host call direction: a capability-gated fs `walk` against the plugin's
  `CapabilityGrant` (PH7.2). The `fuzzy-finder` (PH7.4d) walks the workspace through this.
  **Depends:** PH7.4a. **Decision (locked with Dhruva, option A):** a bounded `host-services`
  `walk` call (host runs the native walker, returns a path list) over raw WASI walk in the
  guest (B, scatters walk policy per-plugin — heuristic #1) or a streaming `dir` resource (C,
  the deferred §15 streaming shape — premature until a live-grep-class consumer lands).
  Protects paramount-#2: the host centralizes walk policy so a plugin source enumerates
  identically to the native `files` source.
  - **Landed:** `wit/host-services.wit` — the `walk(root) -> result<list<string>, string>`
    seam (replaces the empty stub). `wit/plugin.wit` — `import host-services;` so bindgen emits
    the host `Host` trait + `add_to_linker`. New `host_services.rs` — `walk_within_grant`
    (capability gate + native `walk_files_for_picker` policy + non-UTF-8 skip). `lib.rs` —
    `PluginState` now carries the `CapabilityGrant` (threaded through `instantiate_inner`); the
    `Host` impl forwards `walk`; `host_services::add_to_linker::<_, HasSelf<_>>` wires it into
    the (async) linker (sync host func is fine — `walk` is bounded, no suspend).
  - **The capability gate is host-side, mandatory (§6).** Unlike the guest's WASI fs view
    (sandboxed by the `Store`'s preopens), a host-services call runs with full host authority,
    so `walk` re-checks `root ⊆ grant.fs` itself; canonicalized both sides so `..` can't escape
    a prefix; denial is a typed `Err` + an `info!` (user-actionable). Empty grant → reaches
    nothing.
  - **Bench:** cost is OS-bound (the native directory walk, which the native `files` source
    already pays) + a negligible per-call gate; the `list<string>` marshalling is characterized
    by the existing boundary benches. No dedicated fs microbench (it would measure the OS, not a
    boundary overhead). The **guest→host call-overhead** bench lands at PH7.4d, where a real
    guest calls `walk` (mirroring PH7.3d's host→guest bench precedent); the CI-gated per-call
    budget is PH7.5.
  - **Proof depth:** host-layer (the capability gate + walker: granted returns paths, denied
    root / empty grant are typed errors, ignore-policy applies, sub-dir of a granted prefix
    permitted — 5 tests) + the linker wiring proven by the existing instantiate/runtime
    integration tests (a real component instantiates against the host-services-wired `plugin`
    linker). The real guest→host `walk` *call* rides the `fuzzy-finder` consumer at PH7.4d (the
    PH7.3c "prove host-layer, defer the guest call to the real consumer" precedent).
  - **Tests (5, 65 lib green) + 16 integration green.**

  #### PH7.4c — Host adapter (create path) 🚧
  Wrap a component's `picker-source` exports as `Arc<dyn PickerSourceGenerator>` and register
  via `install(boot)` → `register_generator` with a host-stamped `SourceLayer::Plugin(id)`.
  **Depends:** PH7.4a, PH7.4b. **Acid test:** ZERO `Editor::` methods, ZERO new `Action`
  variants. **Decomposed (2026-07-12)** — the Send+Sync adapter must call an async,
  single-threaded, fuel-bounded guest export, so the Store-ownership model is the crux.
  **Decision (locked with Dhruva, option A): the per-plugin actor task** — each plugin runs as
  a dedicated async task owning its `Store`; the adapter holds an mpsc `Sender`; `init`/`accept`
  send a request + await a oneshot reply (design §5.7; no lock, Store stays single-threaded,
  per-call fuel arming in the task loop, and the bridge is reused by every future guest-backed
  `Arc<dyn>` adapter — completion/grammar/modes). Over `Arc<async-mutex<Store>>` (B, a lock
  across `.await` + a runtime dep in the lib, weaker foundation). Sub-sliced:

    #### PH7.4c.1a — `picker-source` world + async export bindings ✅ (2026-07-12)
    `wit/picker-source.wit` — the `picker-source` interface (`spec`/`init`/`accept` +
    `candidate-pair`) + the `picker-source-plugin` world (import `host-services`, export
    `picker-source`). `picker_host.rs` — the **second `bindgen!`** for that world, reusing the
    `plugin` world's `types` + `host-services` via `with:` (the PH7.3d shared-types trick). 65
    lib green.
    - **The `document` handle is deferred — and reframed (Dhruva, 2026-07-12).** Its original
      motivation ("faithfully mirror what native sources like `:picker lines` can read") is
      **dropped: exposing built-in sources via WASM has no value.** Built-in sources stay native
      Rust forever; the WASM picker API exists so **plugins create *custom* pickers**, not to
      re-implement built-ins. Active-buffer *text* access is therefore not a mirroring
      requirement but a **future plugin capability** (a custom picker that wants buffer content),
      added — capability-gated, like `host-services` — only when a real plugin needs it ("the API
      grows from real plugins", §5.5). The ⭐ exit needs none of it (`fuzzy-finder`/`files` walks
      the fs). *Mechanics note for when it lands:* passing a **host-owned resource into a guest
      export** has a bindgen subtlety (a resource referenced only by an exported signature is not
      seen as a host `with`-mapped import); wasi-http is the precedent that it is solvable —
      resolve the world shape then, not now. The PH7.3c `DocumentResource` backing stays ready.

    #### PH7.4c.1b — Per-plugin actor task + call protocol ✅ (2026-07-12)
    The bridge (`src/picker_task.rs`): the `PickerCall` enum (Spec/Init/Accept, each carrying a
    `oneshot` reply), the `PickerActor` task loop owning the `Store<PluginState>` + picker
    bindings (per-call fuel/epoch armed inside the loop via the extracted `arm_store`), and the
    `Send+Sync` `PickerClient` (an mpsc `Sender` clone; serializes calls onto the single-consumer
    loop the `!Sync` `Store` needs). `PluginHost::spawn_picker_source` instantiates the
    `picker-source-plugin` world under the same grant/data-dir/WASI as `instantiate_plugin`
    (shared `build_plugin_wasi` + `new_store` extraction) and returns `(PickerClient, PickerActor)`;
    the **caller** drives `PickerActor::run` on its multi-thread runtime — the lib still owns no
    runtime (`futures::channel`, chosen over `tokio::sync` to keep that invariant; `tokio` stays a
    dev-dep). A closed channel surfaces as the new typed `PluginHostError::PluginGone` (the caller
    stays live); a trap does not end the loop (the `Store` survives a clean fuel/epoch trap;
    quarantine is PH7.12).
    - **Proof:** a real `wasm32-wasip2` fixture guest (`tests/fixtures/picker-guest`, built by the
      extended `build.rs` → `PICKER_GUEST_WASM`) driven through the channel by `tests/picker_actor.rs`
      (3 tests): spec/init/accept round-trip with inputs (`args` + the `PickerContext` projection)
      provably crossing (the fixture echoes them), the guest's own WIT `err` surfacing as the inner
      `Err` (distinct from a host trap), and the `PluginGone` path after the actor ends. Skips
      cleanly when the wasm target is absent (the trampoline-bench precedent).
    - **Bench:** no new microbench — the per-call cost is the wasm typed call already characterized
      by the PH7.3d trampoline bench; the channel adds a sub-µs mpsc+oneshot hop. The end-to-end
      picker overhead bench rides the real `fuzzy-finder` consumer at PH7.4d (per this plan's §4b
      note), CI-gated at PH7.5.
    - **Public API note:** the picker WIT records the bridge traffics in are re-exported `pub` from
      `picker_task` (`PickerContext`, `RawCandidate`-bearing `CandidatePair`, `RoutingPayload`,
      `PickerAcceptOutcome`, `PickerSourceSpec`, + the `PickerContext` construction family) so
      callers get a clean path, not `crate::lattice::…`. **Depends:** PH7.4c.1a.

    #### PH7.4c.2 — `WasmPickerSource` adapter + async-accept seam ✅ (2026-07-12)
    `impl PickerSourceGenerator` over the `PickerClient` (`lattice-plugin-host/src/picker_source.rs`),
    boundary conversions via PH7.4a. The trait is synchronous but the guest exports are async +
    actor-bound, resolved per method: **`spec`** fetched + converted once at `connect` (cached,
    so `spec(&self)` is a borrow); **`init`** → `PickerInitResult::Future` (sync prelude projects
    `PickerContext`→WIT via `project_picker_context`, the `'static` future awaits `client.init`
    off-thread + converts the candidate pairs back), dropping into the host's existing
    `pending_picker_init` drain; **`accept`** via a **new generic async-accept seam** (below), with
    the sync `accept` a defensive tripwire.
    - **The async-accept seam (option C, locked with Dhruva).** `PickerSourceGenerator` gains
      `accept_async(&self, ctx, routing) -> Option<AcceptFuture>` (default `None` — every native
      source unchanged). A `Some` return is spawned on the LSP runtime exactly like the init
      Future and committed by the new `Editor::drain_pending_picker_accept` on the shared
      `async_landed` wake (`PendingPickerAccept` state; MRU + the accepted event fire at accept
      time, only the outcome application defers). This keeps paramount #4 intact — no synchronous
      path from a keystroke to plugin code, so a slow/hostile plugin accept can never freeze the
      actor thread. Rejected: pre-resolving accept at init (O(N) guest calls + stale ctx) and
      blocking the actor thread (a plugin could freeze the UI up to the epoch deadline). **No
      renderer changes** — the drain rides the shared sequence + wake both TUI and GPUI already
      run (parity automatic).
    - **Proof:** `lattice-plugin-host/tests/picker_source.rs` (2) drives the fixture guest through
      the adapter — spec convert + registry registration, `init` Future → native batch (inputs
      echoed → provably crossed), `accept_async` Future → native outcome, and the guest WIT `err`
      as the future's `Err`. `lattice-host` `async_accept_defers_then_drain_commits_the_outcome`
      proves the host seam: an async source defers at accept (picker closes, pending set, nothing
      applied sync) and `drain_pending_picker_accept` commits + clears it (one-shot open-target
      invariant upheld on the async path).
    - **Bench:** none new — the per-call cost is the wasm typed call (PH7.3d trampoline bench) plus
      a sub-µs channel hop; the end-to-end picker overhead bench rides `fuzzy-finder` at PH7.4d,
      CI-gated at PH7.5.
    - **Scope calls:** (1) no `SourceLayer::Plugin` stamping at registration — a picker source's
      identity is `spec().id`; provenance is a grammar-contribution concern (PH7.7). (2) The full
      `install(boot)` SubsystemBoot picker seam + component discovery is deferred to PH7.4d — the
      plugin host is not boot-wired yet and `SubsystemBoot` exposes no picker accessor; 4c.2 proves
      registration via `PickerRegistry::register_generator` directly (+ the `connect_picker_source`
      helper that wraps `Arc<dyn PickerSourceGenerator>`). **Depends:** PH7.4c.1b.

  #### PH7.4d — `fuzzy-finder` validation plugin ⭐ ✅ (2026-07-12)
  The real `wasm32-wasip2` guest (`plugins/fuzzy-finder`) replicating the native `files`
  picker, proving the plugin substrate end-to-end through ONLY the generic seams: it walks the
  workspace via the capability-gated `host-services` `walk` (PH7.4b) and emits `OpenFile`
  candidates via the `picker-source` world; the host drives it through the `WasmPickerSource`
  adapter (PH7.4c.2) + `PickerClient` bridge (PH7.4c.1b).
  - **Reframe (Dhruva, 2026-07-12) — NO cutover.** The original "unregister native `files`,
    register the WASM one" is **dropped**: replacing a native built-in with a slower WASM
    re-implementation has negative user value. **Built-in sources stay native Rust forever**
    (§5.5 / the 4c.1a reframing — the WASM picker API exists so plugins author *custom* pickers,
    not to re-implement built-ins). So `fuzzy-finder` is a **test/bench validation artifact**
    only: never registered in the shipping editor, no boot wiring, no `PluginHost`-in-`Editor`.
    The plugin uses a DISTINCT id (`"fuzzy-finder"`), so if ever loaded it is an *additive*
    custom source, never a cutover. This satisfies the §13 exit *intent* (a plugin replicates
    the file picker via generic seams + CI budgets) without shipping a worse picker.
  - **Parity is by construction.** The host's `walk` reuses the SAME
    `lattice_picker::picker_sources::walk_files_for_picker` the native `FilesSource` runs, so the
    candidate set matches native exactly. `tests/fuzzy_finder_parity.rs` (2) formalises it: the
    `(display, OpenFile-path)` sets are equal for a temp tree, and `accept` resolves the same
    `OpenFile` outcome (through the async-accept seam). Skips cleanly without the wasm target.
  - **Overhead bench.** `benches/fuzzy_finder.rs` — the warm `init` through the bridge (channel
    hop + guest export + guest→host `walk` round-trip + candidate marshalling): descriptive
    baseline ≈110µs for a 50-file tree (the walk + 50-pair marshalling dominate; the per-call
    bridge overhead is sub-µs). The guest→host call-overhead bench the plan reserved for here.
    CI-gated at PH7.5.
  - **Build.** `plugins/fuzzy-finder` is a standalone `[workspace]` crate (not a main-workspace
    member, like the fixture guests), built by `build.rs`'s shared `build_guest` (generalised to
    an explicit path) → `FUZZY_FINDER_WASM`.

  > **⭐ Phase-7 exit gate MET:** a WASM plugin replicates the file picker using only generic
  > seams (zero bespoke host code, zero `Editor::` methods, zero `Action` variants); overhead is
  > benched. PH7.5 turns the benches into CI-gated ratchets. **Depends:** PH7.4c.

  #### PH7.4e — "Utilize an existing picker" seam 📝
  Formalise/validate the guest→host open-picker path (a plugin opens/reuses an existing
  source). Largely expressible today via `Effect::OpenPicker`; this slice proves it end-to-end
  and adds any missing surface. Post-exit (the exit gate doesn't require it). **Depends:** PH7.4c.

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
