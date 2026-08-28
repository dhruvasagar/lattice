# Plugin Host (Phase 7) — WASM Component Model extension substrate

**Status:** ✅ Phase 7 complete (design 2026-07-01; last refreshed 2026-07-18). **PH7.0–7.12
landed** — the `lattice-plugin-host` crate, the `wit/` package, the capability/WASI model, the
boundary mirrors (`Effect`/picker/completion/grammar/event/decoration), the `fuzzy-finder`
validation plugin (⭐ Phase-7 exit), the grammar-extension sync seam (PH7.7), the **event/hook
seam** (PH7.8), the **decoration seam** (PH7.9), config/modes/keymap seams, host-services
(`walk`), and the teardown + graceful-degradation audit, plus CI perf gates. **Phase 8 landed on
top** (branch `phase-8-plugin-loader`): the `lattice-plugin-loader` (discovery + `:plugin-load`/
`unload`/`reload` + `init.rs`), the `:plugins` manager view (PL8.H), and the **plugin
observability** stack (PO.1–PO.5 — see the sibling fragment
[`plugin-observability.md`](plugin-observability.md): the boundary tracer, the gated hot-path
grammar seam, the `*plugin-trace*` buffer views, the live `plugin.trace-level` option, and the
`wasi:logging` guest import). This fragment is the detailed "what/why" that expands
`design.md` §5.5 (Plugin Subsystem), §9 (Plugin API), §10 (extension tiers), §3.1
(core-vs-plugin split), §14 (risks). Slice sequencing + landed status lives in
`docs/dev/operations/slice-plans/archive/plugin-host.md` (+ `plugin-loader.md`, `plugin-observability.md`);
a conformance review of the whole host against the paramount goals + the three extension goals
(grammar / modes / rich UI) is `docs/dev/audit/plugin-host-architecture.md`.

> **Superseded (PH7.7, 2026-07-12) — the grammar seam is synchronous, not async.** §3's
> "Async ABI is canonical … no synchronous path from UI input to plugin code" and §4.1's
> "the trampoline is async + fuel-bounded" describe the pre-PH7.7 assumption. They were
> corrected when the grammar seam landed: a plugin **motion / operator / text-object must
> resolve synchronously** to compose with its operator and to keep dot-repeat / macros
> synchronous (async grammar would break operator∘motion atomicity and the keystroke
> contract). So grammar is the **one bounded synchronous-on-keystroke seam** — the renderer
> still never calls WASM (absolute), and every *other* seam (picker, completion, events,
> decorations, ui, config) stays async / off-keystroke. The exact invariant + the
> Reflex-class budget it requires are in the audit (I2 / F1). The paragraphs below are left
> in place for provenance with an inline supersession marker.

**Scope (locked with Dhruva, 2026-07-01):** *Phase 7 proper* — the host runtime, the
capability/security model, the WIT interface set mirroring the **already-exercised** native
trait seams, and the `fuzzy-finder` validation plugin. Modes-as-components (full), the
bundled-plugin manager, config-as-WASM (`init.rs`), live-eval, and the post-1.0 ABI-freeze
policy are **deferred to their own fragments**, cross-referenced in §12. The design spine is
the **exercised-trait → WIT mirror** (chosen over WIT-first and layered framings): every WIT
interface is derived from a real, object-safe Rust trait that ships and is exercised today,
because that is exactly the mitigation `design.md` §14 names for "WIT design proves wrong."

---

## 1. Thesis: the boundary is a seam we already built, not a wall we bolt on

`design.md` §3.1 is the load-bearing principle and this fragment's spine:

> **Fast path stays in core. Configuration / orchestration / authoring goes to plugins.
> The trait surface between them is the extensibility seam.**

The WASM Component Model boundary has real, measured cost (typed-call p99 < 500ns;
round-trip < 5μs — §5.5.2). Anything that fires per-keystroke or holds keystroke-hot-path
state would pay that cost on every input event and erode the one-frame keystroke→glyph
ceiling (8.3ms @120Hz). So we earn extensibility *around* the hot-path state machines via
traits, never *inside* them via WASM dispatch. Phases 4–6 deliberately grew a rich set of
**object-safe provider traits** for exactly this moment; the plugin host ships against that
concrete, exercised set rather than a speculative API.

The codebase already pre-reserved the plugin surface — this is not retrofitting:

| Reserved seam | Where | What it reserves |
|---|---|---|
| `SourceLayer::Plugin(u32)` | `lattice-grammar/src/source.rs:38` | Host-issued plugin-id provenance for registered commands |
| `Args::Bytes(Vec<u8>)` | `lattice-grammar/src/args.rs:18` | "When WASM lands, WIT-typed args replace this byte form" |
| `CandidateData::Extension { kind_id, payload }` | `lattice-completion/src/candidate.rs:128` | msgpack escape hatch for plugin candidates |
| `SubscriptionTarget::Plugin` (omitted) | `lattice-runtime/src/events.rs:23` | Bus variant deliberately absent "pending WASM hosting" |
| `DecorationProvider` (stub) | `lattice-mode/src/contributions.rs:44` | "reserved for the WIT plugin-facing contribution surface (M.10)" |
| `Mode::required_capabilities() -> CapabilitySet` | `lattice-mode/src/mode.rs:262` | Capability declaration already on the mode trait |
| `KeymapCapability::MinorMode` | `lattice-keymap/src/registry.rs:79` | Write-gate: plugins may write `MinorMode`/`Buffer` layers only |

The plugin host's job is to fill these reserved slots, not to invent a parallel universe.

---

## 2. Paramount-goal alignment (the whole-doc frame)

Every subsequent design choice is mapped locally; the frame:

> **UX (higher court):** a plugin must never stall a keystroke, never crash the editor,
> never paint the wrong pixel. Crash isolation, fuel limits, and the async ABI make "good
> UX is the reliable outcome" a structural property, not a discipline. A trapping plugin
> yields a notification, not a dead editor.
> **Paramount #1 (performance):** protected by the fast-path/orchestration split — WASM is
> off the keystroke hot path by construction; per-call budgets are CI-gated (the Phase-7
> exit gate). Sacrifices nothing on the hot path because nothing hot crosses the boundary.
> **Paramount #2 (extensibility):** the reason this phase exists. WIT is the canonical API;
> the interface set is sized against exercised traits so it is honest on day one.
> **Paramount #3 (vim grammar):** the grammar-extension WIT mirrors `register_motion` /
> `register_text_object` / `register_operator` / `register_ex_command` exactly — plugin
> motions are first-class grammar citizens, not a bolt-on command layer.
> **Paramount #4 (asynchronicity):** each plugin owns its `wasmtime::Store`, runs as tokio
> task(s), suspends at every host call. Many plugins execute in parallel across cores; the
> UI thread never invokes WASM directly.

---

## 3. Host runtime architecture

The `lattice-plugin-host` crate owns the wasmtime engine and the per-plugin lifecycle.

- **Runtime:** `wasmtime` + Component Model + WASI (preview2). `wit-bindgen` generates the
  guest bindings; `wasmtime::component::bindgen!` generates the host side. WIT files live in
  `wit/` (canonical, one file per interface — §5).
- **Store-per-plugin.** Each plugin instance owns its own `wasmtime::Store<PluginState>`.
  Stores are independent; there is no global plugin lock. `PluginState` holds the plugin's
  capability grant, its WASI context (scoped fs/net), its resource tables (document handles,
  callback registries), and its fuel meter.
- **Task-per-Store.** Each Store is driven by one or more tokio tasks on the multi-thread
  runtime. Two plugins doing CPU work run on two cores. A Store is `!Send` *while executing*
  but yields at every host call, so no OS thread is pinned (normal tokio multi-thread
  behavior). The editor actor is `current_thread`; plugin tasks run on the shared
  multi-thread runtime, never on the actor thread (paramount #4 + the CLAUDE.md
  no-UI-thread-work rule).
- **Async ABI is canonical *for every seam except grammar*.** Host functions are async; a
  plugin host call suspends the WASM stack and releases the OS thread. This is what makes "no
  plugin can stall the UI, ever" hold *by construction* for picker, completion, events,
  decorations, ui, and config: there is no synchronous path from UI input to those. **The one
  exception is the grammar seam** (§4.1, PH7.7): a motion/operator/text-object `apply` resolves
  *synchronously* on the dispatch thread (it must, to compose with its operator), bounded by a
  Reflex-class fuel + epoch budget so a runaway traps well inside the frame. The renderer never
  calls WASM regardless. See the supersession note at the top of this fragment + audit I2 / F1.
- **AOT compile at install; module cache on disk.** Cranelift compiles the component ahead
  of time; the artifact is cached under `<user-cache>/lattice/plugin-cache/`
  (`${XDG_CACHE_HOME}` on Linux, Application Support on macOS, LocalAppData on Windows) so a
  second launch reuses it (resolves `design.md` §15 Q17). Re-installs and editor upgrades reuse
  artifacts. Per-instantiation cost is linear-memory allocation + import resolution, not codegen.
  > **Superseded (PH7.1b, 2026-07-01):** the original proposal — a hand-rolled
  > `sha256(component_bytes + wasmtime_version + target_triple + wit_revision)` key plus
  > `Component::serialize`/`deserialize` — is **not** how this shipped. wasmtime 46 provides a
  > built-in on-disk cache (`Config::cache` + `CacheConfig::with_directory`) that owns the keying
  > and invalidation (bytes + compiler config + target + wasmtime version) and needs **no
  > `unsafe`**. The host sets only the directory. Chosen on paramount-#2/security (keeps the
  > workspace `unsafe_code = "deny"` gate intact) + heuristic #1 (less code, upstream-maintained
  > invalidation vs. a hand-rolled key). See the PH7.1b slice.
- **Lazy instantiation.** A plugin that is never invoked is never instantiated. 50 installed
  plugins contribute 0 instantiation cost to startup; a plugin instantiates on first
  invocation of one of its contributions. Cold-start budget for 50 lazily-loaded plugins:
  < 30ms total (§5.5.2).
- **Fuel + epoch limits.** Every plugin call runs under a fuel budget (wasmtime fuel) and an
  epoch-interruption deadline. Exhaustion traps cleanly: the offending task is killed, a
  `PluginCrashed` event fires, the plugin is quarantined, and *every other plugin, the
  document actor, the UI, and the LSP clients keep running*.

