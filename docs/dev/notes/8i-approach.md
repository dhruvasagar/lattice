# Slice 8.i Approach Memo -- Retiring the `bind_legacy` Bridge

**Status:** draft, pre-implementation. This memo is the architecture
artefact that has to land *before* slice 8.i code, per CLAUDE.md's
"non-trivial design changes ship four artefacts together" rule.

**Scope:** the close-out slice for the M3 keymap migration. Slices
8.d-h promoted Replace / Visual / Insert / Normal bindings into the
`KeymapRegistry` trie, but every site still routes through
`KeymapHandle::bind_legacy` -- the migration-phase escape hatch that
embeds an App-level `Action` directly inside `BoundCommand`. Slice
8.i retires that escape hatch and finishes the conversion to typed
`CommandInvocation` dispatch.

**Companion docs:**
- [`docs/../architecture/keymap-architecture.md`](../architecture/keymap-architecture.md) §9 (slice
  list, Action-collapse trade-off flagged in §10).
- [`docs/../architecture/design.md`](../architecture/design.md) §5.2.1 (unified dispatcher), §5.2.3
  (five-layer keymap), §5.2.4 (extensibility).
- [`docs/m3-binding-census.md`](m3-binding-census.md) -- raw inventory
  of every built-in binding.


## 1. What is the bridge today

Every per-mode keymap module
(`crates/lattice-ui-tui/src/keymap_{replace,visual,insert,normal}.rs`)
calls `handle.bind_legacy(...)` to register a chord. The signature is:

```rust
fn bind_legacy(
    &self,
    layer: KeymapLayer,
    mode: BindingMode,
    path: &[ChordPattern],
    action: Action,
    source: SourceLocation,
);
```

Internally `bind_legacy` constructs a `BoundCommand` via
`BoundCommand::from_legacy_action(action, source, layer)`. That
helper sets:

```rust
command:        CommandInvocation::of(legacy_action_command_id())  // sentinel id 0
legacy_action:  Some(action)
```

Lookup returns the `BoundCommand`; per-mode `dispatch_*` functions
then branch on `legacy_action.is_some()` and route through the App's
`Action` enum (`App::apply(action)`) instead of the dispatcher's
`execute(invocation)` path.

**Counts (verified):** 106 `bind_legacy` call sites across four
files (Normal 90, Replace 6, Visual 3, Insert 7). The distinct
`Action` *shapes* bound this way are around 30-40 (most of those 90
Normal sites bind the same `Invoke(...)` variant with different
operand args, so they don't need promotion -- they already carry a
real `CommandInvocation` inside the legacy `Action::Invoke`).

The bridge was deliberate: each per-mode slice landed green by
moving the *trie shape* without rewriting the *dispatch tail*. Slice
8.i is the rewrite of the dispatch tail.

`BoundCommand` carries this in its public type today (one of the
debts the slice retires):

```rust
pub struct BoundCommand {
    pub command: CommandInvocation,
    pub legacy_action: Option<Action>,
    pub source: SourceLocation,
    pub layer: KeymapLayer,
}
```

The unfilled slot at the dispatcher end is in
`crates/lattice-grammar/src/dispatcher.rs:65`:

```rust
CommandKind::Action => Err(CommandError::InvalidArgs(
    "free-form actions are not yet wired in Phase 1",
)),
```

That's the seam slice 8.i fills.


## 2. The principle in play

The four paramount goals (CLAUDE.md) push this slice in one
direction unambiguously:

- **Performance** is neutral on this -- a bound `CommandInvocation`
  and a bound `Action` resolve at the same big-O. The only cost
  difference is one branch (`legacy_action.is_some()`) on the hot
  path; the slice removes it.
- **Extensibility** wants every keymap binding to be the same shape
  as a plugin contribution. Plugins can only produce
  `CommandInvocation`s through the WIT-shaped bind API (slice 8.h,
  `try_bind_chord_string`). A built-in binding that smuggles an
  App-level `Action` past the registry is a second-class citizen
  the WIT surface can't replicate.
- **Vim grammar** -- the unification claim of §5.2.1 is "operators
  / motions / text-objects / ex-commands / plugin contributions /
  palette entries all share `CommandInvocation` and flow through one
  `execute(...)`." App-level chords (`<Esc>`, `<C-w>v`, `o`, `gv`)
  are part of the grammar; if they don't share that shape the
  unification is partial.
