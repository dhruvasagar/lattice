# Completion / picker unification under `CompletionPipeline`

Phase 5.8 follow-up architecture doc. Captures the design that
unifies the picker and the cmdline-completion popup onto a
single completion-engine substrate, while preserving their
UX-distinct surfaces.

> **Status:** proposed, awaiting confirmation. No code yet. The
> shape below is what slices `3c.unify.completion.*` will land
> against. Marginalia for option-value completion is part of
> the same design.

## Problem

The picker (`lattice-picker`) and cmdline completion
(`lattice-completion` + the host wiring) carry overlapping but
divergent infrastructure today. Audit of duplication after slice
`3c.cmdline-completion-fuzzy-shared` (`71d3063`) which unified
the fuzzy-match algorithm:

| Concern                      | Picker                                   | Cmdline-completion                                                         | Status                               |
|------------------------------|------------------------------------------|----------------------------------------------------------------------------|--------------------------------------|
| Fuzzy-match algorithm        | `fuzzy_match` (5-tier)                   | `fuzzy_match` (5-tier)                                                     | **shared** (slice 71d3063)           |
| Row rendering (TUI)          | `candidate_to_line`                      | `candidate_to_line`                                                        | **shared**                           |
| Row rendering (GPUI)         | inline per-cell builder                  | mirror of picker's (slice d38110b)                                         | **structural parity**                |
| Filter loop                  | inline in `Picker::filter()`             | `CompletionPipeline::run()`                                                | **duplicated**                       |
| Score model                  | `match_score + mru_bonus`, combined sort | match_score only                                                           | **MRU only in picker**               |
| Annotations (kind, doc, ...) | none                                     | populated by annotator pipeline; rendered by TUI; not yet rendered by GPUI | **only in cmdline; GPUI render gap** |
| Source pattern               | candidates pre-supplied at `open()`      | generator picked by slot detection                                         | **two patterns**                     |
| Plugin extension surface     | none today                               | `CompletionRegistry` traits (generator, matcher, ranker, annotator)        | **only cmdline today**               |

The shared parts (match algorithm, row layout) are now consolidated.
The unshared parts (filter loop, MRU, annotations, generator-source
pattern, plugin extensibility) are the next consolidation target.

## Architectural choice

Three directions considered. Final choice: **Direction 2 — make
`CompletionPipeline` the shared engine; surfaces become thin
clients.**

The directions debated and rejected:

- **D1: "Cmdline-completion uses Picker as engine."** The richer
  abstraction (generators/matchers/rankers/annotators) is in
  `lattice-completion`. Forcing cmdline through Picker's simpler
  candidates-pre-supplied model loses generator slot-routing,
  the annotator pipeline, the cache (`GeneratorCache`), and the
  registered extension surface. Backward direction.
- **D3 (compromise): "Share infrastructure, keep separate state
  types."** Functionally equivalent to D2 but framed less
  cleanly: both surfaces have a state type, both call Pipeline.
  Once stated explicitly, D2 and D3 collapse into the same
  outcome. Picking D2's framing — "Pipeline is the engine,
  surfaces are clients" — because it makes the plugin contract
  explicit.

### Why D2 against the paramount goals

| Paramount goal | D2 against it |
|----------------|----------------|
| **#1 Performance** | Equivalent. One filter loop with vtable dispatch at ~5ns per stage × ~5 stages per candidate ≈ 25ns of overhead per candidate (noise vs `fuzzy_match`'s actual cost ~µs at 5-tier). Pipeline already supports caching at the generator stage; picker gains it. |
| **#2 Extensibility** | **Strongest fit.** One extension surface (`CompletionRegistry`) reaches both picker and cmdline. Plugin registers `CandidateGenerator`/`CandidateAnnotator`/`CandidateMatcher`/`CandidateRanker` once → available to every completion surface in Lattice. The WIT API exposed in the plugin host phase has ONE shape (`completion-source` interface), not two. |
| **#3 Extensible vim modal editing** | Neutral — neither modal grammar nor command-grammar surface changes. |
| **#4 Asynchronicity** | Pipeline's generator stage is sync today; the future `AsyncGenerator` extension (LSP completion, FS walk) plugs into the same registry both surfaces consume. |

### Why D2 against the decision heuristics

| Heuristic | Verdict |
|-----------|---------|
| #1 "Best long-term fit beats easy implementation" | D2 is the larger refactor today but the right shape for the next 5 years of completion sources. Picker's inline filter is the easy-now choice that grows debt every time a new source is added. |
| #2 "Evaluate against paramount goals, not against other editors" | The picker model exists because other editors (helix, telescope) have separate engines. Lattice's design uniqueness is the unified `CommandRegistry`; the completion surface deserves the same unification. |
| #3 "Treat user-suggested options as input, not the menu" | User suggested "extend Picker for cmdline." Reversing direction (Pipeline as engine) is the right answer for the paramount goals — surfaced explicitly here. |
| #4 "Confirm the plan before non-trivial work" | This doc IS the confirmation step. |
| #5 "Non-trivial design changes ship four artefacts together" | Each slice below ships docs (this file updates) + bench (Pipeline-per-frame measurement) + tests + graceful failure (Pipeline-missing surfaces echo, never panic). |

