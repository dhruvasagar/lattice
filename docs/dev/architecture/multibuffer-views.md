# Multibuffer Views

Authoritative design for Lattice's multibuffer aggregator: a
single editor surface composed of **anchored excerpts** spliced
from N existing Documents, where edits at the surface
propagate back through the standard edit pipeline to the
underlying buffers. This is the primitive that lights up
project-wide diff, AI multi-file `openDiff`, search-as-buffer,
LSP references-as-buffer, and diagnostics-as-buffer with one
implementation rather than four.

This document is a *companion* to `design.md` (§5.1 buffer
model, §5.6 rendering, §5.9 UI components, §5.10 event system)
and to `diff-system.md` (the diff data layer that composes with
multibuffer to deliver project-wide diff and AI multi-file
flows). Multibuffer and diff are independent designs that
**meet at their consumers, not at their implementations**:
neither requires the other to land, and each is testable on
its own.

## 1. The design goal

A `MultibufferDocument` is a `Document` whose content is
composed of N anchored excerpts into other Documents:

```
+-- *project-diff* MultibufferDocument ----------------------+
| ── src/app.rs : lines 102–118 ────────────────────────────│
| (excerpt of src/app.rs, rows 102–118, edits propagate    |
| back to the source buffer)                                |
|                                                            |
| ── src/lsp/client.rs : lines 41–55 ───────────────────────│
| (excerpt of src/lsp/client.rs, edits propagate)           |
|                                                            |
| ── tests/integration.rs : lines 7–22 ─────────────────────│
| (excerpt; edits propagate)                                |
+------------------------------------------------------------+
```

Properties that fall out:

- **Renderer and grammar treat it as a Document.** Motions,
  text objects, visual mode, search, marks, jumplist, decorations
  all operate uniformly — no kind-specific branching.
  `feedback_buffers_no_special_case.md` holds.
- **Edits route to the underlying source buffers** via the
  existing edit dispatch. Undo, macros, autocmds, LSP
  `didChange`, persistence — every consumer of the edit
  pipeline observes one consistent stream of source-buffer
  edits, regardless of whether the user typed at the source
  pane or through a multibuffer view.
- **Cross-pane coherence is automatic.** If the user has
  `src/app.rs` open in pane A and a multibuffer containing
  excerpts of `src/app.rs` in pane B, an edit in either pane
  is reflected in the other on the next snapshot — because
  there is only one underlying buffer and arc-swap publishes
  consistently (§5.6.8).
- **Excerpts grow / shrink with source-buffer edits** via the
  existing anchor type (§5.1.1) — multibuffer does not
  recompute its excerpt list on every keystroke, it only
  re-translates display rows lazily.

The UX patterns this primitive enables (each a separate slice
post-M.6):

| Consumer                          | UX it lights up                                                                                           |
|-----------------------------------|-----------------------------------------------------------------------------------------------------------|
| `SearchProvider` (lands with M.6) | Project-wide search/replace as an editable buffer (`wgrep`-style). Edit a result line → file is edited.   |
| `ProjectDiffProvider`             | "Show all changed files in the repo as one scrollable diff." Per-excerpt diff state via `diff-system.md`. |
| `AIProposedEditsProvider`         | Claude / agent proposes edits to N files; all hunks shown in one multibuffer with per-hunk accept/reject. |
| `LspReferencesProvider`           | `gr` opens a multibuffer of every call site as editable excerpts, not a read-only picker.                 |
| `DiagnosticsProvider`             | `:diagnostics` opens a multibuffer of every diagnostic site as editable excerpts.                         |

Without multibuffer these flows degrade to picker → jump →
back-to-picker iteration; the modern multi-file editing
workflow (Cursor, Zed Agent, Windsurf) becomes
meaningfully worse. Lattice commits to the primitive because
AI multi-file flows are a foreseeable v1 workflow and the
primitive is reusable across at least five high-value
consumers.

## 2. Reference points

- **Zed** (`crates/multi_buffer`) — the only meaningful
  reference. Anchored `Excerpt`s, composed snapshots, edit
  propagation, expand-context affordance, provider-driven
  excerpt streams. We adopt the architecture wholesale,
  adapting names to Lattice's vocabulary
  (`MultibufferDocument`, `Excerpt`, `MultibufferProvider`).
- **Emacs `wgrep` / `iedit`** — same UX (edit grep results,
  see edits in source), different mechanism (wgrep edits the
  grep buffer's text, then on `wgrep-finish-edit` walks the
  patch and rewrites the source files). The mechanism is
  inferior: it loses the "live propagation" property, can
  drift if the source file changed since the grep ran, and
  cannot serve AI multi-file flows where partial-accept is
  needed. We commit to the live-propagation model.
- **Helix** — no equivalent. Helix's pickers are read-only
  jump-targets; "search and replace across project" is a
  command, not a buffer.
- **Vim** — no equivalent. quickfix and location lists are
  read-only buffers with jump-to-source affordances; they
  don't propagate edits.

## 3. The data model

Three additions to the data model, plus one architectural
shift (M.0).

### 3.1 The Document trait (M.0)

Today's architecture (verified 2026-05-31):

```
Caller (Editor, renderer, plugin) → DocumentHandle (handle)
                                          ↓ writes (mpsc)
                                    DocumentActor (tokio task)
                                          ↓ owns
                                    Document (struct, &mut self)
                                          ↓ reads (publish)
                                    PublishedSnapshot<DocumentSnapshot>
                                    └─ Arc-snapshot, lock-free
```

`DocumentHandle` (`lattice-runtime`) is the cheap-clone
public surface for one document. Writes round-trip through
an `UnboundedSender<ActorMsg>` to a `DocumentActor` that
owns the inner `Document` struct (`lattice-core`) and mutates
it through `&mut self` under single-writer discipline. Reads
go through `snapshot() -> Arc<DocumentSnapshot>` backed by a
`PublishedSnapshot` cell (lock-free, ~17 ns per load, ~2 ns
through the per-thread `SnapshotCache`).

The lock-free read path + actor-mediated write path is
**already the ArcSwap pattern we want.** Multibuffer's job
is not to redo this at the inner Document layer — that
layer is owned by one actor and never crosses threads. The
job is to add a sibling implementation **at the handle
layer** so dispatch / renderers / plugins can hold a
uniform reference to either a regular document or a
multibuffer composition.

#### The trait, at the handle layer

```rust
pub trait Document: Send + Sync + 'static {
	// Identity / metadata — direct snapshot reads.
	fn id(&self) -> DocumentId;
	fn path(&self) -> Option<PathBuf>;
	fn version(&self) -> u64;
	fn text_version(&self) -> u64;
	fn dirty(&self) -> bool;

	// Read snapshots — Arc-backed, lock-free.
	fn snapshot(&self) -> Arc<DocumentSnapshot>;
	fn snapshot_cache(&self) -> SnapshotCache;
	fn selections(&self) -> Arc<SelectionSet>;
	fn text(&self) -> String;

	// Writes — `Pending<T>` round-trips through the
	// implementation's internal write path. For
	// `RopeDocumentHandle` that's the actor mpsc; for
	// `MultibufferDocumentHandle` it's a fan-out across
	// source handles.
	fn apply_edit(&self, edit: Edit) -> Pending<AppliedEdit>;
	fn apply_edit_batch(&self, edits: Vec<Edit>) -> Pending<Vec<AppliedEdit>>;
	fn set_selections(&self, selections: SelectionSet) -> Pending<()>;
	fn undo(&self) -> Pending<Vec<AppliedEdit>>;
	fn redo(&self) -> Pending<Vec<AppliedEdit>>;
	fn save(&self) -> Pending<PathBuf>;
	fn save_as(&self, path: PathBuf) -> Pending<()>;

	// Grammar dispatch — multibuffer routes the
	// invocation through its row-translation table to
	// the underlying source(s).
	fn dispatch(&self, invocation: CommandInvocation, cursor: Position) -> Pending<Effect>;
}

pub struct RopeDocumentHandle { /* today's DocumentHandle */ }
impl Document for RopeDocumentHandle { /* delegates to actor */ }

pub struct MultibufferDocumentHandle { /* §3.3 */ }
impl Document for MultibufferDocumentHandle { /* fans out to source handles */ }
```

`Arc<dyn Document>` is the canonical reference type. The
Editor's active document slot becomes `Arc<dyn Document>` so
the same dispatch / motion / render code paths serve both
kinds — honoring the everything-is-a-buffer principle that
forbids per-kind branching in those paths.

#### What M.0 actually does

1. Define the `Document` trait in `lattice-runtime` (next to
   `DocumentHandle` today).
2. Rename `DocumentHandle` → `RopeDocumentHandle`. Add
   `impl Document for RopeDocumentHandle` that delegates to
   each existing method (most are already `&self` with the
   right return shape — the trait reflects today's API
   surface almost verbatim).
3. Switch the Editor's active document slot from concrete
   `DocumentHandle` to `Arc<dyn Document>`. Same for any
   buffer-registry slot that today stores a `DocumentHandle`
   for a regular document.