- **Asynchronicity** is neutral.

So the slice's existence is not in question. The architectural call
is the *carrier shape* across the dispatcher boundary, covered next.


## 3. The architectural decision: how does `CommandKind::Action`
return into the App?

The dispatcher's contract today is "every command resolves to an
`Effect`" (or an error / cancellation). The `Effect` enum in
`lattice-grammar/src/effect.rs` already has 64 variants spanning
core (`Edits`, `SelectionChange`, `Yank`) *and* App-level
(`OpenHover`, `BufferDelete`, `LspRestart`, `OpenFileTree`,
`Substitute { ... }`). The boundary between "core Effect" and
"App-side Effect" is already permeable -- ex-commands push
App-level `Effect` variants today.

The keymap-bound `Action` variants we need to promote span ~30 of
the App's ~190 `Action` types. Three options:

### Option α -- one wrapper Effect variant

```rust
// new variant
Effect::AppAction(AppEffect)

// AppEffect is a relocated subset of today's Action enum, in
// lattice-grammar (or a new shared crate).
```

Each `CommandKind::Action` registry entry's `apply` returns
`Effect::AppAction(<concrete variant>)`. The App's `apply_effect`
gains one new arm:

```rust
Effect::AppAction(app) => self.run_app_effect(app),
```

`run_app_effect` is essentially today's `App::apply(action)` body
moved over.

- **Pro:** Effect's variant count doesn't balloon. Clear visual
  distinction between dispatcher-native effects and App-only ones.
- **Pro:** AppEffect can carry App-internal types (`Pending`,
  `ViewportPos`) without those types polluting `lattice-grammar`.
- **Con:** Two enums for what is essentially one concept (the
  return value of `execute()`). Future maintainers have to decide
  "is this new thing an Effect or an AppEffect?" -- another fake
  boundary.

### Option β -- inline the variants into Effect

Promote each bindable App-action to a first-class `Effect` variant
(`Effect::EnterAppend`, `Effect::OpenLineBelow`, `Effect::ExitVisual`,
`Effect::JoinLines { with_space }`, ...). Effect grows from 64 to
~95 variants.

- **Pro:** One type for the dispatcher's return value. No new
  category to reason about.
- **Pro:** Matches the existing pattern -- `Effect` already mixes
  core and App-level (`LspRestart`, `OpenFileTree` are App-level
  too).
- **Con:** Requires hoisting some App-only types into
  `lattice-grammar` (see §4). Increases Effect's size; pattern-match
  arms in `apply_effect` get longer.
- **Con:** `Effect` becomes opinionated about what App-level
  concerns exist (Pane navigation, Macros, Folds). Plugins
  contributing new App-level effects via WIT would have to extend
  Effect or go through a generic carrier.

### Option γ -- separate AppEffect return path

`CommandKind::Action`'s registry entry returns `GrammarResult<AppEffect>`
instead of `GrammarResult<Effect>`. The dispatcher's signature
forks: `execute_action(...)` returns `AppEffect`, the caller (App)
threads two distinct types.

- **Pro:** Cleanest separation.
- **Con:** Forks the dispatcher's return type. Conflicts with
  `Effect::Many(Vec<Effect>)` -- a chord that needs to emit *both*
  an Edit *and* an App-effect (rare but real -- think `<C-r>` redo
  + an EnterMode tail) suddenly can't.
- **Con:** Biggest refactor. Doesn't match the "one dispatcher,
  one Effect" design promise.

### Recommendation: **Option α, with a sunset clause toward β**

Option α is the right *first* shape. It admits the practical truth
that ~190 App actions live in `lattice-ui-tui` and most of them have
no business in `lattice-grammar`'s namespace. Wrapping them in
`Effect::AppAction(AppEffect)` keeps the dispatcher contract honest
("everything returns Effect") without dragging
`Pending`/`ViewportPos`/`PaneDirection`/etc. into the grammar
crate.

If, after slice 8.i lands, AppEffect proves to be a stable surface
that plugins *also* want to dispatch (via WIT), the natural next
step is to promote some AppEffect variants into Effect proper --
case-by-case, as the WIT need surfaces. Option α leaves that door
open without forcing the call now.

