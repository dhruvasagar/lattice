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

### PO.3 — the hot-path grammar seam (gated + benched)  📝
Instrument the sync grammar trampoline behind the per-plugin atomic gate: a
relaxed load + predicted-not-taken branch when off (zero alloc / zero format),
a cheap enqueue when on; all formatting off-thread. **Exit:** with tracing off,
the keystroke→glyph bench shows ≈0 delta vs. the current ratchet (the perf gate
holds); with a plugin traced at `debug`, its grammar calls appear in the buffer.
The bench is the load-bearing artefact here.

### PO.4 — the trace-buffer views (shared + per-plugin)  📝
`*plugin-trace*` (shared stream) + `*plugin-trace:<name>*` (per-plugin filtered)
synthetic Documents — read-only modes whose `on_activate` seeds from the ring and
subscribes to `PluginTracePushed` for the live tail (the `lsp-trace-mode`
precedent). A `t` chord on a row in the `:plugins` manager view (PL8.H) drills
into that plugin's trace. Verbosity a typed option (`plugin.trace-level` +
per-plugin override), live. **Exit:** the shared + a per-plugin view open and
live-tail; the manager `t` drill-in works; `:set plugin.trace-level=debug` raises
verbosity live.

### PO.5 — `wasi:logging` guest import (Layer 2)  📝
A `wasi:logging/logging`-shaped host import in `wit/`; the host impl routes each
guest `log(level, context, message)` into the same `PluginTracer` (a
`HostImport` record, `seam = logging`). Fixture guest + host round-trip test.
**Exit:** a guest that calls the logging import has its lines captured into the
trace buffer, interleaved with the boundary trace.
