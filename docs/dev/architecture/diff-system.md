# Diff System

Authoritative design for Lattice's diff subsystem: the data
model, the presentation transforms that turn hunks into rendered
rows, the grammar surface, and the slice plan. Two-way and
three-way diff share one data model; inline overlay and
side-by-side are two presentations of that data; the existing
`CommandRegistry` carries the user-facing motions and operators.

This document is a *companion* to `design.md` (§5.1 buffer model,
§5.2 modal engine, §5.6 rendering, §5.9 UI components) and to
`multibuffer-views.md` (the orthogonal aggregator that makes
project-wide and AI-multi-file diff flows possible). It does
not supersede them — it specifies a §5.13 subsystem that those
documents reference but do not specify.

## 1. The design goal

Lattice ships with **the four flows users expect from a modern
editor's diff system**, and no more:

1. Inline overlay against a baseline (file vs. disk, file vs.
   git HEAD, file vs. AI-proposed content). Gutter signs +
   deletion blocks + line-background tints. This is the surface
   that Claude Code's `openDiff` lands on, that `:Gdiff <ref>`
   lands on, and that "show me what changed since I saved" lands
   on.
2. Side-by-side two-way diff between two documents. Aligned
   panes, scroll-locked, filler rows on each side to keep
   hunks visually parallel. This is `:diffsplit a.txt b.txt`
   and the surface most users reach for during code review.
3. Three-pane three-way merge. The same machinery as (2) with
   one more document and conflict-region awareness. This is
   `:diffsplit base.txt local.txt remote.txt` and the surface
   merge-conflict resolution lands on.
4. Hunk motions and transfer operators (`]c` / `[c` / `do` /
   `dp`) registered through the central `CommandRegistry`
   (§5.2.1), composable with the vim grammar like every other
   motion / operator.

Two saved invariants frame the choices:

> Everything is a buffer. -- `CLAUDE.md`, `design.md` §5.9.8

> Buffers must not have kind-specific logic — never branch
> render / motion / scroll on `BufferKind`. -- saved feedback
> `feedback_buffers_no_special_case.md`

The diff system is therefore **decoration + presentation
transform layered over existing Documents**, not a new buffer
kind. The renderer, the grammar, and the buffer registry treat
the documents participating in a diff as ordinary Documents.

## 2. Reference points

Three editors solved adjacent problems; the strengths to borrow
and the parts to leave behind are explicit.

- **Helix** (`helix-vcs`) — per-buffer `DiffHandle` actor,
  Histogram via `imara-diff`, RCU-published hunks, gutter-only
  presentation. We adopt the actor + RCU shape **verbatim** —
  it's exactly what `actor-seam-discipline.md` already
  describes for every other subsystem. We diverge by adding
  inline-overlay and side-by-side and three-way presentations
  on top of the same data layer; helix has only gutter.
- **Zed** (`buffer_diff` + `editor::DiffMap`) — splits the diff
  into a **data layer** (`BufferDiff`: baseline + hunks,
  RCU-published) and a **display transform** (`DiffMap`: turns
  hunks into the row stream the editor renders, inserting
  virtual rows for deletion blocks). We adopt this split
  wholesale. The same hunks feed inline (DiffMap on) and
  side-by-side (DiffMap off, two panes with shared hunks +
  filler virtual rows). We do **not** adopt Zed's multibuffer
  excerpts in this design — that's `multibuffer-views.md`.
- **Vim** (`diffthis` / `:diffsplit` / `]c` / `[c` / `do` /
  `dp`) — the grammar surface is right. Operators and motions
  over hunks compose with the rest of the vim grammar, which
  is exactly what §5.2.1 commits to. We adopt the surface,
  not the implementation: vim's `&diff` window-local boolean
  ties diff state to the UI; ours lives on the Core thread as
  data.
- **Emacs `ediff`** — the structural decision worth borrowing
  is that two-way and three-way share one model with a control
  surface. Lattice's `CommandRegistry` is that control surface.
  Lattice does **not** adopt ediff's separate control buffer
  or its dated UX.

## 3. The data model

Three types carry the system. The names live in a new
`lattice-diff` crate; the actor + RCU plumbing lives in
`lattice-host` adjacent to the existing subsystems.

### 3.1 `Hunk` and `HunkIndex`

```rust
#[derive(Clone, Debug)]
pub struct Hunk {
	/// Which kind of change this hunk represents.
	pub kind: HunkKind,
	/// One range per participating document, in document order.
	/// Length is the session's document count (2 for two-way,
	/// 3 for three-way, 1 for single-doc-vs-baseline).
	pub ranges: SmallVec<[LineRange; 3]>,
}

#[derive(Copy, Clone, Debug)]
pub enum HunkKind {
	Add,        // present in later doc(s), absent in earlier
	Remove,     // present in earlier doc(s), absent in later
	Change,     // present in both, contents differ
	Conflict,   // three-way only: changed differently in 2+
}

pub struct HunkIndex {
	pub hunks: Vec<Hunk>,
	/// Algorithm used (recorded so the user can see it via
	/// `:describe-diff`).
	pub algorithm: DiffAlgorithm,
	/// Monotonic counter incremented every recompute.
	pub revision: u64,
}
```