The trade-off accepted: one more enum to maintain. The alternative
(option β) trades that for a 50% bigger Effect that mixes editor-
core and editor-shell concerns. Worth a paragraph in §10 of
../architecture/keymap-architecture.md as the resolution of the
"`Action` enum collapse" trade-off flagged there.

### Where AppEffect lives

Three considered homes:

1. **`lattice-ui-tui::app::AppEffect`** -- rejected: `lattice-grammar`
   can't depend on `lattice-ui-tui` (cycle) and an opaque/erased
   Effect variant is worse than no abstraction.
2. **A new `lattice-app-effects` crate.** Initially attractive, but
   AppEffect's parameterised variants (slice 8.i.2) need
   `VisualKind` / `SearchDirection` / `ModalState` from
   `lattice-grammar`, so `lattice-app-effects` would have to depend
   on `lattice-grammar`. `lattice-grammar`'s
   `Effect::AppAction(AppEffect)` would then depend on
   `lattice-app-effects`. Cycle. Resolvable only by hoisting those
   types out of `lattice-grammar` into a shared sub-crate or into
   `lattice-protocol` -- both options pollute lower layers with
   grammar-specific concepts. Not worth the churn for slice 8.i.0.
3. **`lattice-grammar/src/app_effect.rs`** -- AppEffect as a sibling
   enum to Effect, in the same crate. **Picked.** Preserves α's
   intent (AppEffect is a distinct type from Effect; the carrier
   variant `Effect::AppAction(AppEffect)` keeps the dispatcher
   contract honest) without the dep-cycle. App-only types
   (`ScrollPos`, `ViewportPos`, `PaneDirection`) get added to
   `lattice-grammar` alongside AppEffect during 8.i.2 -- grammar
   already owns the closely-related `VisualKind`,
   `SearchDirection`, `ModalState`, `Register`, so the additions
   don't break the crate's tone. Future extraction into a separate
   crate is a no-behaviour-change refactor once the right shared-
   types home exists.


## 4. Type hoisting -- which App-only types move

Some Action variants carry types defined in `lattice-ui-tui` today.
For each, the call is "hoist into lattice-grammar (Effect-carried)
or keep in the new lattice-app-effects crate (AppEffect-carried)":

| Type | Used in | Recommendation |
|---|---|---|
| `ModalState` | `Effect::EnterMode` already | Already in lattice-grammar. No-op. |
| `Register` | `Effect::Yank` already | Already in lattice-grammar. No-op. |
| `VisualKind` | `Action::EnterVisual` | **Hoist** to lattice-grammar -- the modal engine owns visual mode; the App is downstream. |
| `SearchDirection` | `Action::SearchWordUnderCursor`, `Action::EnterSearch` | **Hoist** -- search direction is a grammar concept (used by `*`/`#`/`/`/`?`). |
| `ScrollPos` (`zz`/`zt`/`zb` target) | `Action::ScrollCursorTo` | Borderline. Keep in app-effects -- it's a viewport concern, not a grammar concern. |
| `ViewportPos` (`H`/`M`/`L` target) | `Action::JumpViewport` | Same: keep in app-effects. |
| `Pending` | `Action::SetPending` | **Retire entirely.** The trie's `Partial` lookup result subsumes pending state; `SetPending` is a leftover from the hand-rolled parser. See §6 below. |
| `OperatorId` | inside `Pending::AfterOperator` | Retired with Pending. |
| `PaneDirection`, `EchoMessage`, `VisualKind` (overlap above) | various | Keep in app-effects. |

The hoist of `VisualKind` and `SearchDirection` is independently
sensible -- those are concepts the grammar already reasons about
(visual mode IS a grammar `Range::Selection`; search direction is a
motion arg). Slice 8.i lands them as a small prelude.


## 5. Three categories of Action variants

Not every Action needs promotion. The 190-variant enum splits cleanly
into three groups:

### 5.1 Bindable (must promote)

Variants currently bound via `bind_legacy` in the per-mode keymap
modules. These get a `CommandKind::Action` registry entry, an
`AppEffect` variant, and the bridge dissolves. Examples:

- `EnterAppend`, `EnterMode(_)`, `EnterVisual(_)`, `ExitVisual`,
  `ReselectLastVisual`
