# Plugin observability

> **Design fragment.** Contracts, data model, rationale, rejected alternatives,
> paramount-goal alignment. Sequencing lives in the slice plan
> ([`../operations/slice-plans/archive/plugin-observability.md`](../operations/slice-plans/archive/plugin-observability.md)).
> Sibling fragments: [`plugin-host.md`](plugin-host.md) (the seam spine +
> capability/fuel/crash model), [`boot-composition.md`](boot-composition.md)
> (the `install(boot)` contract).

## 1. Why

A WebAssembly Component-Model plugin host has three audiences who need to see
what a plugin is doing, and the tooling must serve all three:

- **Plugin users** — "which plugin caused this? is it healthy? why did it
  crash?" Low-detail, actionable. Surfaced by the `:plugins` manager view
  (PL8.H) and `*messages*`.
- **Plugin authors** — developing an extension: every host↔guest call, its
  arguments, timing, fuel cost, capability denials, and trap provenance.
  High-detail, opt-in.
- **Lattice core devs** — debugging the host itself.

This becomes *more* important, not less, as plugins ship in many languages
(Rust, Zig, Go, AssemblyScript, …) cross-compiled to wasm: the level of
guest-internal debugging we can offer is bounded by what each toolchain emits,
so the tooling that is *always* available must not depend on the guest's source
language at all.

## 2. The core insight — the boundary is the observability seam

Every plugin interaction is host-mediated (paramount #2/#4: typed message
passing, the host owns the linker and every WIT import/export). So the host sees
**every** call in and out of a plugin — name, arguments, duration, fuel delta,
result-or-trap, capability grants/denials, lifecycle transitions — and it sees
them by instrumenting *its own side* of the boundary, **independent of the
guest's source language**. That is the richest signal we can offer portably, and
it is the backbone of this design.

### What we can and cannot observe

| | Host-observable (portable) | Needs guest cooperation / debug-info |
|---|---|---|
| Every WIT call (args, timing, fuel, result) | ✅ boundary instrumentation | — |
| Traps + wasm backtrace | ✅ wasmtime (backtraces on by default; we classify fuel/epoch/trap) | source-level frames need DWARF |
| Capability grants/denials, lifecycle | ✅ (`grant` outcome + `PluginCrashed`) | — |
| The guest's own narrative ("parsing X") | — | ✅ a `wasi:logging` host import the guest calls |
| Step-through, locals, breakpoints | — | ✅ wasmtime DWARF + gdb/lldb (author-side) |

### The Emacs-debugger analogy, corrected

Emacs can step-debug because Emacs *is* the Lisp runtime — total introspection.
We do not own the guest runtime; wasmtime executes opaque wasm. A source-level
step-debugger is possible only via wasmtime's DWARF/gdb-lldb integration —
author-side, heavy, language-dependent. **But** for a *message-passing* plugin
architecture the right debugging granularity is not stepping through the guest's
inner loops; it is the **boundary interaction trace + crash forensics**, which
the host produces completely and portably. That is our "debugger." It is a
property of the architecture, not a limitation of it.

## 3. The layered stack

Four layers, each serving a paramount goal and an audience. This fragment
specifies Layers 1–2; Layer 0 is built; Layer 3 is deferred.

- **Layer 0 — lifecycle + crash isolation (resilience).** *Built.*
  `Event::PluginCrashed` + quarantine (PH7.12b) + the loader health/status model
  (PL8.H.1). The `:plugins` manager view (PL8.H) is its user-facing surface.
- **Layer 1 — boundary trace (transparency).** Instrument the host side of every
  import/export: `[plugin:name] apply-motion(args…) → ok 34µs, 1.2k fuel`.
  Config-gated verbosity, streamed to a plugin-trace buffer. **Language-agnostic;
  needs no guest cooperation.** The backbone.
- **Layer 2 — guest narrative.** A `wasi:logging`-style host import so guests
  emit their *own* structured logs, host-captured into the same buffer, tagged by
  plugin + level. Complements Layer 1 (the guest's intent vs. the host's observed
  behavior).