4. **Remove `RopeDocumentHandle::replace(...)` entirely**
   (today's `DocumentHandle::replace`). Slot replacement —
   not in-place actor swap — is the only mechanism for
   "the active document changes." `do_edit` (currently calls
   `replace_document_blocking`) is rewritten to:
   `BufferRegistry::open(path) -> BufferId` → spawn a fresh
   `RopeDocumentHandle` for the buffer → assign
   `editor.document = registry.handle_for(buffer_id)`. The
   old handle drops when no caller holds it; its actor task
   exits cleanly. One uniform path for `:edit foo`, `:edit
   bar`, regular ↔ multibuffer transitions, `:b N` switches,
   etc.

**Zero change** to the inner `Document` struct, the
`DocumentActor`, `PublishedSnapshot`, `DocumentSnapshot`, or
the mpsc-mediated write path. The trait is a thin
abstraction over the existing handle API.

#### Why no `replace`

The in-place actor swap (`DocumentHandle::replace`) was a
vim-shaped shortcut: it preserved BufferId across a content
replacement so `:edit path` re-used the active buffer's
slot in the actor map. Under everything-is-a-buffer the
shortcut becomes a quirk: `:edit foo` should create a new
buffer (vim semantic is `:edit` *replaces*; emacs `find-file`
*opens*; lattice's BufferRegistry-keyed model aligns with the
latter). With slot replacement:

- `:edit path` creates a fresh `BufferId` for the new file,
  registers a `RopeDocumentHandle` for it, swaps the slot.
  The previous buffer stays in `BufferRegistry` reachable via
  `:bn` / `:b N` / `:ls`.
- Cross-kind transitions (regular → multibuffer, multibuffer
  → regular) use the exact same mechanism. The edit-time
  dispatch never branches on kind.
- Subscribers that care about a specific buffer (LSP,
  syntax worker, diff overlay) key by `BufferId` and resolve
  through `BufferRegistry` per operation — slot replacement
  is invisible to them.
- WIT plugins hold `Resource<Buffer>` (a `BufferId`-shaped
  capability) and re-resolve through the host on each call —
  again invisible.
- The renderer's `SnapshotCache` is held per-frame and
  rebuilds from `Editor.document` each frame; slot
  replacement is naturally absorbed at the frame boundary.

No subscriber needs an explicit invalidation hook because
no subscriber holds `Arc<dyn Document>` long-term across a
slot change — they all go through the registry's stable
`BufferId` indirection.

`MultibufferDocumentHandle` arrives in M.1. It owns:

- A `Vec<Excerpt>` (§3.2),
- An `Arc<RowTranslation>` cache (§3.3) backed by
  `ArcSwap<RowTranslation>` so renderer-side translation
  lookups stay lock-free across cache rebuilds,
- `Vec<Arc<dyn Document>>` references to its source documents
  (typically `RopeDocumentHandle` instances but the trait
  bound allows multibuffer-of-multibuffer composition for
  N.1's stacked narrow case),
- Its own `PublishedSnapshot<MultibufferSnapshot>` for
  composed reads (so `multibuffer.snapshot()` returns the
  same `Arc<DocumentSnapshot>` shape the renderer already
  consumes — buffer + selections + version + path + dirty —
  with `buffer` containing the composed rope cache).

#### Rejected alternatives

- **Trait at the inner `Document` struct layer with
  `&self` + `ArcSwap<Rope>` + `ArcSwap<SelectionSet>`
  interior mutability** (the architecture sketched in the
  pre-2026-05-31 draft of this section). This proposal
  duplicated infrastructure that `DocumentActor` +
  `PublishedSnapshot` already provide: the actor already
  serialises writes; the `PublishedSnapshot` already
  publishes lock-free Arc snapshots to readers. Adding a
  second ArcSwap layer inside the actor's owned struct
  would add per-write Arc-clone allocations for no
  paramount-goal benefit, and would force replacing the
  actor model to take advantage of `Arc<dyn Document>`
  direct sharing across threads — a substantially larger
  architectural change that the paramount goals don't
  demand (the actor model **is** goal #4's "multi-threaded
  by construction" principle expressed in code, with
  natural send-order edit-ordering guarantees lock-based
  sharing would have to re-derive).
- **Trait at the snapshot layer only** (`DocumentReadable`
  for read-side composition; no write-side trait). Edit
  dispatch would have to branch on whether the active
  buffer is a regular document or a multibuffer to know
  whether to route to one actor or fan out to N actors —
  violating the "buffers must not have kind-specific
  logic" rule that everything-is-a-buffer rests on.
- **Replacing the actor model with `Arc<dyn Document>` +
  internal `Mutex`.** Possible, but trades one expression
  of paramount goal #4 for another that's slightly worse
  on edit-ordering guarantees and dismantles a working
  abstraction without a paramount-goal-justified reason
  to. The handle-layer trait gets us the same WIT-plugin
  resource shape (`Arc<dyn Document>`) without touching
  the actor.

#### Performance

Trait dispatch through `Arc<dyn Document>` adds one vtable
indirection per call — measured at ~1–3 ns on x86-64.
Negligible against:
- The actor mpsc round-trip on writes (~few µs).
- The per-frame snapshot Arc-load (~17 ns, ~2 ns through
  the per-thread cache).
- The one-frame ceiling (8.3 ms at 120 Hz).

The `PublishedSnapshot` machinery and the `SnapshotCache`
optimisation (~17 ns → ~2 ns for thread-local repeat reads)
both compose with the trait — `Arc<dyn Document>` callers
keep both fast paths.

### 3.2 `Excerpt`

```rust
pub struct Excerpt {
	pub id: ExcerptId,
	pub source: BufferId,
	pub start: Anchor,        // character-precise (line + col)
	pub end: Anchor,
	pub header: ExcerptHeader,
	/// Rendering mode at partial-line edges (where the excerpt
	/// starts/ends mid-line):
	/// - `LineSnapped`: render the full source row at the edge;
	///   reject edits outside `[start.col, end.col]` on the
	///   first/last row. M.1 default. Good for diff / search /
	///   diagnostics consumers where surrounding-row context is
	///   useful and the col range is fuzzy.
	/// - `Strict`: render only `[start.col, end.col]` on the
	///   first/last row — the partial line becomes its own
	///   display row of width `end.col - start.col`. Used by
	///   N.1 narrow-to-region for Emacs-style narrow semantics.
	pub edges: ExcerptEdgeMode,
}

pub enum ExcerptEdgeMode {
	LineSnapped,
	Strict,
}

pub struct ExcerptHeader {
	pub title: String,    // e.g., "src/app.rs : 102–118"
	pub style: ExcerptHeaderStyle,
}
```

`Anchor` is the existing position-history primitive
(§5.1.1) — `(line: u32, col: u32)` plus generation tracking.
It already moves with source-buffer edits without the
multibuffer doing any work. When the user inserts a line at
source row 50 and the excerpt covers source rows 102–118, the
anchors slide to 103–119 automatically. When the user inserts
3 chars at column 5 of the excerpt's start row, `start.col`
slides from `c` to `c + 3`.

**Character-precise from M.1.** The data model is anchor-based
end-to-end so narrow-to-region (N.1) drops in without
revisiting the excerpt structure. The `edges: ExcerptEdgeMode`
field is the only narrow-vs-non-narrow rendering difference —
M.1 ships both modes (default `LineSnapped`); N.1 just sets
`Strict` on the excerpts it creates. The row-translation cache
(§3.3) handles partial-line rows by emitting a `RowEntry`
variant that carries the col range; the renderer slices the
source row to that range when materialising the display cells.

When the user edits *inside* an excerpt's range, the anchors
again slide automatically — no excerpt mutation needed. The
multibuffer's row-translation cache invalidates on the source
buffer's `revision` bump, but the excerpt structure itself is
durable across edits.

### 3.3 `MultibufferDocument`

```rust
pub struct MultibufferDocument {
	pub id: BufferId,
	pub excerpts: Vec<Excerpt>,        // sorted by source / position
	pub revision: Revision,            // bumped on excerpt mutation
	pub row_translation: ArcSwap<RowTranslation>,
	pub provider: Option<Arc<dyn MultibufferProvider>>,
	pub source_subs: Vec<EventSubscription>,
}

pub struct RowTranslation {
	/// Per multibuffer display row: which excerpt, source row,
	/// or virtual row (header / separator).
	pub entries: Vec<RowEntry>,
}

pub enum RowEntry {
	/// Full source row (the common case).
	Excerpt {
		excerpt_id: ExcerptId,
		source_row: u32,
	},
	/// Partial source row — only `col_range` is visible /
	/// editable. Emitted by `ExcerptEdgeMode::Strict` for
	/// narrow-to-region's first / last rows.
	PartialExcerpt {
		excerpt_id: ExcerptId,
		source_row: u32,
		col_range: std::ops::Range<u32>,
	},
	Header(ExcerptId),
	Separator,
}
```

The translation table is the bridge between the renderer
(asking "what's at display row R?") and the source documents
(holding the actual content). Lookup is O(log n) over
`entries`; rebuild is O(N) where N is the total source row
count across all excerpts — done off-thread and published via
arc-swap. The renderer treats `PartialExcerpt` identically to
`Excerpt` for highlighting + decoration; it just slices the
source row's cells to `col_range` before emitting glyphs.

### 3.4 `MultibufferProvider`

```rust
pub trait MultibufferProvider: Send + Sync {
	fn id(&self) -> ProviderId;
	fn initial_excerpts(&self) -> Vec<Excerpt>;
	/// Subscribe to provider-specific events; emit excerpt
	/// mutations through the provided sink.
	fn run(self: Arc<Self>, sink: ExcerptSink) -> JoinHandle<()>;
}

pub enum ExcerptMutation {
	Add(Excerpt),
	Remove(ExcerptId),
	UpdateRange { id: ExcerptId, start: Anchor, end: Anchor },
	UpdateHeader { id: ExcerptId, header: ExcerptHeader },
	Replace(Vec<Excerpt>),   // bulk reset
}
```

Providers run as tokio tasks owned by the
`MultibufferSubsystem` (§3.5). The provider's job is to
translate its source data (grep matches, git diff hunks, LSP
references, agent-proposed edits) into a stream of excerpt
mutations. The multibuffer subsystem applies those mutations,
recomputes the row translation off-thread, and publishes the
new translation via arc-swap.

Providers do **not** own multibuffer state — they only emit
mutations. The subsystem owns the `MultibufferDocument` and
its lifecycle.

### 3.5 `MultibufferSubsystem` — the owner

A `MultibufferSubsystem` lives on `lattice-host` next to
`DiffSubsystem` and the existing supervisors. It owns:

- The `HashMap<BufferId, MultibufferDocument>` keyed by the
  multibuffer's BufferId (multibuffers are real entries in
  `BufferRegistry`).
- The provider task for each multibuffer (one tokio task per
  provider).
- Subscriptions to source-buffer edit events for invalidating
  the row-translation cache.
- The translation-rebuild task pool (`spawn_blocking` for
  large multibuffers).

Per saved feedback *"Modes own their buffers, App is a host"*
the subsystem follows the same pattern. App does not gain
`ensure_multibuffer_for` or `drain_multibuffer_events` shims.

### 3.6 Crate layout and major-mode ownership (M.2.b, 2026-05-31)

Decided 2026-05-31 (revising §3.3 / §3.5):

**Dedicated crate.** All multibuffer concerns live in a new
`lattice-multibuffer` crate. Host has zero multibuffer-
specific code beyond a one-line `lattice_multibuffer::register
(mode_registry, grammar)` call at boot. Reasons:

- M.1 carried multibuffer data types in `lattice-runtime`,
  which conflated "the actor + handle + Document trait
  substrate" with "one specific kind of document." Pre-v1 is
  the window to fix that conflation.
- Plugins that want multibuffer functionality can depend on
  `lattice-multibuffer` without pulling in the full
  `lattice-runtime` actor machinery.
- The crate boundary makes the design self-documenting —
  everything multibuffer is in one tree; everything else
  doesn't know multibuffer exists.

**Dependency edges** (one-way; no cycles):
`lattice-multibuffer` →
`{lattice-runtime, lattice-mode, lattice-cells,
lattice-grammar, lattice-core}`.
`lattice-host` → `lattice-multibuffer` (boot wiring only).

**`BufferKind::Multibuffer`.** New variant in
`lattice-core::BufferKind`. Multibuffers are first-class
buffer kinds, not Documents-in-disguise. The kind tag
informs:

- `:ls` / `:bn` / `:bp` / `:b N` (already kind-uniform; the
  variant adds itself).
- Major-mode lookup (see below).
- Status-line / picker display (`[mb]` tag).

The everything-is-a-buffer principle stays intact:
dispatch / motion / render still don't branch on `BufferKind`
for universal operations. The kind tag is metadata; behavior
that's actually kind-specific dispatches through the major
mode active on the buffer.

**`MultibufferMode` is the major mode for
`BufferKind::Multibuffer`.** Activated when a buffer of that
kind registers in `BufferRegistry`; deactivated when the
buffer drops. The major mode is the gateway to every kind-
specific concern:

```rust
// in lattice-multibuffer::mode
pub struct MultibufferMode;

pub struct MultibufferModeContext {
    handle: Arc<MultibufferDocumentHandle>,
}

impl MajorMode for MultibufferMode {
    type Context = MultibufferModeContext;

    fn id() -> ModeId { /* "multibuffer" */ }
    fn kind() -> BufferKind { BufferKind::Multibuffer }

    fn on_activate(&self, ctx: &mut ActivationContext<Self>) {
        // Register the header provider against the buffer's
        // virtual_row_providers slot — kind-specific setup
        // the mode owns instead of host.
        let provider = MultibufferHeaderProvider::new(
            ctx.context().handle.clone()
        );
        ctx.host_services().register_virtual_row_provider(
            ctx.buffer_id(),
            Arc::new(provider),
        );
    }

    fn on_deactivate(&self, ctx: &mut ActivationContext<Self>) {
        // Symmetric tear-down.
    }
}
```

The mode owns:
- The typed per-buffer context (`MultibufferModeContext`
  carrying `Arc<MultibufferDocumentHandle>`).
- The keymap that binds excerpt motions (`]e` / `[e` / `]E` /
  `[E`) to grammar motion ids **and `<CR>`
  jump-to-source** (see below).
- The lifecycle wiring for header / fold providers.
- Future kind-specific behaviour (M.5 expand-context,
  M.7 / M.8 fold providers).

**`<CR>` jump-to-source is generic multibuffer, not per-provider.**
`<CR>` binds to `action:multibuffer-jump-to-source`, and
`MultibufferMode::on_activate` registers the handler on the per-buffer
`ActionHandlerRegistry`: it translates the composed cursor position to
its source `(BufferId, Position)` via the view handle
(`translate_composed_to_source` + `source_path`) and emits
`RecordJump` + `OpenBufferAt`. Because this lives in the shared major
mode, **every** multibuffer kind — project search, `*problems*`,
narrow, future references / diff / diagnostics views — gets `<CR>`
jump plus the excerpt-jump motions (`]e` / `[e` next/prev excerpt,
`]E` / `[E` next/prev file boundary) for free, with no per-provider
wiring. The handler was originally implemented inside the search
provider (`providers/search.rs`) and was hoisted out into the mode
(the provider shrank ~115 lines); a provider-minor may still bind its
own `<CR>` to shadow the generic handler via minor-mode keymap
priority, but none needs to for basic jump-to-source. This is the
mode-owns-its-surface rule applied to a capability shared across all
providers: the binding *and* the body belong to the mode that owns the
buffer kind.

**Motions as grammar operations.** `]e` / `[e` / `]E` / `[E`
register as `lattice-grammar` motions. Operators (`d` / `c`
/ `y`) compose with them; counts (`3]e`) compose with them;
visual mode (`v]e`) composes with them — same surface as
built-in motions, just bound to multibuffer-mode's keymap so
they activate only when that mode is active.

Motion handlers reach the typed handle via the major mode's
context:

```rust
// in lattice-multibuffer::motions
pub fn next_excerpt_start(
    cursor: Position,
    ctx: &MultibufferModeContext,
) -> Option<Position> {
    let excerpts = ctx.handle.excerpts();
    // Walk excerpts in source order; find first whose
    // first-composed-row > cursor.row; return that row.
}
```

Grammar dispatch passes the active mode's context to the
motion handler — no downcasting, no `Any`, no kind-
branching at the universal layer.

**Why this is right.** Mode ownership is the existing
lattice precedent for "everything kind-specific lives in the
mode" (see `feedback_mode_owns_its_buffers`). Host pollution
is minimized to a single registration call. Future kind-
specific features (M.5 / M.7 / M.8) compose into the mode
without touching host at all. Plugin-defined buffer kinds
post-v1 follow the same pattern: new crate + new major mode
+ new keymap + one registration line.

**Rejected alternatives** (recorded for future re-review):

- *Sidecar `MultibufferRegistry` keyed by BufferId in
  lattice-host.* Mirrors `DiffSubsystem`. Works but
  duplicates storage (multibuffer handles in `BufferRegistry`
  AND in the sidecar) and puts kind-specific surface in
  host. Major-mode ownership achieves the same typed-access
  goal without the storage duplication or host pollution.
  **Note (2026-06-01):** §3.7 introduces a `MultibufferRegistry`
  living **in `lattice-multibuffer` itself** (not host) as a
  typed lookup service. That's a different shape from this
  rejected alternative: storage isn't duplicated (the registry
  holds the typed `Arc<MultibufferDocumentHandle>` for downcast-
  free provider access; `BufferRegistry` holds the upcast
  `Arc<dyn Document>` for kind-agnostic dispatch — the same
  underlying handle, two access paths for two access patterns).
  Host stays oblivious.