- `OpenLineBelow`, `OpenLineAbove`
- `JoinLines { with_space }`, `MatchBracket`, `ToggleCaseAtCursor`
- `RepeatLastChange`, `Undo`, `Redo`
- `JumpHistoryBack`, `JumpHistoryForward`, `WalkMarkHistoryBack/Forward`
- `PageDown`, `PageUp`, `ScrollLineUp/Down`, `ScrollCursorTo(_)`,
  `JumpViewport(_)`
- `SearchWordUnderCursor(_)`, `SearchNext`, `SearchPrevious`
- Fold ops (`zo`, `zc`, `za`, `zR`, `zM`, `zd`, `zj`, `zk`, `zi`,
  `zf`)
- LSP-bound chords (`K`, `gd`, `gD`, `gy`, `gI`, `gr`)
- Completion / snippet popup chords (`<C-x>`-family in Insert)
- Pane chords (`<C-w>v`, `<C-w>c`, `<C-w>{h,j,k,l}`)
- Macro chords (`q`, `Q`, `@@`, `@<reg>`)

Estimated ~35 distinct AppEffect variants needed. The bound chord
count (106) is higher because the Normal mode `<C-w>` sub-tree, find-char
prefixes, count digits, etc. expand into many concrete bindings of
the same underlying action shape.

### 5.2 Transient input-layer state (don't promote)

Variants that aren't user-bindable -- they're emitted by the
cmdline / picker / search dispatchers as part of *raw character
ingestion*:

- `CommandLineAppend(c)`, `CommandLineBackspace`,
  `CommandLineSubmit`, `CommandLineCancel`,
  `CommandLineHistoryPrev/Next`, `CommandLineClear`,
  `CommandLineDeleteWordBackward`, `CommandLineAppendChord(_)`,
  `CommandLineCompleteOrAdvance`, ... (~15 cmdline variants)
- `PickerAppend(c)`, `PickerBackspace`, `PickerSelectNext/Prev`,
  `PickerAccept`, `PickerDismiss`
- `SearchAppend(c)`, `SearchBackspace`, `SearchSubmit`,
  `SearchCancel`

These are emitted by the per-mode dispatchers (`dispatch_command`,
`dispatch_picker`, `dispatch_search`) which read raw key events
straight off the input stream. They bind to *every* printable key,
not to specific chords. They have no business in the keymap trie
and stay as direct `App::apply(action)` calls from those
dispatchers.

After slice 8.i this is still true -- those dispatchers stay,
they just work alongside the keymap trie rather than being the only
game in town.

### 5.3 Internal control flow (retire)

- `Action::None` -- meaning "do nothing"; trie equivalent is
  `LookupResult::Unbound`. No AppEffect needed.
- `Action::Invoke(CommandInvocation)` -- the explicit
  CommandInvocation lane. Bindings that already have a real
  `CommandInvocation` (the bulk of Normal mode's 90 sites: motions,
  operators, text-objects) bind via `handle.bind(...)` directly,
  not via `bind_legacy`. After slice 8.i, every keymap binding's
  `BoundCommand.command` IS the dispatch.
- `Action::SetPending(_)` -- retired; trie's `LookupResult::Partial`
  replaces it. See §6.


## 6. Retiring the `Pending` state machine

The `Pending` enum (~13 variants: AfterCtrlW, AfterCtrlX, AfterG,
AfterOperator(OperatorId), AfterFindChar { ... }, AfterTextObject
{ ... }, AfterZ, AfterRegister, AfterMacroStart, AfterMacroPlay,
AfterSetMark, AfterJumpMarkLine, AfterJumpMarkExact, None) was the
hand-rolled parser's way of encoding "we're partway through a
multi-key chord."

The trie already has this concept built in. `KeymapTrie::lookup`
returns:

- `LookupResult::Partial { .. }` -- "we're at an intermediate node,
  waiting for the next key."
- `LookupResult::Bound { .. }` -- "complete chord, here's the
  binding."
- `LookupResult::Unbound` -- "no path from this prefix."

`Partial` carries the in-progress chord stack. The App needs to
preserve that across keystrokes. Today it preserves `Pending::AfterG`
between `g` and `gd`; in a trie world it preserves the partial
chord `[g]` until the next key resolves it.

### What does Pending become?

Two flavours:

- **Mechanical pending** (AfterCtrlW, AfterG, AfterZ, AfterCtrlX)
  -- these are pure prefix states. Trie subsumes them. The App's
  state machine just remembers "current partial chord."