- **Layer 3 — author deep-debug (deferred).** Trap backtraces (wasmtime,
  available today), per-call fuel/time profiles, and — as an advanced escape
  hatch only — the wasmtime DWARF/gdb path for authors who ship debug-info. **Not
  core:** it serves only some authors and no users, and is a large,
  language-dependent surface. Documented as an escape hatch, not built.

## 4. The hot-path contract (paramount #1 + #4) — LOAD-BEARING

**Tracing is never on the editor hot path.** This is the design's hardest
constraint, and it shapes every mechanism below.

- Trace records are **streamed via the event bus, exactly like LSP logs**: the
  producer appends to a per-plugin ring and fires a typed `PluginTracePushed`
  event; an **off-thread** drain (a spawned task, the `LspLogPushed` precedent)
  formats and appends to the buffer. Nothing is formatted or written on the
  UI/actor thread.
- The **async seams** (picker / completion / decorations / events / modes /
  keymap / config) already run on the multi-thread runtime, off the actor
  thread — they instrument richly for free.
- The **grammar/sync-trampoline seam** is the keystroke hot path (CI ratchet:
  typed call < 500ns p99). Its instrumentation must be **zero-alloc,
  zero-arg-format when off** — a single relaxed-atomic per-plugin gate load and a
  predicted-not-taken branch. When *on*, emission is at most a cheap
  non-blocking enqueue onto a ring/channel; all formatting happens off-thread on
  the drain side. By default the hot-path seam carries only lifecycle/crash
  signal, not per-call traces. A bench pins that the off-state adds ≈0 to the
  500ns budget.

## 5. Data model

Mirrors the LSP logging substrate (`LspLogger` / `LspLogPushed` /
`LogRecord`), which already proves the ring + boot-wired-publisher +
off-thread-drain shape.

### `PluginTraceRecord`

One boundary event. Structured (not a pre-formatted line) so the view owns
presentation and filters can key on fields.

```rust
struct PluginTraceRecord {
    plugin: u32,              // host-issued id (SourceLayer::Plugin / PluginCrashed.plugin)
    seam: PluginSeam,         // picker / completion / grammar / …
    direction: Direction,     // HostImport (guest→host) | GuestExport (host→guest)
    call: Cow<'static, str>,  // the WIT function name (e.g. "apply-motion")
    level: TraceLevel,        // Error | Warn | Info | Debug | Trace
    outcome: TraceOutcome,    // Ok { micros, fuel_delta } | Trap { kind, func } | Denied { capability }
    detail: Option<String>,   // arg/result summary, populated only above a verbosity threshold
}
```

`plugin` keys the per-plugin ring and the filtered views; it is the same id the
manager view already renders and `PluginCrashed` carries, so Layer 0 crash rows
and Layer 1 trace rows join cleanly.

### `PluginTracer` (the sink)

The `LspLogger` analogue — an `Arc`-shared handle registered as a boot service
(`PluginTracerHandle`, per the ServiceRegistry Arc/TypeId rule):

- A **global ring** + **per-plugin rings** (`HashMap<u32, ring>`), bounded
  (VecDeque), so memory stays flat.
- A **per-plugin verbosity gate** (min `TraceLevel`, default `Info` — i.e. no
  per-call tracing) + a per-plugin detail toggle. The hot-path gate reads this.
- An **optional event publisher** wired at boot to the runtime bus; `None` in
  tests / pre-wire (no events, ring still fills).
- `trace(record)`: gate by the plugin's min level → push to the ring → fire the
  publisher. The whole call is cheap; formatting is deferred to the drain.

### `PluginTracePushed` (the stream)

A `register_event!` typed event (`"plugin.trace-pushed"`), published via
`publish_typed` on every append. Subscribers (the trace-buffer views) drain it
off-thread and append, exactly as the LSP log views drain `LspLogPushed`.

## 6. The buffer surface — one shared stream + per-plugin filtered views