- *`Document` trait gains `excerpts()` accessor (default
  `None`).* Pollutes the universal trait with one kind's
  surface. Every future Document impl carries an accessor it
  doesn't use.
- *`Any`-based downcasting at the `Arc<dyn Document>`
  boundary.* Functional but reaches for Rust's escape hatch
  instead of designing properly. Opens the door for every
  future feature to do the same.
- *`MultibufferMode` as a minor mode instead of major.*
  Diff-mode and similar mode-attaches-to-state minor modes
  are right for "this buffer has X happening to it." But
  multibuffer ISN'T a state attached to a regular buffer —
  it IS the buffer's identity. Major mode captures that
  correctly.

### 3.7 M.2.b.2 — the in-tree extension-crate seam (2026-06-01)

The H-series ([`kind-agnostic-buffers.md`](kind-agnostic-buffers.md))
opened H.3 as a planned event-driven activation path. The
2026-06-01 design pass for M.2.b.2 worked through a concrete
end-to-end `SearchProvider` example and chose a different shape:
**deferred H.3** + introduced four small abstractions that give
`lattice-multibuffer` (and every future in-tree provider crate
shipped within it) everything it needs without the event-bus
indirection. The event-bus path remains the right design for
WASM plugins; it lands when that plugin host exists.

The abstractions, in dependency order:

#### `ModeActivator` (in `lattice-mode`)

```rust
pub trait ModeActivator {
    fn activate_major_for_kind(&mut self, id: BufferId, kind: BufferKind);
    fn activate_minor_by_id(&mut self, id: BufferId, mode: ModeId);
    fn services(&self) -> Arc<ServiceRegistry>;
    /// Create-or-find a synthetic named `Document` + activate `major`
    /// by id (runs `on_activate`). The `&mut`-backed, mode-owned buffer
    /// **creation** seam — a provider provisions its own synthetic
    /// buffer here (creation can't live on the `&self` `BufferStore`,
    /// which cannot activate a mode). Idempotent.
    fn ensure_named_document(&mut self, name: &str, major: ModeId, flags: BufferFlags) -> BufferId;
}
```

