+++
title = "N-way Diff Membership (D.8)"
+++


**Status:** design, awaiting implementation. Successor to D.6's
fixed-arity (two-way / three-way) descriptor shape. The "what" + "why"
lives here; the "when + in what order" lives in
`docs/dev/operations/slice-plans/diff-system.md` (D.8 row).

## 1. Why D.8 exists

Three reinforcing pressures came out of D.6:

1. **Vim semantics for `:diffthis` are not "two-phase staging."** Each
   `:diffthis` invocation flips a per-buffer `:set diff` bit; the diff
   session is derived state — every buffer in `:set diff` participates
   together, with arity inferred from membership. The D.4.d.3.a
   `pending_diffthis: Option<PaneGroupMember>` singleton is an
   accidental staging mental model that doesn't generalise past 2 and
   doesn't match user expectations.
2. **`DiffDescriptor`'s explicit `baseline + current + remote` slots
   leak arity into the API.** Today's descriptor knows its shape (2 or
   3). The user clarified during D.6.c:
   *"the api should not be designed to know the arity, the session /
   group will be the source of truth for that."*
   The descriptor should carry N opaque sources; the engine layer
   decides how to diff them.
3. **Magit-style plugins need a generic consumption surface.** When
   the magit plugin (per [`magit.md`](../magit/)) consumes
   `DiffSubsystem`, branching on "two-way / three-way" through
   `is_three_way()` would force the plugin to reimplement the same
   arity dispatch the engine already does. One arity-agnostic API at
   each layer.

D.8 is the refactor that flips the diff stack from "fixed shape with
optional remote" to "N participants, engine dispatches on N".

## 2. Paramount-goal alignment

| Goal | How D.8 protects it |
|---|---|
| **#1 Performance** | Per-keystroke overhead unchanged. The hot path is `try_publish_if_newer` + `current_hunks().load_full()`; neither cares about descriptor shape. Refactor is purely structural. |
| **#2 Extensibility** | The new descriptor shape (`Vec<Arc<dyn DiffParticipantSource>>`) is the surface a future magit plugin consumes. No "and-also" branching by arity. |
| **#3 Modal vim editing** | `:diffthis` finally matches vim's per-buffer `:set diff` semantics. Removes the off-spec staging mental model. |
| **#4 Asynchronicity** | Subsystem-on-host + RCU publish unchanged. `add_participant` / `remove_participant` mutate the descriptor under the existing mutex; the published `HunkIndex` is RCU-replaced as before. |

## 3. Data model

### 3.1 The unified participant trait

Collapse today's `BaselineSource` + `CurrentSource` (both shape
`snapshot() -> Rope`) into one:

```rust
pub trait DiffParticipantSource: Send + Sync + 'static + std::fmt::Debug {
    /// Produce the rope this participant contributes to the diff.
    /// Called once per recompute from inside `spawn_blocking`.
    fn snapshot(&self) -> Rope;
}
```

The trait split was always structural sugar (it made
`descriptor.baseline.snapshot()` vs `descriptor.current.snapshot()`
visually distinct). With N participants the visual cue is the slot
index, not the trait. Existing concrete impls move over without
behavioural change:

- `StaticBaseline` → `StaticSource`
- `OnDiskBaseline` → `OnDiskSource`
- `BufferBaseline` + `BufferCurrentSource` → unified `BufferSource`
- D.7's `GitBaseline` → `GitSource`

Old type names stay as `type BaselineSource = dyn DiffParticipantSource`
aliases for one release cycle, then removed.

### 3.2 The descriptor

```rust
pub struct DiffDescriptor {
    /// N participants in slot order. `sources[i]` produces the rope
    /// at `Hunk::ranges[i]`. v1 supports N ∈ {1, 2, 3}; the engine
    /// returns a typed error for N ≥ 4.
    pub sources: Vec<Arc<dyn DiffParticipantSource>>,

    /// Buffers whose edits should wake this session. Subset of
    /// participants for buffer-backed sources; static / file-on-disk
    /// / git sources contribute nothing. Explicit dependency
    /// declaration — sources don't self-introspect.
    pub watch: Vec<BufferId>,

    /// User-visible diff sides that receive `diff-mode` activation
    /// via the bridge. Subset of participants for buffer-backed
    /// sources. Refcounted per buffer so a buffer in two sessions
    /// stays diff-mode-active until both close.
    pub participants: Vec<BufferId>,
}

impl DiffDescriptor {
    /// Number of participants — the source of truth for arity.
    /// Replaces D.6.a's `is_three_way()`.
    pub fn arity(&self) -> usize { self.sources.len() }
}
```