> **Heuristic #1 (long-term fit, on merit):** Store-per-plugin + task-per-Store is more work
> than a single shared interpreter Store, but it is the genuinely-better design — it is the
> only shape that delivers crash isolation *and* cross-core parallelism (paramount #4) *and*
> per-plugin capability/fuel scoping. A shared Store would couple failure domains and
> serialize plugin CPU. Chosen on merit, not novelty.
> **Heuristic #2 (paramount, not other editors):** wasmtime Component Model is chosen because
> it gives us capability sandboxing + fuel + cross-language WIT on our async substrate — not
> because "editor X embeds WASM." Lua/Scheme/Rhai are rejected in §11 on paramount grounds.

---

## 4. The boundary-crossing problem (the crux)

The exercised traits are object-safe (`Arc<dyn Trait>`) — the shape that wraps cleanly as a
WASM-backed `Box<dyn Trait>`. But four kinds of trait content **cannot cross a WASM boundary
as-is**. How each is resolved is where the design earns or loses paramount #1 vs #2.

### 4.1 Closures → callback-id + exported guest function

The grammar specs and the picker/completion providers carry **boxed Rust closures**
(`Box<dyn Fn(&Ctx) -> Result<...>>`). A WASM component cannot hand the host a Rust closure.
Inventory of every closure seam (from `lattice-grammar/src/registry.rs`):

| Spec | closure field(s) | inputs → output |
|---|---|---|
| `MotionSpec` | `apply` | `MotionContext` → `MotionResult` |
| `OperatorSpec` | `apply` | `&mut OperatorContext` → `Effect` |
| `TextObjectSpec` | `apply` | `TextObjectContext` → `ProtoRange` |
| `ExCommandSpec` | `parse_args` | `(rest: string, bang: bool)` → `Args` |
| `ExCommandSpec` | `apply` | `ExCommandContext` → `Effect` |
| `ActionSpec` | `apply` | `ActionContext` → `Effect` |

**Resolution:** registration returns a stable id (already: `MotionId`/`ExCommandId`/…). The
WIT mirror turns "spec carries a closure" into "the guest component *exports* a function; the
host stores `(command_id → guest_export_ref)` and, on dispatch, calls the export by id." The
host constructs the native spec with an `apply` closure that is a thin shim: it projects the
dispatch context into an owned WIT record, calls the guest export (**synchronously** —
superseded from "async"; fuel + epoch bounded, PH7.7), maps the returned WIT `effect` back to
the native `Effect`. From the dispatcher's view
(`dispatcher.rs`), nothing changes — it still looks up an entry by `CommandId` and calls a
boxed `Fn`; the `Fn` just happens to trampoline into WASM.

> **UX (higher court) [superseded PH7.7 — sync, not async]:** the trampoline is
> **synchronous** on the dispatch thread + fuel/epoch-bounded. A slow/looping plugin motion
> cannot freeze the keystroke because the epoch deadline traps it well inside the frame — the
> motion is a no-op with a logged warning, never a hang. This requires a **Reflex-class**
> budget (sub-frame), distinct from the lifecycle/async budget — the default ~1s epoch would
> let a plugin motion stall a keystroke (audit F1); PH7.7c sets the tight budget.
> **Paramount #1:** grammar round-trip budget < 5μs p99 (§5.5.2) is the CI gate on the
> *boundary marshalling* (measured ~40ns, PH7.7a); the guest's own compute is bounded by the
> Reflex fuel/epoch budget, not this gate. Built-in motions stay native (`lattice-grammar`)
> and never pay either.
> **Paramount #3:** because registration is the *same* `insert_*` path with a host-supplied
> `SourceLayer::Plugin(id)`, a plugin motion is indistinguishable from a builtin to the
> grammar — first-class, per goal #3.

### 4.2 Borrows of live host state → owned snapshot projection

`PickerContext<'a>`, `ActiveBufferSnapshot<'a>`, `GenerateContext<'a>`, and the grammar
contexts carry `&'a Buffer`, `&'a Path`, `&'a str`. The rope especially cannot cross. The
host adapter projects each context into an **owned, WIT-serializable snapshot** at the call
boundary. Bulk text never rides the snapshot: the guest gets a `document` **resource handle**
and calls back (`get-text-range(doc, range)`) for the slices it needs — zero-copy at the
slice level (§9.6). This preserves the §5.5.2 rule "resource handles, not copies."

> **Heuristic #3 (third option):** the naive options were (A) copy the whole rope into the
> snapshot — violates paramount #1 on large buffers; (B) expose `&Buffer` as a raw pointer —
> unsound across the sandbox. The chosen (C) — owned metadata snapshot + resource-handle
> callbacks for text — is neither, and it is what §9.6 already commits to.

### 4.3 `Future`/`Stream` carriers → host-owned async adapters

`PickerInitResult::Future(Pin<Box<dyn Future>>)` and `::Stream(mpsc::UnboundedReceiver)` are
host-runtime objects, not serializable. The WIT mirror expresses async results as **guest
returns batches; host owns the plumbing**: for one-shot the guest export is `async` and the
host awaits it; for streaming the guest holds a host-provided `result-sink` resource and
pushes batches, or the host polls a guest `next-batch()` export. Either way the `Future`/
`mpsc` stays host-side; the guest never names a tokio type. This is the picker's
`Inline`/`Future`/`Stream` trichotomy re-expressed in Component-Model record/future/stream
terms (§9.4 already anticipates `stream<rg-match>`).

The same async-carrier discipline extends to **`accept`**, which the native
`PickerSourceGenerator::accept` declared *synchronous* — fine for a native source (accept is a
pure routing→outcome translation) but not for a guest, whose `accept` export is `async` and
bound to its actor task (§5.7). PH7.4c.2 adds a generic seam: `accept_async(&self, ctx,
routing) -> Option<AcceptFuture>` (default `None`, so native sources are untouched). A `Some`
return is spawned and drained the same way `init`'s `Future` is — the host owns the plumbing,
the guest returns one outcome. This preserves the §3 guarantee **by construction**: there is no
synchronous path from a keystroke to plugin `accept`, so a slow/hostile plugin accept can never
freeze the actor thread. Blocking the actor on the guest call (the naive alternative) is
rejected on exactly that ground; pre-resolving accept during `init` is rejected too (O(N) guest
calls + it would carry the init-time context, not the accept-time one).

### 4.4 The `Effect` closed enum → WIT variant mirror; partial-serde fields → explicit records

`Effect` is a **closed ~105-variant enum** in `lattice-grammar` (`effect.rs`), deliberately
"the host boundary vocabulary." The WIT mirror declares an `effect` variant type covering it
whole; operator/ex-command/action guest exports return it, and the host maps back 1:1. The
partially-`#[serde(skip)]` candidate fields (`accept_action`, `annotations`, `display_spans`)
and the non-serde `Annotation` enum get **explicit WIT records** — they do not ride the
existing serde path. `RoutingPayload::InvokeCommand` / `PickerAcceptOutcome::InvokeCommand`
carry `Args`; the WIT maps `Args` to a typed variant (retiring the `Args::Bytes` escape hatch
for typed calls, per the `args.rs` module note).

> **Heuristic #1:** mirroring the *whole* closed `Effect` enum is more upfront WIT surface
> than a "generic opaque effect blob," but it is the honest design: a typed variant is
> introspectable, versioned, and lets the host reject malformed effects at the boundary
> rather than at apply-time. The blob would smuggle an untyped seam into the one place §14
> flags as highest-risk (WIT-design-wrong). Rejected.

### 4.5 In-place mutation → pass-in / return-out

`CandidateRanker::rank(&mut Vec<..>)` and `CandidateAnnotator::annotate(&mut RenderedCandidate)`
mutate host-owned data by reference. Across WIT the host passes the data in and takes the
reordered/annotated data out. `OperatorContext` holds `&mut Document`; document mutation
becomes host calls (`apply-edits(doc, edits)`), never a `&mut` handle.

---

## 5. The WIT interface set (exercised-trait → WIT mirror)

One `.wit` file per interface under `wit/`. Each interface below names the **native trait it
mirrors**, the **adapter direction**, and its **Phase-7 status** (⭐ = required for the
Phase-7 exit; ➕ = exercised-seam coverage that hardens the WIT before the 1.0 freeze). Only
⭐ is on the Phase-7-proper critical path; the rest are designed here so the WIT is sized
against the full exercised set (§14 mitigation), and land as follow-on slices.

| WIT interface | Mirrors native | Adapter direction | Phase 7 |
|---|---|---|---|
| `buffer` | `Document`/`Buffer` + `apply_edit` | host→guest resource; guest calls back for text | ⭐ |
| `host-services` | tree-sitter / ripgrep / regex / fs / net / proc | guest→host calls, capability-gated | ⭐ |
| `picker-source` | `PickerSourceGenerator` (`source.rs:294`) | guest exports spec/init/accept; host wraps as `Arc<dyn>` | ⭐ |
| `plugin` (lifecycle) | activate / deactivate / on-event | host→guest exports | ⭐ |
| `completion-source` | `CandidateGenerator` (✅ PH7.6); `Matcher`/`Ranker`/`Annotator` type-mirrored only | guest exports **async `generate`**; host produces candidates off-keystroke (LSP pattern) → native `match_and_rank` | ➕ |
| `grammar` | `register_{motion,text_object,operator,ex_command}` | guest exports apply/parse; host shim closures | ➕ |
| `command` | `CommandRegistry` + `CommandInvocation` + `Effect` | guest→host `invoke`; host→guest apply | ➕ |
| `events` | `EventBus::subscribe` + `Event` enum (✅ PH7.8) | host owns mpsc + a type-erased sink; forwards each `Event` to the guest `on-event` off-keystroke | ➕ |
| `decorations` | `Mode::gutter_decorations` + `GutterDecoration` (✅ PH7.9) | host calls guest producer OFF the render path per trigger; caches per-line data (the PH7.6 fork) | ➕ |
| `modes` | `Mode` trait (`mode.rs:148`) + `ModeRegistry` | guest declares mode; host registers `Arc<dyn DynMode>` | ➕ |
| `ui` | status/gutter segments, popups, notifications, sprites (type-mirror ✅ PH7.9) | guest→host emit data (no draw calls) — emit producer deferred | ➕ |
| `config` | `ConfigRegistry` + `OptionType`/`OptionSpec` | guest declares typed option; host registers into registry | ➕ |

### A seam that spawns its own Store must be handed the config registry

Store-per-plugin (§3) means each seam's `spawn_*` builds a store from
`new_store`, which starts with `config_registry: None`. A guest calling
`config.get-option` / `config.set-option` from *that* seam then takes the
"plugin has no config registry wired" branch: a `warn!` into the log and a
no-op return. Nothing traps, nothing surfaces to the user, and `:set <opt>?`
keeps reporting the compiled default — so the symptom is "my config does not
apply" with a working-looking plugin behind it.

Every seam whose guest can read or write an option must therefore take a
`config: Option<&Arc<ConfigRegistry>>` and set it on the store before driving
any guest export. Today that is `config`, `context`, `transient`, `grammar`
(via the trampoline) and `events`.

**This has now been the same bug three times** — the transient seam (OC.3,
`org-capture.md` §5), the context seam, and the events seam. Each time the
seam was wired end to end and each time the *option* path through it was the
one path no test took. The events case is the sharpest illustration: the CI.5
deferred-config chain test drove `modes.enable-mode` from `on-event` and
passed, but `enable-mode` reaches the **bus**, not the registry — so the
documented `init.rs` pattern for configuring a user plugin
(`docs/user/init.md`, "IMMEDIATE vs DEFERRED") had never been executed once.
`init_deferred_config.rs` now asserts both halves.

A test for a new seam that exposes `config` must set an option through it and
read the value back off the **live registry**, not merely observe that the
guest call returned.

**Registration is into the *same* registries, never a parallel plugin registry — and the
mechanism already exists.** Boot assembles `EventBus`, `CommandRegistry`, `ModeRegistry`,
`ServiceRegistry`, `CompletionRegistry`, `PickerRegistry`, `ConfigRegistry` in `Editor::boot`
(`editor_boot.rs:225`), and native subsystems contribute into them through **one abstract
surface**: the `SubsystemBoot` trait (`lattice-mode/src/subsystem_boot.rs:51`), implemented by
the host's `BootContext`. Each Phase-B subsystem already installs via a single call —
`lattice_lsp::install(&mut boot)`, `lattice_diff::install(&mut boot)`,
`lattice_multibuffer::install(&mut boot)` (`editor_boot.rs:477`). `SubsystemBoot` exposes
`commands_mut` / `modes_mut` / `register_service` / `service` / `inbound` / `wake_on_event` /
`tick_callback` / the event bus — the complete generic contribution vocabulary.

**A plugin is just another subsystem that installs through the same `install(boot)` seam.**
The plugin host is a subsystem whose `install` walks each loaded component's declared
contributions and, for each, wraps the WASM export as the corresponding `Arc<dyn Trait>` and
calls the existing insert path on `boot`. No new registry, no new contribution vocabulary —
the acid test (a new plugin adds ZERO `Editor::` methods) is satisfied because plugins reach
exactly the surface `lattice_lsp` and friends already reach. (Watch the documented
`ServiceRegistry` Arc/TypeId double-Arc footgun — register and look up with the *same* `T`;
`subsystem_boot.rs:65`.) The per-seam insert paths:

- Picker: `PickerRegistry::register_generator(Arc<dyn PickerSourceGenerator>)` (`source.rs:167`).
- Completion: the `pub(crate) insert_*` seam that takes `Arc<dyn Trait>` + explicit
  `SourceLocation` (`registry.rs:225`) — the public `register_*` takes `impl Trait` (generic,
  monomorphized) and is *not* reachable by a dyn plugin wrapper; the host uses `insert_*`.
- Grammar/command: the `pub(crate) insert_*` path with a host-constructed
  `SourceLayer::Plugin(id)` (§4.1). The guest never supplies its own provenance.
- Events (**✅ PH7.8**): the host filled the reserved `SubscriptionTarget::Plugin { plugin,
  handler, sink }` slot. The `sink` is a **type-erased** `Arc<dyn Fn(Event) -> bool>`
  (`lattice_runtime::PluginEventSink`) the plugin host builds over its own `futures` mpsc — so
  the bus stays channel-agnostic (no plugin-host dep in `lattice-runtime`); the bus calls it in
  the **lock-dropped** dispatch phase (audit M1) so a slow handler never delays the publisher or
  another subscriber, and a `false` return prunes a closed sink. The per-plugin `EventActor`
  drains the channel off the keystroke path and drives the guest `on-event` export. **The seam
  is its own async `events-plugin` world** (`import events` + `export register-events` +
  `on-event`), NOT the base `plugin` world — a world's exports are mandatory, so hanging
  `on-event` off `plugin` would force every non-observer component (incl. the WAT scaffolds) to
  implement it; the dedicated world matches the picker/completion/grammar precedent. A guest
  subscribes with a self-chosen `handler` id (the grammar `callback` precedent); the declarative
  filter crosses (a custom `predicate` is the guest filtering inside `on-event`). §5.10.4. *(A
  component trap taints its instance, so a trapping plugin's later deliveries also fail —
  dead-until-reinstantiation, PH7.12; the guarantee is cross-plugin/host isolation.)*
- **Periodic wakes (✅ OC.2)**: the same world carries `wake-every(ms) -> wake-id` /
  `cancel-wake(id)` and an `on-wake(id)` export. A plugin whose *display* changes without the
  buffer changing has otherwise no way to say so — org's running clock re-renders once a minute
  over a file that is already correct — and before this its segment could only advance on the
  next keystroke, which is the "works, but only after I hit something" failure the
  boot-composition rules exist to design out.

  It lives on the **events actor**, not on a scheduler of its own: a wake is a future on the
  actor's `FuturesUnordered`, selected against the bus channel, so it inherits the actor's
  budget, its quarantine and its task-abort teardown without any of them being re-implemented.
  `events` is not on the sync grammar linker, so a wake is unreachable from the keystroke path
  structurally (paramount #4). Two consequences worth naming: the actor loop now ends when the
  channel is closed **and** nothing is armed (a plugin that subscribes to nothing but arms a
  wake is a real shape); and a trapping `on-wake` quarantines exactly as a trapping `on-event`
  does, cancelling the plugin's wakes so a dead store is not re-entered once a period forever.

  The **timer is injected**, not owned: `lattice-plugin-host` keeps `tokio` a dev-dependency on
  purpose (`futures` was chosen over `tokio::sync` so the lib owns no runtime and every actor is
  spawned by its caller), so the host holds a `Sleeper` trait object the loader supplies. A
  harness that wires none leaves `wake-every` answering `0` — the same honest degradation every
  unwired context here uses, rather than a fabricated tick. Rejected: a host-owned scheduler
  thread (mirrors `EpochTicker`, but adds a thread and needs its own teardown story) and taking
  a real `tokio` dependency (breaks the stated invariant for one function).
- **Local wall-clock offset (✅ OC.4)**: `host-services.local-utc-offset-seconds() -> s32`,
  east-positive. A component's `SystemTime::now()` resolves through `wasi:clocks`, which is UTC,
  and the host builds each plugin's `WasiCtxBuilder` with **no environment inheritance** — so
  there is no `TZ` either. A guest thus has a correct *instant* and no way to render the
  wall-clock time the user reads. Org's `CLOCK: [2026-08-28 Fri 16:02]` is local by definition,
  so without this every clock line, every `%U`/`%T`/`%t` capture stamp and the agenda's "today"
  anchor is wrong by the user's offset — near midnight, wrong by a day.

  **Not capability-gated**, unlike its `walk` / `read-file` neighbours: it names no path and
  reaches no resource, and gating it would mean a plugin with no filesystem grant renders every
  timestamp in the wrong zone. **Resolved per call, never cached** — the offset changes at a DST
  boundary and when the user changes their system zone, so caching would make an editor left
  open overnight write clock lines an hour wrong for the rest of the session.

  This is the workspace's **first timezone dependency**, and the absence was deliberate:
  `lattice-agent`'s log formatter renders UTC and says so, because a log line in UTC is legible.
  A `CLOCK:` line in UTC is not — it is a wrong number in the user's file. `chrono` with
  `default-features = false, features = ["clock"]`, used for exactly
  `Local::now().offset().local_minus_utc()`. Chosen over the `time` crate because `time`'s
  `local-offset` deliberately refuses in a multi-threaded process (it would answer UTC here,
  always) unless `unsound_local_offset` is on; chrono resolves the zone through
  `iana-time-zone` — already in the graph via `wasmtime-wasi` — rather than `localtime_r`.
  Rejected: an `org.utc-offset` option, which makes the user maintain what the OS knows and
  breaks twice a year.
- Modes: `ModeRegistry::register(Arc<dyn DynMode>)`. Each plugin mode also gets an
  auto-generated `:<mode-name>` toggle ex-command, built by the shared helper
  `lattice_grammar::registry::mode_toggle_ex_command_spec` — the *same* helper boot's
  `register_mode_toggle_commands` uses for native modes — registered under
  `SourceLayer::Plugin(id)` by the loader's `drain_mode` so unload reverses it. A plugin
  mode is therefore togglable by `:<mode-name>` and shows in completion / `:describe-*` /
  `:list-*` exactly like a native mode.
- Config: `ConfigRegistry::register_with_typeid::<T>` — the plugin declares
  name/type-label/default/doc; option **values round-trip as strings** (the `OptionType`
  `parse`/`format` contract, `option_type.rs:37`), so the config WIT carries only
  `name + string-value + type_label + default-string` and the host synthesizes the
  `OptionType`. Registered into the same store core options live in, so `:set`,
  `:describe-option`, and `:customize` treat plugin options uniformly. Change events flow via
  the existing `EventPublisher` sink (`registry.rs:141`).

**The command and mode registries are runtime-mutable and read *live*.** Both are
`arc_swap::ArcSwap` handles: a plugin's grammar/command + mode contributions RCU-drain into
them at load (and reverse on unload). Every consumer reads the live handle, so a plugin's
contributions are visible everywhere native ones are — the dispatcher, `:`-command-name
completion, and every introspection command (`:describe-command`, `:apropos`,
`:list-commands`, `:describe-key`, `:describe-mode`, `:list-modes`). Completion is the one
surface that needs a nudge: it caches its candidate list, so `CommandRegistry` carries a
`generation()` counter bumped on every register/unregister, and the `:`-command generator
keys its cache on it — a plugin load/unload changes the key and the fresh command set
regenerates, with no manual flush. Net: a plugin-contributed command or mode is
indistinguishable from a native one at every user surface.

> **Standing-rule check (mode ownership):** every seam keeps *both* the contribution (spec/
> keymap) *and* the handler body with the plugin that owns it — the host stays a thin bridge
> exposing generic primitives. No `Editor::do_<plugin_x>` method, no new `Action` variant per
> plugin: the plugin contributes ids via the registry and the handler bodies live in the
> component. This is the acid test from CLAUDE.md — a new plugin adds ZERO `Editor::` methods.

### 5.13 Plugin-API introspection catalog (Facet A)

The WIT package above *is* the plugin API. Rather than re-document it by hand, a
**derived catalog** answers "what CAN a plugin do" for both audiences (in-editor users via
`:describe-plugin-api`, and plugin authors via a JSON/markdown export). The contract:

- **Single source of truth = `wit/`, parsed at build time.** A `wit-parser` build step walks the
  package into a `PluginApiCatalog { interfaces, worlds }`; each `ApiInterface` carries
  name + doc + `Vec<ApiFunction>` (name + doc). Deriving-not-authoring is the whole point: the
  catalog cannot drift from the interface it documents — a new function or `///` comment surfaces
  the moment the WIT lands. (Runtime component reflection was rejected: it needs boot-wiring and
  only sees *loaded* plugins, which is Facet B, not the static "what's possible" surface.)
- **Two fields the parser can't infer, host-authored:**
  - **direction** (`GuestExport` / `GuestImport` / `Both` / `TypesOnly`) — world-derived. Caveat:
    `use foo.{ty}` registers an import edge indistinguishable from a callable `import foo;`, so an
    interface is `GuestImport` only when imported *and non-empty of functions*; a zero-function
    type bag (`types`) is `TypesOnly`. Descriptive hint, not a contract.
  - **capability** (`Fs` / `Net` / `Proc` / `None`) — which OS capability a seam requires, which
    the WIT syntax can't express. One annotation row per interface; a test asserts the annotation
    covers *every* parsed interface, so a new seam forces a deliberate capability decision before
    it ships. Only `host-services` reaches the OS today (`Fs`, its `walk`).
- **Crate placement = a wasmtime-free leaf `lattice-plugin-api`.** `wit-parser` is a *build*
  dependency; the lib's only runtime input is `std`. So `lattice-host` deps the catalog for the
  introspection ex-commands **without** pulling the WASM runtime into the host — the
  no-per-frame-WASM invariant (§7) is preserved structurally, and the heavy `lattice-plugin-host`
  stays out of the host until Phase-8 boot-wiring. The test-only `trampoline-fixture` world is
  excluded from the catalog (it is not an API). Slice: PI.1 (see the slice plan). Facet B — the
  human-readable `Plugin(id)→name` provenance *label* shown in `:list-commands` — is PI.3+;
  the listing itself already includes plugin commands live (only the friendly plugin-name
  column is deferred, not the entries).

---

## 6. Capability & security model

- **Manifest-declared capabilities.** Each plugin ships a manifest declaring requested
  capabilities: `fs:read:<prefix>`, `fs:write:<prefix>`, `net:http:<host-allowlist>`,
  `proc:spawn`, plus editor capabilities via the existing `CapabilitySet`
  (`Mode::required_capabilities`). The runtime enforces via wasmtime's WASI configuration —
  a plugin's `Store` is built with exactly its granted `wasi:filesystem`/`wasi:http` view.
  > **Refined (PH7.2, 2026-07-01):** WASI-layer enforcement covers **filesystem only**. Each
  > `Store`'s `WasiCtx` is built from the grant's `fs:*` preopens (data dir writable + each granted
  > prefix at its declared perms); a path outside the grant is unreachable because WASI has no
  > ambient authority. `net:http` and `proc:spawn` are carried on the `CapabilityGrant` as metadata
  > but are **not** wired into the raw WASI view — a `net:http` grant does not enable raw
  > `wasi:sockets`, and `proc:spawn` does not enable subprocess spawning at the WASI layer. Both are
  > serviced (and allowlist-/tier-checked) by the capability-gated `host-services` calls (PH7.3+),
  > which read the *same* grant. Enabling raw sockets/subprocess for those grants would leak
  > authority the host-services check exists to contain. So the original "`wasi:filesystem`/
  > `wasi:http` view" is, in v1, a `wasi:filesystem` view; `net`/`proc` enforcement lives at the
  > host-services seam, not the WASI view. See the PH7.2 slice + `capability.rs`.
- **Per-plugin data dir.** `${XDG_DATA_HOME}/lattice/plugins/<plugin-id>/data/` is mounted;
  writes outside it require an explicit broader grant (§5.5.6 prerequisite 2).
- **Trust tiers.** *Bundled* plugins inherit the editor's trust (capabilities pre-granted at
  build time, no prompt). *User-installed* plugins prompt for consent on first install
  (§5.5.6). `proc:spawn` is bundled-only in v1; user plugins ship pre-built binary recipes,
  not source-build spawn paths (§5.5.6 prerequisite 4).
- **Keymap write-gate.** Already enforced: `KeymapCapability::MinorMode` permits writes only
  to `KeymapLayer::MinorMode(_)`/`Buffer`, denies `Builtin`/`MajorMode`/`User`
  (`registry.rs:132`). A plugin cannot shadow universal vim grammar.
- **Provenance is host-issued.** No public API accepts a `SourceLocation`; the host stamps
  `SourceLayer::Plugin(id)` for every plugin contribution (`source.rs:38`). A plugin cannot
  forge a builtin/user provenance.

> **UX (higher court):** capability prompts are the one place plugins touch consent UX; they
> must be legible ("git-gutter wants: read files under this repo") and never block boot —
> a denied capability degrades the plugin gracefully, it does not fail the editor.

---

## 7. Performance contract & CI gates (the Phase-7 exit)

The Phase-7 exit criterion (`design.md` §13) is: **"a WASM plugin replicates the file picker
without host changes; CI enforces overhead budgets."** The budgets (§5.5.2), each gated in
CI with a ratchet that only moves down:

| Call class | p50 | p99 |
|---|---|---|
| Typed host function call | < 100ns | < 500ns |
| Grammar-extension round-trip | < 1μs | < 5μs |
| Status / gutter segment update | < 10μs | < 50μs |
| Picker filter pass per item | < 500ns | < 2μs |
| Major-mode event handler | < 50μs | < 250μs |

Plus the cold-start budget (50 lazily-loaded plugins < 30ms; first-paint plugin work < 5ms).

> **Known gap — the picker row is currently un-gated (found 2026-08-27).**
> The ratchet enforcing it, `picker_init_round_trip_stays_within_ceiling`,
> was removed along with the `fuzzy-finder` validation plugin (`0d068f9b`)
> and never re-pointed at the surviving `tests/fixtures/picker-guest`. So
> the guest→host **picker** path is benchmarked by nothing today, while the
> other five rows still gate in `lattice-plugin-host/tests/perf_ratchet.rs`.
> Recorded here rather than in a slice plan because both plans that owned
> it are complete and archived — this fragment is what stays. Re-pointing
> the ratchet at `picker-guest` is the fix; it is small, and it is the one
> row of this table CI cannot currently defend.
The **no-per-frame-WASM rule** is absolute: the renderer never calls into a plugin on the UI
tick. Plugins compute on triggers; the renderer reads cached results (the same rule that
already governs `gutter_decorations` — the host builds the decoration snapshot off the render
path and the renderer reads per-line values).

---

## 8. Lifecycle & error handling

- **Activate / deactivate / on-event** are the guest's three lifecycle exports (`plugin`
  interface, mirrors `design.md` §9.4).