`impl ModeActivator for Editor` is thin wrappers — the existing
`Editor::activate_major_for_buffer_kind` /
`Editor::activate_minor_by_id` already run the full cascade
(major → default minor → auto minors → recompute options +
completion → maybe-auto-LSP), and `ensure_named_document`
forwards to `Editor::ensure_named_synthetic_document`.
`services()` returns a cloned `Arc<ServiceRegistry>` so
extension-crate code can pull service handles without fighting the
borrow checker against the `&mut` activator borrow.

**Why a trait, not `&mut Editor`:** `lattice-multibuffer` can't
depend on `lattice-host` (host already depends on
`lattice-multibuffer`). The trait lives in `lattice-mode` (both
crates depend on it). The seam is also what the WASM plugin
host will eventually need to implement against a synthetic
activator that publishes events for off-thread plugin trigger;
the trait surface stays the same.

**Signal routing.** `activate_major_for_kind` returns `()`.
`Editor`'s impl drains internal `RendererSignal`s into the
existing per-Editor pending-signals queue (already shipped for
the M-async cascade). Extension-crate code never touches
`RendererSignal`. Keeps `lattice-mode` free of a host-layer
type.

#### `MultibufferRegistry` (in `lattice-multibuffer`)

```rust
pub trait MultibufferRegistry: Send + Sync {
    fn handle(&self, view: BufferId) -> Option<Arc<MultibufferDocumentHandle>>;
}
pub type MultibufferRegistryHandle = Arc<dyn MultibufferRegistry>;

pub struct InMemoryMultibufferRegistry { /* RwLock<HashMap<BufferId, Arc<MultibufferDocumentHandle>>> */ }
```

Registered in `ServiceRegistry` at boot.
`create_multibuffer_view` writes; providers + multibuffer-
internal code read by `BufferId`. Returns the **typed**
`Arc<MultibufferDocumentHandle>` so providers can call typed
methods (`append_excerpts`, `replace_excerpts`,
`refresh_for_source`) without downcasting from
`Arc<dyn Document>`.

**Cleanup.** `register_multibuffer_modes(&mut registry, &events)`
subscribes a typed `DocumentClosed` handler at boot that removes
the entry when its view buffer closes. Self-contained; no
external bookkeeping.

**Why typed-handle registry instead of `Document: Any` +
downcast:** keeps `Document` clean; matches the existing
`TerminalStoreHandle` precedent; isolates the multibuffer-
specific lookup to its own crate.

**Why not in host (the §3.6 rejected alternative):** the §3.6
rejection was about storage *duplication* — multibuffer handles
in `BufferRegistry` AND in a sidecar in `lattice-host`. The
M.2.b.2 registry lives in `lattice-multibuffer` and stores the
same underlying handle that `BufferRegistry` upcasts to
`Arc<dyn Document>`. Two access paths for two access patterns,
zero kind-specific surface in host.

#### `create_multibuffer_view` (in `lattice-multibuffer`)

```rust
pub fn create_multibuffer_view(
    activator: &mut dyn ModeActivator,
    sources: Vec<BufferId>,
    excerpts: Vec<Excerpt>,
    name: Option<String>,
    flags: BufferFlags,
) -> BufferId
```

The atomic "make me a multibuffer view" call. Internally:

1. Allocate `BufferId`.
2. Build `MultibufferDocumentHandle` from `sources` + `excerpts`
   (may be empty — providers commonly create empty and stream
   excerpts in asynchronously; see "async-provider pattern"
   below).
3. Insert the typed handle into `MultibufferRegistry` (pulled
   from `activator.services()`).
4. Insert into the buffer registry via
   `BufferStore::insert_document_buffer(id, BufferKind::Multibuffer, handle.clone() as Arc<dyn Document>, flags, name)` (H.1).
5. `activator.activate_major_for_kind(id, BufferKind::Multibuffer)` — H.2's registry lookup finds `MultibufferMode`; the activation cascade runs through `Editor`'s impl.
6. Return `id`.

Failures (missing `BufferStore` / `MultibufferRegistry` service,
activation cascade failure) log + skip + return early via the
existing `ModeEvent::ModeActivationFailed` path. No panics. No
`Result` return type — matching `activate_major_for_buffer_kind`'s
existing signature. Provider can verify post-create via
`activator.services().get::<MultibufferRegistryHandle>()
.and_then(|r| r.handle(id))` returning `Some`.

#### `MultibufferMode` (in `lattice-multibuffer`)

```rust
pub struct MultibufferMode;
impl Mode for MultibufferMode {
    type Guard = ();
    fn id(&self) -> ModeId { Self::mode_id() }                       // "multibuffer-mode"
    fn kind(&self) -> ModeKind { ModeKind::Major }
    fn target_buffer_kind(&self) -> Option<BufferKind> {
        Some(BufferKind::Multibuffer)                                // consumed by H.2
    }
    fn options(&self) -> OptionOverrideSet {
        overrides! { ReadOnly = true, NoFile = true }                // M.3 flips ReadOnly conditional on edit-propagation policy
    }
    fn keymap(&self) -> Keymap { excerpt_motion_keymap() }           // M.2.b.3 fills in ]e/[e/]E/[E
    fn on_activate(&self, _ctx) -> LifecycleFuture<'_, ()> { Box::pin(async { Ok(()) }) }
}
```

Generic. Knows nothing about *why* excerpts exist. Provider-
minors layer provider-specific behaviour on top.

### Provider model

A multibuffer **provider** is a unit of code (a submodule of
`lattice-multibuffer/src/providers/`) that owns one
multibuffer-backed user surface (project-search, lsp-
references, project-diff, ai-edits, diagnostics). Each provider
ships:

- A **public trigger function** (`project_search(activator,
  query, options) -> BufferId`) that the host's ex-command
  dispatcher calls.
- A **provider-minor mode** (`ProjectSearchMultibufferMode`)
  carrying the provider-specific keymap, lifecycle
  subscriptions, and Guard cleanup.
- A **provider service trait + impl**
  (`ProjectSearchService` + `ProjectSearchServiceHandle`)
  holding per-view state keyed by the multibuffer's
  `BufferId`. Registered in `ServiceRegistry` at boot. The
  service trait abstracts storage so the impl can use whatever
  fits (`RwLock<HashMap>`, dashmap, ArcSwap of an Arc map,
  etc.).
- **Typed events** (`ProjectSearchBatchReady`,
  `ProjectSearchCompleted`, `ProjectSearchRefreshed`,
  optionally `ProjectSearchProgressUpdated`) declared via
  `lattice_protocol::register_event!`. The minor subscribes;
  the scan task publishes.
- A **`register_<provider>(&mut registry, &mut services, …)`**
  helper called from boot wiring.

**Crate layout** (locked 2026-06-01):

```
crates/lattice-multibuffer/
├── Cargo.toml
└── src/
    ├── lib.rs                   # re-exports
    ├── document.rs              # MultibufferDocumentHandle (M.1.a + update API)
    ├── mode.rs                  # MultibufferMode + register_multibuffer_modes
    ├── registry.rs              # MultibufferRegistry trait + InMem impl + DocumentClosed subscriber
    ├── view.rs                  # create_multibuffer_view
    ├── motions.rs               # ]e/[e/]E/[E (M.2.b.3)
    ├── header.rs                # MultibufferHeaderProvider (M.2.a, landed)
    └── providers/
        ├── mod.rs
        ├── search.rs            # M.6
        ├── lsp_references.rs    # later
        ├── diff.rs              # later
        ├── diagnostics.rs       # later
        └── ai_edits.rs          # later
```

Each provider gated behind a cargo feature so opt-out builds
stay clean:

```toml
[features]
default = ["search", "lsp-references", "diff", "diagnostics", "ai-edits"]
search = ["dep:ignore", "dep:grep-matcher", "dep:aho-corasick"]
lsp-references = ["dep:lattice-lsp"]
diff = ["dep:lattice-diff"]            # diff subsystem extraction precedes this provider
diagnostics = ["dep:lattice-diagnostics"]
ai-edits = []
```

**Dep-direction audit**: `lattice-host` already depends on
`lattice-multibuffer` (per recent M.2.b.0 / M.2.b.1). So
`lattice-multibuffer` MUST NOT depend on `lattice-host`. The
`diff` provider's data source (`lattice-host::diff` today)
gets extracted to `lattice-diff` first; same for diagnostics.
`lattice-lsp` doesn't depend on `lattice-multibuffer`, so the
`lsp-references` provider can depend on `lattice-lsp` cleanly.

### Async provider pattern

Providers that compute their excerpts via I/O (search, diff,
LSP, diagnostics) run the computation in a `tokio` task —
**never on the UI thread**, per paramount goal #1 (sub-frame
input latency: UI thread does no I/O, no parsing, no shaping).

The pattern:

1. Trigger function (sync, returns fast):
   - Calls `create_multibuffer_view` with **empty `sources`
     and `excerpts`** — the view opens immediately, showing
     the "Searching..." placeholder in its headerline (see
     "Status surface" below).
   - Registers initial provider state in the provider service.
   - Activates the provider-minor.
   - Spawns the compute task (via the provider service's
     `spawn_scan` / equivalent, which retains the `JoinHandle`
     for cancellation on refresh / view close).
   - Returns the `BufferId`.
2. Compute task (async, on tokio worker threads):
   - Walks its source data.
   - Accumulates into batches (size + interval bounded — e.g.,
     50 files OR 16ms, whichever first).
   - Publishes typed events (`ProjectSearchBatchReady { view,
     files }`) on each batch boundary.
   - Publishes a `ProjectSearchCompleted { view, total }`
     event when done.
