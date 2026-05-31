# Kind-agnostic buffer + mode infrastructure (H-series)

## 1. Vision

Today every new `BufferKind` requires hand-edits in `lattice-host`:

- `resolve_major_mode(kind, lang)` — hardcoded match over every kind to find its major mode id.
- `BufferRegistry` insertions — host-internal call sites; extension crates can't push entries.
- Major-mode activation — synchronous calls from host code adjacent to the buffer-creation site.

This wiring violates paramount goal #2 (extensibility, WASM Component Model plugin path). When a plugin defines a buffer kind, the plugin **also has to land patches in lattice-host** to make the kind reachable — exactly the cross-plugin coupling the everything-is-a-buffer principle is supposed to eliminate.

The H-series removes that coupling. The shape (verbal):

> When ANY producer (in-tree code, plugin) creates a buffer, host's role is uniform dispatch: read the kind, find the major mode for that kind via the mode registry, activate it. Host never names a specific kind.

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

### H.3 — Event-driven major-mode activation

New event:

```rust
// in lattice-protocol::event
Event::BufferOpened {
    id: BufferId,
    kind: BufferKind,
}
```

(Distinct from `DocumentOpened`, which stays as the LSP-specific event with `text` payload.)

Host subscribes once at boot:

```rust
event_bus.subscribe(
    EventFilter::kind(EventKind::BufferOpened),
    SubscriptionTarget::Sync(Arc::new(move |event| {
        if let Event::BufferOpened { id, kind } = event {
            let major_id = mode_registry.find_major_for_kind(kind)?;
            mode_registry.activate_major(/* … */, id, major_id, /* … */)?;
        }
    })),
);
```

This single subscriber covers every kind, in-tree or plugin-defined. Producers replace their direct `mode_registry.activate_major(...)` calls with `event_bus.publish(Event::BufferOpened { id, kind })`.

## 4. Migration shape (existing producers)

Producers that today create buffers + activate modes:

| Producer | Today | Post-H.3 |
|---|---|---|
| `editor_boot` (initial document) | Direct `activate_major(...)` from boot | Publishes `BufferOpened`; subscriber activates |
| `synthetic_buffers::ensure_named_document_for` | Direct `activate_major(...)` for `*lsp*` / `*messages*` etc. | Publishes `BufferOpened` |
| `dispatch::do_edit` (brand-new-file) | Direct `activate_major(...)` inside `open_fresh_into_active_slot` | Publishes `BufferOpened` |
| File-tree / Oil openers | Direct `activate_major(...)` | Publish `BufferOpened` |
| Future `MultibufferDocumentHandle` producers | (doesn't exist yet) | Publishes `BufferOpened` |
| Future plugin producers | (doesn't exist yet) | Publishes `BufferOpened` via host import |

Every producer becomes uniform: insert into registry → publish event. Activation is dispatched centrally.

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

- `docs/dev/architecture/multibuffer-views.md` §3.6 — the original "MultibufferMode is a major mode" design that the H-series unblocks.
- `feedback_mode_owns_its_buffers` (memory) — the principle the H-series enforces at the infrastructure level.
- `feedback_buffers_no_special_case` (memory) — the no-kind-branching rule the H-series makes infrastructure-enforceable.