Everything-is-a-buffer. The trace surfaces as synthetic Documents, the
`lsp-trace-mode` / `*lsp*` precedent:

- **`*plugin-trace*`** — the shared stream: every plugin's records, interleaved,
  tagged by plugin. The firehose.
- **`*plugin-trace:<name>*`** — a per-plugin filtered view over the same
  underlying stream, opened on demand. A `t` chord on a row in the `:plugins`
  manager view (PL8.H) drills into that plugin's trace.

Both are read-only synthetic Documents whose mode `on_activate` seeds from the
ring and subscribes to `PluginTracePushed` for the live tail (filtering by
`plugin` for the per-plugin view) — the LSP-log mode's exact structure.

## 7. Configuration + verbosity

A typed option (`§5.12`) drives the gate, so it is `:set`-able and lives in the
customize buffer:

- `plugin.trace-level` — global default (`off` / `error` / `warn` / `info` /
  `debug` / `trace`); `off`/`info` filter per-call noise by default.
- Per-plugin override (set from the manager view or `:set`), stored on the
  tracer's per-plugin gate — the `LspLogger` per-instance-level precedent.

The option write updates the tracer's gate live; the hot-path seam reads the
atomic gate, so raising verbosity for one plugin never touches another's cost.

## 8. Layer 2 — `wasi:logging` guest import

A `wasi:logging/logging`-shaped host import (`log(level, context, message)`) in
the `wit/` package. Any component-model language calls it; the host impl routes
each call into the same `PluginTracer` (as a `Direction::HostImport` record with
`seam = logging`), so the guest's narrative interleaves with the boundary trace
in the same buffer. Standards-aligned and language-agnostic — the same principle
as Layer 1, extended to guest-originated lines. Registration-only WIT change; no
hot-path involvement (logging is an async-linker import).

## 9. Paramount-goal alignment

- **#1 Performance.** The hot-path contract (§4) is the design's spine: nothing
  formats or writes on the UI/actor thread; the sync seam gates to ≈0 when off,
  pinned by a bench. Element fan-out is unaffected (the trace buffer is a normal
  Document, O(viewport) to render).
- **#2 Extensibility.** Language-agnostic boundary tracing is the tooling that
  makes a cross-language plugin ecosystem debuggable — part of the plugin API
  contract, not an afterthought.
- **#4 Asynchronicity.** Trace emission is a cheap enqueue; all work is
  off-thread via the bus + a spawned drain. The tracer never blocks a plugin
  actor or the UI.
- **UX (higher court).** Zero pixel impact on unedited content: the trace buffer
  is opt-in and off the render path of any other buffer.

## 10. Rejected alternatives

- **Guest-side / DWARF step-debugging as the primary model** — rejected: not
  portable (language/toolchain-dependent), serves no users, large surface. Kept
  as a deferred Layer-3 escape hatch (§3).
- **Synchronous `tracing::debug!` with arg-formatting at the seams** — rejected:
  formatting on the actor/hot-path thread violates §4. `tracing` remains the
  *diagnostic-log* transport (per the `debug!`-not-`info!` rule); the plugin
  *trace* is its own bus-streamed, buffer-backed channel.
- **A bespoke trace pipe parallel to the event bus** — rejected: the typed bus +
  the `LspLogger` ring/publisher/drain shape already exist and are proven;
  reusing them is the genuinely-better long-term fit (heuristic #1).
- **Per-call tracing on by default** — rejected: needless hot-path cost and
  buffer noise; default `info` (no per-call), authors opt a plugin up to
  `debug`/`trace`.

## 11. Slices

See the slice plan
([`../operations/slice-plans/archive/plugin-observability.md`](../operations/slice-plans/archive/plugin-observability.md)):
PO.1 trace record + event + tracer sink · PO.2 instrument the async seams · PO.3
the hot-path grammar seam (gated + benched) · PO.4 the trace-buffer views
(shared + per-plugin, manager drill-in) · PO.5 `wasi:logging` guest import.