3. Provider-minor's `on_activate`:
   - Pulls the typed view handle via
     `ctx.service::<MultibufferRegistryHandle>().handle(ctx.buffer_id())`.
   - Subscribes to provider events via `ctx.events()`.
   - Spawns lightweight forwarder tasks that filter events to
     **this** view's `BufferId`, then call
     `view.append_excerpts(batch.to_excerpts())`.
   - Stashes subscription IDs + forwarder `JoinHandle`s in the
     `Guard`. `Drop` aborts the forwarders + lets the bus
     prune the channels lazily.

This is the canonical shape for **any** async multibuffer
provider. Sync providers (e.g., `lsp_references` triggered
after the LSP server's response is in hand) skip the spawn
step and pass `sources` + `excerpts` to
`create_multibuffer_view` directly.

### Status surface — the headerline convention

**Cross-cutting convention** (applies to every multibuffer
provider, and to any future async-populated buffer mechanism):
async operation status is surfaced through the multibuffer's
**headerline** — the view-header virtual row above the first
excerpt, distinct from per-excerpt headers.

Concrete progression:

- **Project-search:** `"Searching... 42 hits, 1.2k files
  scanned"` → `"Search complete: 87 hits in 2.3k files"`
- **LSP-references:** `"Fetching references..."` → `"12
  references across 4 files"`
- **Project-diff:** `"Computing diff..."` → `"47 changes
  across 9 files (working tree vs HEAD)"`
- **Diagnostics:** `"Loading diagnostics..."` → `"23 errors,
  14 warnings, 5 hints"`

**Why:** async operations need progress + completion signals
visible inline with the data the user is waiting on. Status
lines / notification badges fragment attention. The headerline
is already part of the multibuffer's render surface (M.2.a
`MultibufferHeaderProvider`); extending it to carry view-level
status keeps the affordance uniform across providers.

**API surface** (lands with M.4's live-update infrastructure,
sketched for M.2.b.2 / M.6 awareness):

```rust
impl MultibufferDocumentHandle {
    /// Set the view-level header text. Renders above the first
    /// excerpt. Provider-driven; typical use is progress +
    /// completion status. Publishes `MultibufferHeaderlineChanged`.
    pub fn set_headerline(&self, status: HeaderlineStatus);
}

pub enum HeaderlineStatus {
    Idle,                                    // no headerline rendered
    InProgress { label: String, count: Option<usize> },
    Complete { summary: String },
    Failed { reason: String },
}
```

#### Two streams — provider + view-system status

The headerline surfaces **two distinct status streams** that
compose on the same view-header row (added 2026-06-03 from
user-testing observation post-M.11):

- **Provider status** — what the user explicitly invoked (the
  `set_headerline` API above). Search progress, LSP-references
  fetching, AI-proposed-edits computing. Steady state once the
  async operation completes.
- **View-system status** — transient UX-relevant view state the
  user otherwise has no visibility into:
  - `Building cells…` during the cells-worker's first matrix
    build after activation. M.11's compose-heavy multibuffer
    makes this a perceptible one-time cost — user observed a
    short delay before syntax styling appears on the first
    activation; the marker explains it.
  - `N edits pending source sync` while the M.11 source
    forwarder has unflushed edits queued for source actors.
  - `Sources stale — refresh?` when an external pane edited a
    source after the user started typing into the multibuffer
    (M.4.x merge-policy hook; placeholder marker in v1).
  - Multibuffer dirty indicator — distinct from regular `[+]`
    since multibuffer save is multi-source (M.11.b).

The two streams are independently produced and rendered side-
by-side rather than concatenated into one string — providers
contribute to the right segment, view-system status occupies
the left. Future streams (LSP request count, diagnostics
indicator) slot in as additional structured segments without
collision on a string-format protocol.

Heuristic-mapping: structured-segments-over-concatenation
protects paramount #2 (each contribution point is independent
and pluggable) and heuristic #1 (long-term fit — additional
streams compose cleanly).

#### Renderer adoption

M.6.5 lands the view-header virtual-row publisher reading both
the headerline cell AND a new `ViewSystemStatus` cell on
`MultibufferInner`. The cell is populated by:

- M.11's source-forwarder publishes its queue depth to a
  shared cell.
- The cells-worker emits `WorkerDecision` (`CacheHit /
  Recomputed / RecomputedIncremental / Clear`); a subscriber
  on `MultibufferInner` maps `Recomputed` → `Building cells…`,
  `CacheHit` after Recomputed → clears the marker.
- M.4 forwarder publishes "external source edits while local
  edits pending" → `Sources stale` marker.

TUI + GPUI updated in lockstep per `feedback_tui_gpui_parity`.
Renderer styling uses existing host_theme conventions; progress
glyph during `InProgress` follows `feedback_icon_palette`
(nerd-font + BMP fallback).

**Convention applies beyond multibuffer.** Any future
async-populated buffer kind (REPL streaming output, log tails,
build output, etc.) surfaces operation status through its own
headerline / view-header equivalent. Keeps the affordance
discoverable and uniform across the editor.

### Worked example — SearchProvider end-to-end

The composition of all five abstractions, traced for
`:search "TODO"`:

```text
User → :search "TODO" (host ex-command dispatch, &mut Editor)
      → lattice_multibuffer::providers::search::project_search(
            &mut editor, "TODO".to_string(), SearchOptions::default()
        )
          → search_svc = editor.services().get::<ProjectSearchServiceHandle>()
          → view_id = create_multibuffer_view(
                editor, Vec::new(), Vec::new(),
                Some("*search:TODO*"), BufferFlags::default()
            )
            → registers handle in MultibufferRegistry
            → BufferStore::insert_document_buffer (H.1)
            → editor.activate_major_for_kind(id, Multibuffer)
              → resolve_major_mode finds multibuffer-mode (H.2)
              → cascade runs (ReadOnly + NoFile contributions land)
          → search_svc.set_state(view_id, ProjectSearchState::scanning(...))
          → view.set_headerline(InProgress { label: "Searching", count: Some(0) })
          → editor.activate_minor_by_id(view_id, ProjectSearchMultibufferMode::mode_id())
            → on_activate(ctx) subscribes to ProjectSearchBatchReady /
              ProjectSearchCompleted / DocumentChanged-on-sources
          → search_svc.spawn_scan(view_id, "TODO", opts)
            → tokio::spawn { walker.next() → matcher.scan_file() → ... }
          → returns view_id

[scan task, tokio worker thread]
  for each (file_batch, batch_count):
      events.publish_typed(ProjectSearchBatchReady { view: view_id, files: batch })
  events.publish_typed(ProjectSearchCompleted { view: view_id, total: N })

[provider-minor forwarder task, tokio]
  while batch = rx.recv().await:
      if batch.view != view_id: continue
      view = mb_registry.handle(view_id)
      view.append_excerpts(batch.to_excerpts())
      view.set_headerline(InProgress { label: "Searching", count: Some(view.excerpt_count()) })

[completion forwarder task]
  let evt = completion_rx.recv().await
  if evt.view == view_id:
      view.set_headerline(Complete { summary: format!("{} hits", evt.total) })

[user closes view]
  :bd → DocumentClosed published
       → MultibufferRegistry cleanup subscriber removes entry
       → ProjectSearchService.clear(view_id) (own subscriber)
       → ModeRegistry deactivates minor → Guard drops → forwarders abort,
         subscriptions go silent (closed-tx pruned lazily on next publish)
```

Every transition uses an existing or M.2.b.2-introduced seam.
Host has zero search-specific code, zero references to
`MultibufferDocumentHandle`, zero `match BufferKind::Multibuffer`
arms.

### Decisions locked 2026-06-01

| ID | Decision | Rationale |
|---|---|---|
| Activation seam | `ModeActivator` trait | `lattice-multibuffer` can't depend on `lattice-host`; trait in `lattice-mode` works both directions. Same surface WASM plugin host will eventually need. |
| Major activation | `create_multibuffer_view` does it | Provider can't forget; insert+activate stays atomic. |
| Minor activation | Provider does it explicitly | Provider seeds per-view state between major activation and minor activation; folding minor activation into `create_multibuffer_view` adds a parameter that's awkward for sync providers without minors. |
| Registry cleanup | `DocumentClosed` subscriber in `lattice-multibuffer`'s register helper | Self-contained; matches the typed-event-driven cleanup pattern used elsewhere. |
| Source-buffer close | Multibuffer holds `BufferId`s only; publishes `MultibufferSourceClosed { view, source }` on close; providers choose policy | Different providers want different behaviour (search drops; diff keeps as historical reference); deferring to provider is correct. |
| Activator return type | `()` on the trait; signals drain via Editor's internal queue | Keeps `RendererSignal` out of `lattice-mode`. |
| Async-scan UX | Open empty view immediately; populate via batched events; headerline shows progress | Paramount goal #1 (UI thread does no I/O); goal #4 (async by construction). |
| Provider crate location | All in-tree providers live in `lattice-multibuffer/src/providers/`, feature-gated | User decision (2026-06-01) — providers we ship are part of multibuffer's deliverable, not standalone crates. **Partially reversed 2026-08-10, see below.** |
| Status surface | Headerline (view-header virtual row) | Discoverable, uniform across providers, doesn't fragment user attention. Convention extends to all future async-populated buffer kinds. |
| Excerpt motions location | `lattice-multibuffer/src/motions.rs`, contributed via `MultibufferMode::keymap()` | Excerpts only exist in multibuffers; motion ownership tracks data ownership. |

### Rejected during the 2026-06-01 pass