- **Semantic pending** (AfterFindChar, AfterTextObject,
  AfterRegister, AfterSetMark, AfterJumpMarkLine, AfterMacroStart,
  AfterMacroPlay) -- these are wildcard-tail states where the next
  key isn't a fixed literal but a *captured* character (`f<X>`,
  `i<text-obj>`, `"<reg>`, `m<X>`, `'<X>`, `q<X>`, `@<X>`). The
  trie's `ChordPattern::CharLiteral` already encodes the wildcard;
  `LookupResult::Bound { command, captured }` returns the captured
  char.

So Pending dissolves. The trie does the prefix tracking; the
captured-char machinery does the wildcard tail. `Action::SetPending`
goes away and the App's `pending: Pending` field is replaced with
`partial_chord: Vec<KeyChord>` -- a thin parser cursor that resets
on every fully-resolved chord or on an explicit `<Esc>`.

This is the largest semantic change in the slice. It's fully
internal -- no test touches `Pending` directly -- but it's the
piece that takes the longest to land and verify.

**Caveat:** `Action::SetPending` is currently published into recorded
macros (`rec.actions.push(action.clone())`). Macros record
`CommandInvocation`s anyway (per design ../architecture/design.md §5.2 "Macros
record `CommandInvocation` sequences, not keystrokes") -- the
SetPending entries in today's macro recordings are migration debt
that goes away with the parser rewrite. Verify slice 8.i.4's macro
tests cover the gJ / `gd` / `dd` chords end-to-end so the macro
playback still works after Pending dies.


## 7. Slice plan

Each sub-slice lands green. The whole 8.i family is bigger than
the typical slice (because the dispatcher's Action branch is
unfilled) but the boundaries are clean.

### 8.i.0 -- AppEffect carrier + dispatcher's Action branch

- New crate `lattice-app-effects`. One file: `AppEffect` enum (the
  ~35 promoted variants from §5.1) + supporting types
  (`ScrollPos`, `ViewportPos`, `PaneDirection`, ...).
- `lattice-grammar`'s `Effect` gains
  `Effect::AppAction(lattice_app_effects::AppEffect)`.
- `lattice-grammar`'s registry gains `ActionSpec` (parallel to
  `OperatorSpec` / `MotionSpec` / `ExCommandSpec`) carrying an
  `apply: fn(&ActionContext) -> GrammarResult<Effect>`. Most
  ActionSpec applies are one-line: `Ok(Effect::AppAction(AppEffect::Foo))`.
- `dispatcher.rs::execute`'s `CommandKind::Action` branch fills in:
  `let spec = require_action(entry)?; (spec.apply)(&ctx)`.
- Hoist `VisualKind` and `SearchDirection` into lattice-grammar (§4).
- Tests: a new `crates/lattice-grammar/src/dispatcher.rs` test
  asserts an Action-kind invocation flows through `execute()` and
  produces the expected Effect.
- No call-site changes yet. Bridge still active.

**Land condition:** `cargo test --workspace` green; new dispatcher
test passes; no behaviour change.

### 8.i.1 -- Promote no-payload Action variants

Easy variants first: `EnterAppend`, `ExitVisual`,
`ReselectLastVisual`, `MatchBracket`, `ToggleCaseAtCursor`,
`OpenLineBelow`, `OpenLineAbove`, `Undo`, `Redo`,
`RepeatLastChange`, `JumpHistoryBack`, `JumpHistoryForward`,
`SearchNext`, `SearchPrevious`, `PageDown`, `PageUp`, fold ops
(except `zf` which uses Visual selection).

For each: register a `CommandKind::Action` entry in the catalog
(`crates/lattice-ui-tui/src/keymap.rs`), assign a `CommandId`,
replace the `bind_legacy(..., Action::Foo, ...)` site(s) with
`bind(..., CommandInvocation::of(FOO_ID), ...)`.

Drift tests stay -- they assert dispatch_replace / dispatch_visual
/ ... still produce the same `Action` for each input. The legacy
reference body in each per-mode test module changes from "match
KeyCode -> Action" to "match KeyCode -> AppEffect (then wrap in
`Action::AppEffect` for the comparator)" -- or the App's
`apply_effect` arm for `AppEffect::Foo` reuses the same body the
old `Action::Foo` arm did, so the comparator can compare AppEffects
directly.