`HunkIndex` is the unit of RCU publication: an `Arc<HunkIndex>`
is swapped atomically when recomputation finishes. Renderers and
motions read the current `Arc<HunkIndex>` under arc-swap, exactly
like syntax trees and diagnostics.

### 3.2 `DiffSession`

```rust
pub struct DiffSession {
	pub id: DiffSessionId,
	/// 1..=3 participating documents. The "1" case is
	/// single-document-vs-baseline (e.g., file vs. disk, file
	/// vs. AI proposal); the baseline document is registered
	/// in the buffer registry as a hidden Document.
	pub documents: SmallVec<[(BufferId, AnchorRevision); 3]>,
	/// Atomic published hunks. Replaced on each recompute.
	pub hunks: ArcSwap<HunkIndex>,
	/// Recompute scheduling state. The session subscribes to
	/// edit events on each participating document and debounces
	/// recomputes.
	pub recompute: RecomputeState,
	/// Optional completion signal — used by `openDiff`-style
	/// callers that want to know when the user accepts or
	/// rejects.
	pub completion: Option<oneshot::Sender<DiffOutcome>>,
	/// Algorithm preference; defaults to `Histogram`.
	pub algorithm: DiffAlgorithm,
}

pub enum DiffOutcome {
	Accept,                  // user chose to apply the change(s)
	Reject,                  // user dismissed
	Partial(Vec<HunkId>),    // user accepted some, rejected others
}
```

A `DiffSession` is **data on the Core thread**, owned by a
`DiffSubsystem` peer (§3.4). Sessions are not panes, not
windows, not buffers — they're a relationship between two or
three documents that decorations and presentations consult.

### 3.3 `DiffMap` — the presentation transform

A `DiffMap` is per-pane state that turns hunks into a row
stream:

```rust
pub struct DiffMap {
	pub session: DiffSessionId,
	pub mode: PresentationMode,
	/// Cached row-translation table; invalidated when the
	/// session's HunkIndex revision changes.
	pub rows: RowTranslation,
}

pub enum PresentationMode {
	/// Single-pane inline overlay. Hunks render in-place with
	/// gutter signs; removed lines render as virtual rows
	/// (D.0) above the line they were deleted before.
	Inline,
	/// Two- or three-pane side-by-side. Each pane reads the
	/// same HunkIndex; the pane index determines which
	/// document's row range it follows. Filler virtual rows
	/// keep parallel hunks aligned.
	SideBySide { pane_index: u8 },
}

pub struct RowTranslation {
	/// Maps display row -> (DocumentRow | DeletionBlock |
	/// FillerRow).
	pub entries: Vec<RowEntry>,
}
```

The `EditorRenderer` (§5.6.2) reads through `DiffMap` when one
is active on the pane. With no active `DiffMap` the pane reads
document rows directly — identical to today's path. With an
active `DiffMap`, document rows are interleaved with virtual
rows (deletion blocks in inline mode, fillers in side-by-side).
The renderer's fast paths do **not** branch on diff state —
they iterate `RowEntry`s, which look the same whether they
came from a `DiffMap` or directly from the document.

### 3.4 `DiffSubsystem` — the owner

A `DiffSubsystem` lives next to `LspSupervisor` and the
existing terminal supervisor on `lattice-host`
(`lattice-host::diff_subsystem`, landed D.2.a 2026-05-28).
What follows is the model the subsystem implements end-to-end
across D.2.a (registry) → D.2.b (compute) → D.2.c (routing +
debounce). Each piece names the slice that landed it so the
prose stays load-bearing without going stale.

#### 3.4.1 Data model — sessions, descriptors, watchers

A diff session is a derivation: published `HunkIndex` =
`compute_two_way(baseline_rope, current_rope)`. The session
itself is the publish channel (D.2.a / D.2.b — `Arc<DiffSession>`
in a `BufferId`-keyed registry, `ArcSwap<HunkIndex>` for RCU
reads, monotonic revision counter, gated publish). The
**sources** that feed the derivation are decoupled from the
session and stored alongside it in a **descriptor** (D.2.c):

```rust
pub trait BaselineSource:    Send + Sync + 'static + Debug { fn snapshot(&self) -> Rope; }
pub trait CurrentSource:     Send + Sync + 'static + Debug { fn snapshot(&self) -> Rope; }
pub trait BufferTextProvider: Send + Sync + 'static + Debug {
    fn buffer_rope(&self, id: BufferId) -> Option<Rope>;
}

pub struct DiffDescriptor {
    pub baseline: Arc<dyn BaselineSource>,
    pub current:  Arc<dyn CurrentSource>,
    /// Buffers whose edits should trigger a recompute of this
    /// session. The descriptor's author declares this
    /// explicitly because only the author knows which sources
    /// read from live buffers (StaticBaseline contributes no
    /// watch entry; BufferBaseline contributes its `buffer_id`).
    pub watch: Vec<BufferId>,
}
```

