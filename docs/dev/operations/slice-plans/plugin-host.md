# Slice plan — Plugin Host (Phase 7)

**Status:** **Phase-7 PROPER COMPLETE (PH7.0–PH7.5 ✅) — ⭐ exit reached (2026-07-12).** PH7.0 ✅,
PH7.1 ✅ (7.1a + 7.1b), PH7.2 ✅, **PH7.3 ✅** (a + b(b1a+b1b+b2) + c + d); **PH7.4 ✅** (⭐ exit,
a–e): **4a ✅** (picker boundary mirrors + marginalia + context projection), **4b ✅**
(host-services `walk` seam — first guest→host call, capability-gated), **4c ✅** (host adapter;
**4c.1a ✅** picker-source world + async bindings, **4c.1b ✅** per-plugin actor task +
`PickerClient` bridge, **4c.2 ✅** `WasmPickerSource` adapter + async-accept seam), **4d ✅ ⭐**
(`fuzzy-finder` validation plugin — exit gate MET, parity + overhead benched, no cutover);
**PH7.5 ✅** (CI perf gates — ratchet TESTS on the exercised §7 rows + the no-per-frame-WASM
dep-graph guard + `wasm32-wasip2` in CI); **PH7.6 ✅** (completion-source seam — generator-only,
LSP-async pattern; the other 3 traits type-mirrored). **PH7.7 ✅ COMPLETE** (grammar-extension
seam — **sync trampoline, not the async actor**, fork locked): 7.7a ✅ (boundary mirrors +
projections + marshalling bench), 7.7b ✅ (two-interface sync `grammar` world + 4th `bindgen!` +
`register-*` recording), 7.7c ✅ (sync trampoline + `register_plugin_*` cross-crate seam +
`PluginBudget::grammar`, validated e2e through a real wasm fixture), 7.7d ✅ (`< 5 µs p99` gate:
~340 ns release round-trip; the bench caught the **F1** epoch false-positive → fuel-primary
Reflex budget). **PH7.8 ✅ COMPLETE** (event/hook seam — dedicated async `events-plugin` world +
the reserved `SubscriptionTarget::Plugin` slot filled with a type-erased lock-dropped sink;
`events-guest` fixture proves delivery + graceful trap-isolation e2e; `< 250 µs` gate at
~3.75 µs debug). **PH7.9 ✅ COMPLETE** (decoration seam — the `Mode::gutter_decorations` mirror
as an async off-render producer, the completion PH7.6 fork; `ui` row type-mirror-only;
`decorations-guest` fixture proves producer→cache e2e; `< 50 µs` gate at ~63 µs debug; renderer
read-from-cache is a tracked Phase-8 boot-wiring item). **PH7.10–7.12** (WIT-seam hardening before
ABI freeze) remain. A conformance
audit of the whole host is `../../audit/plugin-host-architecture.md` (8 findings; **F1
resolved**). Design fragment:
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

### PH7.5 — Perf gates + CI ratchet ✅ (2026-07-12)
The CI gate on the §7 plugin budgets. The ratchet is a **test** (not a criterion compare),
mirroring `lattice-host/tests/keystroke_publish_ratchet.rs`: `cargo test --workspace` (already
the CI correctness gate) runs `tests/perf_ratchet.rs`, which measures a warm op inline and
asserts a **generous absolute ceiling** — orders of magnitude above the real release cost, so it
catches a *gross* regression (an O(file) term, a boundary blowup, a lost module cache) without
flapping on the ~20% GitHub-runner variance or debug inflation. The criterion benches
(`trampoline`, `fuzzy_finder`, `instantiate`) stay the descriptive record on `main`.
- **Scope (locked with Dhruva) — gate the EXERCISED rows only.** Three §7 rows have real benches
  today and are gated: **typed host call** (`typed_call_stays_within_ceiling`, on the trampoline
  fixture — §7 `<500ns p99` release; debug median ≈2µs, ceiling 50µs), **guest→host picker path**
  (`picker_init_round_trip_stays_within_ceiling`, the fuzzy-finder init round-trip; debug median
  ≈130µs, ceiling 20ms), **cold-start** (`cold_start_50_instantiations_stays_within_ceiling` — §7
  `50 plugins < 30ms` release; debug ≈200µs for 50, ceiling 2s). The forward-looking rows
  (grammar round-trip, status/gutter segment, picker-filter-per-item, major-mode event) map to
  seams that don't exist yet (PH7.6–7.11) — you can't bench non-existent code, so each lands its
  own ratchet with its seam. PH7.5 establishes the pattern.
