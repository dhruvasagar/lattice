# Plugin observability — slice plan

> **Slice plan.** Sequencing, slice IDs, dependencies, status icons.
> Design contract: [`../../architecture/plugin-observability.md`](../../architecture/plugin-observability.md).
> Follows Phase 8 (the plugin loader + manager, [`plugin-loader.md`](plugin-loader.md)).

Status icons: ✅ done · 🚧 in progress · 📝 planned. Every non-trivial slice ships
the four artefacts (doc + bench-where-perf-relevant + test incl. failure modes +
graceful error handling).

## Sequencing

**PO.1 → PO.2 → PO.3 → PO.4 → PO.5.** PO.1 lays the substrate (record + event +
tracer sink); PO.2/PO.3 emit into it (async seams, then the gated hot-path seam);
PO.4 surfaces it as buffers; PO.5 adds the guest-narrative import. PO.4 can begin
once PO.1 lands (it reads the tracer + subscribes to the event); PO.2/PO.3 fill it
with data.

## Slices

### PO.1 — trace record + event + tracer sink  ✅
The substrate, mirroring the LSP logging shape (`LspLogger` / `LspLogPushed`).
- `PluginTraceRecord { plugin, seam, direction, call, level, outcome, detail }`
  (design §5).
- `PluginTracer` — the `LspLogger` analogue: a global ring + per-plugin rings
  (bounded VecDeque), a per-plugin verbosity gate (min `TraceLevel`, default
  `Info`), an optional boot-wired event publisher, and `trace(record)` (gate →
  push → publish). Registered as a boot service (`PluginTracerHandle`).
- `PluginTracePushed` — a `register_event!` typed event streamed via
  `publish_typed` on every append.
- **No instrumentation yet** — just the pipe + sink + the boot wiring.
- **Exit:** a `trace(record)` call above the gate lands in the ring AND fires
  `PluginTracePushed`; below the gate it is dropped; the tracer resolves as a
  boot service. Tests: gate filtering, ring bounding, publish-on-append,
  per-plugin level override. No bench (no hot-path emission yet).

  > **Landed 2026-07-17.** Home: `lattice-plugin-host/src/trace.rs` (the crate
  > owns the boundary + `PluginSeam`, symmetric with `LspLogger` in `lattice-lsp`).
  > `TraceLevel` (`Off`..`Trace`, `Ord` — `Off` is gate-only), `Direction`,
  > `TraceOutcome` (`Ok{micros,fuel_delta}` / `Trap{kind,func}` / `Denied{cap}`),
  > `PluginTraceRecord`, `PluginTracePushed` (a `register_event!` typed event —
  > `linkme` added to plugin-host for the descriptor), and `PluginTracer` (global
  > + per-plugin bounded VecDeque rings, per-plugin gate override over a default,
  > optional boot-wired publisher; `trace` = gate→push→publish; `snapshot_*`;
  > `forget_plugin` on unload; a poison-tolerant `lock` so observability never
  > crashes the editor). `PluginTracerHandle = Arc<PluginTracer>` registered as a
  > boot service in the loader's `install`, its publisher bound to
  > `bus.publish_typed(PluginTracePushed{..})`. Tests: 7 unit
  > (`trace.rs` — gate keep/drop, per-plugin override, `Off` silences, ring
  > bounding evicts oldest, publish-on-kept-append, `forget_plugin`) + boot pin
  > `plugin_tracer_service_present_at_boot`. Green: trace tests + boot pin + clippy
  > clean. No bench (the pipe only; no hot-path emission yet — that's PO.3).

### PO.2 — instrument the async seams  ✅
Emit `PluginTraceRecord`s from the host side of the async seam calls
(picker / completion / decorations / events actors). Off-thread already, so rich.
Each records the call name, timing, and result/trap.

> **Landed 2026-07-17.** The chokepoint: every seam guest call funnels through
> `trip_and_map` (map result / trip quarantine). Added `trip_and_map_traced`
> (lib.rs) — same mapping PLUS emit a `PluginTraceRecord` into the actor's tracer:
> a success records `Debug`/`Ok{micros}` (dropped by the default `Info` gate — no
> per-call noise unless a plugin is raised to `debug`), a trap records
> `Error`/`Trap{kind,func}` (always kept). Each async actor (`PickerActor` spec/
> init/accept, `CompletionActor` spec/generate, `DecorationActor`
> gutter-decorations) gained a `tracer: Option<PluginTracerHandle>` field + a
> `with_tracer` builder and routes its calls through the traced variant;
> `EventActor` (fire-and-forget `on-event`, no `trip_and_map`) emits an equivalent
> record inline. The loader captures the tracer in `LoaderServices` (built in
> `install`) and calls `actor.with_tracer(self.env.tracer.clone())` before spawning
> each seam's `run()`. `fuel_delta` is `0` for now (wall-time is the primary
> signal; fuel accounting is a later refinement). Tests: 3 unit
> (`trip_and_map_traced` — success→Debug/Ok, trap→Error/Trap + trips quarantine,
> no-tracer no-op) + `picker_actor::picker_calls_emit_boundary_trace_records` (a
> real actor's `spec`/`init` calls land `PickerSource` records; the client awaits
> each reply so the emit is already done). Mode/keymap/config/grammar are
> one-shot load-time registrations (no actor loop) — their instrumentation is a
> follow-on if load-time boundary calls prove worth tracing; the ongoing async
> seams are the ones that produce continuous signal. No bench (off-thread).