**Invariants** (debug-checked at register-time):
- `sources.len() >= 1`
- `participants.len() <= sources.len()` (participants is a subset)
- Every `BufferId` in `participants` appears at most once
- Every `BufferId` in `watch` appears at most once

### 3.3 Hunks

`Hunk::ranges` already carries `SmallVec<[LineRange; 3]>` — fits N=1,
2, 3 inline; spills to heap for any N≥4. No type change.

For a session of arity N, every `Hunk` produced by the engine carries
exactly N ranges. Renderers + mappers index by slot.

## 4. Engine — `lattice-diff`

### 4.1 The new entry point — the *only* public surface

```rust
pub fn compute_diff(
    sources: &[Rope],
    algorithm: DiffAlgorithm,
) -> Result<HunkIndex, DiffEngineError> {
    match sources.len() {
        0 => Err(DiffEngineError::Empty),
        1 => Ok(HunkIndex::empty(algorithm)),     // dormant — no peers
        2 => Ok(two_way(&sources[0], &sources[1], algorithm)),
        3 => Ok(three_way(&sources[0], &sources[1], &sources[2], algorithm)),
        n => Err(DiffEngineError::Unsupported { n }),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DiffEngineError {
    #[error("diff requires at least one participant")]
    Empty,
    #[error("v1 supports N ≤ 3; got N = {n}")]
    Unsupported { n: usize },
}
```

**`compute_diff` is the only public engine entry point.** The
existing `compute_two_way` and `compute_three_way` get **demoted to
crate-private helpers** (renamed `two_way` / `three_way` to make the
internal-only status visible) called only from inside `compute_diff`.
Removing them from the public surface enforces the API discipline:
all consumers (subsystem, plugin, tests) go through `compute_diff` and
let the engine pick the algorithm by arity. No external code branches
on "is this 2-way or 3-way" — that decision lives in one place.

Engine-internal tests can still call `two_way` / `three_way` directly
(they're in the same crate). External crates (`lattice-host`,
benches) consume `compute_diff` exclusively.

