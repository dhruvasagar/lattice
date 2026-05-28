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

| Consumer | UX it lights up |
|---|---|
| `SearchProvider` (lands with M.6) | Project-wide search/replace as an editable buffer (`wgrep`-style). Edit a result line → file is edited. |
| `ProjectDiffProvider` | "Show all changed files in the repo as one scrollable diff." Per-excerpt diff state via `diff-system.md`. |
| `AIProposedEditsProvider` | Claude / agent proposes edits to N files; all hunks shown in one multibuffer with per-hunk accept/reject. |
| `LspReferencesProvider` | `gr` opens a multibuffer of every call site as editable excerpts, not a read-only picker. |
| `DiagnosticsProvider` | `:diagnostics` opens a multibuffer of every diagnostic site as editable excerpts. |

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

Today `Document` is a struct around a rope. Multibuffer
requires at least two implementations sharing one interface —
the rope-backed file Document and the excerpt-composed
multibuffer Document. The shift:

```rust
pub trait Document: Send + Sync {
	fn id(&self) -> BufferId;
	fn rope(&self) -> &Rope;           // composed view for multibuffer
	fn revision(&self) -> Revision;
	fn line_count(&self) -> usize;
	fn apply_edit(&self, edit: Edit) -> EditResult;
	fn anchor_at(&self, pos: Position) -> Anchor;
	fn position_at(&self, anchor: &Anchor) -> Position;
	// ... existing Document API surface, hoisted to the trait
}

pub struct RopeDocument { /* today's Document */ }
impl Document for RopeDocument { /* ... */ }

pub struct MultibufferDocument { /* §3.3 */ }
impl Document for MultibufferDocument { /* ... */ }
```

M.0 lands the trait split with **zero behavioural change**:
every call site that touched `Document` today touches the
trait, with `RopeDocument` as the sole implementation. The
multibuffer Document arrives in M.1.

`MultibufferDocument`'s `rope()` returns a *composed* rope —
not a physical copy of the source ropes' bytes, but a lazy
view that reads through to the source ropes on demand. The
composition is cached per multibuffer revision; cache
invalidates when any source revision changes or any excerpt
range changes.

### 3.2 `Excerpt`

```rust
pub struct Excerpt {
	pub id: ExcerptId,
	pub source: BufferId,
	pub start: Anchor,    // tracks source-buffer edits
	pub end: Anchor,
	pub header: ExcerptHeader,
}

pub struct ExcerptHeader {
	pub title: String,    // e.g., "src/app.rs : 102–118"
	pub style: ExcerptHeaderStyle,
}
```

