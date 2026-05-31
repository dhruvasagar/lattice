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
- The 8 ms / 120 Hz frame budget.

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
[`docs/dev/operations/slice-plans/multibuffer-views.md`](../operations/slice-plans/multibuffer-views.md);
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