## Target architecture

```
                ┌─────────────────────────────────────┐
                │      CompletionRegistry             │
                │                                     │
                │  generators ─────────┐              │
                │  matchers   ─────────┤  registered  │
                │  rankers    ─────────┤  via plugin  │
                │  annotators ─────────┘  or builtin  │
                └────────────────┬────────────────────┘
                                 │ build pipeline for source
                                 ▼
                ┌─────────────────────────────────────┐
                │      CompletionPipeline             │
                │                                     │
                │  1. generators → Vec<RawCandidate>  │
                │  2. matcher    → score + ranges     │
                │  3. rankers    → reorder            │
                │  4. annotators → marginalia         │
                │                                     │
                │  pipeline.run(ctx, query, cache)    │
                │   → Vec<RenderedCandidate>          │
                └────────────────┬────────────────────┘
                                 │
              ┌──────────────────┼──────────────────┐
              │                  │                  │
        ┌─────▼──────┐  ┌────────▼────────┐  ┌──────▼──────┐
        │  Picker    │  │    Cmdline      │  │  Insert     │
        │  (focused) │  │   Completion    │  │  Completion │
        │            │  │   (overlay)     │  │  (overlay)  │
        │  title +   │  │   slot-routed   │  │  cursor-    │
        │  query +   │  │   replace_start │  │  anchored   │
        │  candidates│  │                 │  │             │
        └────────────┘  └─────────────────┘  └─────────────┘
            │                  │                    │
            │  ┌─── render ────┴────────────────────┘
            │  │  shared `candidate_to_line` (TUI)
            └──┤  shared row block (GPUI, post-d38110b)
               │  marginalia column (TUI today; GPUI new — see below)
               └─
```

Each surface holds its own state (UX-specific: focused vs overlay
vs cursor-anchored) but goes through `CompletionPipeline` for
filter+rank+annotate. The render path is already shared.

### Component changes

- **`Pipeline::ranker` → `Pipeline::rankers: Vec<Arc<dyn CandidateRanker>>`.**
  Multiple rankers compose by re-sorting; MRU becomes one ranker
  stacked atop `ScoreRanker`. Each ranker reads the current
  ordering, applies its dimension, leaves the result for the
  next.
- **`MruRanker`** moves from `lattice-picker` to
  `lattice-completion`. Takes an `Arc<HashMap<String, f64>>`
  bonus map at construction; `rank()` shifts scores within tiers
  and re-sorts. Picker plugs it in for buffer/file/jump
  pickers. Cmdline can opt in via the same registry (future
  `cmdline.mru` typed option).
- **Picker's `raw: Vec<RawCandidate>`** becomes a `PreSuppliedGenerator`
  that owns the Vec and yields it on `generate()`. The picker's
  inline filter loop is replaced with a `Pipeline::run` call.
- **`SourceRegistration` plugin API.** A new public type that
  bundles (generator, matcher_default, ranker_defaults,
  annotator_defaults, ui_hints). Plugins call
  `registry.register_source(SourceRegistration { ... })`. Two
  surfaces (picker, cmdline) consume the registration via
  different routing: picker via `:picker <name>`; cmdline via
  slot match.

### What stays the same

- **Picker's focused-mode UX.** Title bar, dedicated query line,
  Enter accepts, Esc dismisses. The state type
  (`lattice_picker::Picker`) keeps `title`, `query`, `selected`.
  Filter now calls Pipeline; everything else unchanged.
- **Cmdline completion's overlay UX.** Tab cycles, Enter inserts,
  ride-along edit. `CompletionState` keeps `replace_start`,
  `original_line`. Filter already calls Pipeline (no change).
- **Insert-mode completion.** Unchanged for v1; the
  `InsertCompletionState` machinery already uses the pipeline
  pattern and shares `fuzzy_match`. Full migration is a follow-up.

## Marginalia for option / option-value completion

The improvement requested with this arc. Feasibility verified
in source:

- `OptionDecl::DOC: &'static str` already exists per spec. For
  `:set <Tab>` (option name completion), the doc is one accessor
  away.
- `OptionType::enumerate() -> Option<Vec<&'static str>>` exists
  for option-value completion. Currently returns string forms only;
  needs an extension to optionally return per-value docs.

### Design