### PO.3 — the hot-path grammar seam (gated + benched)  ✅
Instrument the sync grammar trampoline behind the per-plugin atomic gate: a
relaxed load + predicted-not-taken branch when off (zero alloc / zero format),
a cheap enqueue when on; all formatting off-thread. **Exit:** with tracing off,
the keystroke→glyph bench shows ≈0 delta vs. the current ratchet (the perf gate
holds); with a plugin traced at `debug`, its grammar calls appear in the buffer.
The bench is the load-bearing artefact here.

> **Landed 2026-07-18.** The tracer became the single owner of verbosity and now
> *publishes* a per-plugin hot-path gate: `HotGate` (an `Arc<AtomicU8>` over the
> effective `TraceLevel`, `trace.rs`), handed to the trampoline and read once per
> guest call with a relaxed load. `TraceLevel` gained `#[repr(u8)]` +
> `as_u8`/`from_u8` (fails closed to `Off`). `PluginTracer` grew a `gates` cache +
> `hot_gate(plugin)` (seeds to the effective level, cached so clones share the
> atomic); `set_plugin_level` / `set_default_level` republish to the live gates
> (override → that plugin; default → every un-overridden gate), so `:set
> plugin.trace-level` (PO.4) flows to the next keystroke with no lock; `forget_plugin`
> drops the gate. The sync trampoline's `run_callback` (`grammar_trampoline.rs`)
> now reads `gate.records_calls()` (a single relaxed load + not-taken branch): a
> *success* times + emits `Debug`/`Ok` **only** when the gate admits it (off at the
> default `Info` — the hot-path zero-cost state); a guest `err` emits `Warn` (kept
> at the default gate, mirroring the async seam); a trap emits `Error`/`Trap` (cold,
> once per quarantine trip). `instantiate_grammar_plugin` took an
> `Option<&PluginTracerHandle>` (the loader passes `env.tracer`; tests / benches
> pass `None`, getting `HotGate::disabled`). Tests: 8 unit in `trace.rs` (gate
> round-trip / seed / republish-on-set / default-propagation-skips-overrides /
> cache-shares-atomic / disabled / forget) + 3 real-fixture integration in
> `grammar_source.rs` (default gate records nothing; raised-to-`Debug` captures the
> `apply-motion` call; a guest `err` records `Warn` at the default gate). Bench
> `grammar_trace_gate.rs` (benchmarks.md → PO.3): **trace-off is ≈ +1 ns (~0.3 %)
> vs. untraced** — the gate load + branch is the whole off-state cost; `debug` adds
> ~104 ns, never on the default path. Green: trace unit + grammar_source + perf
> ratchet + clippy.