Land in batches of ~5-10 variants per commit.

### 8.i.2 -- Promote parameterised Action variants

`JoinLines { with_space }`, `JumpViewport(ViewportPos)`,
`ScrollCursorTo(ScrollPos)`, `EnterMode(ModalState)`,
`EnterVisual(VisualKind)`, `SearchWordUnderCursor(SearchDirection)`,
`SetMark(c)`, `JumpToMarkLine(c)`, `JumpToMarkExact(c)`,
`SelectRegister(_)`, `StartMacroRecord(c)`, `PlayMacro(c)`.

Two encoding choices for the parameter:

- **Distinct CommandIds per parameter value** -- one ID for
  `JoinLines{with_space:true}`, another for `false`. Works fine
  for small param spaces (booleans, 3-4 enum variants).
- **CommandInvocation args** -- one ID for `JoinLines`, parameter
  encoded as a `CommandArg::Bool` in `invocation.args`. Necessary
  for char-payload variants (`SetMark(c)`) where the param space is
  too large for ID-per-value.

Mostly use distinct IDs for enum-bounded params; use args for char
captures and the rare numeric param. Document the convention as a
follow-on edit to `docs/../architecture/keymap-architecture.md` §3.5.

### 8.i.3 -- Wildcard-captured variants

The trie already returns `LookupResult::Bound { command, captured }`
for `ChordPattern::CharLiteral` paths. The captured char(s) need to
flow into the dispatched `CommandInvocation`'s args.

Two layers to wire:

- **Per-mode dispatcher** (`dispatch_replace`, `dispatch_normal`,
  ...): on a `Bound` lookup whose binding declares "expects
  capture", build a fresh `CommandInvocation` cloning the bound
  one and overriding `args` with the captured char.
- **Bound declaration:** the binding needs a flag indicating
  "args[0] receives the captured char" so the dispatcher knows
  to substitute. Today this is implicit (legacy_action carries
  `OverwriteChar('\0')` placeholder; dispatcher sees the placeholder
  and overrides). Replace mode's wildcard is the canonical example.

Concretely: the wildcards we need to carry through this slice are
`OverwriteChar(c)` (Replace), `f<c>`/`F<c>`/`t<c>`/`T<c>` (find),
`m<c>` (set mark), `'<c>`/`` `<c> `` (jump mark), `"<c>` (register
prefix), `q<c>` / `@<c>` (macro). All map to existing motions /
operators / actions; the slice is wiring the captured char into the
typed `CommandInvocation`.

### 8.i.4 -- Retire Pending; finalise

- Replace `App::pending: Pending` with `App::partial_chord:
  Vec<KeyChord>` (or whatever the trie cursor wants). Remove
  `Action::SetPending`, the `Pending` enum, the `resolve_after_*`
  family in `input.rs`, and the App match arms for them.
- Remove `bind_legacy`, `KeymapHandleLegacyExt`,
  `BoundCommand::legacy_action`, `BoundCommand::from_legacy_action`,
  `legacy_action_command_id`. Drop the placeholder `CommandId(0)`
  reservation.
- Drop legacy `translate_replace`/`translate_visual`/
  `translate_insert`/`translate_normal` from `input.rs` and the
  reference bodies in each per-mode test module.
- Replace the per-mode drift tests with the test
  ../architecture/keymap-architecture.md §9 (slice 8.i bullet) calls for: "every
  catalog entry resolves to a real `CommandInvocation`."
- Bench rollup. Confirm no regression on the keymap_handle_lookup
  hot path now that the trie is the only path.
- Doc updates: `docs/../architecture/keymap-architecture.md` §10 closes the
  "`Action` enum collapse" trade-off; `docs/../operations/implementation.md`
  records 8.i landed; this file gets a "completed" note.

**Land condition:** `cargo test --workspace` green; bench numbers
within baselines; manual smoke (open buffer, normal-mode editing,
visual selections, macros, marks, registers, find-char,
LSP go-tos, completion popups, picker) confirms no regression.


## 8. What this slice does *not* do

Bounded scope is part of the deliverable. Out of scope for 8.i:

- **Cmdline / picker / search wildcard handlers** stay imperative
  (§5.2). Promoting those to the keymap trie would bloat the trie
  with 95+ per-printable-char wildcards per mode for no
  user-facing win -- they're already wildcard-handled at the
  dispatcher level.