1. **Extend `OptionType` with `enumerate_with_docs()`:**

   ```rust
   pub struct EnumeratedValue {
       pub form: &'static str,
       pub doc: &'static str, // "" when no per-value doc
   }

   fn enumerate_with_docs() -> Option<Vec<EnumeratedValue>> {
       Self::enumerate().map(|forms| {
           forms.into_iter()
               .map(|f| EnumeratedValue { form: f, doc: "" })
               .collect()
       })
   }
   ```

   Default impl uses existing `enumerate()` with empty docs.
   Implementors override for richer marginalia.

2. **`OptionValueDocAnnotator`** in `lattice-completion::builtins`:

   ```rust
   pub struct OptionValueDocAnnotator;

   impl CandidateAnnotator for OptionValueDocAnnotator {
       fn annotate(&self, c: &mut RenderedCandidate) {
           if let CandidateData::OptionValue { doc, .. } = &c.raw.data
               && !doc.is_empty()
           {
               c.annotations.push(doc.clone());
           }
       }
   }
   ```

3. **`CandidateData::OptionValue`** new variant carrying the doc.
   Generators for option-value completion (already exist; the
   slot detector routes `:set X=<Tab>` to them) populate it.

4. **GPUI annotation rendering.** Right-aligned within the row,
   dimmer color, same line. TUI already does this in
   `candidate_to_line`. GPUI's picker overlay + cmdline-completion
   strip (slice d38110b) need the equivalent — currently they
   render `display` + match highlights only.

End-user experience:

```
:set foldmethod=<Tab>

   marker        Fold by markers (`{{{...}}}`)
 ▶ indent        Fold by indent level (vim's foldmethod=indent)
   manual        User-defined folds only
   syntax        Folds from tree-sitter syntax tree
   expr          Custom expression
```

Equivalent for `:set <Tab>` (option name completion), pulling
`OptionDecl::DOC`:

```
:set <Tab>

 ▶ foldmethod    How folds are computed (`marker` / `indent` / ...)
   number        Show line numbers
   relativenumber  Show line numbers relative to cursor
   ...
```

## LSP-registered picker sources

Audit of host-side picker openings reveals a SECOND construction
pattern beyond the first-party generator-driven sources
(`FilesSource`, `BuffersSource`, `CommandsSource`, etc.):

| LSP picker | Trigger | Pattern |
|------------|---------|---------|
| References / definitions / impls / type-defs / declaration | `gr` / `gd` / `gI` / `gy` / `gD` | **Host-pre-supplied** — async LSP response builds `Vec<LspLocationRow>`; host calls `open_lsp_locations_picker(title, locations)` which builds a `Picker::new(_, PickerSource::LspLocations, PickerAction::JumpToLspLocation)` |
| Diagnostics workspace list | `:diagnostics` | Host-pre-supplied via `open_lsp_locations_picker` |
| LSP completion items popup | post-completion-request | Host-pre-supplied (`PickerAction::AcceptLspCompletion`) |
| LSP code actions | `:code-actions` / `gA` | Host-pre-supplied (`PickerAction::AcceptLspCodeAction`) |
| LSP code lens | `:lsp-code-lens` | Host-pre-supplied (`PickerAction::AcceptLspCodeLens`) |
| Color presentation | `:lsp-color-presentation` | Host-pre-supplied (`PickerAction::AcceptColorPresentation`) |
| LSP server instances | `:lsp-log` / `:lsp-trace-log` / `:lsp-server-log` | Host walks supervisor's actor table; `PickerSource::LspInstances` |
| `window/showMessageRequest` | server-initiated | Host-pre-supplied (`PickerSource::LspShowMessageRequest`) |

This is fundamentally different from the generator-pull pattern.
Rows come from **async LSP responses**, computed host-side,
seeded into the picker at construction. There's no per-keystroke
"generate" call — only filter against the seeded rows.

### Two source kinds, one substrate

The `SourceRegistration` substrate (slice 7) must therefore admit
both shapes:

```rust
pub enum CandidateSource {
    /// Pull-based — `generate(ctx)` runs per `Pipeline::run`.
    /// First-party uses: Files, Buffers, Commands, Marks, Jumps.
    /// Plugin uses: anything synchronously enumerable.
    Generator(Arc<dyn CandidateGenerator>),

    /// Push-based — caller supplies the candidate set at picker-
    /// open time (typically from an async response). The pipeline
    /// treats the supplied Vec as a fixed input and runs only
    /// match + rank + annotate stages.
    /// First-party uses: every LSP picker.
    /// Plugin uses: anything async / network-driven.
    PreSupplied(Arc<Vec<RawCandidate>>),
}
```

The `Pipeline::run` body becomes:

```rust
let raw = match &self.source {
    Generator(g) => g.generate(ctx),         // existing
    PreSupplied(rows) => rows.as_ref().clone(), // new
};
// match + rank + annotate stages unchanged
```