`Anchor` is the existing position-history primitive
(§5.1.1) — it already tracks source-buffer edits without the
multibuffer doing any work. When the user inserts a line at
source row 50 and the excerpt covers source rows 102–118,
the anchors slide to 103–119 automatically.

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
	Excerpt {
		excerpt_id: ExcerptId,
		source_row: u32,
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
arc-swap.

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

Per saved feedback on dashed naming, these are all
dashed multi-word commands.

### 6.4 Options

- `multibuffer.default-expand-rows` — `u32`, default 5.
- `multibuffer.header-style` — `inline | bracketed |
  separator-only`, default `inline`.
- `multibuffer.show-separators` — `bool`, default `true`.

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
- **Folding inside excerpts** — should excerpts be foldable
  as units? Useful for navigation in large multibuffers.
  Lean yes, but defer to a polish slice after M.6.
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

Each slice ships green-on-merge with the four artefacts
CLAUDE.md mandates: architecture documentation (this doc,
updated as needed), benchmark coverage where load-bearing,
test coverage of the new scenarios + failure modes, graceful
error handling.

| Slice | Title | What lands |
|---|---|---|
| **M.0** | Document-as-trait refactor | Hoist `Document` to a trait; port today's struct to `RopeDocument`; keep every call site green. The refactor's correctness gate is the existing test suite passing unchanged, plus a bench-no-regression on `editor_render_p99_us` and `edit_dispatch_p99_us` (no new abstraction overhead). No multibuffer code yet. Reviewed independently and merged as its own PR before M.1 starts. |
| **M.1** | `MultibufferDocument` (read-only) | New `MultibufferDocument` impl of the `Document` trait. Excerpts as `Vec<Excerpt>` with anchored ranges; composed `rope()` view backed by lazy read-through; row-translation cache built on first access, invalidated on source-edit events. Registered in `BufferRegistry`. **Edits are rejected** (read-only mutability) in this slice. Tests: create multibuffer with 3 excerpts across 2 source buffers; `rope().lines()` returns the expected composed content; source-buffer edits propagate to the composed view; closing a source buffer auto-removes orphaned excerpts (per §8 decision). No rendering yet — assertions are via direct Document reads. |
| **M.2** | Excerpt rendering | Consumes D.0's virtual-row primitive (if D.0 hasn't landed yet, this slice lands it). Excerpt headers + separators render as virtual rows. `]e` / `[e` / `]E` / `[E` motions registered. Tests: open a multibuffer in a pane, render correctly with headers and separators; motions land on expected rows. Bench: `multibuffer_render_p99_us` ≤ 200µs at 50 visible excerpts. |
| **M.3** | Edit propagation | Flip multibuffer to editable. Edit dispatch at multibuffer row → translation lookup → source dispatch → standard pipeline. Boundary clipping per §4. Multi-excerpt selections split into per-excerpt edits in source-ascending order. Undo composes correctly. Tests: edit within an excerpt → source buffer reflects; edit at excerpt boundary clips; multi-excerpt selection delete fires one undo group spanning N source buffers; macros recorded against a multibuffer replay correctly. Bench: `multibuffer_edit_dispatch_p99_us` ≤ 100µs at 1k excerpts. |
| **M.4** | Live updates from source buffers | Source-buffer edits propagate to the multibuffer view. Anchor-driven excerpt range tracking (existing anchor type handles this). Translation rebuild debounced and run off-thread. Cross-pane consistency: edit in source pane reflects in multibuffer pane on next snapshot. Tests: edit source buffer outside any excerpt — multibuffer unchanged; edit source buffer inside an excerpt — multibuffer's composed view reflects; rapid edits coalesce into one rebuild. Bench: `multibuffer_source_edit_p99_us` ≤ 200µs at 1k excerpts. |
| **M.5** | Expand-context affordance | `:multibuffer-expand [n]` / `:multibuffer-contract [n]` ex-commands and the bound keys (`+` / `-` on excerpt header). Translates to anchor-range mutation on the relevant excerpt; translation rebuild fires through the standard path. Tests: expand grows the excerpt; contract shrinks; expand below 1 row is a no-op; expand past the source buffer's end clips. |
| **M.6** | `MultibufferProvider` trait + first consumer | The provider trait + the `MultibufferSubsystem` that owns provider tasks. **First consumer lands in the same slice**: `SearchProvider` — wraps ripgrep, emits initial excerpts from match locations, observes search-query changes and re-emits excerpts. `:search-buffer <pattern>` ex-command opens the result in a multibuffer. Tests: provider lifecycle (create, mutate, close); ripgrep-driven excerpts populate correctly; query change replaces excerpts; search-buffer responds to subscribed event mutations. Bench: `multibuffer_bulk_replace_p99_us` ≤ 5000µs at 1k excerpt replacement. |

Slice sequencing:

- **M.0 is the load-bearing slice** — the trait refactor.
  Lands green standalone; everything else depends on it.
- **M.1** depends on M.0.
- **M.2** depends on M.1 + D.0 (consumes the virtual-row
  primitive; lands D.0 if D.0 hasn't already shipped).
- **M.3** depends on M.2 (need rendering to test edit
  visibility) + M.1.
- **M.4** depends on M.3 (need editable multibuffer to test
  cross-pane edit visibility).
- **M.5** depends on M.4.
- **M.6** depends on M.4 (provider needs editable +
  live-updating multibuffer).

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
