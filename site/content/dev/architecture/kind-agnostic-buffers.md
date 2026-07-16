+++
title = "Kind-agnostic buffer + mode infrastructure (H-series)"
+++

# Kind-agnostic buffer + mode infrastructure (H-series)

> **Status (2026-06-01):** H-series closed after H.2. H.1 + H.2 ✅ landed; **H.3 deferred** — see §10 below for rationale.

## 1. Vision

Today every new `BufferKind` requires hand-edits in `lattice-host`:

- `resolve_major_mode(kind, lang)` — hardcoded match over every kind to find its major mode id.
- `BufferRegistry` insertions — host-internal call sites; extension crates can't push entries.
- Major-mode activation — synchronous calls from host code adjacent to the buffer-creation site.

This wiring violates paramount goal #2 (extensibility, WASM Component Model plugin path). When a plugin defines a buffer kind, the plugin **also has to land patches in lattice-host** to make the kind reachable — exactly the cross-plugin coupling the everything-is-a-buffer principle is supposed to eliminate.

The H-series removes that coupling for the **insertion** and **mode-resolution** axes. The activation axis was originally planned for H.3 but deferred (§10) after the design pass showed it was the wrong shape pre-v1. Post-H.2 verbal goal:

> When ANY producer (in-tree code, in-tree extension crate) creates a buffer, host's `resolve_major_mode` looks up the kind in a registry-indexed table that the mode itself populated — no `match BufferKind` block in host code. WASM-plugin-driven activation (the event-driven path originally framed as H.3) lands when the plugin host actually exists.

## 2. Paramount-goal alignment

| Goal | How the H-series serves it |
|---|---|
| #1 perf | Activation hops through one event-bus dispatch (~µs). Not a hot path; happens once per buffer-open. No measurable cost. |
| #2 extensibility | **Load-bearing.** Plugin-defined buffer kinds compose: register a `Mode` declaring `target_buffer_kind = Some(MyKind)`, call the generic `insert_buffer(entry)`, publish `BufferOpened`. Host never knows the kind exists. |
| #3 grammar | Neutral — motions / operators / text objects are kind-agnostic already. |
| #4 async | Event-driven activation is naturally non-blocking — producers don't wait for activation to complete. |

## 3. The three pieces

### H.1 — Generic buffer insertion

`BufferStore` (in `lattice-mode`) gains:

```rust
pub trait BufferStore: Send + Sync {
    // existing methods stay (find_by_name, ensure_named_document,
    // handle_for, name_for)…

    /// Insert a fully-constructed `BufferEntry` into the
    /// registry. Returns the assigned `BufferId`. Extension
    /// crates use this to register kind-specific buffers
    /// (Multibuffer, future plugin-defined kinds) without
    /// host knowledge of the kind.
    fn insert_buffer(&self, entry: BufferEntry) -> BufferId;
}
```

Plus `BufferEntry` + `BufferData` move (or get re-exported) so extension crates can construct them. Most likely: `BufferEntry`, `BufferData`, `DocumentEntry`, `BufferFlags` get hoisted from `lattice-host` into a smaller crate (or `lattice-core`) so `lattice-multibuffer` / future plugin crates can `use` them.

**Open question**: where do `BufferData`'s variants live? If `BufferData::Document(DocumentEntry)` stays in host, plugin-defined kinds can't extend the enum. Options:

- (A) Hoist `BufferData` to `lattice-core`; extensions add via host's `BufferData::Custom(Box<dyn CustomBufferData>)` escape hatch.
- (B) Replace the enum with a trait-object slot: `BufferEntry.payload: Box<dyn BufferPayload>` where each kind impls the trait.
- (C) Keep the enum closed pre-v1; plugin-defined kinds come in v2 with a sealed extensibility design.

For H.1 we adopt **(C)** — closed enum, add `BufferData::Multibuffer(DocumentEntry)` for the multibuffer slot now, leave plugin-kind extensibility for v2. This is consistent with lattice's "pre-v1 = build the right shape, not the most flexible shape" stance.

### H.2 — Modes declare their `BufferKind`

`Mode` trait gains:

```rust
pub trait Mode: Send + Sync + 'static {
    type Guard: Send + 'static;
    fn id(&self) -> ModeId;
    fn kind(&self) -> ModeKind;  // Major vs Minor

    /// M.2.b.2 / H.2 (2026-05-31): for major modes, the
    /// `BufferKind` this mode is the default major for.
    /// Returns `None` for minor modes and for major modes
    /// that don't bind to a kind (e.g., language modes like
    /// `rust-mode` — they activate via `Lang` detection on
    /// `BufferKind::Document`, not via the kind directly).
    fn target_buffer_kind(&self) -> Option<BufferKind> { None }

    // existing methods stay…
}
```

`ModeRegistry` indexes registered modes by `target_buffer_kind` so the lookup is O(1):

```rust
impl ModeRegistry {
    pub fn find_major_for_kind(&self, kind: BufferKind) -> Option<ModeId> {
        self.kind_index.get(&kind).copied()
    }
}
```

Host's `crate::modes::resolve_major_mode(kind, lang)` becomes a thin shim:

```rust
pub fn resolve_major_mode(kind: BufferKind, lang: Lang) -> ModeId {
    // Document kind delegates to language detection (rust/markdown/etc.).
    if kind == BufferKind::Document {
        return major_mode_id_for_lang(lang);
    }
    // Other kinds look up their declared major mode.
    mode_registry.find_major_for_kind(kind).unwrap_or(text_mode_id())
}
```

The hardcoded `match kind { BufferKind::Help => ..., BufferKind::FileTree => ..., ... }` disappears. Each mode self-declares.

### H.3 — Event-driven major-mode activation **(deferred — see §10)**

The original H.3 design proposed a typed `BufferOpened { id, kind }` event with a single host subscriber driving activation. The 2026-06-01 design pass (§10) showed this is the right shape *for the WASM plugin host*, but premature pre-v1. In-tree extension crates (`lattice-multibuffer` today, future in-tree provider crates) reach activation through the synchronous `ModeActivator` trait introduced by the multibuffer slice plan — no event-bus indirection needed because in-tree dispatch already runs on the App thread with `&mut Editor` access.

Sketch retained below for the future slice that lands the WASM-plugin path:

```rust
// (Deferred — design preserved for the WASM plugin host slice.)
Event::BufferOpened { id: BufferId, kind: BufferKind }

event_bus.subscribe(EventFilter::kind(EventKind::BufferOpened),
    SubscriptionTarget::Sync(Arc::new(move |event| {
        if let Event::BufferOpened { id, kind } = event {
            let major_id = mode_registry.find_major_for_kind(kind)?;
            mode_registry.activate_major(/* … */, id, major_id, /* … */)?;
        }
    })));
```

## 4. Migration shape (existing producers) **(deferred with H.3)**

The original H.3 carve migrated five existing producers off direct `activate_major` calls onto event publication. Per §10's deferral, all five **stay on the direct-call path**. The `lattice-multibuffer` slice plan introduces a `ModeActivator` trait (see [`multibuffer-views.md`](multibuffer-views) §3.6) that gives extension crates the same synchronous activation surface without requiring `&lattice_host::Editor`. That seam is what H.3 would have eventually generalised; pre-v1 the in-tree shape is enough.

## 5. Rejected alternatives

### Hybrid — multibuffer crate calls `activate_major` directly

What we built mid-session (M.2.b.2 draft). Pollutes host's resolve_major_mode with a Multibuffer arm; producers (M.6 search etc.) couple to mode activation explicitly. Failing heuristic #1 (long-term fit) — the same coupling shows up for every kind-specific feature.

### Trait-object `BufferData::Custom(Box<dyn CustomBufferData>)`

Maximum extensibility but pre-v1 we don't have the design space worked out (what's the trait surface? capability gating? serialization?). Defer to v2.

### Keep host `resolve_major_mode` hardcoded match, multibuffer just adds an arm

Minimal change, but every future plugin-defined kind also needs a host patch. Fails the goal #2 extensibility test.

## 6. Slice carve

H lands in three slices, in this order:

| Slice | What | Validation |
|---|---|---|
| **H.1** | `BufferStore::insert_buffer` API + `BufferData::Multibuffer(DocumentEntry)` variant + matching `BufferEntry::kind()` arm. Existing host inserts NOT migrated yet — they keep using direct `BufferRegistry::insert(...)`. Just unlocks the trait method. | Existing tests pass. New tests: extension-crate-style insertion compiles + round-trips correctly. |
| **H.2** | `Mode::target_buffer_kind()` method + `ModeRegistry::find_major_for_kind`. Existing major modes (FileTreeMode, OilMode, HelpMode/MarkdownMode, MessagesMode, TerminalMode) override the method. Host's `resolve_major_mode` rewritten to use the registry lookup; the hardcoded match disappears. | Existing mode activation works through the new lookup path. |
| **H.3** | New `Event::BufferOpened { id, kind }`. Host subscribes once. Existing producers migrated from direct `activate_major` calls to `BufferOpened` publication. | All existing buffer-creation flows continue to activate the right major mode; tests prove the migration preserves observable behaviour. |

After H.3, **M.2.b.2** ships its `MultibufferMode` + `create_multibuffer_view(...)` and works through the same generic pipeline. Host gains zero lines of multibuffer-specific code.

## 7. Testing strategy

Each H-slice ships its own tests:

- H.1: BufferStore trait impl roundtrip; BufferData::Multibuffer variant pattern-matches as expected.
- H.2: Mode registry returns the right ModeId for each registered kind; modes without `target_buffer_kind` default to None.
- H.3: Buffer creation in the existing test fixtures publishes BufferOpened; subscriber activates the correct major mode; no observable behaviour change in existing tests.

Plus a cross-slice integration test (lands in H.3): construct a buffer of each kind via the new generic path; verify the right major mode activates.

## 8. Open questions

### Q1 — `BufferEntry` location

Initial framing assumed `BufferStore::insert_buffer` would take a `BufferEntry`, requiring the type to be reachable by extension crates. That would mean hoisting `BufferEntry` + `BufferData` from `lattice-host`, which drags `FileTreeBuffer` / `OilBuffer` / `HelpBuffer` / `TerminalBuffer` with them — or inverts deps.

**Revised decision (2026-05-31)**: the trait method takes **primitives** instead. Signature:

```rust
fn insert_document_buffer(
    &self,
    id: BufferId,
    kind: BufferKind,
    handle: Arc<dyn lattice_runtime::Document>,
    flags: BufferFlags,
    name: Option<String>,
);
```

All five argument types already live in `lattice-core` / `lattice-runtime` (reachable by `lattice-mode`). Host's `BufferStore` impl constructs the appropriate `BufferData::Document` / `BufferData::Messages` / `BufferData::Multibuffer` variant from the `kind` tag. Kinds whose payload is NOT a `Arc<dyn Document>` (FileTree, Oil, Terminal, Help) keep their host-internal insertion path — they're not extension-crate-relevant.

Net effect: no hoist needed. `lattice-multibuffer` (and future Document-shaped extension crates) calls the trait method with primitives; host's impl maps to the right variant internally.

### Q2 — `BufferData` payload variants for plugin kinds

How does a plugin add a new `BufferData` variant when the enum is closed?

**Decision (2026-05-31)**: closed enum pre-v1. Plugin-defined kinds defer to v2. The H-series unblocks in-tree extension crates (`lattice-multibuffer`); plugin-defined kinds need their own design pass when the WASM Component Model plugin host work starts.

### Q3 — `BufferOpened` event payload

Should the event carry the handle (`Arc<dyn Document>`) or just the BufferId + kind?

**Decision (2026-05-31)**: just `BufferId + BufferKind`. Subscribers look up handles via `BufferStore::handle_for(id)`. Keeping the event payload small avoids the "every subscriber gets a typed handle" antipattern.

## 9. Cross-references

- `docs/dev/architecture/multibuffer-views.md` §3.6 — the original "MultibufferMode is a major mode" design that the H-series unblocks; §3.7 (2026-06-01) introduces the `ModeActivator` trait that supersedes H.3 for in-tree extension crates.
- `feedback_mode_owns_its_buffers` (memory) — the principle the H-series enforces at the infrastructure level.
- `feedback_buffers_no_special_case` (memory) — the no-kind-branching rule the H-series makes infrastructure-enforceable.