- **H.3 event-driven activation pre-v1.** See [`kind-agnostic-buffers.md`](kind-agnostic-buffers.md) §10. Reintroduced when the WASM plugin host slice begins.
- **Buffer-locals access on `ModeContext`.** ModeContext's design rationale (App-managed buffer-locals, App-side writes) stays. Per-provider state lives in per-provider services keyed by `BufferId`. Buffer-locals stay reserved for cross-cutting App-owned state (syntax handles, fold lists, icons).
- **Sync initial scan + async continuation.** Even a "fast first batch" sync scan on the dispatch thread violates paramount goal #1. Trigger returns immediately; everything happens via events.
- **`Document: Any` + downcast for typed handle access.** Contaminates the universal trait; opens the escape-hatch pattern for every future kind. Typed registry (`MultibufferRegistry`) keeps the downcast confined to multibuffer's own crate.
- **Provider crates external to `lattice-multibuffer`.** User decision (2026-06-01): providers ship with multibuffer; the convention is one crate, feature-gated provider submodules. **Partially reversed 2026-08-10 — see below.**

#### Reversed 2026-08-10: a provider owned by a subsystem lives in that subsystem's crate

The 2026-06-01 lock holds for providers with **no owning subsystem** —
`search`, `narrow` and `problems` stay in
`lattice-multibuffer/src/providers/`. It is reversed for providers that
*are* a subsystem's user-facing surface: **the owning crate owns the
provider**, and takes a dependency on `lattice-multibuffer`.

- LSP surfaces (references, workspace symbols, refactor preview, call /
  type hierarchy) → `lattice-lsp`.
- VCS surfaces (project diff, git hunks) → `lattice-magit`.

**Why (heuristic #1, on merit).** The original lock treated "where does
the code live" as a packaging question. It is a mode-ownership question.
`gr` is bound in `lattice-lsp`'s `LspNavMode`; the handler body sits on
`Editor` in `lattice-host`. Adding the *view* in `lattice-multibuffer`
would spread one surface across three crates — the half-migration the
mode-ownership standing rule exists to prevent, with the acid test
(a provider landing should require zero `Editor::` additions) failing
outright. Putting the provider in the owning crate lets the chord, the
handler closure and the view land together, and pulls the handler off
`Editor` as a side effect.

**Direction is clean:** `lattice-multibuffer` depends on neither
`lattice-lsp` nor `lattice-magit`, so `lattice-lsp → lattice-multibuffer`
and `lattice-magit → lattice-multibuffer` are both acyclic. It is
strictly *better* than the locked direction, which would have made
`lattice-multibuffer` — a substrate crate — depend on LSP and VCS.

**What the lock got right and keeps:** the provider *template* is
unchanged (service trait + handle, provider-minor mode, public trigger
function, typed events, `register_<provider>` boot helper). Only the
crate it is compiled into moves. Feature-gating moves with it — a
provider is gated by its owning crate's features, not multibuffer's.

### 3.8 Excerpt-header visual model (MH, 2026-06-17)

Sequencing + slice status:
[`slice-plans/archive/multibuffer-header-polish.md`](../operations/slice-plans/archive/multibuffer-header-polish.md).

The per-excerpt header (distinct from the §3.7 view-header
status line) is the row a user reads to know *which file*
they're looking at. M.2 shipped it as a placeholder:
`default_header_cells` emits `── <title> ──` in box rules,
default foreground, tagged `VirtualRowKind::Generic` — which
in both renderers falls through to the
`diff_deletion_block_bg` backdrop. That faint-red tint is an
accident of kind-reuse, not a design: a search header is not
a deletion block.

The cross-editor convention for a multibuffer / search-result
header (Zed, VSCode) is a **per-file group header**:
file-type icon, basename emphasised, directory path dimmed,
match-count badge, on its own subtle background. We already
emit one header per consecutive source
(`compose_header_rows`), so the structure is correct — only
the cell content, a dedicated row kind, and theme colours are
missing.

```
  filename.rs  src/multibuffer/  ·  7 matches
──────────────────────────────────────────────
```

#### Data model

The header renderer is **generic substrate** — it stays in
`MultibufferExcerptHeaderProvider` so every multibuffer kind
(search, diff, references, diagnostics) inherits
icon+path+count for free. But the *semantic data* it renders
is supplied by the **producing mode** on the excerpt:

```rust
pub struct ExcerptHeader {
	pub title: String,             // fallback label, kept
	pub style: ExcerptHeaderStyle,
	pub path: Option<PathBuf>,     // MH: drives icon + basename/dir split
	pub match_count: Option<u32>,  // MH: badge; mode-owned datum
}
```

`match_count` is **mode-consumed data set at production**
(the search mode counts hits per source as it builds
excerpts); the header *render function* is the uniform-host
consumer, so it stays substrate per the substrate-vs-mode
distinguishing rule. `path` drives both the icon
(`entry_visual`) and the basename-bright / dir-dim split.

#### Resolve at `collect()`, never bake

Icon glyph and segment colours are resolved **inside
`MultibufferExcerptHeaderProvider::collect()`**, reading the
live `ui.nerd_fonts` option and the header theme colours
through the `FrameView::for_buffer` seam
([[project_per_buffer_options_direction]]) — *not* baked into
the cells at excerpt-creation time. This is load-bearing: the
icon-palette rule requires a live `ui.nerd_fonts` toggle to
re-render every glyph surface. Baking the glyph would freeze
the palette at search time and silently break the toggle.
Rejected for that reason.

#### Row kind + theme

A new `VirtualRowKind::Header` variant carries the row's
provenance so renderers pick a header backdrop instead of the
deletion-block fallback. New `host_theme` fields, propagated
to **both** renderers in lockstep
([[feedback_tui_gpui_parity]]):

| Field | Role |
|---|---|
| `multibuffer_header_bg` | header row backdrop (subtle, distinct from deletion red) |
| `multibuffer_header_fg` | basename / primary text |
| `multibuffer_header_path_fg` | dimmed directory path |
| `multibuffer_header_count_fg` | match-count badge |

The §3.7 status-row colours (previously hardcoded
`0x999999` / `0x44cc88` / `0xff4444`) move into theme fields
in the same pass so the whole multibuffer surface is
themeable.

Icons follow [[feedback_icon_palette]] — nerd-font glyph when
`ui.nerd_fonts=on`, BMP-shape fallback otherwise; both occupy
the same cell width so the header geometry doesn't shift on
toggle.

#### Incremental-append contract (MH.B)

M.11's `append_excerpts` (§3.3) appends the new excerpts to
`state.excerpts` but then **recomposes the entire composed
rope + `RowTranslation` from all sources** — O(total
excerpts) per batch. Under streaming search (§3.7, ~50-file
batches) a 10k-hit result drives ~200 full recomposes, the
later ones each walking the whole accumulated set. That cost
is what makes a large search's progressive reveal hitch.

Contract: `append_excerpts` extends the composed rope (append
the new sources' segments) and the `RowTranslation` (push the
new `RowEntry`s) **incrementally** — O(batch), independent of
accumulated total. `replace_excerpts` (the `gr` refresh path)
stays a full rebuild; it is a deliberate reset, not a
hot-loop append.

The invariant the append must preserve: the composed rope is
byte-identical to what a full `compose_text_from_sources` over
the same excerpt list would produce, and `RowTranslation`
entries stay in composed-row order. Edit-forwarding (§4)
depends on this — a regression here mistranslates user edits
to wrong source coordinates. The equivalence is pinned by a
test (`streamed append == batch build`) before the
optimisation lands.

## 4. Edit propagation

The mechanism that turns "edit at multibuffer row M" into
"source buffer edit dispatched through the standard pipeline":

```
User edits at multibuffer display row 17:
  1. Translation lookup: row_translation[17] = Excerpt { excerpt_id: e3, source_row: 109 }
  2. Excerpt lookup: excerpts[e3] = { source: BufferId(src/app.rs), start: anchor_at(102), end: anchor_at(118) }
  3. Translate (excerpt_id, source_row, column) -> (BufferId, Position)
  4. Apply edit through standard pipeline: edit_dispatcher.apply(BufferId, Edit { range: ..., text: ... })
  5. Source buffer publishes EditEvent
  6. Multibuffer's source_sub receives event; bumps multibuffer.revision; schedules row_translation rebuild
  7. Renderer reads new arc-swapped row_translation on next snapshot
```

**Boundary clipping.** When the cursor is at the last display
row of an excerpt and the user types, the new content stays
*inside* the excerpt — the excerpt does not grow magically.
If the user wants more context, they invoke
`:multibuffer-expand` (§6.3). This mirrors Zed and avoids the
surprise of "I typed one character and now the excerpt
extends to the end of the file."

**Multi-excerpt selections.** A visual selection that spans
two or more excerpts is held at the multibuffer-row level.
On edit dispatch (e.g., `d` on the selection), the multibuffer
splits the selection per-excerpt and dispatches N edits in
**row-ascending source-order** — so earlier edits don't shift
ranges underneath later edits. Per-source-document undo
entries are grouped via the standard undo-group mechanism:
one user action (one keypress, one operator+motion) generates
one undo group spanning N source buffers.

**Cross-pane edit visibility.** Because edits flow to the
source buffer through the standard pipeline, any other pane
showing the source buffer sees the edit on its next snapshot.
No bespoke fan-out — this falls out of the buffer system's
existing arc-swap publish.

## 5. Foundational primitive sharing

Multibuffer reuses `diff-system.md`'s D.0 virtual-row
primitive: excerpt headers and separators are virtual rows
anchored to the first / last row of their excerpt:

- **Header row** — `VirtualRow { position: Above, content:
  Header }` anchored to the excerpt's first display row.
- **Separator row** — `VirtualRow { position: Below, content:
  Separator }` anchored to the excerpt's last display row.

If the diff system lands D.0 first, the multibuffer slices
consume the primitive directly. If multibuffer lands first,
M.2 lands D.0 instead. Either way, **one primitive, two
consumers** — the primitive's design (which is in
`diff-system.md` §5.1) is validated by both consumers, and
neither slice ships virtual-row support privately.