**Why a `Result`:** the v1 cap (N ≤ 3) lives in the engine, not the
descriptor (per the user's D.6.c clarification). Plugins / future
extensions can probe by attempting `compute_diff` and matching on
`Unsupported`; the API doesn't have to grow a `max_supported_arity()`
constant. When the cap moves (e.g., post-Phase-7 generalised pairwise
engine), only the `compute_diff` body changes.

### 4.2 N=1 dormant sessions

A session with one participant produces no hunks (nothing to diff
against). The session is **dormant** — registered, has a debouncer,
mode-bridge refcount = 1 on the participant, but `current_hunks()`
returns the empty `HunkIndex`. This is the state after the user's
first `:diffthis` and before the second.

Vim parity: `:set diff` on a single buffer with no other `:set diff`
buffers is a no-op visually.

## 5. Subsystem — membership API

### 5.1 New surface

```rust
impl DiffSubsystem {
    /// Add a participant to an existing session. Appends to
    /// `descriptor.sources` and (optionally) `participants`.
    /// Triggers a recompute through the existing debouncer.
    /// Returns the new arity after the add.
    pub fn add_participant(
        &self,
        session_key: BufferId,
        source: Arc<dyn DiffParticipantSource>,
        participant_buffer: Option<BufferId>,
    ) -> Result<usize, MembershipError>;

    /// Remove a participant by slot index (0-based). Subsequent
    /// slot indices shift down by 1.
    /// If arity drops below 2, the session enters dormant state
    /// (continues to exist, publishes empty `HunkIndex`); if
    /// arity drops to 0, the session is auto-dropped.
    /// Returns the new arity after the remove.
    pub fn remove_participant(
        &self,
        session_key: BufferId,
        slot: usize,
    ) -> Result<usize, MembershipError>;

    /// Convenience: remove the slot whose participant buffer is
    /// `buffer_id`. Looks up the slot first; errors if `buffer_id`
    /// isn't a participant.
    pub fn remove_participant_buffer(
        &self,
        session_key: BufferId,
        buffer_id: BufferId,
    ) -> Result<usize, MembershipError>;

    /// Replace the entire descriptor — equivalent to
    /// `drop_session` + `register_with_sources` but preserves
    /// session identity (the `Arc<DiffSession>` stays). Used by
    /// `:diffsplit` to atomically transition a session from 1
    /// participant to 2.
    pub fn replace_descriptor(
        &self,
        session_key: BufferId,
        descriptor: DiffDescriptor,
    ) -> Result<(), MembershipError>;
}

#[derive(Debug, thiserror::Error)]
pub enum MembershipError {
    #[error("no diff session registered for buffer {0:?}")]
    NoSession(BufferId),
    #[error("slot {slot} out of range; session arity = {arity}")]
    SlotOutOfRange { slot: usize, arity: usize },
    #[error("buffer {0:?} is not a participant of this session")]
    NotParticipant(BufferId),
    #[error("engine rejected new arity: {0}")]
    EngineRejected(#[from] DiffEngineError),
}
```

`MembershipError::EngineRejected` fires if a fourth `:diffthis` push
takes arity to 4. Surfaces as a clear user-facing message
("v1 supports up to 3 buffers in a diff group; got 4").

### 5.2 What happens to mode-bridge refcount

The diff-mode bridge (D.5.a) ref-counts per `(buffer_id, session)`.
Add/remove flow through the bridge:

- `add_participant(session, source, Some(buf))` →
  `bridge.note_session_extended(session, buf)` increments refcount.
- `remove_participant_buffer(session, buf)` →
  `bridge.note_session_shrunk(session, buf)` decrements; when refcount
  hits 0 the bridge flips `ActiveModes.diff-mode` off on that buffer.

The existing `note_session_opened` / `note_session_closed` stay for
the wholesale-create / wholesale-drop paths (`:diffsplit` /
`:diff-accept`).

### 5.3 Routing — `watchers` index updates

`add_participant` with a buffer-backed source mutates `descriptor.watch`
to include the new buffer; the `watchers` inverse index gains the new
entry. `remove_participant` scrubs it. Mutation under the existing
subsystem mutex.

### 5.4 Multi-session shared buffer revisited

Today's `secondary_index: HashMap<BufferId, BufferId>` (single-valued)
is already wrong for the multi-session case — D.6.g's
`all_sessions_for(buffer_id)` worked around it by iterating
`descriptors`. D.8 doesn't need to fix the secondary index (the
workaround stays); but a future cleanup could widen it to
`HashMap<BufferId, SmallVec<[BufferId; 2]>>`. Out of scope for D.8.

## 6. `:diffthis` semantics (vim parity)

### 6.1 Model — the `:diffthis` group is a singleton, isolated from
### other sessions

There's no "staging." Each `:diffthis` toggles the active buffer's
membership in **the** `:diffthis` group — a single named group that's
the *only* session `:diffthis` ever touches. Other diff sessions
created through other entry points (`:diffsplit`, AI-driven flows
like `openDiff`, future magit-driven sessions) are **entirely
separate** — `:diffthis` never adds to them, never reads from them.

This resolves the "which group does `:diffthis` mean?" ambiguity by
collapsing the question: there's exactly one diffthis group at any
time, and only `:diffthis` and per-buffer `:diffoff` mutate it. The
editor tracks it as:

```rust
pub struct Editor {
    /// Session key of the `:diffthis` group, if any.
    /// **Only** `:diffthis` and per-buffer `:diffoff` touch this
    /// field. `:diffsplit`, AI workflows, magit-driven sessions
    /// run as independent `DiffSession`s and do not affect this.
    /// `None` means no diffthis group exists; the first `:diffthis`
    /// creates one and assigns its session key here. Last-buffer
    /// removal (via `:diffoff`) drops the group and clears back to
    /// `None`.
    pub diffthis_group: Option<BufferId>,
    // ... (existing fields)
}
```

Drop `pub pending_diffthis: Option<PaneGroupMember>` entirely.

