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

| # | Slice | Effort | Description |
|---|-------|--------|-------------|
| 1 | `3c.unify.ranker-stack` | small | `Pipeline::ranker` → `Pipeline::rankers: Vec<>`. Composable rankers. Builtin `ScoreRanker` unchanged. Tests updated. |
| 2 | `3c.unify.mru-promotion` | small | `MruRanker` moves from `lattice-picker` to `lattice-completion`. Picker's `mru_bonuses` field plumbed into a `MruRanker` instance at filter time. Drops picker's inline `combined = score + bonus` arithmetic. |
| 3 | `3c.unify.picker-via-pipeline` | medium | `Picker::filter()` rewritten to call `Pipeline::run` with a `PreSuppliedGenerator` adapter that yields `raw: Vec<RawCandidate>`. Inline filter loop deleted. **Net deletion**, not addition. |
| 4 | `3c.unify.option-doc-annotator` | small | `OptionType::enumerate_with_docs()` extension + default impl. `OptionValueDocAnnotator` + `OptionNameDocAnnotator`. Wired into the cmdline pipeline for option-name + option-value slots. |
| 5 | `3c.unify.option-docs-builtin` | small | Concrete per-value docs added to built-in options that have enumerable values (foldmethod, foldenable, picker.display, ...). |
| 6 | `3c.unify.gpui-annotation-render` | small | GPUI picker overlay + cmdline-completion strip render annotations right-aligned (TUI already does). |
| 7 | `3c.unify.source-registration-api` | medium | `SourceRegistration` public type that bundles generator + matcher + rankers + annotators + UI hints. Wired into `:picker <name>` lookup and slot routing. Sets up the WIT contract shape that the plugin host will mirror. |
| 8 | `3c.unify.benchmarks` | small | Per-slice perf check: pipeline-vs-inline overhead at picker scales (5k, 50k candidates) recorded in `benchmarks.md`. Catches regressions before they ship. |

Slices 1-3 are the architectural core. Slices 4-6 deliver the
marginalia improvement. Slice 7 sets up the future plugin
contract. Slice 8 enforces no perf regression.

Each slice ships green and on its own — none depend on a later
slice landing. Slices 1-3 land in order (each depends on the
previous); 4-6 land in order; 7 can land anytime after 3; 8 is
a continuous obligation.

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