## 10. H.3 deferral (2026-06-01)

### What changed

H.1 (`BufferStore::insert_document_buffer`) and H.2 (`Mode::target_buffer_kind` + `ModeRegistry::find_major_for_kind`) landed as designed. **H.3 (event-driven activation) is deferred** to the slice that lands the WASM Component Model plugin host.

### Why

The 2026-06-01 design pass for `multibuffer-views.md` M.2.b.2 (covering the first extension crate to consume H.1 + H.2) worked through a concrete end-to-end SearchProvider example and found:

1. **In-tree extension crates already have synchronous activation access.** `lattice-multibuffer` (and every future in-tree provider crate that ships within it) is invoked from host dispatch paths that already hold `&mut Editor`. The activation cascade (`activate_major_for_buffer_kind` → default minor → auto minors → recompute options + completion → maybe-auto-LSP) is `&mut Editor`-bound. A thin `ModeActivator` trait in `lattice-mode` exposes that surface to extension crates without requiring `&lattice_host::Editor` in the type signature (see `multibuffer-views.md` §3.7). No event-bus indirection needed.

2. **H.3's design serves a use case that doesn't exist pre-v1.** The architectural rationale for publish-and-drain activation is: "a producer that can't hold `&mut Editor` needs a way to trigger activation." That producer is a WASM Component Model plugin instance running in `wasmtime::Store`. Pre-v1 the plugin host doesn't exist; pre-v1 every producer is in-tree and holds `&mut Editor`.

3. **H.3 was the most-flexible shape, not the right one pre-v1.** Per the H-series doc's own §3.1 stance ("closed enum pre-v1 — build the right shape, not the most flexible shape"), preemptively shipping an event-driven path that no current producer needs and that the WASM-plugin design will want to redesign anyway (capability gating, fuel-limited dispatch, error propagation through the plugin boundary) trades correctness for premature generality.

4. **The full activation cascade is `&mut Editor`-bound in non-trivial ways.** Default-minor + auto-minor + recompute-options + recompute-completion-sources + maybe-auto-LSP-mode all mutate Editor state beyond `ActiveModes`. A sync subscriber inside `Arc<dyn Fn>` can't reach those without interior mutability (regressive for goal #1) or a per-tick drain (semantic deferral the slice plan didn't acknowledge). Both rough edges disappear when activation happens on the App thread with `&mut Editor` in scope.

### What stays

- H.1: `BufferStore::insert_document_buffer` — extension crates insert Document-shaped buffers without host knowing the kind.
- H.2: `Mode::target_buffer_kind` + `ModeRegistry::find_major_for_kind` — kind-to-major lookup goes through the registry, not a host-side match.
- The five existing producers stay on direct `activate_major_for_buffer_kind` calls; nothing migrates.

### What the deferred work looks like when it lands

When the WASM Component Model plugin host slice begins:

- `BufferOpened { id, kind }` typed event (using the typed-event-bus path established post-§8 Q3).
- One host subscriber that runs the activation cascade.
- The subscriber forwards into the App's per-tick drain (same shape as `drain_mode_lifecycle_events`).
- WASM plugins publish `BufferOpened` through the capability-gated bus surface; the cascade runs deferred-by-one-tick on the App thread.
- Capability gating, fuel-limited dispatch, error propagation, and trust-domain isolation get designed in from the start — not retrofitted onto a pre-v1 surface.

### Sources for this decision

- The 2026-06-01 M.2.b.2 design conversation (this file's edit log + `multibuffer-views.md` §3.7).
- Project memory `feedback_evaluate_against_paramount_goals` — H.3's value tested against goals #2 (extensibility for plugins) and #4 (async); it lands when goal #2 has a concrete claimant (plugin host) and a capability boundary to design against.
- CLAUDE.md decision heuristic #1 ("best long-term fit beats easy implementation") + the explicit pre-v1 stance ("build the right shape, not the most flexible shape").