- **Crash isolation.** A trap (panic, fuel exhaustion, epoch deadline, capability violation)
  kills only the offending Store's task, fires `PluginCrashed { plugin }` on the event bus,
  and quarantines the plugin (no auto-restart in v1). The editor, actor, other plugins, LSP,
  and UI are untouched. This is the §7.5 "plugin crashes mid-render" contract applied to all
  plugin work.
- **Graceful degradation everywhere** (the four-artefact "graceful error handling" clause):
  unknown-source / init-error → echo, picker stays closed; grammar apply trap → motion is a
  no-op, logged; event handler fuel-exhaust → skipped, save/quit proceeds; capability denied
  → plugin loads with reduced function + a notification. Never a panic on the hot path, never
  a silent swallow.
- **Teardown reverses every contribution by provenance.** On unload the host RCU-snapshots
  the command / mode / picker / decoration registries and reverses this plugin's entries.
  `CommandRegistry::unregister_plugin(id)` runs **unconditionally** — it removes every
  `SourceLayer::Plugin(id)` command in one pass (grammar contributions AND the `:<mode>`
  mode-toggle ex-commands), with no per-seam "did I register a command?" gate: the provenance
  *is* the token. Modes reverse via `ModeRegistry::unregister` + their gated keymap layer;
  options / subscriptions / picker + decoration sources by their returned tokens. Every
  reversal is idempotent, so a double-unload (or unload after a partial crash) is safe.