### PO.4 — the trace-buffer views (shared + per-plugin)  🚧
`*plugin-trace*` (shared stream) + `*plugin-trace:<name>*` (per-plugin filtered)
synthetic Documents — read-only modes whose `on_activate` seeds from the ring and
subscribes to `PluginTracePushed` for the live tail (the `lsp-trace-mode`
precedent). A `t` chord on a row in the `:plugins` manager view (PL8.H) drills
into that plugin's trace. Verbosity a typed option (`plugin.trace-level` +
per-plugin override), live. **Exit:** the shared + a per-plugin view open and
live-tail; the manager `t` drill-in works; `:set plugin.trace-level=debug` raises
verbosity live. Sub-sliced into three commits (confirmed 2026-07-18).

#### PO.4.1 — the shared `*plugin-trace*` view  ✅
> **Landed 2026-07-18.** New pure-provider crate `lattice-plugin-trace` (the
> `lattice-plugin-manager` shape): `plugin-trace-mode` (major, read-only, no-file)
> + the `:plugin-trace` ex-command → `Effect::OpenSyntheticBuffer{
> "*plugin-trace*", "plugin-trace-mode" }`. ONE mode serves both surfaces (design
> §6): `on_activate` computes an `Option<u32>` plugin filter (`None` for the shared
> firehose; PO.4.2 parses `*plugin-trace:<name>*`), seeds from
> `tracer.snapshot_global()`, subscribes `PluginTracePushed`, and drains OFF-thread
> (the `lsp-log-mode` batch-append verbatim — nothing formats on the UI/actor
> thread). `format_trace_line` (`format.rs`) owns presentation: `{level}
> [plugin:{id}] {seam} {»/«}{call} → {outcome}` where outcome is `ok Nµs[, k fuel]`
> / `guest-err` (a Warn-level Ok — the PO.3 grammar no-op) / `trap(kind)` /
> `denied(cap)`. Wired one line into the host Phase-B list (`editor_boot.rs`); ZERO
> `Editor::` methods, zero host `Action` variants (the acid test holds). Tests: 9
> provider (5 `format` — success/fuel/guest-err/trap/denied; 4 `mode` —
> shared-keeps-all / filter-keeps-one / empty / read-only-no-file-major) + 3 host
> (`plugin_trace_view` ex-command-registered + open-activates-mode; boot pin
> `plugin_trace_mode_registered_at_boot` + `plugin-trace` in the ex-command pin).
> No bench (off the hot path — the emit gate was PO.3's bench).

#### PO.4.2 — the per-plugin view + manager `t` drill-in  ✅
> **Landed 2026-07-18.** The mode gained a `TraceFilter` (`Shared` / `Plugin(id)` /
> `Unknown`) resolved once at activation from the buffer name: `resolve_filter`
> parses `*plugin-trace:<name>*` (`parse_per_plugin_name`) and maps `<name>→id` via
> `PluginLoaderHandle.plugin_status()`; an unloaded name is `Unknown` (an empty
> view, never the firehose that would mislabel the buffer). Seed reads the matching
> ring (`snapshot_plugin(id)` vs `snapshot_global()`); the drain filters the same
> way. `lattice-plugin-manager` gained a `t` chord → `action:plugins-trace` + a
> `trace_handler` (both in the manager — mode-ownership) that reads the row's
> plugin and emits `OpenSyntheticBuffer{ per_plugin_buffer_name(name),
> TRACE_MODE_ID }`; the buffer-name scheme is single-sourced in `lattice-plugin-trace`
> (manager depends on it — acyclic) so producer/consumer can't drift. Tests: +2
> mode (`resolve_filter` shared-without-loader / unresolvable→Unknown) +1 format
> (per-plugin name round-trip) +2 manager (five action commands, name round-trip)
> +1 host (`t` binds to `action:plugins-trace` in the plugins-mode layer).

#### PO.4.3 — live verbosity option  📝
A typed enum option `plugin.trace-level` (default `info`) + an `Event::OptionChanged`
observer wired at boot calling `tracer.set_default_level(parsed)` (PO.3's republish
reaches the hot gates live). Per-plugin override via a `:plugins` manager chord
(`tracer.set_plugin_level(id, …)`).

### PO.5 — `wasi:logging` guest import (Layer 2)  📝
A `wasi:logging/logging`-shaped host import in `wit/`; the host impl routes each
guest `log(level, context, message)` into the same `PluginTracer` (a
`HostImport` record, `seam = logging`). Fixture guest + host round-trip test.
**Exit:** a guest that calls the logging import has its lines captured into the
trace buffer, interleaved with the boundary trace.