- **No-per-frame-WASM guard (paramount #4, by construction).** `tests/no_per_frame_wasm_guard.rs`
  asserts the renderer crates (`lattice-ui-tui`/`lattice-ui-gpui`) do not list `lattice-plugin-host`
  as a **runtime** dependency — a renderer that cannot *name* the plugin host cannot call it on
  the tick. Chosen (option a) over deferring: the plugin host is validation-only (no boot wiring),
  so there is no per-frame plugin call to catch dynamically yet; the structural dep-graph guard is
  the enforceable form NOW. Checks DIRECT runtime deps only — a transitive path renderer →
  `lattice-host` → plugin-host is *expected* once plugins are boot-wired, and even then the
  renderer reaches plugins only through `lattice-host`'s host-mediated + off-thread API.
- **CI wiring.** `ci.yml` test job gains `targets: wasm32-wasip2` so `cargo test --workspace`
  builds the plugin guests (via build.rs) and the ratchets/parity/picker tests actually run in CI
  instead of skipping (they skip gracefully without the target — local boxes / forks unaffected).
- **Artefacts:** ratchets = `tests/perf_ratchet.rs` (3) + `tests/no_per_frame_wasm_guard.rs` (1);
  descriptive benches already exist; `benchmarks.md` §7 plugin-host rows added. **Depends:** PH7.4.

> **Phase-7 exit reached at PH7.5 ✅.** The remaining slices (PH7.6–7.12) harden the WIT against
> the rest of the exercised seams before the ABI freeze.

---

## Exercised-seam hardening (post-exit, pre-freeze)

Each mirrors one more exercised trait so the WIT is sized against the full set before any
ABI-freeze (design.md §14). All reuse PH7.3 primitives; each is independently landable.

### PH7.6 — Completion-source WIT seam ✅ (2026-07-12)
The completion analogue of the picker seam (PH7.4c), but **generator-only** — a design fork
reviewed + locked with Dhruva.
- **The fork.** The plan said "mirror `Candidate{Generator,Matcher,Ranker,Annotator}`; host wraps
  as `Arc<dyn>` via `insert_*`." But the completion pipeline (`CompletionPipeline::run` /
  `compute_completion_state`) is **synchronous on the keystroke path**, and `matches` + `annotate`
  run **per candidate** (pipeline.rs:75,100) — crossing them to an async, actor-bound guest per
  item = hundreds of boundary calls per keystroke (paramount #1). So all-four-via-`insert_*`
  conflicts with reality. **Chosen: option A — generator-only, LSP-async pattern.** A WASM
  completion source produces candidates *asynchronously off the keystroke path* (the LSP
  precedent: `match_and_rank` "pre-supplies rows from async LSP responses"), and the host runs the
  NATIVE matcher/ranker/annotator over them. Matching/ranking/annotation stay native (good
  defaults plugins rarely override — "API grows from real plugins" §5.5); their data types are
  still mirrored in `types.wit` so the WIT is sized against the whole trait set. Rejected: (B)
  all-four with batch-reframed per-candidate calls (diverges from the native trait shape,
  paramount #3), (C) type-mirror-only (the mirror shape depends on the A-vs-B choice, so it
  doesn't avoid the fork).
- **What landed.** `wit/completion-source.wit` — the `completion-source` interface (`spec` +
  async `generate(ctx) -> result<list<raw-candidate>, string>`) + the `completion-source-plugin`
  world (import `host-services`, export `completion-source`). `types.wit` += `generate-context`
  (prefix + case-sensitive; `&Buffer`/`&CommandRegistry` don't cross — the document-handle
  precedent) + `completion-source-spec` (id/doc). `completion_host.rs` — the THIRD `bindgen!`
  (shared types via `with:`). `completion_task.rs` — the `CompletionActor`/`CompletionClient`
  bridge (a focused DUPLICATE of `picker_task.rs`; rule-of-three — generalise the actor over the
  bindings type at the 3rd consumer, grammar/PH7.7; reuses `arm_store`/`new_store`/
  `build_plugin_wasi`). `completion_source.rs` — `WasmCompletionSource`, an **async producer**
  (NOT `impl CandidateGenerator` — the sync trait can't await the guest): `connect` caches the
  spec, `generate(prefix, case) -> Vec<RawCandidate>` drives the guest + converts. Candidates use
  the `candidate-data.extension` hatch (the plugin-payload path).
- **Proof.** `tests/completion_source.rs` (2): the fixture `completion-guest` produces 4 keyword
  candidates (Extension data crossing verified), fed through the native `match_and_rank` — the
  fuzzy matcher keeps only the `"al"`-matching ones (`alpha`/`alphabet`), proving the
  produce→native-match flow; + the `PluginGone` path after the actor ends. `benches/completion.rs`
  = warm `generate` overhead (no walk — isolates the bridge + produce cost). Skips without the
  wasm target; `benchmarks.md` row added.
- **Not executable (by design):** matcher/ranker/annotator as WASM guest exports — deferred until
  a real plugin needs a custom matcher (and even then a batch-reframe, not per-candidate).
  **Depends:** PH7.3.

### PH7.7 — Grammar-extension WIT seam (paramount #3) 🚧
The plugin-facing **extension API**: the guest→host surface a plugin calls to *contribute*
new grammar (`register_{motion,operator,text_object,ex_command,action}`). The grammar
*handling* — the dispatcher, the `:`-line + chord parser, operator∘motion composition,
ranges, counts, registers — stays native, **sync**, and untouched; a plugin only adds
entries. **The PH7.7 fork (locked with Dhruva):** unlike picker (PH7.4) / completion
(PH7.6), which are async off-keystroke producers on a per-plugin actor task, grammar
`apply` is **synchronous** on `dispatcher::execute` (a motion must return a `MotionResult`
inline to compose with its operator). So the seam is a **sync trampoline**, not the async
actor — the completion_task.rs "grammar is the 3rd async-actor / rule-of-three" premise is
FALSIFIED (grammar is a different shape). wasmtime-46 makes sync guest calls available on
the same engine (the PH7.3d trampoline-fixture proves it); fuel + epoch trap synchronously,
so a runaway plugin motion no-ops with a warn (UX: no hang) without any async plumbing on
the keystroke path. Built-ins stay native and never pay it. **Depends:** PH7.3.
**Gate:** grammar round-trip < 5μs p99 (PH7.7d).

#### PH7.7a — Grammar boundary type mirrors + projection ✅ (2026-07-12)
The owned WIT mirrors + host-side `boundary_grammar.rs` conversions, no guest yet.
- **What landed.** `wit/types.wit` += the grammar-extension section: the five dispatch
  contexts (`motion-context`, `operator-context`, `text-object-context`,
  `ex-command-context`, `action-context`), `motion-result`, the five spec-metadata records
  (`motion-spec`/`operator-spec`/`text-object-spec`/`ex-command-spec`/`action-spec` — each
  the native `*Spec` minus its `apply`/`parse_args` closure, since the behavior is a guest
  export called back by callback-id at 7.7c, not a field), plus `count` (= `u32`),
  `latency-class`, `surface-form`. `position`/`range`/`args`/`register`/`effect`/`arg-spec`
  were already mirrored (PH7.3b/PH7.4a) and are reused. NB `%from` (a WIT keyword) is
  escaped in `motion-context`.
- **`boundary_grammar.rs` (new).** `WitBoundary` for `LatencyClass`, `SurfaceForm`
  (interned `&'static str` hint, the `boundary_picker::intern` precedent), and
  `MotionResult`; five one-way `project_*` fns for the contexts (host→guest, the
  `project_picker_context` precedent — contexts carry `&Buffer`/`&CancellationToken`/
  `Option<&dyn ScopeResolver>` borrows so they can't round-trip; the guest never sends a
  context back). Bulk buffer text never rides a context — it crosses via the `document`
  handle (§4.2), so a projection reads only owned scalars. The WIT-record → native-`*Spec`
  direction is 7.7c's trampoline job (needs the callback closure), so no spec `from_wit`
  lands here. `ExCommandContext.range` (recursive grammar `Range`) is absent by design (the
  `Global`/`NarrowTrigger` precedent).
- **Proof.** 8 unit tests (enum + `MotionResult` round-trips; each context projection).
  `benches/grammar.rs` (5 marshalling benches) → `benchmarks.md` PH7.7a row: every
  conversion is tens of ns (motion ctx ~41 ns, operator ctx ~42 ns, effect `from_wit`
  ~31 ns, text-object ctx ~11 ns, motion-result round-trip ~314 ps) — < 1% of the 5 µs
  budget, so the wasmtime call (7.7c/d) owns effectively the whole budget. 73 crate lib
  tests green.

#### PH7.7b — `grammar` world + sync bindgen + `register-*` host imports ✅ (2026-07-12)
The WIT + the 4th `bindgen!` + the contribution-recording path. No guest yet (the
trampoline that calls the callbacks + registers into `CommandRegistry` is 7.7c).
- **The shape (option A, locked with Dhruva).** WIT can't import+export the same interface
  name, so the seam is **two interfaces + a sync world**. `interface grammar` (host-provided,
  guest **imports**) = the extension API: `register-{motion,operator,text-object,action}(name,
  doc, spec, callback: u32)` + `register-ex-command(name, doc, spec, parse-callback,
  apply-callback)`. `interface grammar-callbacks` (guest **exports**) = the behavior,
  dispatched by callback-id (the PH7.3d trampoline): `apply-motion → result<motion-result>`,
  `apply-{operator,action,ex-command} → result<list<effect>>` (the `Effect::Many`-flatten
  boundary form), `apply-text-object → result<range>`, `parse-ex-args → result<args>`. `world
  grammar-plugin { import grammar; export register-grammar: func(); export grammar-callbacks; }`.
- **Sync, no actor (the PH7.7 fork).** The `bindgen!` sets **no** `exports: { default: async }`
  — `register-grammar` + the `apply-*` callbacks are sync-callable from the dispatch thread.
  Registration entry is a dedicated sync `register-grammar()` export (option A over "drive via
  async `plugin::activate`", option B) — keeps the whole grammar seam on one sync path, no
  async/sync store mixing; matches how `picker-source`/`completion-source` carry their own
  world export rather than driving registration through the async lifecycle. `buffer` is NOT
  imported yet — a v1 motion computes from the projected context scalars; the `document` handle
  for text-reading/structural motions is the deferred follow-on (picker's `init(doc)` precedent;
  audit F4).
- **What landed.** `wit/grammar.wit` populated (the two interfaces + world). `grammar_host.rs`
  (new) — the 4th `bindgen!` (sync; shared `types` via `with:`) + `RecordedContribution` (per-kind
  name/doc/WIT-spec/callback) + `GrammarContributions` (the `record_*` accumulator, factored like
  `host_services::walk_within_grant` so it unit-tests without a guest). `lib.rs` — `PluginState`
  gains a `grammar_contributions` field; the generated `grammar::Host` is impl'd on `PluginState`
  (the `register-*` bodies forward to the accumulator — sync + infallible, name-collision/registry
  errors surface at drain, not here); the `grammar` import is wired into the shared linker (inert
  for worlds that don't import it). **Acid test held:** ZERO `Editor::` methods, the register API
  is the same imperative shape as native `register_*`.
- **Proof.** 1 unit test (records all five kinds through the accumulator, preserves callback ids,
  `take()` drains). 74 crate lib tests green; full build clean (the 4th bindgen + `grammar::Host`
  paths + linker wiring resolve). No bench this slice — no new hot path yet (the guest call is
  7.7c; the `<5µs` round-trip gate is 7.7d). **Depends:** PH7.7a.

#### PH7.7c — Host trampoline adapter + registry wiring ✅ (2026-07-12)
The sync trampoline + the cross-crate registration seam + the Reflex budget (F1) — proven
end to end through a real wasm guest (the fixture came forward from 7.7d to validate D2).
- **Decisions (A/A, locked with Dhruva).** **D1** — new public `lattice-grammar` seam:
  `CommandRegistry::register_plugin_{motion,operator,text_object,ex_command,action}(plugin_id:
  u32, name, doc, spec)` stamping `SourceLayer::Plugin(plugin_id)` via a new
  `SourceLocation::plugin(id)` (forgery-safe: takes a `u32`, never a `SourceLocation`; the
  "no public fn takes a SourceLocation" invariant holds — this is the deferred
  "first-cross-crate-trusted-subsystem" seam §6 anticipated). **D2** — a **second linker**
  (`grammar_linker`) on the shared engine with **sync WASI** (`add_to_linker_sync`) + the
  sync `grammar` import, so a grammar guest's sync `instantiate` + `apply` have no async
  host import to reach — the sync path is correct by construction, not luck (chosen over a
  whole dedicated sync *engine*; the AOT cache stays shared). **D3** — `instantiate_grammar_plugin`
  returns a `GrammarContributionSet`; the **caller** invokes `register_all(&mut registry)`
  (mode-ownership; ZERO `Editor::` methods). **F1** — `PluginBudget::grammar()` = Reflex-class
  (epoch 2 ticks ≈ 2ms, fuel 10M) armed before every `apply`, distinct from the ~1s
  lifecycle budget so a plugin motion traps well inside a frame.
- **What landed.** `lattice-grammar`: `SourceLocation::plugin` + the 5 `register_plugin_*` +
  `CommandError::Plugin(String)` (the graceful apply-failure class). `lattice-plugin-host`:
  `grammar_trampoline.rs` — `GrammarGuest{store,bindings}` behind `Arc<Mutex<>>` (shared by
  all a plugin's contribution closures, serializes the `!Sync` store); `run_callback` (lock
  → arm Reflex budget → sync guest call → map trap/guest-err/conv-err → `CommandError::Plugin`,
  graceful §8); `build_*_spec` (WIT spec → native `*Spec` with a sync trampoline `apply`/
  `parse_args`); `GrammarContributionSet::register_all`; `instantiate_grammar_plugin` (sync
  instantiate via `grammar_linker` → `call_register_grammar` → drain → build). `PluginBudget::grammar`
  + `PluginHostError::GrammarSpec`.
- **Proof (D2 empirically resolved).** `tests/fixtures/grammar-guest` (a `wasm32-wasip2`
  component: `register-grammar` contributes 2 motions + 1 text object; `apply-motion`
  computes line+count from the projected context — no document handle) + `tests/grammar_source.rs`
  (3): registration + host-stamped `Plugin(id)` provenance on all three; a plugin motion
  **dispatches through `execute_motion_only`** — the sync trampoline fires into the guest,
  the `motion-result` crosses back (line 1 + count 3 → line 4); a guest `err` → graceful
  `CommandError::Plugin` no-op. **The sync guest↔host call works on the shared engine** (the
  PH7.7 fork validated). 74 plugin-host lib + 212 grammar tests green; `lattice-host` (matches
  `CommandError`) builds — the new variant is absorbed by its wildcard arm.
- **Deferred (unchanged):** text-reading/structural motions need the `document` handle +
  `scope-resolver` callback (audit F4) — a v1 motion computes from context scalars.
  **Depends:** PH7.7b.

#### PH7.7d — grammar round-trip perf gate (`< 5 µs p99`) ✅ (2026-07-12)
The `< 5 µs p99` §7 row, now the seam exists — and the bench that caught the F1 budget bug.
- **What landed.** `benches/grammar_roundtrip.rs` (`grammar_motion_round_trip`) — the
  descriptive end-to-end dispatch of `down-n` through the sync trampoline; **release median
  ~340 ns**, ~15× under budget. `tests/perf_ratchet.rs::grammar_round_trip_stays_within_ceiling`
  — the CI gate (debug median ~2.3 µs, generous 250 µs ceiling, the typed-call/picker ratchet
  pattern). `benchmarks.md` PH7.7d row.
- **The bug the bench caught (F1 refinement).** The first `PluginBudget::grammar` used
  `epoch_deadline: 2` (≈2 ms). The ratchet (2 000 iters) passed, but the **bench's** 3 s
  warmup (millions of iters) hit a rare OS deschedule mid-guest-call and the 2 ms epoch
  *false-positived* → trap. Insight: a grammar guest runs on the sync linker with **no async
  import**, so it cannot block — it can only compute/spin, both **fuel**-bounded. So **fuel is
  the primary Reflex bound** (10M ≈ one frame of compute); the **epoch is a jitter-proof
  backstop** (raised to 50 ms), not a sub-ms tripwire. This is the correct resolution of
  audit F1 — the keystroke bound is fuel (deterministic per-work), not a fragile wall-clock
  deadline at epoch-tick granularity.
- **Proof.** Bench runs clean (no trap) at ~340 ns release; the ratchet gates it; full
  plugin-host suite green (74 lib + all integration binaries). **Depends:** PH7.7c.

**⇒ PH7.7 (grammar-extension seam) COMPLETE.** A WASM plugin contributes first-class vim
motions / operators / text-objects / ex-commands / actions through the same `register_*`
path builtins use, stamped `SourceLayer::Plugin(id)`, dispatched by the native (unchanged,
sync) grammar engine, bounded by a fuel-primary Reflex budget, crash-isolated. Residual
(audit F4, not blocking): text-reading / tree-sitter-structural motions await the `document`
handle + `scope-resolver` callback.

### PH7.8 — Event/hook WIT seam ✅ (2026-07-12)
Fills the reserved `SubscriptionTarget::Plugin` slot: the host owns an mpsc, the native
`EventBus` pushes each matched `Event` into it via a host-owned sink (lock dropped), and the
per-plugin `EventActor` drives the guest `on-event` export off the keystroke path. `:autocmd`
from a plugin desugars to the guest→host `events.subscribe`. **Depends:** PH7.3.
(Before-class veto/mutation stays out — the native bus is observation-only in v1.)
- **Decisions (locked with Dhruva 2026-07-12):**
  - **D1 — bus routing = a new `SubscriptionTarget::Plugin { plugin, handler, sink }`,
	direct-push (option A).** The sink is a **type-erased** `Arc<dyn Fn(Event) -> bool + Send +
	Sync>` (`lattice_runtime::PluginEventSink`) so the bus stays channel-agnostic — the
	plugin-host builds it over its own `futures` mpsc (the lib keeps `tokio` a dev-dep; the bus's
	`Channel` uses `tokio` mpsc), so `lattice-runtime` grows NO plugin-host/channel dep. Chosen
	over reusing `Channel` (can't carry the handler tag — needs a parallel merge layer) or an
	id-only variant + per-tick drain (couples off-keystroke delivery to the App tick). Fills the
	reserved slot with real provenance (`plugin`/`handler` for teardown + introspection); `false`
	→ prune (the closed-`Channel` discipline). The sink runs in the audit-M1 **lock-dropped**
	dispatch phase, so a slow handler never stalls the publisher or another subscriber.
  - **D2 — a dedicated async `events-plugin` world (option A over "on the base `plugin`
	world").** Originally locked as "on the `plugin` world" (design §8 calls `on-event` a
	lifecycle export), **reversed mid-build**: a world's exports are mandatory, so adding
	`on-event` to the base world would force every no-op WAT fixture (4 suites: instantiate /
	runtime / cache / capability) + every non-observer plugin to implement it. The dedicated
	world (`import events` + `import host-services` + `export register-events` + `export
	on-event`) matches the picker/completion/grammar precedent exactly (each got its own world so
	unrelated components don't implement its exports) and breaks nothing.
  - **Guest-supplied handler id (the grammar `callback` precedent) + subscribe-only.** No host
	id allocation, no return value; the guest's own dispatch key routes `on-event(handler, ev)`.
	No `unsubscribe` in v1 — subscriptions live for the plugin's lifetime, torn down en masse on
	deactivate/quarantine (PH7.12). The declarative filter (`kinds`/`path-globs`/`major-modes`)
	crosses; the native `predicate` closure does not (a guest filters inside `on-event`).
- **Sub-slices (all ✅):**
  - **PH7.8a** — boundary mirrors (`boundary_event.rs`): `WitBoundary for Event` (compiler-
	exhaustive both ways, reusing the PH7.3b `Range`/`SelectionSet` mirrors; ids→`u64`,
	paths→`string` non-UTF-8-typed-error) + `EventKind` + `project_event_filter` (WIT→native
	one-way). New `event`/`event-kind`/`event-filter` + payload records in `types.wit` (distinct
	`event-applied-edit` — the event form carries no `delta`). 9 unit tests; marshalling
	`~23 ns` (`benches/boundary.rs`). Host-layer only, no guest.
  - **PH7.8b** — the `events` WIT (`subscribe`, guest imports) + the dedicated async
	`events-plugin` world (`export register-events` + `export on-event`) + the **5th `bindgen!`**
	(`events_host.rs`, shared `types`/`host-services` via `with:`) + `EventContributions`
	accumulator + `events::Host` impl on `PluginState` (records subscribe) + linker wiring into
	the async linker. 1 unit test; scaffold/runtime unaffected.
  - **PH7.8c** — the `SubscriptionTarget::Plugin` variant (+ `PluginEventSink`, lock-dropped
	dispatch + prune, in `lattice-runtime`; 3 bus tests) + the fire-and-forget `EventActor` +
	`spawn_event_plugin` (`event_task.rs`: instantiate → `call_register_events` → drain → wire
	each sub to the bus with a handler-tagging sink; the caller drives `run` + holds the
	`SubscriptionId`s for teardown). Real `events-guest` wasm fixture + `tests/event_source.rs`
	(2 e2e): happy delivery (subscribe → publish → bus → actor → guest `on-event` writes its
	data-dir mount → test reads) + a **poison handler** that traps → graceful skip (host doesn't
	panic, a native co-subscriber still receives the event — §8 isolation). **Finding:** a
	component trap **taints its instance**, so the trapping plugin's later deliveries also fail
	(logged + skipped) — the plugin is dead-until-reinstantiation (PH7.12); the guarantee held is
	cross-plugin/host isolation, not within-plugin survival.
  - **PH7.8d** — the §7 "major-mode event handler < 250 µs p99" gate: `PluginBudget::event()`
	(fuel-primary 100M ≈ ~10 frames; epoch a generous ~1 s backstop because an event handler runs
	on the async linker and may `await` `host-services`, unlike grammar's Reflex budget) +
	`tests/perf_ratchet.rs::event_handler_stays_within_ceiling` (mean over 1 000 no-op deliveries,
	**~3.75 µs debug**, 2 ms ceiling). No dedicated criterion bench — per-call cost decomposes
	into 7.8a marshalling + the PH7.3d typed call + a sub-µs channel hop (the picker/completion
	precedent). `benchmarks.md` PH7.8 row. CI `wasm32-wasip2` target already added (PH7.5) so the
	fixture builds + the gate runs.

### PH7.9 — Decoration + UI-contribution WIT seam ✅ (2026-07-13)
Mirror `Mode::gutter_decorations` → per-line `GutterDecoration` data (no draw calls); the host
builds the decoration snapshot off the render path. **Depends:** PH7.3. **Gate:** segment update
< 50μs p99 (`decoration_produce_stays_within_ceiling`, ~63µs debug).
- **The fork (locked with Dhruva) = the completion PH7.6 shape.** `Mode::gutter_decorations` is
  a **sync** trait the renderer reads **per frame** (`render.rs:3847` / `window.rs:1818`) — a
  WASM mode can't satisfy it inline (per-frame WASM = a paramount-#1 violation). So a plugin
  decoration provider is an **async producer** the host calls OFF the render path on a trigger,
  caching the returned `Vec<GutterDecoration>`; the renderer reads the cache. Rejected a "sync
  decoration trampoline" (the grammar shape) — it would put WASM on the render path.
- **Scope (locked with Dhruva):** the exercised `decorations` row is built FULLY (a git-gutter
  plugin's exact need); the broader `ui` row (status/gutter segments, notifications, sprites) is
  **type-mirror-only** (the PH7.6 matcher/ranker precedent — modeline already rides the event bus
  ML.3, notifications ride `effect.echo`; sprites have no native struct, not mirrored). **PH7.9
  is validation-only** (like PH7.4d/7.6/7.8 + the no-per-frame-WASM guard): the fixture proves
  producer→host-cache; **the renderer-reads-the-cache + boot-wiring is a tracked Phase-8 item**
  (one-time HOST work — a git-gutter *author* writes only the producer; the native gutter path
  already reads an off-render `ServiceRegistry` snapshot, so the plugin cache is one more per-line
  map to merge). Confirmed with Dhruva: the deferred piece is host plumbing, never an author gap.
- **Sub-slices (all ✅):**
  - **PH7.9a** — boundary mirrors (`boundary_decoration.rs`): `WitBoundary for GutterDecoration`
	(compiler-exhaustive) + `GutterDiffKind`/`GutterSeverityLevel` + `project_decoration_context`
	(host→guest one-way; the native `DecorationCtx` carries a `ServiceRegistry`, so the host
	builds the owned ctx from buffer id/path/line-count). `types.wit` += the `gutter-decoration`
	variant + the `ui` type-mirror (`ui-segment`/`ui-notification`/`ui-zone`, reusing `echo-level`).
	5 tests; ~tens-of-ns marshalling (`benches/boundary.rs`).
  - **PH7.9b** — `wit/decorations.wit` (the `decorations` producer interface + `decorations-plugin`
	world) + the **6th `bindgen!`** (`decoration_host.rs`, shared `types`/`host-services`) + the
	request/reply producer actor (`decoration_task.rs`: `DecorationClient`/`DecorationActor` +
	`spawn_decoration_source`, the completion bridge shape) + `PluginBudget::decoration()`.
  - **PH7.9c** — `WasmDecorationSource` (`decoration_source.rs`, the native-typed producer facade
	a boot-wired `Editor` polls — project → guest → `from_wit`, graceful-keep-prior-cache on err)
	+ a real `decorations-guest` wasm fixture + `tests/decoration_source.rs` (2 e2e: context
	crosses in [last-line decoration keyed off `line_count`] + returns native decorations; empty
	buffer → graceful guest `err`).
  - **PH7.9d** — the §7 gate: `tests/perf_ratchet.rs::decoration_produce_stays_within_ceiling`
	(warm produce median **~63 µs debug**, 5 ms ceiling) + the marshalling bench + `benchmarks.md`
	PH7.9 row.
- **Tracked Phase-8 follow-on (NOT PH7.9):** boot-wire the plugin host into the `Editor` + both
  renderers merge the per-buffer plugin decoration cache into `diff_map`/`sev_map` (alongside the
  native `mode.gutter_decorations` read). Until then plugin decorations produce + cache but do not
  visibly render. This is the phase's standing boot-wiring milestone; no plugin-author work.

### PI — Plugin-API Introspection (PRIORITIZED, before PH7.10) 🚧 (PI.1–PI.4 ✅; PI.4 = scaffold, loader Phase-8)
A baked-in, discoverable introspection layer for APIs — **especially plugin APIs** (Dhruva,
2026-07-13). Extends the existing self-documenting-help spine (design §5.11) — `:describe-*` /
`:apropos` / `render_introspection()` already surface `SourceLayer::Plugin(id)` provenance — to
the **plugin API surface** + **human-readable plugin provenance**. Two facets under one surface
(both audiences: in-editor users AND plugin authors).
- **Decisions (locked with Dhruva 2026-07-13):**
  - **Catalog source of truth = parse the WIT at build time** (`wit-parser`, already in-tree via
	wasmtime). The `wit/` package IS the canonical API; the catalog is *derived* from it so it
	can't drift. Chosen over a hand-authored+drift-test catalog (more "baked in", zero drift) and
	over runtime component reflection (needs boot-wiring; only sees loaded plugins → a Facet-B
	enhancement, not the static catalog).
  - **Crate placement = a NEW lightweight `lattice-plugin-api` crate** (wit-parser build-dep, NO
	wasmtime) that owns the catalog, so `lattice-host` can dep it for `:describe-plugin-api`
	WITHOUT pulling the wasmtime runtime into the host (keeps the no-per-frame-WASM invariant; the
	heavy `lattice-plugin-host` stays out of the host until Phase-8 boot-wiring).
  - **First slice = PI.1–PI.3** (catalog + discovery ex-commands + provenance resolution +
	`:list-commands`). PI.4 (loaded-plugin enumeration) partly waits on boot-wiring.
- **Facet A — the API-surface catalog ("what CAN a plugin do"):** a `PluginApiCatalog` derived
  from `wit/`: each interface (`grammar`/`events`/`decorations`/`picker-source`/`completion-
  source`/`host-services`/`config`/`modes`/`ui`) → name, doc, functions (name+doc), + world-
  derived direction (guest-exports vs guest-imports) + a thin host-authored **capability**
  annotation (fs/net/proc/none) the WIT can't carry. Discoverable via `:describe-plugin-api
  [<seam>]` / `:list-plugin-apis` / `:apropos`, exportable (JSON/markdown for authors).
- **Facet B — contribution introspection ("what HAS a plugin done"):** `Plugin(id) → manifest
  name` resolution (provenance reads "git-gutter", not "<plugin:7>") + `:list-commands` (the one
  missing enumeration, source-grouped) + (PI.4, partly boot-wired) `:describe-plugin`/`:list-plugins`.
- **Carve:** **PI.1** the `lattice-plugin-api` crate — build.rs `wit-parser` → generated catalog
  (interfaces+funcs+worlds+docs) + the host-authored capability annotation + a test that the
  annotation covers every parsed interface. **PI.2** `:describe-plugin-api`/`:list-plugin-apis`/
  `:apropos` extension (via the existing HelpBuffer + `render_introspection`) + machine-readable
  export. **PI.3** `Plugin(id)→name` provenance resolution + `:list-commands` + source grouping.
  **PI.4** `:describe-plugin`/`:list-plugins` (contribution plumbing now; loaded-plugin enum with
  Phase-8 boot-wiring). **Depends:** PH7.9 (the WIT set is now complete enough to catalog).
- **Four artefacts:** doc = this section + design §5.11; bench = n/a (introspection is off any hot
  path; build-time parse); test = catalog-covers-every-interface + describe/apropos output +
  provenance-name resolution; error = a malformed/absent WIT interface → the catalog omits it +
  logs, never a panic; a plugin id with no manifest → falls back to `<plugin:id>`.

#### PI.4 — `:describe-plugin` / `:list-plugins` scaffold + plugin-doc source ✅ (2026-07-13)
  The loaded-plugin introspection surface (Facet B). Built as a **scaffold** (like PI.3's name
  seam): the ex-commands + registry + rendering exist and are unit-tested by injection, but the
  **populator is Phase-8** (the plugin loader — no plugins are loaded today, so `:list-plugins`
  shows an empty-state and `:describe-plugin <name>` echoes "no loaded plugin").
  - **Doc source (locked with Dhruva):** a plugin's doc is its **own** documentation — the embedded
	**WIT world doc-comment** (preferred; the author's `///`) with the manifest **`doc` field** as
	fallback. Both are **immutable at editor runtime** (fixed at the plugin's build/package time), so
	the doc is **extracted once at load and cached** in the registry — never re-fetched per
	`:describe-plugin`; only a reload/hot-swap (PH7.12) re-extracts. (For bundled plugins the
	extraction could even happen at the editor's build time, like PI.1's catalog.) Added `doc:
	Option<String>` to `PluginManifest` + `RawManifest` (blank → `None`).
  - **Registry:** PI.3's `PluginNameRegistry` generalised to `PluginMetaRegistry(RwLock<HashMap<u32,
	PluginMeta{name,doc}>>)` (same ServiceRegistry newtype seam). New `Editor::register_plugin(id,
	name, doc)` / `plugin_meta` / `loaded_plugins`; `register_plugin_name` + `plugin_display_name`
	(PI.3) still work (name-only over the same map).
  - **Surfaces:** `Effect::{DescribePlugin{name}, ListPlugins}` (host-applied display effects, the
	`:describe-command`/`:list-commands` wiring class; Effect↔WIT + renderer-classifier lockstep).
	`:describe-plugin` renders through the `Introspectable` spine (kind `plugin`, doc = the plugin's
	doc, a *Commands (N)* section listing the commands whose provenance is `Plugin(id)` — ties to
	PI.3). `:list-plugins` = a table of `exec:describe-plugin` links + doc summaries.
  - **Tests (+6):** grammar parse (describe-plugin required-arg + list-plugins), boundary round-trip,
	manifest `doc` parse (present/absent/blank→None), host (empty list-plugins, register→describe
	renders doc + contributions + list shows it, unknown→None). Green: grammar 218, plugin-host 91,
	host suite 13, GPUI `--features window`.

#### PI.3 — `:list-commands` (source-grouped) + plugin-name provenance seam ✅ (2026-07-13)
  Facet B's discoverable-now half: the one introspection enumeration the help family was missing,
  plus the `Plugin(id)→manifest-name` resolution seam.
  - **`:list-commands`** (`Effect::ListCommands`, host-applied display effect — same wiring class as
	`:list-plugin-apis`): `Editor::build_list_commands_content` walks `registry.names()`, groups by
	`spec.source.layer` (Built-in / User config / Project config / Modeline / Runtime / Plugin), sorts
	`(group, name)`, renders each as a `:describe-command` link under a `## <group>` heading.
  - **Plugin-name seam:** a `PluginNameRegistry(RwLock<HashMap<u32,String>>)` newtype registered
	empty in the `ServiceRegistry` at boot (newtype so the `TypeId` can't collide; `RwLock` for the
	interior mutability a post-boot populate needs behind the shared `Arc<ServiceRegistry>`).
	`Editor::register_plugin_name(id,name)` / `plugin_display_name(id)->Option<String>`. The Plugin
	group resolves the id to its manifest name, else falls back to `<plugin:id>`. **No populator
	exists yet** — the Phase-8 plugin loader is it; the seam is ready and unit-tested by direct
	injection.
  - **Deferred (documented, not skipped):** applying the name resolver to the `render_introspection`
	source *links* (`:describe-command` on a plugin-contributed command) — it would thread a resolver
	through grammar's render for a surface with zero live plugin sources today; folds in when a real
	plugin lands (heuristic #1 / §5.5 "API grows from real plugins"). `:list-commands` is where the
	grouping value concentrates now.
  - **Tests (+4):** grammar parse, boundary round-trip, host `:list-commands` grouping + the
	plugin-name seam (inject → resolve → unknown-id None). Green: grammar 216, plugin-host 90, host
	plugin-api suite 10, GPUI `--features window`.

#### PI.2b — `:export-plugin-api [markdown|json]` (savable buffer) ✅ (2026-07-13)
  The author-facing machine-readable export (Facet A), as a **savable synthetic buffer** the user
  writes with `:w <path>` (the option locked with Dhruva over a direct fs-write command).
  - **Mechanism (the `OpenSyntheticBuffer` pattern, buffer-open effect):** `Effect::ExportPluginApi
	{ format: Option<String> }` (+ WIT-mirror + renderer-classifier lockstep). Unlike the display
	effects (host-applied), this is **peer-applied** — each renderer's dispatch arm calls the shared
	host `Editor::do_export_plugin_api` via `mutate_editor` (the `OpenSyntheticBuffer`/`OpenAiLog`
	precedent), so it sits with `OpenSyntheticBuffer` in the effect *classifiers*, NOT in the
	peer-no-op group (grouping it with the display effects made the handler unreachable — corrected).
  - **The buffer is a real, savable `text-mode` Document** (`*plugin-api.md*` / `*plugin-api.json*`)
	via the generic `ensure_named_synthetic_document` — `text-mode` contributes no read-only/NoFile
	override (that is mode-contributed, not a `BufferFlags` field), so `:w <path>` works. Idempotent:
	a re-export reuses the by-name buffer, so content is fully **replaced** via a new generic
	`Editor::replace_owned_buffer` (sibling to `append_to_owned_buffer`; must span the true last line
	— `line_count()-1`, NOT `last_addressable_line`, which backs past the trailing newline and would
	only overwrite line 0 on a reused buffer).
  - **Dumps** built host-side from the catalog: markdown (default) and hand-built JSON (a local
	`json_escape` keeps `lattice-plugin-api` serde-free). Format is pre-validated by the grammar
	(`markdown`/`md`/`json`) — a typo echoes a parse error, not a silent default.
  - **Tests (+5, 13 total for PI.2):** grammar parse (format validation + emit), boundary round-trip,
	host (json content + markdown default + re-export-replaces-not-doubles via a booted `Editor`).
	Regression: grammar 215, plugin-host 90, GPUI `--features window` build all green.

#### PI.2a — `:describe-plugin-api` / `:list-plugin-apis` / `:apropos` extension ✅ (2026-07-13)
  The in-editor discoverable surface (Facet A), rendered through the existing help spine. Export
  (Facet A machine-readable) is carved to **PI.2b** (buffer-backed). `lattice-host` now deps
  `lattice-plugin-api` (the wasmtime-free leaf — no runtime added).
  - **Wiring (matches the existing help-command family exactly):** two `Effect` variants
  (`DescribePluginApi { seam: Option<String> }`, `ListPluginApis`) in `lattice-grammar`, registered
  as builtin ex-commands (`ex:describe-plugin-api` optional-string arg, `ex:list-plugin-apis`) +
  host aliases; dispatched to new `Editor::build_{describe_plugin_api,list_plugin_apis}_content`
  → `HelpContent` → `RendererSignal::DisplayBuffer` (renderer-agnostic — **TUI+GPUI parity
  automatic**; only the Effect-mirror + the two renderers' effect *classifiers* needed the new
  arms, added in-lockstep).
  - **The Effect↔WIT lockstep (compiler-forced):** the whole-enum mirror (`boundary_effect.rs`)
  is compiler-exhaustive, so both variants got `to_wit`/`from_wit` arms + a `wit/types.wit`
  `effect` arm the same slice (the exhaustiveness *is* the guard — `lattice-plugin-host` won't
  compile otherwise). The analog of the TUI/GPUI parity rule for the plugin boundary.
  - **Describe = the `Introspectable` spine, not freeform.** A host-local `View(&ApiInterface)`
  impls `lattice_grammar::Introspectable` (kind `plugin-api`, `extra_sections` = a *Seam:* block
  [direction+capability prose] + a *Functions (N):* block, both anchored for scroll-to), so the
  body is uniform with `:describe-command` AND `lattice-plugin-api` stays dependency-pure (the
  trait lives in grammar; the wrapper in the host). `:list-plugin-apis` is a freeform table (the
  `:list-options` style), each seam an `exec:describe-plugin-api <seam>` link. `:apropos` now
  also scans the catalog (interface names+docs); plugin-API hits carry the `plugin-api` kind and
  link via `exec:` (not `:describe-command`).
  - **Graceful:** an unknown seam echoes an error + returns `None` (no help buffer); no-arg
  `:describe-plugin-api` delegates to the list. A `gen:plugin-apis` completion generator is a
  deferred follow-up (the `describe-element` precedent — the catalog is host-side).
  - **Tests (8):** grammar parse (2), plugin-host boundary round-trip (1), host builders + apropos
  via a booted `Editor` (5, incl. unknown-seam→None + exec-link targets asserted on
  `metadata.links`, since `HelpContent` strips links out of buffer text). Regression: grammar
  214, plugin-host 90, GPUI `--features window` build all green. **Next: PI.2b**
  (`:export-plugin-api [markdown|json]` → a savable synthetic buffer).

#### PI.1 — `lattice-plugin-api` crate + build-time catalog ✅ (2026-07-13)
	The wasmtime-free leaf crate owning Facet A's `PluginApiCatalog`, derived from `wit/` at build
	time so it can't drift. Design fragment: [`../../architecture/plugin-host.md`](../../architecture/plugin-host.md) §5.13.
  - **Landed:** `crates/lattice-plugin-api/` — `build.rs` (`wit-parser` `Resolve::push_dir` over the
	workspace-root `wit/` → generates `$OUT_DIR/catalog.rs`, two free fns building the public
	types; `rerun-if-changed` per wit file) + `src/lib.rs` (the `PluginApiCatalog` / `ApiInterface` /
	`ApiFunction` / `ApiWorld` / `Direction` / `Capability` types, `include!`s the generated data,
	merges the host-authored `CAPABILITY_ANNOTATIONS`, `catalog()` `OnceLock`-cached, `interface`/
	`world` lookups) + `tests/catalog.rs` (4). Workspace: new member + `wit-parser = "0.251"` in
	`[workspace.dependencies]` (reuses the version wasmtime 46 already locks — **zero** new runtime
	dep in any graph; it's a build-dep of a leaf crate, so `lattice-host` can dep the catalog
	without pulling wasmtime — the no-per-frame-WASM invariant PH7.5 guards stays intact).
  - **The two parser-underivable fields (host-authored):** *(1) direction* — world-derived, but
	`use foo.{ty}` registers an import edge indistinguishable from a callable `import foo;`, so the
	classifier only calls an imported interface `GuestImport` when it **has functions**; a
	zero-function type bag (`types`) stays `TypesOnly` (`host-services`→GuestImport,
	`picker-source`→GuestExport, `types`→TypesOnly are the spot-checked anchors). *(2) capability* —
	`CAPABILITY_ANNOTATIONS` (one row per interface; only `host-services`→`Fs` today, rest `None`);
	a test asserts it **covers every parsed interface**, so a new WIT interface fails the test gate
	until someone makes a deliberate capability decision. The test-only `trampoline-fixture` world
	is excluded from the catalog.
  - **Graceful error:** an unparseable canonical `wit/` is a hard **build** error (you can't ship a
	plugin editor with a broken API package; a silent empty catalog would hide it); an unnamed
	(inline-world) interface is skipped with a `cargo:warning`. Bench = n/a (build-time parse, off
	any hot path). Green: 4 catalog tests. **Depends:** PH7.9. **Next:** PI.2 (the
	`:describe-plugin-api` ex-commands via HelpBuffer + `render_introspection`).

### PH7.8b — Plugin-DEFINED events (register + emit + subscribe) ✅ (all three halves)
Extends the PH7.8 event seam (which was **subscribe-only** — plugins subscribed to *built-in*
events) to let plugins **declare and use their OWN custom events** (Dhruva, 2026-07-13: "important
that plugins should be able to register custom events for custom use cases of their own"). This
surfaced from the completion/introspection verification: `EVENT_DESCRIPTORS` is a compile-time
`linkme` slice and the bus `Event` is a **closed, WIT-mirrored enum**, so neither could be extended
by a runtime-loaded plugin.

**Design (locked with Dhruva 2026-07-13):**
- **Wire = opaque bytes; host is a thin router.** The bus/WIT carries `name: string` +
  `payload: list<u8>` (MessagePack the plugin owns). The host NEVER interprets a plugin payload —
  the boundary discipline the whole plugin host rests on (paramount #1/#2). Rejected a structured
  `Value` payload (imposes a host value-model + per-event parsing for zero host benefit).
- **A payload TRAIT adds real author value — but as a GUEST-SIDE SDK layer OVER the opaque wire**
  (heuristic #3: the answer is "both", different layers). `#[derive(PluginEvent)]` gives type-safe
  `emit(MyEvent)` / `on_event::<MyEvent>()`, **auto name + doc from the struct's `///` doc-comment**
  (the PI.4 "doc from doc comments" principle applied to events), and — the biggest win for the
  coordinating-plugins use case — a **typed cross-plugin event contract** via a shared crate. It's
  additive: the host wire is identical with/without it, so **zero rework** landing it later.
- **Subscribe-by-name stays in the guest's `on-event`** (the PH7.8 rule — declarative filter
  crosses, guest filters by name in `on-event`), so `EventFilter` needs no new dimension; all plugin
  events share one `EventKind::Plugin`.
- **Validation-only** (like every plugin-host slice) — the host isn't boot-wired into the `Editor`,
  so no live plugin emits in the shipping editor; proven host-layer + via the wasm fixture.

**Three landable halves:**

#### PH7.8b.1 — Runtime event registry + introspection/completion ✅ (2026-07-13, `7b5a3c39`)
The process-wide RUNTIME event-descriptor registry alongside the compile-time linkme slice, so a
runtime plugin can declare events + they surface in introspection/completion.
- `lattice-protocol::event_registry`: `EventInfo { name, doc, source, builtin }` (owned, source-
  tagged) + `register_runtime_event(name, doc, source) -> bool` (false if it would shadow a built-in)
  / `unregister_runtime_event` / `all_events()` (builtin ∪ runtime, sorted) / `event_info_by_name`.
  A `OnceLock<RwLock<BTreeMap<String, EventInfo>>>` — additive, the linkme path unchanged.
- Consumers moved to the unified view: `gen:events` completion (`host_generators.rs`),
  `:describe-events` (grouped by source), `:describe-event <name>` (tags built-in vs plugin).
- Tests: protocol round-trip + built-in-shadow rejection; host describe surfaces a runtime event.
  Green: protocol + host suite 14.

#### PH7.8b.2 — The emit/subscribe wire ✅ (2026-07-13)
The dynamic event on the bus + the guest→host seams. Landed exactly as scoped:
- `Event::Plugin { name, payload }` + `EventKind::Plugin` (protocol), the WIT
  `event.plugin(event-plugin)` arm + `event-kind.plugin`, and the both-direction
  `boundary_event.rs` mirror — the exhaustiveness *was* the guard (compiler forced
  `event_path` / `event_major_mode` in `lattice-runtime` and both `WitBoundary`
  impls to add the arm).
- `host-services.wit`: `register-event(name, doc) -> bool` + `emit-event(name,
  payload)`; bodies in `host_services.rs` (`register_plugin_event` stamps the
  `plugin:<id>` provenance → `register_runtime_event`; `emit_plugin_event`
  publishes `Event::Plugin` on the bus).
- **The deepest bit:** `PluginState` gained `event_emit: Option<EventEmitCtx{
  plugin_id, bus: Arc<EventBus> }>` — `spawn_event_plugin` (now `bus: &Arc<EventBus>`)
  sets it before `register-events` runs, so the guest can register/emit from
  `register-events` *and* from `on-event`. `None` for a plugin not spawned onto a
  bus → emit is a warn + drop (graceful; the host isn't boot-wired yet).
- Tests: `boundary_event` opaque-payload round-trip (incl. non-UTF-8 bytes);
  `host_services` provenance-stamp unit test; the `events-guest` fixture now
  declares + emits `events-fixture.saved-echo` on save, and a new `event_source`
  host-layer e2e proves guest `emit-event` → bus → **native** subscriber receives
  the opaque payload byte-for-byte, plus `register-event` lands in the runtime
  registry under `plugin:<id>`. Green: protocol 66, runtime 58, plugin-host lib +
  `event_source` (3) + `perf_ratchet` (6), plugin-api 4, host introspection/boot 14+14.

Original scope (for reference):
1. `crates/lattice-protocol/src/event.rs`: add `Event::Plugin { name: String, payload: Vec<u8> }`
   + the matching `EventKind::Plugin` (+ its `kind()` mapping). It's the WIT-mirrored enum, so this
   is a **compiler-forced lockstep**: `wit/types.wit` `event` variant gains a `plugin(record{name,
   payload})` arm + `crates/lattice-plugin-host/src/boundary_event.rs` (`WitBoundary for Event`,
   both directions) — the crate won't compile until mirrored (the exhaustiveness IS the guard).
2. `wit/host-services.wit`: add `register-event: func(name: string, doc: string) -> bool` and
   `emit-event: func(name: string, payload: list<u8>)`.
3. `crates/lattice-plugin-host/src/host_services.rs`: impl `register-event` →
   `register_runtime_event(name, doc, format!("plugin:{id}"))`; impl `emit-event` → publish
   `Event::Plugin { name, payload }` on the bus. **The deepest bit:** `PluginState` currently holds
   the SUBSCRIBE wiring (PH7.8) but NOT a bus-PUBLISH handle — add one (an `EventPublisher`/bus
   `Arc`) so `emit-event` can publish. Unregister the plugin's events on teardown (PH7.12 hook).
4. Guest delivery: `Event::Plugin` must cross to the guest's `on-event` (the PH7.8a `boundary_event`
   already mirrors `Event`; the new arm rides it). A subscribing plugin filters by `name` inside
   `on-event`.
5. Tests (PH7.8 precedent): extend `events-guest` (or a new fixture) so a guest `emit-event`s a
   custom event → the bus → a native subscriber receives it (host-layer e2e); boundary round-trip
   for `Event::Plugin`; `register-event` records into the runtime registry (ties to 8b.1). Skips
   without the wasm target.
- **Depends:** PH7.8b.1, PH7.8 (the event actor + `boundary_event`).

#### PH7.8b.3 — `lattice-plugin-sdk` guest crate: `PluginEvent` trait + derive ✅ (2026-07-13)
Landed **approach A** (locked with Dhruva): a **WIT-agnostic** SDK core, not a world-coupled
wrapper. The core is pure serde + a derive — it touches NO plugin-host bindings, so it composes
with every future plugin world (events / grammar / completion / …) unchanged and is the seed the
other SDK seams reuse. Two crates (the serde / serde_derive split):
- `crates/lattice-plugin-sdk-derive` — `#[derive(PluginEvent)]` proc-macro. `DOC` from the struct's
  `///` doc-comment (the PI.4 doc-from-doc-comment principle); `NAME` from `#[event(name = "...")]`
  or the kebab-cased type name; `encode`/`decode` via the SDK's private rmp-serde helpers (so the
  consumer deps only the SDK). A malformed `#[event(...)]` key is a compile error, never a silent
  fallback.
- `crates/lattice-plugin-sdk` — the `PluginEvent` trait (`NAME`/`DOC`/`encode`/`decode`), a typed
  `DecodeError` (decode is fallible → never a panic), the subscriber `try_decode::<E>(name, payload)`
  name-gate helper, and re-exports the derive. `extern crate self as lattice_plugin_sdk;` so the
  derive's paths resolve in-crate.
- **Emit/register stay plugin-side one-liners** (approach A — no speculative host-call binding):
  `host_services::register_event(E::NAME, E::DOC)` / `host_services::emit_event(E::NAME, &ev.encode())`.
  The full `ctx.emit` / auto-register-at-activate sugar (original step 4) is deferred until a real
  multi-world plugin exists to shape the binding — building it now would be an abstraction with no
  consumer (heuristic #1).
- Tests: SDK unit (5) — explicit-name+multiline-doc, kebab fallback, encode/decode round-trip,
  `try_decode` name-gate, typed decode error. The `events-guest` fixture now authors its event via
  the derive (`SavedEcho`), and the `event_source` e2e proves the **cross-plugin contract**: the
  host test redeclares the same `PluginEvent` type and decodes the guest's MessagePack payload
  (guest encode → wire → host consumer decode). Green: SDK 5, `event_source` 3.
- **Depends:** PH7.8b.2. Cross-plugin contract = plugin A publishes the type in a shared crate,
  plugin B deps it — compile-checked, versioned.

Original scope (for reference):
The ergonomic, contract-capable author layer over the opaque wire (guest-side, compiled into
plugins — NOT the host).
1. New crate `crates/lattice-plugin-sdk` (guest-side; deps the generated WIT bindings + a
   MessagePack serde, e.g. `rmp-serde`). Seeds the plugin-SDK the grammar/completion/decoration
   seams will all eventually want.
2. `PluginEvent` trait: `const NAME: &str`, `const DOC: &str`, `fn encode(&self) -> Vec<u8>`,
   `fn decode(&[u8]) -> Result<Self,_>`.
3. `#[derive(PluginEvent)]` proc-macro: reads the struct's `#[doc]` attribute → `DOC` (the
   doc-comment IS the event doc); derives encode/decode via serde/MessagePack; `NAME` from a
   `#[event(name = "...")]` attr (or the type name kebab-cased).
4. Ergonomic wrappers: `ctx.emit(ev: impl PluginEvent)` (→ `emit-event(E::NAME, ev.encode())`) and
   `on_event::<E>(|e| ...)` (name-filter + decode). At `activate`, a helper auto-calls
   `register-event(E::NAME, E::DOC)` so the event self-registers (8b.1) + gets its doc from the
   doc-comment.
5. Tests: derive round-trips (encode→decode); the doc-comment lands in `DOC`; a fixture plugin uses
   the SDK to emit + subscribe end-to-end.
- **Depends:** PH7.8b.2. **NB:** cross-plugin contracts = plugin A publishes the `PluginEvent` type
  in a shared crate, plugin B deps it — a compile-checked, versioned event contract (the coordinating-
  plugins use case).

### PH7.10 — Config/options WIT seam ✅ (10a + 10b)
Plugin declares options (name + type + default + doc); host registers into the same
`ConfigRegistry` core options use; values round-trip as strings; `:set`/`:describe-option`/
`gen:options`/`OptionChanged` treat them uniformly. **Depends:** PH7.3.

**Design note — `options!` macro cannot cross to a WASM plugin.** The host `options!` macro is a
compile-time/link-time construct (linkme `OPTION_DECLS` slice + a Rust `OptionDecl` `TypeId` +
`&'static str` consts), all resolved when the HOST binary links. A runtime-loaded WASM component is
separately compiled and reached only over WIT — the host can't see its linkme slice or types. So a
WASM plugin declares options by CALLING a host function (`register-option`) over WIT, exactly as
PH7.8b.1 built a *runtime* event registry beside the compile-time linkme one. Native/bundled crates
statically linked into the host keep using `options!` unchanged. (See
[[feedback_wit_canonical_sdk_ergonomics]]: the WIT is the language-agnostic contract.)

#### PH7.10a — the `register-option` wire ✅ (2026-07-13)
- `wit/config.wit`: filled the stub — `enum option-type { boolean, integer, %string }` (`%`-escaped,
  `string` is a WIT keyword); `interface config { register-option(name, ty, default, doc) -> bool;
  get-option(name) -> option<string> }`; `world config-plugin { import config; export register-options }`.
- Type mapping: `option-type` → a concrete `OptionType` impl (`bool`/`i64`/`String`); `default`
  parsed via `T::parse`. **No new registry value-variant** — a plugin option is a plain
  `Option<T>` in the SAME registry, so all erased consumers (`:set`, `:describe-option`,
  `gen:options`, `OptionChanged`) work with zero host kind-branch.
- **Owned-name resolution (locked with Dhruva):** `ErasedOption`/`Option<T>` store `&'static str`,
  but a runtime plugin's name/doc arrive as owned `String`s. Chose **leak** (`Box::leak`, bounded —
  a few per plugin, once at load; parse/dup checked BEFORE leaking so a rejected registration
  allocates nothing) over a `Cow` refactor of the stable config crate. Freeing on unload is PH7.12.
- `PluginState` gains `config_registry: Option<Arc<ConfigRegistry>>` + `config_contributions:
  Vec<String>`, set by `spawn_config_plugin(registry)` before `register-options` runs (the
  `spawn_event_plugin` precedent; simpler — no actor, registration is synchronous). Host impl:
  `register-option` → `config_host::register_plugin_option`; `get-option` → `registry.lookup(name)
  .get_formatted()`. `None` registry → warn + `false`/`none`.
- Tests: `config_host` unit (4) — type mapping + read-back, bad-default rejected, dup rejected,
  `:set` round-trip; `config_source` e2e (1) — a `config-guest` fixture declares 3 options via the
  RAW WIT (no SDK — the language-agnostic surface) + reads one back via `get-option`, host inspects
  the shared registry + drives `:set`. Green: plugin-host lib 98, `config_source` 1, plugin-api 4.

#### PH7.10b — Rust SDK ergonomics (`#[derive(PluginOption)]`) ✅ (2026-07-13)
Guest-side, WIT-agnostic (the PH7.8b.3 approach-A pattern), in the two SDK crates:
- `lattice-plugin-sdk`: `PluginOption` trait (`NAME`/`DOC`/`DEFAULT`/`KIND` + `type Value`), an
  `OptionKind` enum (WIT-agnostic mirror of `option-type`), and `parse_option::<O>(s)` (typed
  `FromStr` read of a `get-option` result → typed `OptionParseError`, never a panic).
- `lattice-plugin-sdk-derive`: `#[derive(PluginOption)]` on a newtype over `bool`/`i64`/`String` —
  `DOC` from the `///` comment, `NAME` from `#[option(name)]` or kebab type name, `DEFAULT` from the
  required `#[option(default = "...")]`, `KIND`+`Value` inferred from the field type. Malformed attr
  / non-newtype / unsupported field type = compile errors.
- Expands to metadata constants only; the plugin makes the one-liner
  `config::register_option(O::NAME, wit_ty(O::KIND), O::DEFAULT, O::DOC)` call itself (`wit_ty` = the
  one-arm `OptionKind`→WIT map, the approach-A tax). Adds ZERO capability not on the wire — a
  Go/JS/Zig plugin uses the WIT directly.
- Tests: SDK unit (2 new) — derive captures name/doc/default/kind + kebab fallback; `parse_option`
  typed read + typed error. The `config-guest` fixture now authors its options via the derive
  (`Enabled`/`Count`/`Label`) + reads `count` back through `parse_option`; the existing
  `config_source` e2e passes unchanged. Green: SDK 7, config_source 1.

### PH7.11 — Modes declaration WIT seam ✅ (11a + 11b)
`modes` WIT mirroring the `Mode` declaration surface; host builds a marker `Mode` impl and
registers it into `ModeRegistry`; keymap contributions land at `KeymapLayer::MinorMode(id)` only
(the `KeymapCapability` write-gate). Bundled modes-as-components shipping is Phase 8 — this slice
lands the declaration WIT + registration path. **Depends:** PH7.7, PH7.9.

**Scope note.** The `Mode` trait is rich, but only `id()`/`kind()`/`on_activate()` are required
(all else defaults). A WASM mode declares the data-crossable subset; its *behavior* is composed
from the other seams (keymap→commands 11b; action bodies via the grammar `register-action`
trampoline PH7.7). Lifecycle callbacks, decorations, completion-sources, typed option-overrides,
and major modes are deferred (Phase 8 / other seams).

#### PH7.11a — declaration + registration ✅ (2026-07-13)
- `wit/modes.wit`: filled the stub — `enum mode-kind`, `variant activation-policy {manual, global,
  universal, majors(list<string>)}`, `flags mode-capabilities` (mirrors `CapabilitySet`),
  `record mode-declaration {id, kind, activation-policy, capabilities}`, `register-mode(decl)`,
  `world modes-plugin { import modes; export register-modes }`.
- Host builds `PluginMode` — a marker `Mode` impl (the `EmacsKeysMode` template: `Guard = ()`,
  no-op `on_activate`, `kind = Minor`) carrying the declared policy + capabilities — and registers
  it into the SAME `ModeRegistry` builtins use (which enforces the `-mode` suffix). `register-mode`
  records into a `PluginState.mode_contributions` accumulator; `spawn_mode_plugin(&mut ModeRegistry)`
  drains + registers after `register-modes` returns (the `register-grammar` drain precedent —
  registration needs `&mut`, not a live handle). Returns the accepted `ModeId`s; a bad suffix /
  dup / `major` kind is logged + skipped.
- Tests: `mode_host` unit (5) — registers a minor mode, rejects a bare id / a `major` kind / a dup,
  carries policy+caps; `mode_source` e2e (1) — a `modes-guest` fixture declares 2 well-formed + 1
  mis-suffixed via the RAW WIT, host inspects the shared registry. Green: plugin-host lib 103,
  mode_source 1, plugin-api 4.

#### PH7.11b — keymap bindings (chord→command, `OwnedLayer` gate) ✅ (2026-07-13)
- `wit/modes.wit`: added `enum binding-mode` (the plugin-facing subset: normal/insert/visual/
  select/replace/command/search — the transient operator-pending/after-key states stay internal),
  `record mode-keymap-binding {binding-mode, chord, command}`, and a `keymap:
  list<mode-keymap-binding>` field on `mode-declaration`.
- Host `bind_mode_keymap`: for each binding, resolve `command` by name against the `CommandRegistry`
  (`id_by_name`) and install a capability-gated write via
  `KeymapHandle::try_bind_chord_string(KeymapCapability::OwnedLayer{mode_id},
  KeymapLayer::MinorMode(mode_id), binding_mode, chord, CommandInvocation::of(id),
  SourceLocation::plugin(plugin_id))` — so a plugin mode writes ONLY its own layer (the write-gate;
  `capability_allows` permits `OwnedLayer→MinorMode(same id)`). An unparseable chord / unknown
  command / capability denial skips that one binding (logged), never a panic. `spawn_mode_plugin`
  grew `&CommandRegistry` + `&KeymapHandle` params + allocs a `PluginId` for provenance.
- The binding lands in the mode's GATED layer, so it resolves only when the mode is active
  (`lookup_with_context(mode, chord, &[mode_id])` → Bound; inactive → Unbound) — the K.1.c
  per-keystroke-filter contract, identical to a native minor mode's keymap.
- Tests: `mode_host` unit (2 new) — a well-formed binding lands in the owned layer + resolves
  when active / not when inactive; an unknown command binds nothing. The `modes-guest` fixture now
  declares a `<C-s>→ex:write` binding on `git-blame-mode`; `mode_source` e2e asserts it resolves via
  the gated layer. Green: plugin-host lib 105, mode_source 1.

A Rust SDK `#[derive(PluginMode)]` (declarative bindings + doc-comment) is a possible future
follow-on (the PH7.8b.3 / PH7.10b pattern); not needed for the wire.

### PH7.12 — Crash isolation + lifecycle hardening + four-artefact close ✅ (2026-07-14)
Trap → `PluginCrashed` event + quarantine; graceful degradation audit across every seam;
reload/hot-swap seam (teardown + re-instantiate) for the deferred `init.rs`/plugin-manager
consumers; fuzz malformed components/payloads/timing. **Depends:** all above. Sub-sliced —
**12a ✅** crash-quarantine, **12b ✅** reload/unload seam (12b.2 intern reclamation deferred to
the Phase-8 reload consumer, decision C), **12c ✅** graceful-degradation audit + fuzz. Closes
the Phase-7 plugin-host substrate.

#### PH7.12a — Crash-quarantine + `PluginCrashed` ✅ (2026-07-14)
The first trap on any repeated-call surface taints its `wasmtime` `Store` irrecoverably (no
rollback), so instead of re-failing every later call (each logged), the host quarantines the
instance: fires **exactly one** crash signal, then short-circuits before re-entering the dead
`Store`. Turns "tainted instance keeps re-trapping at held-key frequency" into a one-shot
signal plus a silent no-op. Isolation is the guarantee — tripping touches only this instance's
flag + one bus publish; the actor, bus, every other plugin, LSP, and the editor are untouched.
- **`Event::PluginCrashed { plugin, func, kind }`** (`lattice-protocol`) — a CLOSED,
  host-originated lifecycle transition (the host is the sole publisher), distinct from the OPEN
  `Event::Plugin` escape hatch a *live* plugin publishes through. Subscribers (a future
  crash-notification surface, the Phase-8 plugin manager's reload/health UI) filter by *kind*,
  mirroring the mode-lifecycle quartet. `kind` is a `String` label (`"fuel"`/`"epoch"`/`"trap"`)
  not the host's `TrapKind`, so the protocol layer stays free of the plugin-host type
  (mirrors `ModalModeChanged`). `EventKind::PluginCrashed` discriminator + `lattice-runtime`
  routing arms (no path, no major-mode). At the WIT boundary it's a typed error, never a silent
  drop — no `event-kind` variant, so guests cannot subscribe in v1 (a Phase-8 monitoring plugin
  adds the variant then).
- **`Quarantine` primitive** (`lib.rs`) — per-instance `tripped` flag + `Arc<EventBus>`;
  `is_tripped()` short-circuit + idempotent `trip(func, kind)` (publishes once, `info!` once —
  a one-shot user-actionable event, not a per-keystroke diagnostic). Shared helpers
  `TrapKind::label()` + `trip_and_map()` (map a raw wasmtime result: trap → trip + typed `Trap`).
- **All five repeated-call surfaces wired** — event / picker / decoration / completion actors +
  the grammar trampoline (one `Quarantine` per guest, so one `apply-*` trap quarantines every
  contribution). Each `spawn_*` / `instantiate_grammar_plugin` takes `&Arc<EventBus>`; each
  export checks `is_tripped()` first. New `PluginHostError::Quarantined { func }` is the typed
  post-crash no-op (distinct from `Trap` — ran-and-failed — and `PluginGone` — channel closed).
- **Test** — `first_trap_quarantines_and_emits_one_plugin_crashed`: one-shot (second trap fires
  no second event), full-instance quarantine (a post-crash *good* delivery is skipped, not just
  the trapping handler), native co-subscriber isolation. **Green: lib + event_source (4).**

#### PH7.12b — Reload / hot-swap seam ✅ (2026-07-14; 12b.2 reclamation deferred)
Teardown (drop the actor / poison the guest lock, unsubscribe events, unregister
grammar/picker/decoration/completion/config/mode contributions) + re-instantiate a fresh,
untripped instance — the seam the deferred `init.rs` / plugin-manager consumers reload through,
and the recovery path out of a PH7.12a quarantine. Also frees the PH7.10a leaked option names
and tears down PH7.11a mode registrations on unregister. **Depends:** PH7.12a. Because the
registries span six crates with very different removal shapes, sub-sliced by registry so each
lands green:

- **The teardown-token map (from the PH7.12b design exploration).** Only **grammar**
  (`CommandRegistry`), **picker** (`PickerRegistry`), **config** (`ConfigRegistry`), **modes**
  (`ModeRegistry` + the `MinorMode` keymap layer), and **plugin-defined events** have a
  host-side registration path that needs a reverse. **Completion** + **decoration** are
  *channel-drop only* — the host never registers them with plugin provenance (completion's
  adapter goes through the generic builtin-stamped `register_generator`; decoration has no
  registry, mode-owned in Phase 8), so dropping the client kills the actor and there is nothing
  to unregister. Event *subscriptions* already reverse via `EventBus::unsubscribe`.

#### PH7.12b.1a — Grammar + picker + mode registry removal ✅ (2026-07-14)
The three clean, `HashMap`/id-keyed registries get a provenance/id-driven remove — the
foundation the teardown bundle (PH7.12b.3) drives. Each is idempotent (a second unload removes
nothing) and keeps every index consistent.
- `CommandRegistry::unregister_plugin(plugin_id) -> usize` — removes every entry whose
  `spec.source.layer == SourceLayer::Plugin(plugin_id)` (built-in / config / runtime untouched,
  honouring the forgery invariant — a caller supplies only a `u32`), cleaning `by_id`,
  `by_name`, and the word-forward tag set.
- `PickerRegistry::unregister(id: &str) -> bool` — the registry keys on `spec.id` with no
  plugin provenance, so removal is by id (the bundle records the id it registered).
- `ModeRegistry::unregister(id: ModeId) -> bool` — removes the mode + any `kind_index` claim it
  owned, so a reload re-claims the kind instead of hitting `Duplicate`. (Note: `ModeId` is
  interned by name, so a reloaded same-name mode is the *same* id — proven in the test.)
- **Tests (3, one per registry): register → unregister → truly gone + siblings untouched +
  idempotent second unload.** Green: grammar 220, picker +1, mode 134.

#### PH7.12b.1b — Config option removal ✅ (2026-07-14)
`ConfigRegistry` stores options in a `by_id: Vec` whose indices ARE the stable
`OptionHandle`s, so removal can't shift the vec. Fixed with a tombstone + free-list (chosen on
heuristic #1 over a bare append-only leave-in-place: it bounds `by_id` across reloads, the F6
goal, without invalidating live handles):
- `by_id: Vec<Option<Arc<dyn ErasedOption>>>` — a slot is `None` once unregistered; `free_list:
  Vec<usize>` recycles freed indices so a plugin reload re-registering the same options reuses
  slots instead of growing the vec. `len`/`iter`/`Debug`/lookup/`get_typed`/`bootstrap`/
  `erased_at` all skip tombstones.
- `ConfigRegistry::unregister(name: &str) -> bool` — drops every name+alias mapping pointing at
  the slot, any `TypeId` mapping, tombstones the slot (frees the `Arc`), pushes the index to the
  free-list. Idempotent. Driven by the host bundle with `PluginState::config_contributions`.
  Live handles left dangling by contract: the slot reads `None` (never another option's value);
  an index is only reused by a *new* registration, never silently re-pointed under an old handle.
- **Test: register (with alias) → unregister by alias → name+alias gone + sibling untouched +
  idempotent + reload reuses the freed slot.** Green: config 160; `lattice-host` +
  `lattice-plugin-host` rebuild clean (public API unchanged).
- **Leaked `&'static str` option names** (PH7.10a `Box::leak`) are still freed separately by the
  per-plugin intern pool — PH7.12b.2.

#### PH7.12b.1c — Keymap `MinorMode`-layer removal ✅ (2026-07-14)
`bind_mode_keymap` binds a plugin mode's chords into `KeymapLayer::MinorMode(mode_id)` via
`try_bind_chord_string` — an *implicitly-created* layer, so the host never holds a `LayerId` to
`pop_layer` with.
- `KeymapHandle::remove_layer(layer: KeymapLayer)` — removes the whole layer by its
  `KeymapLayer` identity (the key the host *does* know: the mode's `MinorMode(mode_id)`),
  dropping every binding across all binding-modes and rebuilding the merged / gated / reverse
  caches exactly as `pop_layer` does. Idempotent (no-op if absent).
- **Plugin-defined events need NO new code**: the process-wide runtime registry already exposes
  `lattice_protocol::event_registry::unregister_runtime_event(name)` (String-keyed `BTreeMap`,
  no leak) — the teardown bundle (PH7.12b.3) calls it directly with the recorded event names.
- **Test: two plugin `MinorMode` layers → `remove_layer` one → its chord gone from every layer,
  the other survives, idempotent second removal.** Green: keymap 96.

#### PH7.12b.2 — Intern-leak reclamation: DEFERRED (decision C, 2026-07-14) ✅
The plugin-sourced spec strings (`config_host` option name/doc, `boundary_picker::intern`,
`boundary_grammar::intern`) are `Box::leak`'d to `&'static str` because the native spec types
(`PickerSourceSpec`/`ArgSpec`/`ConfigOption`/`SurfaceForm`) hold `&'static str`. 12b.1 already
removes the registry ENTRY on teardown; only the string bytes linger, and only under *repeated*
hot-reload (a v1 plugin loads once — process-lifetime metadata, not a leak in practice).
Three options were mapped (see the design decision in-session):
- **A — `unsafe` per-plugin arena** (reclaims `&'static str` via `transmute`): **rejected** —
  cuts against the deliberate `unsafe_code = "deny"` stance the `intern` doc calls out, and adds
  a teardown-order safety contract the compiler can't verify.
- **B — `Cow<'static, str>` on the native spec types** (frees with the entry, no unsafe): the
  durable fix, but a ~100-site sweep of stable picker/grammar/config types (28 `PickerSourceSpec`
  + 54 `ArgSpec` + 47 config + 147 `SurfaceForm` construction sites) — disproportionate to a
  Low–Medium leak with **no consumer until Phase-8 reload wiring** exists to exercise it.
- **C (chosen) — defer B until the reload consumer lands.** The audit's "F6 must land *with* the
  reload seam" predates 12b.1's auto-freeing entry removal and assumed a live seam; the Phase-7
  seam is validation-only, so the leak is never exercised now. B lands *with* the Phase-8
  `init.rs` file-watcher / plugin-manager reload consumer, which gives the sweep a concrete
  present win (heuristic #1: no rewrite ahead of the consumer).
Landed as the three reframed `intern` / `build_and_register` doc comments pointing here.

#### PH7.12b.3 — `PluginTeardown` bundle + unload driver ✅ (2026-07-14)
The capstone: aggregate every teardown token and reverse each against the host registries,
composing the reload cycle. New `crates/lattice-plugin-host/src/teardown.rs`:
- **`PluginTeardown`** — the union of teardown tokens (`plugin_id`, `has_grammar`,
  `picker_sources`, `modes`, `config_options`, `events_defined`, `subscriptions`), filled by the
  spawning caller from the tokens the `spawn_*` fns already return. A plugin populates only the
  surfaces it used.
- **`TeardownRegistries<'a>`** — a borrow struct grouping the six host registries
  (`&mut CommandRegistry/PickerRegistry/ModeRegistry`, `&KeymapHandle/ConfigRegistry/EventBus`),
  so `unload` takes one arg, not six, and the caller passes exactly what a `&mut Editor` holds.
- **`PluginTeardown::unload(&mut TeardownRegistries) -> TeardownReport`** — runs each PH7.12b.1
  `unregister_*` for its recorded tokens (idempotent; returns per-surface removed counts). Modes
  reverse BOTH halves (registry entry + `remove_layer(MinorMode(id))`).
- **Explicit driver, not `Drop`** (heuristic #1): the registries have mixed mutability
  (`&mut` vs `Arc`); a `Drop` bundle would force every registry behind `Arc<Mutex>` — a weaker
  foundation. **No `reload` method**: reload = `unload` + re-invoke the same `spawn_*` (a fresh
  `Store` + fresh untripped `Quarantine`), composed by the caller (the Phase-8 plugin manager).
- **Completion + decoration** absent by design (channel-drop only — no plugin-provenance
  registration path).
- **Tests (2):** a host-layer driver unit test (`unload` reverses grammar+picker+config+events
  +mode-keymap in one call, spares co-resident built-ins/native entries, idempotent second
  unload = all zeros); and the wasm capstone `tests/plugin_teardown.rs` — spawn events plugin →
  crash (trap → one `PluginCrashed`) → `unload` (unsubscribes) → **reload** (fresh
  `spawn_event_plugin`) → the reloaded instance delivers normally, exactly one crash across the
  cycle. Green: lib teardown (1) + plugin_teardown (1).

#### PH7.12c — Graceful-degradation audit + fuzz ✅ (2026-07-14)
**Audit conclusion — the production guest-input path is panic-free by construction.** A sweep of
every `unwrap`/`expect`/`panic!`/`unreachable!`/unguarded-index in `lattice-plugin-host/src`
found the entire malformed-input surface already degrades to typed errors: every boundary
`from_wit`/`to_wit` returns `Result<_, String>`, host-services return `Result`, actor calls map
traps to typed `Trap` (→ quarantine, PH7.12a). Splitting each file at its first `#[cfg(test)]`
marker, *all* panic sites live in test modules bar two production sites, both safe: `lib.rs`'s
`.expect("spawning the epoch-ticker thread")` (host construction, not the guest path — a
thread-spawn failure is a host-environment error) and `boundary_effect.rs`'s `effects.remove(0)`
(guarded by `else if effects.len() == 1`). No code change needed — the boundary discipline
already enforces the four-artefact "log + skip, never panic" rule.
- **Fuzz (`tests/fuzz_robustness.rs`, `proptest`):** the two boundaries where *untrusted* bytes/
  text cross into the host — component bytes (`compile`) and manifest TOML (`from_toml_str`) —
  hammered with randomised + adversarial inputs, asserting a typed error, never a panic/hang.
  Property tests: `compile` on arbitrary bytes + wasm-magic-prefixed garbage; `from_toml_str` and
  `Capability::from_str` on arbitrary text. Deterministic batteries: adversarial component
  prefixes (empty / magic-only / a valid *core-module* header that must be rejected as
  not-a-component / truncated real component / ascii garbage) → all typed `Compile` errors; and
  malformed-but-parseable manifest TOML (wrong types, unknown capability, array-of-tables) →
  graceful + deterministic.
- **Not fuzzed, with reason:** the guest→host *value* path (`from_wit`) can't take arbitrary
  bytes — wasmtime's typed ABI only hands the host well-typed WIT values, and every `from_wit`
  returns `Result` by construction. Malformed *timing* (fuel/epoch traps mid-call) is covered by
  PH7.12a's quarantine tests — a trap becomes a typed `Trap` + one `PluginCrashed`, never a panic.
- **Depends:** PH7.12a. Closes PH7.12 (and the Phase-7 plugin-host substrate).

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