The author of the descriptor knows what they wrote — sources
are *not* expected to self-introspect their dependencies.
Static, file-on-disk, and git-blob sources have empty watch
contributions; buffer-backed sources contribute their
`BufferId`. The watch list is the **explicit dependency
declaration**.

Concrete sources land progressively:
- `StaticBaseline { rope }` — D.2.b. AI proposed-edits, static
  comparisons, tests.
- `BufferBaseline { provider, buffer_id }` — D.2.c. Live rope
  read from a sibling buffer. Handles the unsaved-buffer-vs-
  unsaved-buffer case directly: both sides resolve through
  `BufferTextProvider` at snapshot time; neither needs a
  filesystem path.
- `BufferCurrentSource { provider, buffer_id }` — D.2.c. The
  live-rope side for any session backed by a real buffer.
- `GitBaseline { path, repo }` — D.7. Reads `HEAD:path` via
  `gix`. Contributes no watch entry (HEAD doesn't change mid-
  session unless we add a refs watcher, which we don't in v1).
- `OnDiskBaseline { path }` — re-reads file at snapshot. Rare
  (for explicit `:diff` against current on-disk state).

**`BufferTextProvider` is the single host seam** the subsystem
needs into buffer storage. The host supplies one impl over
`BufferRegistry`; future ephemeral-buffer providers (e.g.,
plugin-owned virtual buffers) plug into the same trait.

#### 3.4.2 Dependency graph — a buffer can wake many sessions

A buffer can be `current` for one session and `baseline` for
another *at the same time*. Concrete example: two scratch
buffers X and Y opened with `:diffsplit`, neither on disk.

- **Session(X)**: current = live X, baseline = live Y, watch = [X, Y]
- **Session(Y)**: current = live Y, baseline = live X, watch = [Y, X]

When the user types in X, both sessions must recompute (X's
current changed for Session(X); X's baseline changed for
Session(Y)). The subsystem fans the edit-event on X out to
both. Project-wide diff and AI multi-file `openDiff`
generalise this: one multibuffer session whose watch list is
the N source buffers (M.2 + D.3 compose); an edit to any one
of them wakes only that one session and recomputes only the
affected excerpt.

#### 3.4.3 Routing — centralized inverse index, lazy debouncers

The subsystem keeps three maps:

```rust
sessions:    Mutex<HashMap<BufferId, Arc<DiffSession>>>,        // identity + publish
descriptors: Mutex<HashMap<BufferId, DiffDescriptor>>,          // what to diff against what
watchers:    Mutex<HashMap<BufferId, Vec<BufferId>>>,           // INVERSE: watched buffer → sessions
debouncers:  Mutex<HashMap<BufferId, Arc<Debouncer>>>,          // per-session debounce state
```

The `watchers` map is the inverse index over descriptors —
"which session keys depend on edits to buffer X?" Rebuilt on
register / drop. This is the routing structure: when a
`DocumentChanged` event lands for buffer X, one `HashMap`
lookup returns the (small) set of sessions to wake.

**Bus subscription is centralized**: one `EventBus` subscription
on the subsystem, one drainer task. The drainer translates
`DocumentId` → `BufferId` via a `DocumentBufferResolver` trait
(host supplies the impl over `BufferRegistry`), looks up
dependents in `watchers`, and pokes each session's
`Debouncer`. `DocumentClosed` similarly drives `drop_session`.

**`Debouncer` is lazy** — no task at rest. On first `poke()`,
it bumps an epoch counter and spawns a tokio task that sleeps
the debounce window, re-reads the epoch, and either
reschedules (if more pokes arrived) or fires the recompute
callback and self-terminates. A dormant session costs three
HashMap entries; an actively-edited session costs one tokio
task that lives only as long as the edit burst.

#### 3.4.4 Why centralized routing — scaling discussion

The first instinct (recorded here so future-us doesn't try it
again) was a **per-session actor** — each session spawns a
tokio task that subscribes directly to its watched buffers'
edit events, debounces locally, computes. This is what Zed
does for `BufferDiff`, and it composes beautifully when N is
small or when each "actor" has independent state worth
running. For diff sessions, two things break that down:

| Sessions (N) | Per-session-actor cost / edit  | Centralized-routing cost / edit |
|--------------|--------------------------------|---------------------------------|
| **10**       | 10 receivers cloned + woken    | 1 lookup, fan to dependents     |
| **100**      | 100× clone, 99 discard         | 1 lookup, fan to dependents     |
| **1000**     | 1000× clone per keystroke      | 1 lookup, fan to dependents     |

Per-session-actor cost grows with **total session count**,
not actual dependents — because the global event bus delivers
to every subscriber and the subscriber decides whether to
care. At N=1000 (a large multi-file `openDiff`) the
per-keystroke clone-and-fan-out becomes a real cost.
Centralized routing scales with **actual dependents**, which
is typically 1–2 even at high N because edits are sparse over
the watched set.

Zed avoids this by making each `Buffer` its own publisher
(per-entity subscription, not a global bus). Lattice's bus is
global per-kind, so the equivalent structural decision is to
**centralize the routing inside the subsystem that owns the
sessions**. The cleanliness Zed gets from per-entity
subscription Lattice gets from a single subscription + a
typed inverse index inside the subsystem.

Helix takes the third path — `Document::apply_changes` calls
directly into `DiffHandle`. Tight coupling, but Helix doesn't
have project-wide or multi-file diff at the substrate level,
and its diff has only one consumer of edit events. We
considered it (D.2.c would have been a `note_buffer_edited`
call from `dispatch.rs::publish_document_changed`) and
rejected it because the bus subscription preserves
decoupling without the per-session-actor scaling penalty.

The choice tracks paramount goals: **#1 performance**
(O(1+dependents) per edit), **#4 asynchronicity** (one
drainer task + lazy debouncer tasks; no App orchestration),
**#2 extensibility** (new source types / consumer types
compose without touching routing).