This is a SMALL extension to the pipeline. The match-and-rank-and-
annotate machinery doesn't care where `raw` came from.

### What LSP picker migration looks like

For each LSP picker:

1. **Replace special-case row type with `CandidateData` variant.**
   Today `Picker::set_lsp_locations(Vec<LspLocationRow>)` stores a
   typed Vec. After migration the rows become
   `Vec<RawCandidate>` where each candidate carries
   `data: CandidateData::LspLocation { path, line, col, preview }`.

2. **Replace `PickerAction` enum dispatch with candidate-carried
   accept handler.** The runtime dispatch `match action {
   JumpToLspLocation => ..., AcceptLspCompletion => ... }` is
   today's source of cross-cutting coupling. The migration makes
   the accept handler part of the source registration:

   ```rust
   pub struct SourceRegistration {
       pub name: &'static str,
       pub source: CandidateSource,
       pub accept: Arc<dyn AcceptHandler>,
       // matcher / ranker / annotator defaults...
   }
   ```

   `AcceptHandler::accept(&self, candidate: &RawCandidate, ctx:
   &mut EditorCtx)` does what the matching `PickerAction` does
   today. The accept dispatch becomes one indirect call per
   accept instead of a host-side match arm.

3. **Pipe annotations through the annotator stage.** LSP locations
   already have marginalia (the line preview). Today it's stored
   directly on `LspLocationRow.marginalia` and rendered specially.
   After migration the preview becomes a regular annotation
   populated by an `LspLocationPreviewAnnotator` (or carried
   directly in `RawCandidate.data` and surfaced by a generic
   marginalia annotator).

### What this costs in slices

The 8-slice plan I outlined earlier was scoped to **filter-loop
unification + marginalia + plugin substrate**. The LSP picker
migration is a SEPARATE refactor with its own scope. Adding it
to the plan:

| # | Slice | Effort |
|---|-------|--------|
| 9 | `3c.unify.candidate-source-kind` | small | Add `CandidateSource::{Generator, PreSupplied}` to the pipeline. The match-and-rank-and-annotate machinery already handles `Vec<RawCandidate>` uniformly. |
| 10 | `3c.unify.lsp-locations-via-candidatedata` | medium | Replace `PickerSource::LspLocations` + `set_lsp_locations()` with `CandidateData::LspLocation`. One picker variant at a time (references, then definitions, then impls...) to keep slices small. |
| 11 | `3c.unify.lsp-completion-picker` | small | Same migration for the completion-items popup. |
| 12 | `3c.unify.lsp-code-actions-picker` | small | Same for code-actions. |
| 13 | `3c.unify.lsp-code-lens-picker` | small | Same for code-lens. |
| 14 | `3c.unify.lsp-color-presentation` | small | Same for color-presentation. |
| 15 | `3c.unify.lsp-instances-picker` | small | Migrate `:lsp-log` etc. — same substrate, different `CandidateData::LspServerInstance` variant. |
| 16 | `3c.unify.lsp-show-message-request` | small | Migrate `window/showMessageRequest` picker. |
| 17 | `3c.unify.accept-handler-promotion` | medium | Once all special-case PickerAction variants are migrated, replace the `PickerAction` enum with the `AcceptHandler` trait. Drop the runtime `match action` dispatch on the host. |

Slices 9-17 land AFTER 1-7 and can be sliced independently — each
single-picker migration is a green commit on its own. Slice 17
is the final cleanup that retires the `PickerAction` enum once
nobody references it.

### LSP cross-check: the design that survives

Verified against the host's existing LSP accept paths in
`dispatch.rs::do_picker_accept` (slice 7a planning, 2026-05-21).
Two patterns emerged:

**Stateless accept (the majority):** the candidate's data
field carries everything needed to dispatch. Examples:
  - Files → `OpenFile { path }`
  - Buffers → `SwitchBuffer { id }`
  - LSP locations → `JumpToFileLocation { path, line, col }`
  - Commands → `InvokeCommand { id, args }`
  - Registers / Marks / Snippets — same shape

**Stateful accept (LSP request-bound):** the candidate carries
only an INDEX into a host-side ephemeral table. Accept dispatch
looks up the actual item from `Editor::pending_*_items` and
applies it. Examples:
  - `RoutingPayload::LspCompletion { index }` → look up in
    `pending_completion_items`, splice into buffer.
  - `RoutingPayload::LspCodeAction { index }` → look up in
    `pending_code_action_items`, resolve, apply WorkspaceEdit.
  - `LspCodeLens`, `ColorPresentation` — same pattern.
  - `AcceptShowMessageAction { request_id, ... }` → look up
    pending SMR oneshot, reply.

### Implication: AcceptHandler is stateless