- **Plugin contributions of AppEffect.** Slice 8.h's WIT bind API
  takes `CommandInvocation` already; plugins contributing motions
  / operators / ex-commands work today. AppEffect is internal
  for now; opening it up to plugins is a v1.x decision once we
  have shipped users hitting the surface.
- **Major-mode keymaps.** `KeymapLayer::MajorMode` is wired in
  slice 8.h but no major-mode bindings exist yet. Major-mode
  registration follows the same shape; the slice is independent.
- **Macro recording format change.** Macros record
  `CommandInvocation`s already (../architecture/design.md §5.2). The `SetPending`
  entries in today's recordings are debt that disappears with
  Pending; no format-version bump needed.


## 9. Risks and open questions

- **Effect's growth.** Even with Option α, AppEffect is a 35-variant
  enum. It will grow as we add features (more LSP chords, more
  pane ops). The `lattice-app-effects` crate's job is to absorb
  that growth so `lattice-grammar` doesn't. Keep an eye on whether
  AppEffect develops a meaningful internal taxonomy (LspEffect,
  PaneEffect, ...) or stays flat.
- **Captured-char arg convention.** Slice 8.i.3 needs a one-time
  decision on how the bound `CommandInvocation` declares "I expect
  a captured char in args[0]." Suggested: a small `expects_capture:
  bool` field on `BoundCommand` (cheaper than an extra
  `CommandArg::CapturePlaceholder`). Bench impact: one bool read in
  the lookup tail; trivial.
- **Pending retirement test coverage.** The `resolve_after_*`
  family has subtle vim-isms (e.g. `df<Esc>` aborts the find;
  `2dd` parses as `count=2, op=d, range=CurrentLine`). The trie
  rewrite must preserve these. Suggest: keep the existing
  `input.rs` `match` branches as a *test-only* reference body
  (analogous to the per-mode legacy_translate functions in 8.d-h)
  so the new partial-chord parser has a drift target. Drop after
  one slice's worth of confidence.
- **Capability gating on AppEffect.** `KeymapCapability` (slice
  8.h) gates *which layers* a caller can bind to. It does NOT gate
  *which AppEffects* the dispatched binding can produce. Should it?
  A user-config plugin binding `<Leader>q` to `AppEffect::Quit`
  is fine; binding it to `AppEffect::DeleteFoldAtCursor` is also
  fine. There's no security boundary inside AppEffect today.
  Revisit if/when AppEffect opens to untrusted plugins.
- **Drift test transition.** The per-mode drift tests
  (`registry_dispatch_matches_legacy_translate`) are the M3
  migration's safety net. Slice 8.i.4 retires them. Land the
  retirement *after* the catalog-completeness test
  ("every entry resolves to a real CommandInvocation") plus a new
  end-to-end keystroke test ("every documented chord in
  `m3-binding-census.md` produces the expected Effect"). The
  latter is what the design doc actually wanted from drift tests
  in the first place.


## 10. What I want confirmation on before writing code

Specifically:

1. **Option α vs β** -- recommendation is α (new
   `lattice-app-effects` crate carrying the App-side effect enum;
   `Effect::AppAction(AppEffect)` is the wrapper). Confirm before I
   create the crate.
2. **Hoisting `VisualKind` / `SearchDirection` into lattice-grammar
   (§4)** -- these are grammar concepts; the hoist is independent
   of α/β. Confirm.
3. **Pending dissolution path (§6)** -- replacing `Pending` with
   trie-driven partial-chord state. The mechanical pending states
   are obvious; the wildcard-tail states (find-char, mark, register,
   macro) need careful trie encoding (`ChordPattern::CharLiteral`
   already exists, so the encoding is in hand). Confirm the
   approach.
4. **Slice 8.i.4's drift-test successor** -- proposed pair:
   "catalog entry completeness" + "every census-listed chord
   produces the documented Effect." Confirm or propose alternative.
5. **Sequencing** -- 8.i.0 must land before any of 8.i.1-3.
   8.i.4 must land last. 8.i.1, 8.i.2, 8.i.3 can land in any order
   (they touch disjoint Action variants). Ship 8.i.0 first as a
   no-behaviour-change foundation slice; review there before the
   call-site rewrites start.

If all four answers are "go," I'll start with 8.i.0 and post the
diff for review before touching call sites.