**A buffer can participate in multiple sessions simultaneously** —
e.g., the user might have a `:diffsplit base remote` session running
AND a `:diffthis` group going. The buffer's diff-mode bridge keeps
refcount across both. `:diffthis` only mutates the diffthis group;
`:diffoff!` (cascade per D.6.g) is the only command that touches
every session a buffer participates in.

### 6.2 State transitions

Let A = active pane's buffer; G = `diffthis_group` (the singleton).
Sessions A is in via other entry points (`:diffsplit`, AI, magit)
are **invisible to `:diffthis`** — they're never read or mutated.

| State | `:diffthis` action |
|---|---|
| G is None | Create 1-participant dormant session keyed under A; set G = A; activate diff-mode on A. |
| G is Some(g), A is in session g | Remove A from session g. Membership shrinks. If g's arity drops to 0, drop the session and set G = None; otherwise (arity ≥ 1) g stays — if arity drops below 2 it becomes dormant. |
| G is Some(g), A is NOT in session g | Add A to session g via `add_participant`. Arity grows; if it crosses 2 the engine fires its first recompute. |

The fourth-`:diffthis` case (would push G's arity to 4) returns
`MembershipError::EngineRejected` which dispatch surfaces as
"diffthis: v1 supports up to 3 buffers in a diff group".

Whether A also participates in OTHER sessions (e.g. it's the local
side of a separate `:diffsplit base remote`) is irrelevant to
`:diffthis` — those sessions are not even looked up.

### 6.3 `:diffoff` per-buffer

`:diffoff` removes the active buffer from its session — opposite of
`:diffthis` for an already-in-session buffer. v1's "tear down whole
session" semantic moves to `:diffoff!` (the bang).

This is a semantic shift from D.6.g:
- **Pre-D.8 (D.6.g)**: `:diffoff` tears down the active buffer's
  session; `:diffoff!` cascades across multi-session shared buffers.
- **Post-D.8**: `:diffoff` removes the active buffer from its
  session (which may auto-drop if arity falls below 2); `:diffoff!`
  tears down every session the active buffer is in (cascade
  semantic preserved).

Vim parity check: vim's `:diffoff` is "turn off :set diff for the
current window/buffer," which matches the post-D.8 per-buffer
semantic. v1's "whole session collapses" was an accidental
simplification of fixed-arity two-pane sessions where removing one
side always meant collapsing.

### 6.4 `:diffsplit` composition

`:diffsplit <file>` (1-arg) and `:diffsplit <base> <remote>` (2-arg)
keep their user-facing semantic. Internally:

- 1-arg: opens new pane, loads `<file>`, calls
  `register_with_sources` with `sources = [BufferSource(new),
  BufferSource(active)]` — arity 2. Same observable result as today.
- 2-arg: opens two new panes, calls `register_with_sources` with
  `sources = [BufferSource(base), BufferSource(active),
  BufferSource(remote)]` — arity 3. Same observable result as today.

The existing `register_two_pane_diff` / `register_three_pane_diff`
helpers in dispatch.rs collapse into one `register_pane_group_diff(
members: Vec<PaneGroupMember>)` helper that derives `sources` from
the member list.

### 6.5 Concrete examples

**Three buffers, all unsaved:**

```
:e scratch-a              " create buffer A
:vsplit | :e scratch-b    " create buffer B in pane 2
:vsplit | :e scratch-c    " create buffer C in pane 3
:wincmd h | :wincmd h     " back to pane 1 (A)
:diffthis                 " session created, dormant, G = A
:wincmd l | :diffthis     " add B; arity = 2; compute fires
:wincmd l | :diffthis     " add C; arity = 3; engine switches to compute_three_way
" Now hunks visible across all three panes
:diffoff                  " remove C from session; arity = 2
:diffoff                  " remove B; arity = 1, dormant
:diffoff                  " remove A; session drops; G = None
```

**Four-`:diffthis` rejection:**

```
:diffthis | wincmd l | :diffthis | wincmd l | :diffthis  " arity = 3
wincmd l | :diffthis  " engine rejects: "v1 supports up to 3 buffers"
                      " active_diff_group unchanged; user sees an
                      " error message; no state mutation
```

## 7. Migration of existing call sites

### 7.1 Producers (the 4 source impls)

Pre-v1 — no external consumers — so the rename is a clean break,
not a deprecation cycle. All five renames happen in the slice that
introduces the new shape:

```rust
// New canonical name. No deprecation aliases.
pub trait DiffParticipantSource: Send + Sync + 'static + Debug {
    fn snapshot(&self) -> Rope;
}
```

Concrete source-type renames (also clean break):
- `StaticBaseline` → `StaticSource`
- `OnDiskBaseline` → `OnDiskSource`
- `BufferBaseline` + `BufferCurrentSource` → unified `BufferSource`
- D.7's `GitBaseline` (when it lands) → `GitSource`

Every reference site updates in the same sub-slice that introduces
the rename. No `#[deprecated]` aliases carried forward.

### 7.2 Descriptor consumers

No legacy accessors retained. Every site that referenced
`descriptor.baseline` / `current` / `remote` / `is_three_way()`
updates to `descriptor.sources[0]` / `[1]` / `.get(2)` /
`descriptor.arity() == 3` in the same sub-slice that lands the
shape change (D.8.c).

D.6.d's `compute_get_edit` / `compute_put_plan` already do slot-based
lookups via `pane_index_of` + `snapshot_for_pane` — minimal touch.
`snapshot_for_pane(descriptor, pane)` becomes
`descriptor.sources.get(pane).map(|s| s.snapshot())`.

### 7.3 Engine consumers

`DiffSession::recompute_blocking` signature changes:

```rust
// Before (D.6.a):
pub fn recompute_blocking(
    &self,
    baseline: &Rope,
    current: &Rope,
    remote: Option<&Rope>,
) -> Option<Arc<HunkIndex>>;

// After (D.8):
pub fn recompute_blocking(
    &self,
    sources: &[Rope],
) -> Option<Arc<HunkIndex>>;
```

Callers iterate `descriptor.sources` snapshotting each rope, then pass
the slice. `DiffSubsystem::schedule_recompute` similarly.

### 7.4 Provider ID namespaces (filler rows)

D.6.b's namespace bits are fixed per side:
- `0xD1FF_0001_*` Baseline
- `0xD1FF_0002_*` Current
- `0xD1FF_0003_*` Remote

Generalise to slot-indexed: `0xD1FF_(slot+1)000_*` where slot ∈ [0,
N-1]. For v1 N ≤ 3 this preserves existing namespace bits. When the
cap moves, new slots take `0xD1FF_0004_*`, `0xD1FF_0005_*`, etc.

### 7.5 Tests

The ~111 reference sites in `lattice-host` / `lattice-grammar`
covering `BaselineSource` / `CurrentSource` / `is_three_way` /
`register_two_pane_diff` / `register_three_pane_diff` update at the
sub-slice that introduces each rename or signature change. No
deprecation aliases, no opportunistic sweep — every sub-slice ships
green on first build.

## 8. What this does NOT change

- The diff engine algorithm. `compute_two_way` / `compute_three_way`
  internals unchanged. `compute_diff` is a dispatch shim.
- Renderer surfaces. Both TUI and GPUI consume `HunkIndex` data
  (slot count from `hunk.ranges.len()`); they already work for any
  N. No changes in `lattice-ui-tui` / `lattice-ui-gpui`.
- Theme entries. `DiffSignKind` already has the four kinds it needs;
  no new variants.
- `do_diff_get` / `do_diff_put` dispatch. D.6.d's
  `compute_get_edit(active, row, target)` /
  `compute_put_plan(active, row, target)` already do slot-based
  lookups; they get the same descriptor-shape simplification but no
  semantic change.

## 9. Slice carve (D.8.a–f)

| Sub-slice | Scope |
|---|---|
| **D.8.a** | `lattice-diff::compute_diff(&[Rope], algorithm) -> Result<HunkIndex, DiffEngineError>` + `DiffEngineError` enum. Existing `compute_two_way` / `compute_three_way` demoted to crate-private `two_way` / `three_way` helpers called from inside `compute_diff`. All external callers (subsystem, benches, tests in other crates) switch to `compute_diff` in the same sub-slice. Tests: dispatch matrix (N=0/1/2/3/4+). |
| **D.8.b** | Trait + source-type rename. `DiffParticipantSource` replaces the two-trait split; concrete sources renamed (`StaticBaseline → StaticSource`, etc.). All reference sites updated in this sub-slice — no deprecation aliases. |
| **D.8.c** | `DiffDescriptor` shape change. `sources: Vec<Arc<dyn DiffParticipantSource>>` replaces `baseline + current + remote`. Every `descriptor.baseline` / `descriptor.current` / `descriptor.remote` / `descriptor.is_three_way()` site updates in this sub-slice. `DiffSession::recompute_blocking(&[Rope])` and `schedule_recompute` swap over. |
| **D.8.d** | Subsystem membership API. `add_participant`, `remove_participant`, `remove_participant_buffer`, `replace_descriptor`. `MembershipError`. Bridge `note_session_extended` / `note_session_shrunk`. Tests: per-buffer add/remove; arity transitions; engine-rejected at N=4; dormant N=1; auto-drop at N=0. |
| **D.8.e** | `:diffthis` rewrite. Drop `pending_diffthis`. Add `Editor::diffthis_group: Option<BufferId>` (singleton diffthis-group key, isolated from other sessions). State machine per §6.2. Tests for each transition + the multi-session isolation case (`:diffsplit base remote` + `:diffthis` keeps the two sessions independent). |
| **D.8.f** | `:diffoff` per-buffer semantic + `:diffoff!` cascade (preserves D.6.g but redefined per §6.3). `register_two_pane_diff` / `register_three_pane_diff` collapse into one `register_pane_group_diff(members)`. Final cleanup: update docs (slice plan, implementation ledger, diff-system.md §3.4). |

**Dependencies between sub-slices:** D.8.a is foundational. D.8.b is
independent of D.8.a — pure rename. D.8.c depends on both. D.8.d
depends on D.8.c. D.8.e depends on D.8.d. D.8.f depends on D.8.e.

D.8 ships green-on-each-sub-slice — no big bang, no deprecated
aliases. Pre-v1 means we can take the clean-break renames in each
sub-slice. Diff sizes per sub-slice are larger than they would be
with aliases (each rename touches every call site at once) but the
trees stay coherent at every commit.

## 10. Testing strategy

Per-sub-slice + a final integration pass:

- **Engine layer (D.8.a):** dispatch matrix, error variants, N=1 →
  empty, N=2/3 ≡ existing functions, N≥4 → typed error.
- **Subsystem (D.8.d):** add/remove participant flows; arity
  transitions through 1↔2↔3; dormant at N=1; auto-drop at N=0;
  bridge refcount maintained across add/remove; routing index
  updates on `descriptor.watch` mutation.
- **`:diffthis` (D.8.e):** every transition in §6.2's table has a
  dispatch-level test. Four-`:diffthis` rejection surfaces the
  engine error as a user message.
- **`:diffoff` per-buffer (D.8.f):** removing one of N=3 → arity 2
  (session intact); removing one of N=2 → arity 1 (dormant);
  removing one of N=1 → session drops. `:diffoff!` cascade preserved.
- **Migration tests:** legacy `BaselineSource`/`CurrentSource`
  callers compile (deprecation-warning, not error). `is_three_way()`
  legacy accessor returns correct value.
- **Integration (D.8.g):** end-to-end three-`:diffthis` flow:
  three scratch buffers, three `:diffthis` calls, observe N=3
  session with Conflict hunks; remove one → N=2; commit a hunk via
  `:diffput`; close all via `:diffoff!`.

## 11. Open questions worth flagging

1. **Should N=1 dormant sessions register filler / overlay providers?**
   No (nothing to align), but the provider registration is currently
   in `register_pane_group_diff`. If `:diffthis` creates N=1 sessions
   without going through `register_pane_group_diff`, the providers
   don't register until the second `:diffthis`. The arity-2 transition
   needs to register them then. Decide during D.8.e.
2. **Engine-level `compute_diff` for N≥4.** The post-Phase-7
   generalisation. Likely: pairwise diffs against `sources[0]` as
   anchor, no formal merge semantics, no Conflict variant. Out of
   scope for D.8; design hook lives in the `DiffEngineError::
   Unsupported { n }` variant — extending the engine means
   replacing that arm with computation.

(The original open question 2 — `:diffthis` on a buffer in a
"different" group — is resolved by the singleton-diffthis-group
model in §6.1: there's only one diffthis group; other sessions are
not addressed by `:diffthis` at all.)

## 12. Reference — how other editors handle N≥4

Informational only; Lattice's v1 choice is
`DiffEngineError::Unsupported { n }` for N≥4, matching the most
restrictive end of the landscape. The §11.2 design hook lives in the
`Unsupported` arm; this section captures what we'd be choosing
between when we eventually move the cap.

| Editor | N≥4 behaviour | Notes |
|---|---|---|
| **Vim** | Supported, via independent per-buffer `:set diff` flags + pairwise diffs against an implicit anchor (first-`:set diff`-window's buffer). No merge-conflict semantics; no `Conflict` hunk kind. Side-by-side display works for N panes with filler lines for alignment. | `vimdiff a b c d` actually works. Used in practice for cross-version comparison (e.g., main vs feature1 vs feature2 vs review). |
| **Emacs (ediff)** | Not supported. `ediff-files` is 2-way; `ediff-files3` is 3-way; `emerge` is 3-way merge. No `ediff-filesN`. Users compose multiple ediff sessions or use `ediff-directories` for batched 2-way diffs. | Magit doesn't extend this — magit's diff views are always vs a single reference. |
| **Helix** | Not supported at all — Helix doesn't expose arbitrary N-way diff. Gutter signs against HEAD only (added late 2023 via `gix`). No `:diff` ex-command. Side-by-side comparison defers to external tools or `git diff` in a terminal. | Deliberate minimalism. The diff display lives entirely in the gutter. |
| **Zed** | Not supported. Task-oriented diff UX: "diff vs HEAD/staged" (gutter signs + side-panel) and "three-way merge view" for conflicts. No general-purpose N-buffer diff. | Diff is treated as a git/SCM affordance, not a generic editor operation. |
| **VSCode** | Not supported in the core. Three-way merge editor for `.orig` conflict files; otherwise diff is 2-way against HEAD/staged/branch. Extensions (e.g., Compare Folders) handle N>2 via N-1 pairwise comparisons in tabs. | The integrated source-control UX treats diff as a verb (compare these two things) rather than a noun (here is an N-way diff session). |

**Substrate-weighted read (per
[[feedback-editor-design-references]]):** Helix and Zed are both
Rust-built and both chose **not to support N≥4** because the
task-oriented use cases (gutter-vs-HEAD, three-way merge resolution)
cover the practical need. The general N-way feature exists in Vim
because Vim's architecture treats `:set diff` as an independent
per-buffer toggle — N panes ride for free. Adding it to Lattice
would mean implementing pairwise-vs-anchor in `compute_diff` and
documenting that hunks at N≥4 lose the `Conflict` semantic (it only
makes sense for 3-way merge).

**Convention-weighted read (per
[[feedback-convention-first]]):** Vim is the only editor that
treats `:diffthis` as a general N-way membership toggle. Emacs's
ediff is explicitly arity-tagged. Helix doesn't have `:diffthis`.
Zed doesn't have `:diffthis`. So the "vim-style :diffthis" UX
we're designing in D.8 carries vim's mental model — and a vim user
might reasonably expect `:diffthis` to scale past 3. **That's the
case for eventually moving the cap.** Not now: v1 ships with N≤3,
v2+ revisits when we have user-feedback signal that >3 is
actually wanted in practice.

**Decision (v1):** stay with `DiffEngineError::Unsupported`. The
design hook in the `Unsupported` arm gives us a clean place to
extend later; the error message names the cap so users hitting it
know it's an intentional limit, not a bug.

## 13. Cross-references

- `diff-system.md` §3 — current data model that D.8 replaces.
- `diff-system.md` §6.3 — ex-command table; `:diffthis` /
  `:diffoff` semantics update per §6 above.
- `diff-system.md` §8 — open question "doc-close behaviour":
  D.8's per-buffer `:diffoff` lets the auto-close on document
  closure be expressed as a `remove_participant_buffer` call.
- [`magit.md`](../magit/) — the future magit plugin consumes the D.8
  arity-agnostic descriptor surface.
- `slice-plans/diff-system.md` — D.8 row gets re-carved per §9.