The cross-check tells us the AcceptHandler trait does NOT need
host state access. State lookup happens at dispatch time (where
the Editor is in scope, just like today). The handler is a
pure function:

```rust
pub trait AcceptHandler: Send + Sync {
    fn accept(&self, candidate: &RawCandidate) -> AcceptAction;
}
```

`AcceptAction` is a rename + cleanup of today's `RoutingPayload`
enum:

```rust
pub enum AcceptAction {
    // Stateless (candidate's CandidateData carries the payload)
    OpenFile { path: PathBuf },
    SwitchBuffer { id: BufferId },
    JumpToFileLocation { path: PathBuf, line: u32, col: u32 },
    JumpInBuffer { buffer_id: BufferId, line: u32, col: u32 },
    InvokeCommand { id: CommandId, args: ArgsValue },
    PasteRegister { name: char },
    JumpToMark { name: char },
    ExpandSnippet { id: SnippetId },

    // Stateful (token + index; host resolves)
    AcceptIndexedCompletion { token: AcceptToken, index: u32 },
    AcceptIndexedCodeAction  { token: AcceptToken, index: u32 },
    AcceptIndexedCodeLens    { token: AcceptToken, index: u32 },
    AcceptColorPresentation  { token: AcceptToken, index: u32 },
    AcceptShowMessageAction  { request_id: u32, server_id: String, index: u32 },

    // For cmdline-completion (currently inline; explicit now)
    InsertText { text: String, replace_start: usize },

    // Plugin extension
    Custom(Box<dyn Any + Send + Sync>),
}
```

`AcceptToken` is a type-erased opaque marker (`u64` or `Uuid`)
the host uses to look up the right pending table. Avoids
proliferating per-LSP-feature enum variants in `AcceptAction`.

### SourceRegistration shape (cross-check-validated)

```rust
pub struct SourceRegistration {
    pub spec: SourceSpec,                  // id, doc, args, live flag
    pub kind: CandidateSourceKind,         // pull or push
    pub accept: Option<Arc<dyn AcceptHandler>>,
    pub matcher_override: Option<MatcherId>,
    pub ranker_overrides: Vec<RankerId>,
    pub annotator_extras: Vec<AnnotatorId>,
}

pub enum CandidateSourceKind {
    /// Pull — `generate(ctx)` runs per `Pipeline::run`.
    /// First-party Files / Buffers / Commands / Lines / Jumps /
    /// Marks / Registers / Outline / RecentFiles / Grep.
    /// Plugins doing synchronous enumeration.
    Generator(Arc<dyn CandidateGenerator>),

    /// Push — candidate set is given at registration time.
    /// First-party LSP pickers (references / definitions /
    /// completion / code-actions / code-lens / color /
    /// instances / SMR). Plugins doing async fetch.
    PreSupplied(Arc<Vec<RawCandidate>>),
}
```

### Two registration lifecycles

The LSP cross-check also surfaced that not every source is
boot-registered:

| Lifecycle | Examples | How |
|-----------|----------|-----|
| **Persistent** (registered at boot, lives until shutdown) | Files, Buffers, Commands, all the static first-party sources | `CompletionRegistry::register_source(SourceRegistration)` at boot |
| **Transient** (created per-use, dropped after accept/dismiss) | All LSP pickers; plugin async-fetch sources | Host builds a `SourceRegistration` on the spot from the async response, passes directly to `Picker::open_with(reg)` |

The shape (`SourceRegistration`) is identical. Only the lifetime
differs. Plugins choose: register persistently if their data is
synchronously enumerable, or construct transient registrations
per-use if they need to await before opening the picker.

### Dual-registry decision (slice 7d.1)

Slice 7 planning deferred the trait-shape question:
`PickerSourceGenerator` takes `PickerContext` (surface-
specific snapshots: active buffer, recent files, position
history, marks, registers); `CandidateGenerator` takes
`GenerateContext` (engine concepts: prefix, buffer, command
registry). The two encode different problem domains.

Three options were on the table at 7d.0 land time:

  (a) **Decompose** `PickerSourceGenerator` so picker sources
      impl `CandidateGenerator` directly. Requires either
      bloating `GenerateContext` with surface concerns (breaks
      engine/surface separation) or stripping picker sources
      of context (breaks them).

  (b) **Type-erase** the context via `Box<dyn Any>`. Ugly.

  (c) **Two registries, one shared shape** —
      `CompletionRegistry::sources` for engine-shape
      registrations (cmdline-completion + plugin WIT + future
      simple picker sources); `PickerRegistry` keeps the
      surface-specific first-party sources that need
      `PickerContext`. Same `SourceRegistration` bundle
      serves both worlds.

**Decision: (c).** Justification:

- **Paramount #2 (extensibility) intact.** Plugins land via
  `SourceRegistration` (engine-shape) through one WIT
  interface. They don't get `PickerContext` access through WIT
  — if they need richer context, they ask via separate WIT
  calls. ONE plugin contract.

