# Slice plan — Plugin Host (Phase 7)

**Status:** 🚧 in progress (2026-07-01) — PH7.0 ✅, PH7.1 ✅ (7.1a + 7.1b) landed; PH7.2 next. Design fragment:
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

### PH7.2 — Capability & security model 📝
Manifest parsing (declared `fs:*`/`net:*`/`proc:*` + editor `CapabilitySet`); build each
Store's WASI view from its grant; per-plugin data dir mount; trust tiers (bundled pre-grant
vs user-install consent); host-issued `SourceLayer::Plugin(id)` provenance stamping.
- **Depends:** PH7.1.
- **Exit:** a plugin without `fs:write` cannot write outside its data dir (denied at the WASI
  layer, not by discipline); provenance on a plugin-registered command shows `Plugin(id)`.
- **Artefacts:** bench = n/a (correctness slice); test = capability grant/deny matrix,
  provenance non-forgeability; error = denied capability → plugin loads degraded + notification,
  boot never fails.

### PH7.3 — Boundary primitives (the crux — §4 of the fragment) 📝
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