## 6. Grammar surface

### 6.1 Motions

| Surface | Registered as | Behaviour |
|---|---|---|
| `]e` | motion: next excerpt | Move cursor to start of next excerpt's first row |
| `[e` | motion: prev excerpt | Move cursor to start of previous excerpt's first row |
| `]E` | motion: next excerpt of different source | Skip to next excerpt with a different `source` BufferId |
| `[E` | motion: prev excerpt of different source | Symmetric |

The "different-source" variants matter for project-wide
flows: in a multibuffer with 47 excerpts across 12 files,
`]E` walks files, `]e` walks excerpts.

**Hunk navigation vs. file navigation in multi-file diff
views.** In a project-wide diff (`A.1` `ProjectDiffProvider`)
or AI multi-file diff (`A.2` `AIProposedEditsProvider`), the
two navigation axes compose cleanly: `]c` / `[c` (from
diff-system.md §6.1 / D.3.c) walk hunks within the current
excerpt; `]E` / `[E` walk file boundaries (= excerpt-source
boundaries). The user gets both axes for free, with no new
diff-specific keybindings, because the multibuffer's
"different-source" excerpt navigation is exactly what
"navigate between files" means in these contexts.
`]e` / `[e` walk individual excerpts within a file — useful
when one file has multiple non-contiguous hunks rendered as
separate excerpts.

### 6.2 Operators

No new operators. Existing operators (`d`, `c`, `y`, `>`,
`<`, etc.) work uniformly against multibuffer ranges because
the standard edit pipeline handles the per-excerpt split.

### 6.3 Ex-commands

| Command | Behaviour |
|---|---|
| `:multibuffer-expand [n]` | Expand context around the excerpt under cursor by `n` rows (default 5) |
| `:multibuffer-contract [n]` | Symmetric contract |
| `:multibuffer-jump-to-source` | Open the excerpt's source buffer in a new pane at the cursor's source position |
| `:multibuffer-close` | Close the multibuffer (does not affect source buffers) |
| `:describe-multibuffer` | Open help buffer showing active multibuffers, providers, excerpt counts |
| `:narrow` | (N.1, A.5) Open a single-excerpt multibuffer over the active visual region, `edges: Strict`. Source buffer unchanged until edits propagate via M.3. |
| `:narrow-to-defun` | (N.1) Same as `:narrow` with the range computed from the tree-sitter scope at point. |
| `:narrow-to-paragraph` | (N.1) Same with the prose paragraph at point. |
| `:widen` | (N.1) Close the active narrow multibuffer, returning to the source. Equivalent to `:bd` on a NarrowProvider-backed buffer; named for Emacs muscle memory. |

Per saved feedback on dashed naming, these are all
dashed multi-word commands.

### 6.4 Options

- `multibuffer.default-expand-rows` — `u32`, default 5.
- `multibuffer.header-style` — `inline | bracketed |
  separator-only`, default `inline`.
- `multibuffer.show-separators` — `bool`, default `true`.

### 6.5 Foldability — composes with the existing fold engine

Excerpts and file boundaries must be foldable so the user can
collapse a multi-excerpt view to a navigation outline.
Critically, **this needs zero new keymap surface** — the
existing fold vocabulary (`za` / `zo` / `zc` / `zR` / `zM`,
`foldlevel=N`, `:foldopen` / `:foldclose`) is fold-source-
agnostic. The fold engine accepts a range from any provider
and treats it identically.

Two providers cover the two natural fold scopes:

- **M.7** lands `ExcerptFoldProvider` — one fold range per
  excerpt's composed-row range. `za` on a row inside an
  excerpt collapses to the excerpt's header (M.2 virtual
  row); `zR` opens every excerpt alongside every other
  fold source.
- **M.8** lands `FileBoundaryFoldProvider` — one fold range
  per distinct `source: BufferId` covering the union of
  that file's excerpts. `za` on a file-header row collapses
  the whole file's excerpts to a single one-line file
  summary. Useful in project-wide diff (A.1) and AI multi-
  file diff (A.2) for "review files top-down" workflows.

Composition with diff-system foldability
(`diff-system.md` §6.5):

- Hunk fold ranges (D.3.f) live in each source document's
  local coordinates.
- Excerpt and file-boundary fold ranges (M.7 / M.8) live in
  the multibuffer's composed coordinates.
- When a hunk sits inside an excerpt sits inside a file
  boundary and the user presses `za` on a row inside all
  three, the **smallest enclosing fold wins** — vim's
  convention. Repeated `za` presses walk outward through
  the nesting: hunk → excerpt → file.

The composition runs through the standard fold registry, not
a multibuffer-specific abstraction, so `:foldopen` /
`:foldclose` ex-commands and the entire `z*` family continue
to work without any multibuffer-aware special-casing. No
`:multibuffer-fold-excerpt`, `:multibuffer-fold-file` etc.
ex-commands are added — heuristic #4 (*"Don't add features
beyond what the task requires"*) applies to keymap surface
too.

The slice plan (§9) lists M.7 + M.8 as the gates for this.

## 7. Performance posture

- **Hot path (per-keystroke in multibuffer):** Edit dispatch
  via translation lookup (O(log n) over the row table) +
  standard edit pipeline. Net overhead vs. editing a source
  buffer directly: one binary search. Bench gate
  `multibuffer_edit_dispatch_p99_us` ≤ 100µs at 1k excerpts.
- **Anchor-update fanout.** When a source buffer edits, its
  excerpt anchors update automatically (anchor is a tracked
  position type — no work from multibuffer). The multibuffer
  observes the source's `EditEvent`, bumps its revision, and
  schedules a translation rebuild. Bench gate
  `multibuffer_source_edit_p99_us` ≤ 200µs at 1k excerpts
  spread across 10 source buffers.
- **Translation rebuild.** O(N) where N is the total source
  row count across excerpts. For a 1k-excerpt multibuffer
  with 20 rows each (20k rows total), rebuild is ~1ms on
  off-thread. Bench gate `multibuffer_translation_rebuild_
  p99_us` ≤ 2000µs at the 20k-row corpus.
- **Provider mutation absorption.** Bulk excerpt replacement
  (e.g., grep query change) at 1k excerpts: one mutation
  dispatch, one rebuild, one arc-swap publish. Bench gate
  `multibuffer_bulk_replace_p99_us` ≤ 5000µs.
- **Render cost.** Renderer reads `RowTranslation` like any
  other row source; cost is proportional to visible rows.
  No multibuffer-specific paint cost beyond the virtual-row
  primitive's own cost (already gated in D.0).
- **Memory.** Per multibuffer: `Vec<Excerpt>` (~64 bytes per
  excerpt + anchor storage) + `RowTranslation` (~16 bytes per
  display row). 1k excerpts × 20 rows = ~340KB. Negligible.

The worst-case concern is **anchor-update fanout for a
multibuffer subscribed to a frequently-edited file** (e.g.,
5000 search hits across one file the user is actively
editing). Mitigations available if the bench gate is
exceeded:

1. Batch anchor updates per frame instead of per edit.
2. Skip translation rebuild when the source revision changes
   but no anchor crossed an excerpt boundary (no excerpt's
   row span changed).
3. Coalesce rebuilds across rapid edits via a 16ms debounce.

Bench-gate first, optimise if needed; the naive path may be
adequate.

## 8. Open questions

- **Editing at an excerpt boundary** — typing on the last
  row of an excerpt: clips into excerpt (current spec) or
  extends to start of next excerpt? Lean clip (Zed's
  choice) — explicit `:multibuffer-expand` to grow.
  Decide before M.3.
- **Source-buffer close while excerpted** — the source
  buffer of an excerpt is closed; the excerpt becomes
  orphaned. Options: (a) auto-remove orphaned excerpts;
  (b) freeze excerpt at last-known content and grey out;
  (c) refuse to close the source buffer while excerpted.
  Lean (a) — explicit, no surprise state; the provider can
  re-emit the excerpt if it knows how to recover the source.
  Decide before M.4.
- ~~**Folding inside excerpts** — should excerpts be
  foldable as units? Useful for navigation in large
  multibuffers. Lean yes, but defer to a polish slice
  after M.6.~~ **Resolved 2026-05-29.** Yes — landed as
  M.7 (`ExcerptFoldProvider`) + M.8 (`FileBoundaryFoldProvider`)
  in the slice plan §9. Composes with the existing fold
  engine; no new keymap surface. See §6.5.
- **Multibuffers participating in diff sessions** — when a
  multibuffer is the active buffer and the user invokes
  `:diff <buf>`, what does that mean? Lean: disallow at the
  ex-command level; multibuffers are not diffable directly.
  The per-excerpt diff use case is served by
  `ProjectDiffProvider` (one diff session per excerpt). The
  multibuffer is the *viewer*, not the diffable buffer.
- **Tree-sitter highlighting across excerpts** — does the
  parser run per-excerpt against its source buffer's tree,
  or once across the composed rope? Per-excerpt is correct
  (each excerpt's content is parsed in the source buffer's
  language; mixed-language multibuffers work). The renderer
  reads spans through the excerpt mapping. Decide before
  M.2 (rendering slice).

## 9. Slice plan

Sequencing lives in
[`docs/dev/operations/slice-plans/multibuffer-views.md`](../archive/multibuffer-views.md);
authoritative status per slice lives in
[`docs/dev/operations/implementation.md`](../operations/implementation.md).
This fragment owns *what* and *why*; the slice plan owns
*when* and *in what order*. Follow-on consumers (project
diff, AI proposed edits, references, diagnostics, narrow
mode) are described as design content in §10 below.