- **Paramount #1 (performance) intact.** Dual-lookup at
  `:picker <name>` invocation is two HashMap::get calls
  (~20ns). Imperceptible.

- **Engine stays focused.** `CandidateData` /
  `GenerateContext` / the matcher / ranker / annotator
  pipeline never learns about picker-surface concepts.

- **Honest about the divergence.** Picker is a SURFACE that
  needs surface-specific context. Pretending otherwise via
  trait gymnastics violates heuristic #1 (best long-term fit
  beats easy implementation).

Slice 7d.1 wires the dual-lookup in `open_picker`:

  1. Check `CompletionRegistry::source_by_id` first
     (plugin + future engine-shape registrations).
  2. Fall through to `PickerRegistry::lookup`
     (first-party surface-specific sources).

The 10 first-party picker sources continue to live in
PickerRegistry. They're NOT migrated to
`register_source(SourceRegistration)`. Migration would gain
nothing — they need PickerContext.

Future plugin picker sources that register via
`SourceRegistration::PreSupplied` flow through the new path
and get full DefaultAcceptHandler dispatch (from 7d.0).
PickerContext-needing plugin sources would need a separate WIT
interface — orthogonal to this arc.

### Slice 17 — PickerAction retirement (queued post-arc)

The original slice 17 plan said "retire `PickerAction` enum
once nobody references it." Post-9-16 audit shows the path
forward but the actual retirement requires a follow-up arc:

**Where PickerAction still fires today:**

  - Picker construction (`Picker::new(_, _, on_accept)`).
    Every host-side picker open passes a PickerAction tag.
    Used by the picker to influence behaviour (e.g.
    `set_lsp_instances` reads `self.on_accept` to stamp
    OpenLspLog vs OpenLspTraceLog accept_action).
  - Legacy imperative match in `do_picker_accept` (the
    fallback after the typed-action branch). Reachable today
    only for stateful LSP pickers — `accept_action_to_outcome`
    returns None for AcceptIndexed* / AcceptColorPresentation
    / AcceptShowMessageAction.
  - Preview fallback path in `preview_picker_selection`
    (checks `picker.on_accept == SwitchToBuffer`).

**What it would take to fully retire:**

  1. Grow `PickerAcceptOutcome` with 3 new variants
     (ApplyLspCodeLens, ApplyColorPresentation,
     ApplyShowMessageAction) — mirrors what the legacy match
     calls today.
  2. Grow `accept_action_to_outcome` to map AcceptIndexed* +
     AcceptColorPresentation + AcceptShowMessageAction to
     those new outcome variants.
  3. Grow `apply_picker_outcome` with matching arms that do
     what the legacy match does (take from pending tables,
     apply via existing host methods).
  4. Delete the legacy imperative match in
     `do_picker_accept`.
  5. Remove `Picker::new`'s `on_accept: PickerAction`
     parameter — sources now determine accept via the typed
     `accept_action` on each candidate, not via a picker-
     level enum.
  6. Audit and delete remaining PickerAction references
     (~30 sites in host + ui-tui).
  7. Delete `PickerAction` enum + `PickerAcceptOutcome` may
     also retire if every variant has an AcceptAction
     counterpart.

**Why deferred:** the arc's paramount-goal value has shipped:

  - Unified plugin contract via SourceRegistration (7a-7c).
  - Typed accept dispatch via DefaultAcceptHandler (7d.0).
  - Dual-registry lookup (7d.1).
  - Preview unification — closes the LSP-references-preview
    gap (7g + 10 + 15).
  - LSP location + instance pickers carry typed
    accept_action (10 + 15).
  - LSP stateful pickers carry typed accept_action
    (forward-compatible; 11-14 + 16).

The remaining work is internal hygiene — visible only to
contributors, not to end users. Slot as
`3c.unify.picker-action-retirement` for a future arc once
multi-slot AcceptToken-keyed pending tables are needed for
other reasons (e.g. concurrent LSP pickers).

### Slice 7e + 7f decisions (post-7d.1 retrospective)

After 7d.1 landed the dual-registry shape, the optional
cleanup slices (7e first-party relocation, 7f picker-crate
audit) got reassessed. Both end up as no-op decisions for
the reasons below.