- **Reload seam.** The host supports instance teardown + re-instantiate (Guard-`Drop`
  semantics already model mode teardown; §5.10.5). This is the hook the deferred config-as-
  WASM (`init.rs` hot-swap, §5.12.4) and the plugin manager will consume.

---

## 9. Reference validation plugin: `fuzzy-finder`

`plugins/fuzzy-finder/` — a WASM component that registers a `files` picker source through the
`picker-source` WIT interface and nothing else. It exercises: component instantiation, the
`buffer`/`host-services` resource callbacks (walk workspace via a host fs call), the owned-
snapshot projection (§4.2), the async result carrier (§4.3, `Stream` for a large tree), the
typed `PickerAcceptOutcome::OpenFile` (§4.4), and MRU-for-free (the picker's MRU pipeline
keys off `RoutingPayload` identity regardless of source origin). **Success = the native files
picker is unregistered and the WASM one is registered, and every picker test plus the
overhead benches pass with zero host changes.** If any primitive is painful to write, the WIT
needs to grow (§5.5's second principle: the API grows from real plugins).

---

## 10. Testing strategy (four-artefact discipline)

- **Unit:** WIT type round-trips (native ↔ WIT record ↔ native), capability-set enforcement,
  fuel-exhaustion trapping, cache-key invalidation.
- **Integration:** the `fuzzy-finder` end-to-end through `Editor::boot` — register, open,
  filter, accept, MRU-record — asserting parity with the native picker's tests.
- **Bench (CI-gated ratchet):** every row in §7, plus cold-start with N synthetic plugins.
  Runtime-responsiveness coverage (not just throughput) per the CLAUDE.md rule: assert plugin
  work lands on the multi-thread runtime, never the `current_thread` actor.
- **Property/fuzz:** malformed component bytes, malformed WIT payloads, adversarial fuel/epoch
  timing — the host must reject/quarantine, never crash.

---

## 11. Rejected alternatives

- **A second in-process scripting language (Lua/Scheme/Rhai).** Doubles the API surface and
  binding maintenance, splits the ecosystem into "plugin-shaped" vs "script-shaped." Rejected
  on paramount #2 (`design.md` §10). One substrate, one toolchain.
- **Per-frame WASM (renderer calls plugins on the UI tick).** Direct paramount-#1 violation;
  forbidden by §5.5.2 rule 7. Plugins emit data, the renderer reads it.
- **Copying the rope across the boundary.** Violates paramount #1 on large buffers; resource
  handles + slice callbacks instead (§4.2).
- **A generic opaque `effect` blob instead of mirroring the closed `Effect` enum.** Smuggles
  an untyped seam into the highest-risk area (§14); rejected for a typed variant (§4.4).
- **A parallel plugin registry.** Would split every surface into native-vs-plugin code paths,
  violating "everything is a buffer / one dispatch." Plugins register into the *same*
  registries via the existing `Arc<dyn>` insert seams (§5).
- **Letting the guest supply its own `SourceLocation`.** Forgeable provenance; the host stamps
  `SourceLayer::Plugin(id)` (§6).
- **A single shared `Store` for all plugins.** Couples failure domains, serializes plugin CPU,
  breaks per-plugin capability/fuel scoping (§3).

---

## 12. Deferred to later fragments (out of Phase-7-proper scope)

Each gets its own design fragment + slice plan, cross-referenced here:

- **Modes-as-components (full)** — Phase 8. The `modes` WIT is designed here (§5) but shipping
  bundled major/minor modes as components is Phase 8.
- **Bundled-plugin manager & LSP server manager** — Phase 8 (`design.md` §5.5.6). The LSP
  server manager is the lighthouse that validates the WIT is sized correctly.
- **Config-as-WASM (`init.rs`)** — post-Phase-7 (`design.md` §5.12.2–5.12.4). Consumes the
  host's reload seam (§8) and a `boot`-capability `Config` facet.
- **Live-eval (`*scratch:rust*` → `rustc` → dynamic load)** — Phase 10 (`design.md` §10).
- **ABI-freeze / versioning policy** — the WIT is unstable until ≥3 real plugins have
  exercised it (`design.md` §14 grammar-extension row; §15 Q7). SemVer only post-1.0.

---

## 13. Open questions (tracked; resolve during slices)

Mapped to `design.md` §15: **Q7** (Component-Model versioning policy) — defer to §12
ABI-freeze fragment. **Q9** (live-reload of plugin-defined modes) — reload seam exists (§8);
mode-specific reload is Phase 8. **Q15** (tree-sitter query API shape — one-shot vs cursor
iterator, range scoping, query caching) — settle in the `host-services` slice. **Q16**
(async-task host primitive name/shape — `spawn-task` vs structured-concurrency) — settle in
the runtime slice. **Q17** (AOT cache invalidation key) — resolved in §3 (four-part hash).
New: **the streaming-result WIT shape** (guest-push `result-sink` resource vs host-poll
`next-batch`) — settle when the `fuzzy-finder` `Stream` path lands (§9).

---

## 14. Cross-references

- Spec: `design.md` §3.1 (core/plugin split), §5.5 (subsystem), §5.10 (events), §5.12
  (config), §9 (WIT sketch), §10 (tiers), §11 (layout), §13 (roadmap), §14 (risks), §15 (open
  Qs).
- Exercised trait seams: `lattice-picker/src/source.rs`, `lattice-completion/src/{traits,candidate,registry}.rs`,
  `lattice-grammar/src/{registry,dispatcher,command,effect,args,source}.rs`,
  `lattice-runtime/src/events.rs`, `lattice-mode/src/{mode,contributions,services,registry}.rs`,
  `lattice-keymap/src/{trie,registry,contribution}.rs`, `lattice-config/src/{registry,option}.rs`.
- Boot composition + subsystem-install seam: `lattice-host/src/editor_boot.rs:225`
  (`Editor::boot`), `lattice-host/src/boot_context.rs` (`BootContext`),
  `lattice-mode/src/subsystem_boot.rs:51` (`SubsystemBoot` — the surface plugins install
  through).
- Slice sequencing: `docs/dev/operations/slice-plans/archive/plugin-host.md`.