#### 3.4.5 `SessionKey` — `BufferId` for v1, extension path noted

D.2.a's drafts called for an opaque `DiffSessionId`; D.2.a
collapsed to `BufferId` once the consumer surface was clear —
one session per diffed buffer holds for inline overlay,
side-by-side (each pane = own buffer), three-way (three
buffers, three sessions), and multibuffer (one session keyed
by the multibuffer document's own buffer id). If a later
slice needs ephemeral diff previews unattached to any
registered buffer — or multiple sessions per buffer — we
re-introduce the opaque id then; the routing structures all
key on a `SessionKey` type alias that's currently `= BufferId`
and would swap to an opaque newtype without touching the
inverse index.

#### 3.4.6 Drop, supersede, RCU semantics

- `drop_session(buffer_id)` clears the entry from all four
  maps and aborts the in-flight debouncer task if any.
  Removes the session's watcher entries from each watched
  buffer's bucket. In-flight `Arc<DiffSession>` holders stay
  coherent (RCU); the registry's job is naming, not lifetime
  enforcement. This satisfies the Claude `openDiff` flow: the
  diff outlives any one view because consumers hold their own
  `Arc` clone.
- Supersede policy on overlapping recomputes (D.2.b): no
  abort — both runs complete; the gated publish
  (`try_publish_if_newer`, strict greater-than on revision)
  drops the stale result. The debouncer eliminates most
  redundant spawns *before* the blocking pool sees them.
- Bus subscription lifecycle (D.2.c): `bind(bus, resolver)`
  returns a `DiffSubscriptionGuard`. The guard's `Drop`
  unsubscribes the bus subscription and aborts the drainer
  task. Hosts hold the guard for the editor's lifetime;
  tests drop it to verify cleanup.

The subsystem exposes a small synchronous API to the host:
register a session with sources, query hunks, request
recompute, apply a hunk (`do` / `dp`), close. Ex-commands
(`:diff`, `:diffthis`, `:diff-accept`, `:diff-reject`,
`:Gdiff`) and the future WIT plugin surface for Claude Code
IDE integration are all consumers of this API.

Per saved feedback *"Modes own their buffers, App is a host"*
the subsystem follows the same pattern: storage on the
subsystem, behavior on the subsystem's API. App does not gain
`ensure_diff_session_for` or `drain_diff_events` shims. The
inverse index lives on the subsystem, not on App; the bus
subscription is owned by the subsystem; nothing about the
routing leaks into dispatch.

## 4. The engine

`imara-diff` is the Rust diff library used by `gitoxide`.
Histogram (git's default since 2.7), Myers, and Myers-Minimal
are all supported; we default to Histogram and expose
`diff.algorithm` as a typed option (§5.12). (Patience is
*not* in `imara-diff`'s surface — listed in early drafts of
this design as a vim-`diffopt` parity choice, dropped from D.1
in favour of the engine's actual surface.)

```toml
# options.toml
[diff]
algorithm = "histogram"  # histogram | myers | myers-minimal
debounce-ms = 40         # debounce per session's edit stream
context-lines = 3        # cosmetic context for unified output
```

The library is fast enough that for files in the v1 size band
(P95 file: 5k lines / 200KB; see §8 of `design.md`) recompute
is a sub-millisecond operation, well inside the per-session
debounce. The bench gate (§7) makes this enforceable instead
of aspirational.

The engine runs on `spawn_blocking` — never on the dispatcher
task, never on the UI thread. The recomputed `HunkIndex` is
published via `ArcSwap::store(Arc::new(idx))`. Renderers see
the new index on their next snapshot read; the publish-vs-read
ordering is the same `arc-swap` RCU pattern §5.6.8 commits to.

## 5. Foundational primitives — D.0

Two primitives this design requires that don't exist today.
Both are reusable beyond diff, which justifies landing them
under their own slice (D.0) before any diff-specific work.

### 5.1 Displacing virtual rows

The renderer today supports floating overlays (popup, picker,
completion) that paint **over** content. Inline diff requires
virtual rows that **displace** content — a deletion block above
an addition pushes subsequent rows down. The cell-grid
renderer's row vector becomes "document rows interleaved with
virtual rows," and motions / scroll / cursor positioning all
have to honour the interleaving.

**Full design in [`virtual-rows.md`](virtual-rows.md).** The
pure data + interleaver layer (`VirtualRow`,
`VirtualRowMatrix`, `VirtualRowProvider`, `DisplayRowEntry`,
`CellMatrix::display_slice`) shipped in D.0a (2026-05-28).
D.3 below is the first production consumer.

This primitive is shared with:

- Inline diff deletion blocks (this design).
- Multibuffer excerpt headers and separators
  (`multibuffer-views.md`).
- LSP inlay hints (post-v1, but the primitive is the same).
- LSP code-lens above the function it annotates.
- Inline signature-help preview during Insert.

D.0 lands the primitive **without a consumer** (the primitive's
test suite is its consumer for that slice). D.1+ light it up
for diff; multibuffer slices light it up for excerpts; inlay /
codelens / signature-preview consume it later.

The shape:

```rust
pub struct VirtualRow {
	pub anchor: DocumentRow,         // row this attaches to
	pub position: AnchorPosition,    // Above | Below
	pub content: VirtualRowContent,
	pub height: u16,                 // typically 1
}

pub enum AnchorPosition {
	Above,    // displaces rows ≥ anchor downward
	Below,    // displaces rows > anchor downward
}
```

The renderer's row iterator interleaves virtual rows with
document rows in `DocumentRow` order; the row-translation
table (`DiffMap::rows`, multibuffer's `ExcerptMap::rows`) is
the cache that lets the iterator do this in O(log n).

### 5.2 Scroll-binding

Side-by-side diff requires panes that scroll together — the
user scrolls one and the other follows in lockstep, with row
correspondence respecting the hunk alignment (not naive
row-N-to-row-N). This primitive is shared with:

- Side-by-side diff (this design).
- Three-pane merge (this design).
- Vim's `:set scrollbind` / `:set cursorbind`.
- `:windo` (concurrent operation across panes).
- A future zen-mode "follow this pane" affordance.

D.0 lands a pane-group abstraction with a pluggable row-mapping
function. Diff's row mapping consults the active
`HunkIndex` to map hunk-correspondent rows; the
default (vim's `scrollbind`) maps row-N to row-N.

## 6. Grammar surface

Every diff-related affordance registers through
`CommandRegistry`. Nothing is a hard-coded keybinding outside
the keymap.

### 6.1 Motions

| Surface | Registered as | Vim equivalent | Behaviour |
|---|---|---|---|
| `]c` | motion: next hunk | `]c` | Move cursor to start of next `Hunk` in current document's row range |
| `[c` | motion: prev hunk | `[c` | Move cursor to start of previous `Hunk` |

Both are **motions**, not commands — `d]c` (delete through next
hunk), `c]c` (change through next hunk), `v]c` (visual-select
through next hunk) all compose for free.

### 6.2 Operators

| Surface | Registered as | Vim equivalent | Behaviour |
|---|---|---|---|
| `do` | operator: diff-get | `do` | Pull hunk under cursor from the other side (two-way) or specified side (three-way: `:diffget <bufnr>`) |
| `dp` | operator: diff-put | `dp` | Push hunk under cursor to the other side / specified side |

Both translate to an `ApplyEdit` through the standard edit
pipeline. The diff session's edit-event subscription causes a
recompute on the next debounce tick, which naturally updates
the hunk list. No bespoke "remove this hunk from the index"
code path — the edit invalidates and recomputes uniformly.

### 6.3 Ex-commands

| Command | Behaviour |
|---|---|
| `:diff <buf>` | Open side-by-side diff of current buffer vs `<buf>` |
| `:diffthis` | Mark current buffer as participating; combine with peer buffers via additional `:diffthis` calls |
| `:diffoff` | Drop diff state on current buffer |
| `:diffsplit <file>` | Split-load `<file>` and run `:diff` against it |
| `:diffput [bufnr]` | Push hunk under cursor; `bufnr` mandatory in three-way |
| `:diffget [bufnr]` | Pull hunk under cursor |
| `:diff-mode <inline\|split\|three-way>` | Switch presentation mode on the active diff session |
| `:diff-accept` | Apply remaining hunks and close session (used by `openDiff` flow) |
| `:diff-reject` | Discard session without applying (used by `openDiff` flow) |
| `:diff-algorithm <histogram\|myers\|myers-minimal>` | Set engine for this session |
| `:Gdiff [<ref>]` | (post-D.7) diff current buffer vs git ref (default `HEAD`) |
| `:describe-diff` | Open help buffer showing active sessions, algorithms, hunk counts |

Per saved feedback *"Use dashed naming for ex-commands"*,
multi-word commands are dashed (`diff-mode`, `diff-accept`,
`diff-algorithm`). Single-word vim-compatible commands
(`diffthis`, `diffsplit`, `diffput`, `diffget`) preserve vim's
spelling — they're already canonical names users expect to
type.

### 6.4 Options

Registered against the typed-options system (§5.12):

- `diff.algorithm` — `histogram | myers | myers-minimal`,
  default `histogram`. (`imara-diff` exposes these three;
  Patience is not in the engine's surface so it's not
  available — landed D.1 2026-05-28.)
- `diff.debounce-ms` — `u32`, default 40.
- `diff.context-lines` — `u32`, default 3.
- `diff.presentation` — `inline | split | three-way`, default
  `inline`; per-session override available.
- `diff.scroll-bind` — `bool`, default `true`. Controls
  scroll-locking in side-by-side mode.
- `diff.cursor-bind` — `bool`, default `false`. Optional
  cursor-position lock between panes.
- `diff.fill-char` — `string` (one char), default `"-"`. The
  visual fill in filler rows.

## 7. Performance posture

- **Hot path (per-keystroke):** Unchanged for buffers not
  participating in any diff. For participating buffers, one
  edit dispatch fires one event subscription handler, which
  schedules a debounced recompute and returns. Net per-edit
  overhead: one mpsc send, one timer-arm — sub-microsecond.
- **Recompute:** `imara-diff` Histogram on the v1 P95 file
  (5k lines / 200KB) completes in well under 1ms on
  off-thread. Bench gate `diff_recompute_p99_us` at the v1
  P95 file size, CI gate ≤ 1000µs. Stress observation at
  50k lines × 200 cols; if it violates 10ms p99 the engine
  switches to streaming-hunks (publish partial index after
  each n-hunk batch).
- **Publish:** `ArcSwap::store(Arc<HunkIndex>)`. The renderer
  reads the new arc on its next snapshot fetch — no
  cross-thread sync past the load-acquire / store-release pair
  arc-swap already provides.
- **Row translation:** `RowTranslation` is cached per-pane and
  invalidated when the session's HunkIndex revision changes
  or the document revision changes. Translation lookup is
  O(log n) over the hunk list (binary search on row).
  Bench gate `diff_row_lookup_p99_ns` ≤ 500ns at 1k hunks.
- **Side-by-side scroll:** Scroll-binding consults the
  `HunkIndex` to map between panes' row positions. With cache,
  this is O(log n) per scroll event. Bench gate
  `diff_scroll_bind_p99_us` ≤ 50µs.
- **Inline deletion block render cost:** The renderer
  treats virtual rows the same as document rows for layout +
  shaping. No additional cost beyond proportional to the
  number of deleted lines visible on screen.
- **Memory:** One `Arc<HunkIndex>` per session. At 1k hunks
  with `SmallVec<[LineRange; 3]>` inline, ~50KB. Negligible.
  Baseline documents for single-doc sessions cost their rope
  size — bounded by the file size.

## 8. Open questions

- **Word-level intra-line diff** in change hunks. vim's
  `DiffText` highlights the changed runs inside a line; we
  want this. `imara-diff` can give intra-line diff via
  recursive application on the changed lines. Cost is
  bounded; bench gate to validate. Decide before D.3.
- **Conflict-region semantics for three-way.** "Conflict"
  means "changed differently in 2+ documents" — but the exact
  boundaries (per-line vs per-block vs per-change-region) are
  implementation-defined. Lean on git's conflict-marker
  semantics (per-region) for compatibility; revisit if it
  hurts merge UX. Decide before D.6.
- **Folding of equal regions.** vim defaults to folding
  unchanged regions in `:diffsplit`. Useful for long files
  with localised changes. Adds slice cost (fold provider
  driven by hunk index). Lean defer to D.7 polish; ship D.4
  without auto-fold.
- **What happens to a diff session when a participating
  document is closed?** Options: (a) session auto-closes
  with a banner; (b) session goes dormant until the document
  reopens; (c) session retains the document's last rope as
  a baseline and continues. Lean (a) — explicit, no surprise
  state. Decide before D.2.

## 9. Slice plan

Each slice ships green-on-merge with the four artefacts
CLAUDE.md mandates: architecture documentation (this doc,
updated as needed), benchmark coverage where load-bearing,
test coverage of the new scenarios + failure modes, graceful
error handling (log + skip on recoverable failures, never
panic on the hot path, never swallow silently).

| Slice   | Title                                     | What lands                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
|---------|-------------------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| **D.0** | Foundational primitives                   | (a) Displacing virtual-row primitive on the cell-grid renderer: `VirtualRow` type, row-translation cache, renderer iteration interleaving virtual + document rows, motion / scroll / cursor honour the interleaving. (b) Scroll-binding pane-group primitive: `PaneGroup` with pluggable row-mapping function; default mapping = identity. Tests: virtual-row insert / remove / re-anchor on document edit; cursor lands correctly when typing past a virtual row; scroll-bind round-trip with identity mapping preserves cursor position. Benches: `virtual_row_layout_p99_us` at 1k virtual rows; `scroll_bind_dispatch_p99_us` at 100 paired panes. No diff-specific consumer yet — the primitives stand alone. |
| **D.1** | `lattice-diff` crate                      | Pure crate. `Hunk` / `HunkIndex` types; `compute_two_way(a, b)` / `compute_three_way(base, a, b) -> HunkIndex` via `imara-diff`. Algorithm enum + per-call selector. Property tests: round-trip via patch apply for randomly-generated rope pairs; conflict detection on three-way matches git's behaviour on a set of known cases. Bench: `diff_recompute_p99_us` at v1 P95 file (5k × 80 cols); stress at 50k × 200. No host integration yet.                                                                                                                                                                                                                                                                    |
| **D.2** | `DiffSubsystem` + `DiffSession` lifecycle | New subsystem on `lattice-host`. `DiffSession` open / close / debounced recompute / RCU publish via `ArcSwap`. Subscriptions to document edit events through the existing event bus (§5.10). `:describe-diff` ex-command lists active sessions. No rendering yet — assert observable behaviour by reading `HunkIndex` after mutations. Tests: open session, edit doc, observe new revision; close session releases subscriptions; document-close auto-closes session per §8 decision; subsystem survives a panicking recompute task (one session degraded, others unaffected). **Carved during build:** D.2.a (registry skeleton, ✅), D.2.b (`spawn_blocking` compute + revision-gated publish, ✅), D.2.c (routing + lazy debouncer + bus subscription per §3.4, in progress), D.2.d (`:describe-diff`), D.2.e (bench wiring). See `docs/dev/operations/implementation.md` for slice-by-slice status. |
| **D.3** | Inline overlay presentation               | First consumer of D.0's virtual-row primitive. `DiffMap` in `PresentationMode::Inline` for the single-doc-vs-baseline case (file vs. disk content as baseline — easiest baseline to source). Hunk decorations: gutter signs (sprite, reuses §5.6.7 atlas), line-background tints (`DiffAdd` / `DiffChange` / `DiffRemove` theme entries), virtual-row deletion blocks. `]c` / `[c` motions registered. End-to-end test: open a file, edit it, observe gutter signs and inline deletion blocks; `]c` jumps between hunks. Bench: `diff_render_p99_us` at 100 visible hunks.                                                                                                                                         |
| **D.4** | Side-by-side two-way presentation         | Second consumer of D.0 (scroll-bind primitive). `:diff <buf>`, `:diffthis`, `:diffsplit <file>`, `:diffoff` ex-commands. `PresentationMode::SideBySide` for two panes; pane-group scroll-binding consults `HunkIndex` for hunk-correspondent row mapping. Filler virtual rows on both sides to align parallel hunks. `[c` / `]c` motions work uniformly. Tests: two files diffed render correctly; scroll one pane, other follows; filler rows align hunks; `:diffoff` cleanly drops state on both panes.                                                                                                                                                                                                          |
| **D.5** | Hunk transfer operators (`do` / `dp`)     | Operators registered through `CommandRegistry`. Translate to `ApplyEdit` against the receiving document; edit fires recompute through the standard pipeline. Tests: `dp` on a Change hunk replaces the other document's range; undo composes correctly; macros record and replay correctly; recompute fires once after the edit settles.                                                                                                                                                                                                                                                                                                                                                                           |
| **D.6** | Three-way merge presentation              | `PresentationMode::SideBySide { pane_index: 0/1/2 }` for three panes. `HunkKind::Conflict` populated by `compute_three_way`. Conflict-region rendering uses a distinct theme entry (`DiffConflict`). `:diffput <bufnr>` / `:diffget <bufnr>` honour the disambiguating argument. `:diff-accept` / `:diff-reject` ex-commands for resolving the session. Tests: three documents with non-overlapping changes show two non-conflict hunks; three documents with overlapping changes show one conflict hunk; `:diffput 2` resolves a conflict by pushing pane 1's version.                                                                                                                                            |
| **D.7** | Git baseline integration (`:Gdiff`)       | New small `lattice-vcs` crate (or extension of an existing one) providing `git_baseline(path, ref) -> Rope`. `:Gdiff [<ref>]` opens a single-doc inline session against the git baseline. Uses `gix` (gitoxide) for read-only access; no working-copy mutation. Tests: dirty file shows hunks vs HEAD; clean file shows no hunks; non-git paths return a clear error via the existing diagnostic banner.                                                                                                                                                                                                                                                                                                           |

After D.7, two consumer flows compose on top of this subsystem
without further changes to the diff layer:

- **Claude Code `openDiff` (single-file)** — synthesises a
  baseline Document from the proposed text and opens an
  inline D.3-style session; waits on the
  `DiffSession::completion` oneshot for `Accept` / `Reject`.
  Lands as part of the Claude Code IDE plugin work,
  independent of the diff slices.
- **Project-wide diff** and **AI multi-file `openDiff`** —
  compose with `multibuffer-views.md`'s `MultibufferDocument`
  and an `AIProposedEditsProvider` / `ProjectDiffProvider`.
  Each excerpt's diff state is one `DiffSession` and one
  inline `DiffMap`; the multibuffer composes them. Lands as
  part of those provider slices.

Slice sequencing:

- **D.0** is foundational and reusable beyond diff. Land
  green standalone before D.3.
- **D.1** has no dependency on D.0 — it's a pure crate. Can
  land in parallel with D.0.
- **D.2** depends on D.1 only.
- **D.3** depends on D.0 (a) + D.2.
- **D.4** depends on D.0 (b) + D.3.
- **D.5** depends on D.2 only (no presentation dependency).
- **D.6** depends on D.4.
- **D.7** depends on D.3.

## 10. Testing strategy

- **Unit tests in `lattice-diff`:** `Hunk` algebra (Add /
  Remove / Change classification); round-trip property via
  patch-apply for random rope pairs; algorithm equivalence
  on a fixed corpus (per-algorithm hunk lists match recorded
  golden values); three-way conflict detection against git's
  output on a known corpus.
- **Unit tests in `lattice-host`:** Session lifecycle (open /
  close / auto-close on doc-close); subscription cleanup; RCU
  publish ordering (renderer always sees a coherent
  HunkIndex); recompute debounce (rapid edits coalesce into
  one recompute).
- **Integration tests via the headless host:** `:diffthis` /
  `:diffsplit` end-to-end against pairs of files in
  `lattice-tests/fixtures/diff/`; `]c` / `[c` / `do` / `dp`
  produce expected cursor / document state; `:diff-accept` /
  `:diff-reject` close the session and route the completion
  signal.
- **Renderer tests (TUI + GPUI):** Virtual-row interleaving
  paints correctly; filler rows align; gutter signs anchored
  to correct rows; conflict regions render distinctly from
  Change hunks.
- **Bench:**
  - `diff_recompute_p99_us` (D.1) — CI gate ≤ 1000µs at v1
	P95 file size.
  - `diff_row_lookup_p99_ns` (D.0/D.3) — CI gate ≤ 500ns at
	1k hunks.
  - `diff_render_p99_us` (D.3) — CI gate ≤ 200µs at 100
	visible hunks (sub-frame at 60Hz).
  - `diff_scroll_bind_p99_us` (D.4) — CI gate ≤ 50µs at 1k
	hunks.
  - `virtual_row_layout_p99_us` (D.0) — CI gate ≤ 100µs at
	1k virtual rows.
- **Stress observations** (not gated, recorded in
  `benchmarks.md`): 50k × 200 file pair (~10MB each);
  large-conflict three-way merge with 100 conflict regions.

## 11. Risks

- **Virtual-row primitive (D.0) is the riskiest slice.** It
  touches the renderer's row iteration, cursor positioning,
  scroll math, and every motion that walks rows. Worth doing
  in isolation with full bench + test coverage before D.3
  builds on it. The slice must show no regression on the
  baseline `editor_render_p99_us` bench (the primary
  paramount-goal-#1 gate) for documents with zero virtual
  rows — the fast path stays fast.
- **Scroll-binding row mapping under live edits.** When two
  panes are scroll-bound and the user edits one, the
  HunkIndex is in-flight (debounced recompute). The pane
  group must handle the gap gracefully — fall back to the
  last-known mapping until the new HunkIndex publishes, not
  jitter or hang. Explicit test case in D.4.
- **Three-way conflict UX drift from git.** Users have a
  strong mental model of conflict markers from git CLI. If
  Lattice's conflict regions disagree with `git status` on
  the same merge, users will hit confusion. The corpus tests
  in D.1 (against git's known output) catch this; the manual
  workflow during D.6 needs a "diff this Lattice merge
  against git's `<<<<<<<` markers" verification step.
- **Multi-doc edit-event coordination.** A three-way session
  subscribes to three documents' edit streams. If two
  documents edit "simultaneously" (within the debounce
  window), one recompute should reflect both. The session's
  debounce timer must reset on each event, not fire per-event
  — explicit test for this concurrency case in D.6.
- **Engine choice locked in.** `imara-diff` is the right
  default but if it later proves a poor fit (perf, edge
  cases, license, maintenance), the recompute path is the
  only piece that swaps. The `DiffAlgorithm` enum and the
  `compute_*` function signatures don't leak engine
  internals.

## 12. Cross-references

- `design.md` §5.13 — synopsis paragraph linking here.
- `design.md` §5.2.1 — unified command / grammar dispatch;
  the registered motions / operators / ex-commands in §6 live
  in this dispatcher.
- `design.md` §5.6.2 — `EditorRenderer` layered fast paths;
  the inline overlay's virtual rows hook in here.
- `design.md` §5.6.7 — iconography and sprites; gutter signs
  reuse the existing sprite-atlas pipeline.
- `design.md` §5.6.8 — render-snapshot coherence; the diff
  subsystem's RCU publish follows the same arc-swap contract.
- `design.md` §5.9.5 — gutter columns layout; diff column
  takes its declared place (left of line numbers, right of
  diagnostic markers per §5.6.7 ordering).
- `design.md` §5.10 — event system; diff sessions subscribe
  to `DocumentEdited`.
- `design.md` §5.12 — typed options; diff options registered
  there.
- `multibuffer-views.md` — the orthogonal aggregator that
  composes with this subsystem to deliver project-wide diff,
  AI multi-file `openDiff`, and similar flows. Diff and
  multibuffer are independent designs that meet at their
  consumers, not at their implementations.
- `actor-seam-discipline.md` — the actor + arc-swap publish
  pattern this subsystem inherits.
- `implementation.md` — `## diff-system` ledger tracks
  D.0–D.7 slice status as they land.