**7e `3c.unify.first-party-source-relocation` — DEFERRED indefinitely.**

  The originally-imagined cleanup was: move
  `FilesSource` / `BuffersSource` / `CommandsSource` / etc.
  from `lattice-picker::picker_sources` into
  `lattice-completion::builtins::sources` so cmdline-
  completion can reuse them (`:e <Tab>` → FilesSource, etc.).

  Auditing the 10 sources reveals that 9 of them genuinely
  need `PickerContext` (active buffer, position history,
  marks, registers, recent files, syntax symbols,
  workspace root). The dual-registry decision in 7d.1
  exists precisely to preserve PickerContext access for these.
  Relocating them to `lattice-completion` would force moving
  PickerContext too, undoing the engine/surface separation
  7d.1 just settled.

  The exception is `CommandsSource` — it only needs
  `CommandRegistry` which is already in `lattice-grammar`.
  Could in principle move. But:
    - No consumer asks for it. Cmdline-completion's command
      completion comes from `gen:commands` (separate impl).
    - Migrating would cost more than it returns until a
      consumer materialises.

  Closed as "defer indefinitely; revisit if a consumer needs
  source reuse across surfaces."

**7f `3c.unify.picker-crate-audit` — KEEP `lattice-picker` as picker-surface crate.**

  Post-7d.1 inventory of `lattice-picker`:

    Surface state:
      Picker, PickerMruIndex, PickerContext,
      ActiveBufferSnapshot, BufferEntry, PositionEntry

    Surface trait:
      PickerSourceGenerator (4-method shape) + 10 first-
      party impls

    Surface registry:
      PickerRegistry, PickerSourceSpec

    Surface dispatch enums (retiring slowly):
      RoutingPayload, PickerAction, PickerAcceptOutcome,
      PickerInitResult

    Surface-specific support:
      mru.rs (frecency persistence), events.rs (typed
      surface events), context.rs (snapshot translation)

  This is substantive picker-surface code that doesn't
  belong in `lattice-completion` (engine) or `lattice-host`
  (which would become a kitchen sink — cmdline-completion's
  surface code lives here, but folding picker too would
  conflate two distinct surfaces).

  Closed as "lattice-picker stays as a separate
  picker-surface crate; parallel to cmdline-completion's
  surface state living in lattice-host."

### Picker preview unification (a Design B win)

User concern raised mid-slice-7b (2026-05-21): "I want to
confirm this design will not affect, or retain the preview
feature. In the past I faced issues with the live preview as
well, for instance lsp references preview did not preview the
file at the line where the reference exists."

**Current state (pre-7d):** preview is buffer-switcher-only.
`Editor::preview_picker_selection` (dispatch.rs) hardcodes a
`PickerAction::SwitchToBuffer` guard and bails for every other
shape. Preview for `:picker lines`, `:picker jumps`, `:picker
outline`, LSP references — all silently absent. Not a
regression; missing functionality since the picker landed.

**Through 7b-7c:** preview keeps working unchanged. The
existing `RoutingPayload` parallel vec is still populated next
to `accept_action`, so `picker.routing_for(c)` returns the same
`RoutingPayload::Buffer { id }` it always did. Zero risk of
preview regression while the parallel state lives.

**At 7d (registry cutover):** preview migrates from
RoutingPayload-driven to `accept_action`-driven. This is where
the design gets BETTER for preview, because each `AcceptAction`
variant naturally expresses its preview semantics:

| AcceptAction variant | Preview behaviour |
|---|---|
| `OpenFile { path }` | Open in active pane in previewing mode |
| `SwitchBuffer { id }` | Activate the buffer (today's only preview) |
| `JumpToFileLocation { path, line, col }` | Open file + position cursor — **fixes LSP references/definitions/diagnostics preview** |
| `JumpInBuffer { buffer_id, line, col }` | Activate buffer + position cursor — **fixes lines/jumps/outline preview** |
| `OpenLspLog`, `OpenLspTraceLog` | Activate the log buffer |
| `InvokeCommand`, `PasteRegister`, `JumpToMark`, `ExpandSnippet` | No preview (action is irreversible / side-effecting) |
| `AcceptIndexed*`, `AcceptShowMessageAction` | No preview (stateful; would require apply-and-undo) |
| `InsertText` | No preview (cmdline-completion; no picker context) |
| `Custom` | Plugin-defined |

The preview dispatch becomes a sibling `match` next to the
accept dispatch — same shape, both reading the same typed
`accept_action` field. The slice that makes preview universal
slots into the post-7d cleanup as **slice 7g**:

> `3c.unify.preview-accept-driven` — small — Migrate
> `preview_picker_selection` from `routing_for`+
> `PickerAction::SwitchToBuffer` guard to a typed match on
> `candidate.accept_action`. Adds preview support for
> JumpToFileLocation / JumpInBuffer / OpenFile (the
> functionality the user has been missing). Preserved-behaviour
> tests: existing buffer-switcher preview still fires.

### What's the minimum we MUST do

For the v1 promise of **shared engine + plugin contract**, the
required slices are:

- **1-3** (Pipeline-as-engine, picker filter through pipeline)
- **9** (`CandidateSource` kind admits both Generator and
  PreSupplied)
- **7** (`SourceRegistration` with both source kinds)

Everything else (4-6 marginalia, 10-17 LSP migration) is
incremental polish. The LSP pickers KEEP WORKING from slice 1
onwards — their filter loop runs through Pipeline; their accept
dispatch goes through `PickerAction` as-is. The migration to
`AcceptHandler` is a separate quality-of-life refactor.

### Plugin implication: both source kinds must be exposed via WIT

A future plugin adding a network-driven picker source (workspace
symbols, GitHub issues, JIRA tickets) wants `PreSupplied` because
the network call should run before opening the picker, not on every
keystroke. The WIT API sketched above maps:

```wit
register-source: func(name: string, source: candidate-source);

variant candidate-source {
    generator(generator),
    pre-supplied(list<candidate>),
}
```

A plugin that needs a network call wraps it in
`pre-supplied(await fetch_results(query))` — same source-
registration entry point, plugin is in control of when async work
happens.

## Plugin extensibility implications

The unified pipeline gives plugins a single, principled extension
surface. WIT API shape (post-WASM plugin host):

```wit
interface completion {
    record candidate {
        text: string,
        display: string,
        kind: candidate-kind,
        doc: string,
    }

    resource generator {
        constructor(source-name: string);
        generate: func(query: string, ctx: gen-context) -> list<candidate>;
        cache-key: func(ctx: gen-context) -> option<string>;
    }

    /// Plugin entry point. Registers one or more sources;
    /// each lights up in EVERY completion surface (picker,
    /// cmdline, insert) without per-surface plumbing.
    register-source: func(name: string, gen: generator);

    register-annotator: func(name: string, fn: annotator);
    register-matcher: func(name: string, fn: matcher);
    register-ranker: func(name: string, fn: ranker);
}
```

A plugin that adds (say) workspace-symbol search registers one
generator. The user can immediately use `:picker
workspace-symbols` AND `:rip-grep <Tab>` (a cmdline `rip-grep`
command with the symbol-name slot) without the plugin author
writing per-surface code. That's the extensibility win.

### Source-registration metadata

`SourceRegistration` carries optional UI hints — title (for
picker), trigger-pattern (for cmdline-slot match) — but the
generator + matcher + rankers + annotators are the substantive
contract. Surfaces use UI hints; plugins don't have to know
which surface will actually pick them up.

## Slice plan

Sequencing lives in
[`docs/dev/operations/slice-plans/completion-pipeline-unification.md`](../operations/slice-plans/completion-pipeline-unification.md);
authoritative status per slice lives in
[`docs/dev/operations/implementation.md`](../operations/implementation.md).
This fragment owns *what* and *why*; the slice plan owns
*when* and *in what order*.

## Performance considerations

Picker's `filter()` runs on every keystroke against 5k-50k
candidates (file picker; buffer list is smaller). At 50k
candidates with the post-D2 pipeline:

- Generator stage: zero work (`PreSuppliedGenerator` yields
  the pre-existing Vec)
- Matcher stage: 50k × `fuzzy_match` calls — same as today
- Ranker stage: 50k × N rankers × scalar arithmetic — N=2 today
  (Score + MRU); 50k × 2 × ~ns = ~µs
- Annotator stage: 50k × M annotators × ~ns each; M=2 typical;
  ~µs

Vtable dispatch overhead per stage: ~5ns × 5 = 25ns per candidate
× 50k = ~1.25ms. Within the existing filter loop's budget.

Tested + recorded in `benchmarks.md` slice 8 above. Any
measurable regression triggers a counter-redesign before slice
3 ships.

## Open questions

1. **Pipeline cache scope.** Today's `GeneratorCache` is keyed
   on generator output. For picker's pre-supplied case the cache
   is trivially correct (one entry per open). For cmdline
   completion where the source can change per slot, the existing
   per-generator cache is correct. No change needed but worth
   confirming during slice 3.

2. **MRU storage location.** Today picker holds `mru_bonuses:
   HashMap<String, f64>` per session. After promotion to a
   ranker, MRU could be (a) per-session like today, or (b)
   persisted to a per-app store (so it survives restarts). v1
   keeps per-session; persistence is a follow-up.

3. **WIT interface shape.** The `register-source` WIT shape
   sketched above is provisional; the actual WIT lands when the
   plugin host phase starts. The Rust-side `SourceRegistration`
   type in slice 7 is the substrate that the WIT bindings
   target, so its shape is the contract.

4. **Insert-mode integration.** Insert-mode completion has its
   own state machine (`InsertCompletionState`) and triggers
   (Ctrl-Space, auto-trigger characters). It already uses
   `fuzzy_match`. Should it ALSO move to `CompletionPipeline`?
   Probably yes, in a follow-up — but out of scope for this arc.
   Insert completion has additional concerns (LSP async sources,
   per-document language-specific sources) that deserve their
   own slice.