## 10. Follow-on consumers (appendix)

After M.6, four further providers compose on top without
changing the multibuffer subsystem. Each is its own slice
sequence; the slice IDs below are illustrative, not
committed.

### A.1. `ProjectDiffProvider`

Composes `multibuffer-views.md` (this doc) + `diff-system.md`
D.7 (git baseline). Provider walks git status, opens a
`DiffSession` per changed file (single-doc-vs-baseline), and
emits one excerpt per file whose range covers the dirty
hunks. The multibuffer's renderer composes the diff system's
inline `DiffMap` per-excerpt — virtual deletion blocks and
gutter signs nest inside each excerpt's row range. UX: "show
me everything that changed in the repo as one scrollable,
editable diff."

**Navigation surface**: `]c` / `[c` walk hunks within the
current file's excerpt (the diff-system motion). `]E` / `[E`
walk between files (the multibuffer "different-source"
motion — §6.1). `]e` / `[e` walk individual excerpts within
a file when one file has multiple non-contiguous hunks. No
new keybindings needed; the two-axis nav falls out of the
composition.

Slice cost: one provider + the inline-DiffMap-inside-excerpt
composition path.

### A.2. `AIProposedEditsProvider`

Composes with `diff-system.md` D.3 (inline overlay). Receives
a list of `(SourceBufferId, ProposedText)` pairs from
Claude / agent host. Per pair: synthesises a baseline
Document holding the proposed text, opens a single-doc-vs-
baseline `DiffSession` (matching diff-system.md §9 "Claude
Code `openDiff`"), emits one excerpt per changed file
covering the dirty hunks. The multibuffer renders inline
diff per-excerpt. UX: "Claude wants to change 8 files —
review them all in one view, accept or reject per-hunk."

**Navigation surface**: same two-axis composition as A.1.
`]c` / `[c` walk hunks within the current proposed-file
excerpt; `]E` / `[E` walk between files (each file is its
own excerpt source). For an 8-file proposal, `]E` is "next
file" without scrolling through every hunk in the current
one.

Slice cost: one provider + the per-excerpt acceptance plumbing
(routes to the underlying `DiffSession::completion` oneshot
per excerpt).

### A.3. `LspReferencesProvider`

Composes with the existing LSP subsystem. `gr` opens a
multibuffer of every `textDocument/references` site as an
excerpt with 5 rows of surrounding context. Excerpts are
editable — the multibuffer is "find references and refactor
right here" rather than "find references and jump."

Slice cost: one provider.

### A.4. `DiagnosticsProvider`

Composes with the existing diagnostics layer. `:diagnostics`
opens a multibuffer of every diagnostic site as an excerpt
with surrounding context. Excerpts are editable — the
multibuffer is "fix every clippy warning right here" rather
than "list them and jump per site."

Slice cost: one provider.

### A.5. `NarrowProvider` — Emacs-style narrow mode

Composes purely with M.1 (excerpt structure) + M.3 (edit
propagation). The provider holds **one excerpt** over a single
source buffer's character-precise range with
`edges: ExcerptEdgeMode::Strict` so partial-line edges render
exactly the narrowed text.

UX: `:narrow` over the active visual region creates a
single-excerpt multibuffer pinned to the source's selection
range; the user sees only that range, edits propagate
upstream through M.3's standard pipeline, and `:widen` (or
`:bd` on the multibuffer) returns to viewing the source.
`:narrow-to-defun` is the same primitive with the range
computed from the tree-sitter scope at point;
`:narrow-to-paragraph` from the prose paragraph at point.

Why it's not in-place (vs. Emacs):

- Emacs `narrow-to-region` stashes restriction bounds on the
  buffer itself, so a buffer can be narrowed in at most one
  way at a time.
- The multibuffer approach makes the narrow its own buffer.
  Costs: a new BufferId, a registry entry. Pays: **multiple
  parallel narrows on the same source** (one per pane,
  showing different ranges, all live-synced); **narrow within
  narrow** (a NarrowProvider over a NarrowProvider's output
  buffer chains through M.3's edit propagation transparently);
  **narrow over multibuffer** (narrow inside a search-results
  view, an AI proposed-edits view, a project-diff view —
  same machinery).

The compositional gain is the architectural reason to do this
through multibuffer rather than as a sibling restriction-bounds
field on Document. Saved memory check: aligns with
*"Buffers must not have kind-specific logic"* — a narrowed
view is just a Document, not a special buffer kind.

Slice cost: one provider + the `:narrow` / `:narrow-to-defun` /
`:narrow-to-paragraph` / `:widen` ex-commands.

## 11. Testing strategy

- **Unit tests** in a new `lattice-multibuffer` crate (or
  `lattice-host` module): excerpt anchor tracking under
  source edits; row-translation correctness for varied
  excerpt configurations; edit translation correctness;
  multi-excerpt selection split.
- **Document-trait conformance tests** (M.0): both
  `RopeDocument` and `MultibufferDocument` pass a shared test
  harness covering the `Document` trait surface. Ensures the
  abstraction has no implicit "this only works for ropes"
  assumptions leaking.
- **Integration tests** via the headless host: create a
  multibuffer with provider, mutate the provider's data
  source, observe the multibuffer's excerpt and rendered-row
  state evolve. Tests for SearchProvider's grep-query-change
  flow.
- **Renderer tests** (TUI + GPUI): excerpt headers and
  separators render at expected positions; motions over
  excerpts land cursor at expected display rows; virtual
  rows don't lose alignment under scroll.
- **Cross-pane coherence test**: pane A holds source buffer,
  pane B holds multibuffer with excerpt of source buffer;
  edit through pane B; pane A's view reflects on next
  snapshot. And the reverse.
- **Bench:**
  - `multibuffer_edit_dispatch_p99_us` (M.3) — CI gate ≤
	100µs at 1k excerpts.
  - `multibuffer_source_edit_p99_us` (M.4) — CI gate ≤ 200µs
	at 1k excerpts × 10 source buffers.
  - `multibuffer_render_p99_us` (M.2) — CI gate ≤ 200µs at
	50 visible excerpts.
  - `multibuffer_translation_rebuild_p99_us` (M.1) — CI gate
	≤ 2000µs at 20k rows.
  - `multibuffer_bulk_replace_p99_us` (M.6) — CI gate ≤ 5000µs
	at 1k excerpt replacement.
- **Stress observations** (not gated): 10k excerpts across
  100 source buffers; one source buffer with 5k excerpts
  under continuous edit.

## 12. Risks

- **M.0 trait refactor is the load-bearing risk.** It touches
  every `Document` call site in the codebase. Wrong abstraction
  surface → either every `MultibufferDocument` impl method
  panics with "not applicable to multibuffer," or `RopeDocument`
  carries dead methods only multibuffer needs. The mitigation
  is to do M.0 with **only `RopeDocument` as the impl**, ship
  it green, then design M.1's impl against the trait that
  already exists in production. If M.1 needs trait additions,
  add them in M.1 with a clear name; do not pre-shape the
  trait around an unmerged consumer.
- **Anchor-update fanout under pathological load.** 5000
  search hits in one frequently-edited file. The bench
  catches this; if it exceeds budget, the mitigations
  enumerated in §7 are well-defined (batch, skip, debounce).
  None of them are architectural — they're tuning. The risk
  is wasted slice budget on premature tuning if we treat the
  worst-case as the default. Bench first.
- **Cross-excerpt selection edit-order bugs.** Edits
  dispatched in source-ascending order avoid range-shift
  bugs, but the partial-failure case (edit 3 of 5 fails) is
  subtle: do we roll back edits 1–2? Do we leave the system
  in a partial state with an error banner? Lean: log + leave
  partial (per saved feedback on graceful failure), with a
  clear banner. Explicit test case in M.3 covering this.
- **Provider task lifecycle complexity.** Each multibuffer
  owns a provider task; the task can fail, panic, or stream
  too much. The subsystem must monitor task health; failure
  banners through the diagnostic layer; never crash the
  editor on provider failure. Standard tokio supervisor
  pattern (already used for LSP) — but multibuffer's
  per-multibuffer task ownership means more failure surface
  than the per-server LSP model.
- **Tree-sitter parsing cost across many small excerpts.**
  Per-excerpt parsing through the source's tree is correct;
  if it's slow at scale, lazy / on-visible-only parsing is
  the mitigation. Bench observation in M.2.

## 13. Cross-references

- `design.md` §5.14 — synopsis paragraph linking here.
- `design.md` §5.1 — Buffer / Document model; `Document`
  becomes a trait per M.0 with this doc as the authority.
- `design.md` §5.1.1 — position history / anchor type;
  excerpts use the existing anchor primitive.
- `design.md` §5.2 — modal engine; motions `]e` / `[e` are
  registered through `CommandRegistry` per §6.1.
- `design.md` §5.6 — rendering; the row-translation cache
  feeds the EditorRenderer.
- `design.md` §5.9.8 — buffer-backed views;
  `MultibufferDocument` is a Document with composed content,
  fitting the everything-is-a-buffer model.
- `design.md` §5.10 — event system; multibuffer subscribes
  to `DocumentEdited` on each source buffer.
- `diff-system.md` — the diff data layer that composes with
  multibuffer to deliver project-wide diff and AI multi-file
  flows. The D.0 virtual-row primitive is shared.
- `actor-seam-discipline.md` — the actor + arc-swap publish
  pattern this subsystem inherits.
- `implementation.md` — `## multibuffer-views` ledger tracks
  M.0–M.6 slice status as they land.
