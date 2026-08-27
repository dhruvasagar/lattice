# Picker Architecture (developer reference)

This document is the implementer-side companion to
[design.md §5.9.7](design.md) and [design.md §5.9.10](design.md)
(rich minibuffer). design.md is the terse, principle-led
canonical text; this is the longer-form "how it actually
works", with concrete pointers into the `lattice-picker`
crate.

The picker is one of the four Phase 4–6 features the design
doc names as "trait surfaces plugins will eventually implement
against" (see [`../operations/implementation.md`](../operations/implementation.md)
§ Build order). Its shape is therefore deliberate: every
seam here is also the seam WASM plugin sources will mirror
in WIT during Phase 7.

---

## 1. Goal

One vertico-style picker, source-agnostic. The user types
`:picker <source> [args]`; the picker primitive walks the
candidate set from the named source, runs the same matcher
+ ranker pipeline regardless of source, surfaces a typed
outcome on accept, and the host translates that outcome
into App-state mutation. Recency floats recently-used
candidates to the top automatically, across every source,
without any source opting in.

The three properties that drove the design:

1. **One picker, many sources.** Telescope-style. Adding a
   new source — first-party or plugin — does not add a new
   ex-command, a new keymap entry, or a new rendering path.
   It adds one registration call.
2. **Source emits data, picker does behaviour.** Sources
   generate `(candidate, routing_payload)` pairs and
   nothing else. Matching, ranking, MRU scoring, history
   persistence, preview, dispatch — all picker-owned.
   Plugin sources benefit from MRU and ranking for free.
3. **Outcomes, not effects.** Source generators return a
   typed `PickerAcceptOutcome` describing what should
   happen, never an `Effect` or an App mutation. The host
   translates outcomes into Effects. Plugins (Phase 7) can
   describe the same outcomes over WIT without learning the
   full grammar Effect enum.

---

## 2. Layout

```
crates/lattice-picker/
├── src/
│   ├── lib.rs              -- public re-exports
│   ├── picker.rs           -- Picker state machine (query buf, raw, candidates, selected)
│   ├── candidate.rs        -- RawCandidate / RenderedCandidate / PICKER_ROUTING_KIND_ID
│   ├── routing.rs          -- RoutingPayload enum + routing_identity() (MRU keying)
│   ├── source.rs           -- PickerSourceSpec, PickerSourceGenerator trait
│   ├── context.rs          -- PickerContext + per-feature ContextHandle traits
│   ├── outcome.rs          -- PickerAcceptOutcome enum
│   ├── registry.rs         -- PickerRegistry (sources + iteration for tab completion)
│   ├── matcher.rs          -- fuzzy/substring matcher (re-exported from lattice-completion)
│   ├── rank.rs             -- score combiner: match * α + frecency * (1-α)
│   ├── mru.rs              -- PickerMruIndex (frecency bonus + persistence)
│   └── events.rs           -- typed events: picker.opened, candidate.accepted, dismissed
├── tests/
│   ├── matcher.rs          -- candidate matching invariants
│   ├── rank.rs             -- score-combining edge cases
│   ├── mru.rs              -- frecency math + persistence round-trip
│   └── registry.rs         -- source iteration / tab-completion contract
└── benches/
    └── picker.rs           -- open-p99, refilter-p99, mru-bonus-p99
```

Every public item has rustdoc; this document explains how
they fit together.

---

## 3. Three-stage pipeline

Every picker open / refilter / accept runs through the same
three-stage pipeline:

```
┌────────────────────────────────────────────────────────────┐
│  source.init(ctx, args)  →  Vec<(RawCandidate, RoutingPayload)>
└────────────────┬───────────────────────────────────────────┘
                 │ (once at picker-open)
                 ▼
        snapshot MRU bonus into each candidate
                 │
                 ▼
┌────────────────────────────────────────────────────────────┐
│  per keystroke:                                            │
│    matcher.score(candidate.text, query)  ──┐               │
│                                            ├─▶ rank.combine
│    candidate.mru_bonus  ───────────────────┘               │
│                                                            │
│    → sort descending → top N rendered                      │
└────────────────────────────────────────────────────────────┘
                 │ (on <CR>)
                 ▼
┌────────────────────────────────────────────────────────────┐
│  source.accept(ctx, routing)  →  PickerAcceptOutcome       │
│  mru.record(source_id, routing_identity(routing))          │
│  host translates outcome → Effect → App mutation           │
└────────────────────────────────────────────────────────────┘
```

Stage one and three cross the source boundary; stage two
is picker-internal and runs entirely against host-cached
data. No source call fires per keystroke. **This is what
keeps WASM plugin sources affordable on the keystroke
hot path** — the only crossings are open and accept (the
§5.5 budget targets typed call < 500ns p99; we use the
budget twice per pick, not 50× per second).

### 3.1 Orderless matching — the query is a set, not a token

Stage two's matcher reads the query as a **set of
whitespace-separated components**, all of which must match, in any
order. `pick refil` finds `lattice-picker/src/refilter.rs` whether
the user recalls the crate or the file first. The implementation is
`lattice_completion::orderless_match`; the picker reaches it through
`OrderlessDisplayMatcher`, and `picker.orderless=false` swaps back to
the single-token `FuzzyDisplayMatcher`.

| Written | Means |
|---|---|
| `foo bar` | both `foo` and `bar` must match, either order |
| `!foo` | rows containing `foo` are excluded (literal, not fuzzy) |
| `foo\ bar` | one component containing a literal space |
| `\!foo` | one component whose first character is a literal `!` |

Each component runs the full five-tier ladder (exact → prefix →
word-boundary → substring → subsequence), rather than the stricter
prefix-only style emacs' `orderless-prefixes` uses. The reasoning is
the symptom, not parity: the complaint orderless answers here is
"too few matches", and a prefix-only style narrows the result set.
Prefix preference is preserved where it costs nothing — in the
**ranking**. A component landing on the prefix tier scores 800
against a substring's 400, so widening what matches does not scramble
what sorts first.

The candidate's score is the **mean** of its positive components'
tier scores, plus a 50-point bonus when the components happen to land
left-to-right. Mean rather than sum keeps a multi-word query inside
the same 0..1000 band a single-word query produces, so the MRU bonus
(0..~110, calibrated in §6 as a within-tier tie-break) keeps meaning
the same thing. The order bonus sits below the 200-point gap between
adjacent tiers, so it can never promote a subsequence match above a
substring one.

A query with no whitespace delegates verbatim to the single-token
`fuzzy_match` — same score, same ranges. Orderless is only allowed to
change behaviour after the user types a space, which is what makes it
safe to leave on by default for every picker.

`picker.orderless` is snapshotted onto the `Picker` at open time
rather than read per keystroke. `lattice-picker` deliberately carries
no config dependency — that missing edge is what makes its
off-the-UI-thread guarantee structural rather than a matter of
discipline — and a picker already on screen must not change matching
semantics under a user who is mid-query.

---

## 4. The trait surface

### 4.1 `PickerSourceSpec` (metadata)

```rust
pub struct PickerSourceSpec {
    pub id: &'static str,                  // "files", "recent", "lsp-references"
    pub doc: &'static str,                 // one-line summary, surfaces in :describe-picker
    pub args_schema: Vec<ArgSpec>,         // shared with grammar::ExCommandSpec
    pub args_hint: &'static str,           // shown in cmdline parameter hints
}
```

`ArgSpec` is the same type ex-commands use today
(`crates/lattice-grammar/src/ex_commands.rs`). Reusing it
means `:picker files <Tab>` and `:e <Tab>` complete paths
through the same completion source — no per-picker tab
plumbing.

### 4.2 `PickerContext` (host → source snapshot-in)

Built host-side once on `:picker <source>` open. Composed
of per-feature **handles**, each owned by the feature
crate that has the relevant state:

```rust
pub struct PickerContext<'a> {
    pub active_buffer: BufferSnapshot,             // path, language, cursor, selection
    pub workspace_root: &'a Path,
    pub buffers: &'a BufferRegistryView<'a>,       // iterable view, not the full registry
    pub recent_files: &'a [PathBuf],
    pub marks: &'a MarksView<'a>,
    pub registers: &'a RegistersView<'a>,
    pub position_history: &'a [PositionEntry],
    pub lsp: Option<LspContextHandle<'a>>,         // facade; impl in lattice-lsp
    pub snippets: Option<SnippetContextHandle<'a>>,// facade; impl in lattice-snippet
    pub mode_facade: ModeContextHandle<'a>,
}
```

Each handle exposes only what the picker source needs — not
the full App. `LspContextHandle::request_references(pos)`,
not `&LspSupervisor`. `SnippetContextHandle::for_language(id)`,
not the snippet registry's internal map.

**Composition rule.** Each feature crate owns its handle
trait and the App-side impl. `lattice-picker` declares the
`PickerContext` struct but the handle traits live in the
feature crate that knows the state. Adding a new
context slice does not require editing `lattice-picker` —
just the App-side assembler in `lattice-ui-tui`.

### 4.3 `PickerSourceGenerator` (the trait)

```rust
pub trait PickerSourceGenerator: Send + Sync {
    fn spec(&self) -> &PickerSourceSpec;

    fn init(
        &self,
        ctx: &PickerContext,
        args: &Args,
    ) -> PickerInitResult;

    fn accept(
        &self,
        ctx: &PickerContext,
        routing: &RoutingPayload,
    ) -> Result<PickerAcceptOutcome, String>;
}

pub enum PickerInitResult {
    Inline(Vec<(RawCandidate, RoutingPayload)>),                       // sync, immediate
    Future(BoxFuture<'static, Result<Vec<(RawCandidate, RoutingPayload)>, String>>),
    Stream(BoxStream<'static, Vec<(RawCandidate, RoutingPayload)>>),   // batched chunks
}
```

The three init shapes cover every Phase 4–8 source:

- **`Inline`** — files, recent, lines, marks, registers,
  jumps, commands, snippets, tree-sitter-driven outline.
- **`Future`** — every LSP-backed lookup (references,
  definitions, document symbols, workspace symbols, code
  actions, diagnostics).
- **`Stream`** — live-grep (subprocess streams stdout in
  chunks); live LSP completion.

Each maps cleanly to a Component Model construct (record /
future / stream) when Phase 7 mirrors this in WIT. The
plugin host wraps a WASM call as `Box<dyn PickerSourceGenerator>`
— the registry, the matcher, the renderer don't know which
sources are native and which are WASM-backed.

### 4.4 `PickerAcceptOutcome` (source → host)

```rust
pub enum PickerAcceptOutcome {
    OpenFile { path: PathBuf },
    SwitchBuffer { buffer_id: BufferId },
    JumpInBuffer { buffer_id: BufferId, line: u32, col: u32 },
    JumpToMark { name: char },
    JumpToLocation { path: PathBuf, line: u32, col: u32 },  // path resolved, may need to open
    InvokeCommand { id: String, args: Args },
    PasteRegister { reg: Register },
    ExpandSnippet { id: SnippetId },
    OpenLspLog { server_id: String },
    OpenLspTraceLog { server_id: String },
    ApplyLspCodeAction { handle: CodeActionHandle, index: u32 },
    ApplyLspCompletion { item_index: u32 },
    OpenPrompt {                     // picker-accept's peer of Effect::OpenPrompt
        prompt: String,
        initial: String,
        on_submit_action: String,
        buffer_name: Option<String>,
    },
    NoOp,                                                    // dismissed via accept-on-empty etc.
}
```

A **bounded** enum, smaller than `Effect`, scoped to "things
a picker can ask the host to do." The host translates each
variant into the appropriate `Effect` / App mutation. WIT
mirrors this as a `variant` record with the same arms.

Why not emit `Effect` directly:

- Smaller WIT surface in Phase 7.
- Source generators cannot accidentally emit grammar-level
  effects (compose ops, range-substitute, etc.) that
  bypass picker conventions.
- Audit-able: every picker side-effect on App routes through
  one translator function, easy to test.

**`OpenPrompt` chains "pick, then type."** Same fields as
`Effect::OpenPrompt` (§8's rich-minibuffer cross-reference below),
same name-based `on_submit_action` lookup through
`ActionHandlerRegistry` — no closures cross the source/host boundary.
It exists because a source's `accept` can only return a typed
`PickerAcceptOutcome`, never call `&mut Editor` itself; a source that
wants to *chain* a picker step into a follow-up text prompt (rather
than complete the operation on accept) needs its own outcome variant
to carry the prompt's parameters back to the host, exactly like
`OpenFile`/`SwitchBuffer`/etc. carry theirs. `Editor::apply_picker_outcome`
handles it identically to how it handles every other outcome variant:
by calling `open_prompt_line(prompt, initial, on_submit_action,
buffer_name)`, the same host method `Effect::OpenPrompt` calls. The
first (and, as of this writing, only) consumer is magit's branch-create
wizard: `BranchPickBaseSource` (`lattice-magit/src/picker_sources.rs`)
returns `OpenPrompt` from `accept` to ask for the new branch's name
after the user picks a base branch, stashing the picked base in the
prompt buffer's synthetic name (`*magit:branch-create-from:<base>*`)
for the submit handler to read back — see
[`magit.md`](magit.md) §12.9. This is a picker-crate-level mechanism,
not magit-specific; any source can chain a prompt this way.

---

## 4bis. Transient mode — action menus on picker substrate

> **Design fragment.** The transient interaction mode extends the picker
> to serve magit-style action menus (dispatch, branch, rebase, file operations).
> The magit-specific menu content (dispatch / file-dispatch item lists,
> key choices) lives in [`magit.md`](magit.md) §8. This section covers the
> picker's role as the rendering and interaction substrate — the generic
> mechanism any mode (or plugin, eventually) can build on, not just magit.

### 4bis.1 Why the picker

Transients share the picker's core requirements — floating-overlay /
minibuffer rendering, keyboard capture, scroll, dismiss — but differ in
the interaction model. Instead of filtering candidates by typing a
query, the user presses single-letter keys to fire actions, toggle
flags, or open nested submenus. Rather than building a parallel
rendering + input system, a transient is carried as sibling state on
the existing `Picker` struct: `picker.transient: Option<Arc<TransientSpec>>`,
plus `transient_state` (flag/argument values), `transient_stack`
(submenu back-stack), and `transient_selected`. There is no `PickerMode`
enum — the picker's candidate-list fields (`candidates`, `query`, ...)
simply sit unused while `transient.is_some()`, and every call site that
needs to tell the two apart checks that field directly.

What the picker substrate actually provides that transients reuse:

| Picker feature | Transient use |
|---|---|
| The `Picker` struct + its open/dismiss lifecycle | `picker.transient` is a field on the same struct; `q`/`Esc`/`C-g` close a transient through the same `do_picker_dismiss` every other picker surface uses |
| `picker.display` (config: `"minibuffer"` \| `"popup"`) | The SAME computed flag (`picker_display_is_minibuffer` / `picker_use_minibuffer`) that places regular candidate lists also places the transient — popup box or bottom-anchored strip, see §4bis.5 |
| Theme-consistent overlay chrome (border, background, text colors) | The transient's own popup/strip builders (TUI `draw_transient_overlay`, GPUI `build_transient_gpui`) use the same theme fields as the regular picker overlay, styled as a sibling surface, not a shared render function |

What is genuinely new for transient mode, not reused from candidate-list
pickers at all:

| New capability | Mechanism |
|---|---|
| Grouped entries with section headers | `TransientGroup { label, items }` renders as bold header + indented rows |
| Single-key trigger dispatch | Each `TransientItem` carries `key: Vec<String>`; pressing a matching key fires immediately, no `<CR>`, no cursor navigation |
| Flag toggle in-place | `TransientItemKind::Flag { name, default }` toggles the `TransientValue::Bool` keyed by `name` in `transient_state` |
| Nested submenus with scroll-preserving back-stack | `TransientItemKind::Submenu(Arc<TransientSpec>)` pushes `(parent_spec, parent_state, parent_scroll)` onto `transient_stack`; `BS`/`DEL` pops it — see §4bis.6 |
| Inline live preview | `TransientSpec::preview: Option<Box<dyn Fn(&TransientState) -> String + Send + Sync>>` is called on every render and its output painted below the item list, inside the transient's own popup/strip — NOT the picker's file/buffer preview pane |

Not reused: **MRU**. Transient items are a fixed, spec-defined list —
they never go through the source-generator / scoring pipeline (§6), so
there is no frecency ranking, no `routing_identity`, no MRU record on
accept. Firing a transient action dismisses and invokes an action
handler directly (§4bis.3); it isn't a scored candidate.

### 4bis.2 Data model

The real shape, `crates/lattice-picker/src/transient.rs`:

```rust
pub struct TransientSpec {
    pub title: String,
    pub groups: Vec<TransientGroup>,
    /// Called on every render (flag toggle, argument change, submenu
    /// nav); the picker's file/buffer preview pane is NOT involved.
    pub preview: Option<Box<dyn Fn(&TransientState) -> String + Send + Sync>>,
    pub footer: Option<String>,
}

pub struct TransientGroup {
    pub label: String,
    pub items: Vec<TransientItem>,
}

pub struct TransientItem {
    pub key: Vec<String>,       // ["y", "Y"] — no dedicated KeyChord type
    pub label: String,
    pub description: String,
    pub kind: TransientItemKind,
    // no `marginalia` field — see §4bis.7
}

pub enum TransientItemKind {
    /// Fires an action via the action-handler registry and closes
    /// the transient.
    Action(CommandId),
    /// Opens a nested transient (submenu).
    Submenu(Arc<TransientSpec>),
    /// Toggles in place. `default` seeds `transient_initial_state`.
    Flag { name: String, default: bool },
    /// Defined, but not wired to input yet — see §4bis.7.
    Argument { name: String, default: Option<String>, prompt: String },
    /// Dismisses without firing anything (`n`/`q` in a confirm dialog).
    Dismiss,
}

pub type TransientState = HashMap<String, TransientValue>;

pub enum TransientValue {
    Bool(bool),
    String(String),
}
```

There is no `ActionId` type (transient actions fire through
`lattice_protocol::ids::CommandId`, the same id every other command
dispatch path uses) and no separate `PreviewFn` type alias — the
closure type is written out inline on `TransientSpec::preview`.

### 4bis.3 Two paths to open a transient

There are exactly two ways a `TransientSpec` reaches the screen, and
they exist for a structural reason, not stylistic preference:

**(a) Direct call, from code with `&mut Editor` access.** A mode
handler builds a spec and calls it straight:

```rust
impl Editor {
    /// Seats `spec` as the open picker's transient, hidden query line.
    /// No `PickerMode` variant, no `display` param, no handle type —
    /// the caller already holds `&mut Editor` and gets nothing back
    /// worth returning.
    pub fn open_transient(&mut self, spec: TransientSpec) -> Vec<RendererSignal>;

    /// Tears the transient down via the same `do_picker_dismiss` every
    /// other picker surface uses.
    pub fn close_transient(&mut self) -> Vec<RendererSignal>;
}
```

The only current caller is `Effect::Confirm { prompt, yes_action }`'s
handling (yes/no confirmation dialogs): it resolves `yes_action` to a
`CommandId`, builds a spec via
`lattice_picker::confirm_transient_spec(&prompt, cmd_id)` (a two-item
spec: `y`/`Y` → `Action(cmd_id)`, `n`/`N`/`q`/`Q` → `Dismiss`), and
calls `open_transient` directly. `TransientItemKind::Dismiss` exists
specifically for this — a confirmation's "no" answer must close the
transient without an action ever being registered for it.

**(b) `Effect::OpenTransient { source: String }`, resolved through the
`TransientSourceRegistry`.** An ex-command's `apply` closure can only
return an `Effect` — it has no `&mut Editor`, no service access, just
a typed value handed back to the dispatcher. `Effect` is defined in
`lattice-grammar`, and `TransientSpec` lives in `lattice-picker`, which
depends on `lattice-grammar` (for `CommandId`) — not the other way
around. `lattice-grammar`'s `Effect` enum therefore CANNOT embed a
`TransientSpec` directly without an illegal upward dependency. So
`Effect::OpenTransient` carries only a name; each renderer's
effect-handling site resolves it:

```rust
// TUI: lattice-ui-tui/src/app/picker.rs, do_open_transient
// GPUI: lattice-ui-gpui/src/lib.rs, the Effect::OpenTransient arm
let Some(registry) = e.services.get::<TransientSourceRegistryHandle>() else { ... };
let Some(spec) = registry.build(&source) else { ... };
e.open_transient(spec)
```

This mirrors `Effect::OpenPicker { source: String, args }`'s existing
named-source shape exactly, for the identical structural reason (§5.2).
magit's `magit-dispatch` and `magit-file-dispatch` ex-commands are the
only current users of path (b).

### 4bis.4 `TransientSourceRegistry`

```rust
pub struct TransientSourceRegistry {
    sources: std::sync::Mutex<HashMap<String, Arc<dyn Fn() -> TransientSpec + Send + Sync>>>,
}
pub type TransientSourceRegistryHandle = Arc<TransientSourceRegistry>;

impl TransientSourceRegistry {
    pub fn register(&self, name: impl Into<String>, builder: impl Fn() -> TransientSpec + Send + Sync + 'static);
    pub fn build(&self, name: &str) -> Option<TransientSpec>;
}
```

Registered as a service handle (`SubsystemBoot::register_service`),
same `Arc<X>` register-and-lookup convention as every other service.
`lattice-magit::install()` populates it while it still has direct
`&mut CommandRegistry` access (`boot.commands_mut()`): it resolves the
`CommandId`s for its `action:magit-global-*` handlers first, then
captures them **by value** into a `move || ...` zero-arg builder
closure. That's the trick that lets a zero-arg `Fn() -> TransientSpec`
builder still fire real per-item actions — the ids are resolved once,
at boot, and baked into the closure; `build()` re-invokes the closure
(and therefore reconstructs the spec, with those same ids) every time
the transient opens.

**Deliberate asymmetry with `PickerRegistry`, not an oversight:**
`PickerRegistry` (§5.1) is an `ArcSwap`-backed RCU registry, because
picker sources can be registered at WASM plugin-load time, at runtime.
`TransientSourceRegistry` is a plain `Mutex<HashMap<...>>` — read-only
after boot in practice, since each owning crate's `install()` populates
it once and nothing currently unregisters or re-registers later. That
said, `PickerRegistry` also exposes `unregister`, and this registry
does not; if a future WASM-loaded transient source needs to be pulled
at runtime (plugin disabled/reloaded), that gap would need closing —
flagged here, not yet needed.

### 4bis.5 Display: picker decides where, transient decides what

Before this session, transient always rendered as a floating popup
regardless of `picker.display`, using its own separate (and buggy —
GPUI had no minibuffer path at all, and its popup path duplicated the
scroll-windowing logic) placement code. That violated the same
principle every other picker surface follows: **the picker owns
placement, the surface owns content.**

Now the split is exact:

- **Picker's job — where.** The typed `picker.display` option
  (`lattice_config::core_options::PickerDisplay`, a `String` valued
  `"minibuffer"` or `"popup"`, default `"minibuffer"`) is read once per
  frame into a single boolean (TUI: `picker_display_is_minibuffer` →
  `picker_is_minibuffer`; GPUI: `picker_display_is_minibuffer` →
  `picker_use_minibuffer`). `render()` / `draw_frame` branch on that
  ONE flag to decide whether the *transient* renders as a bordered
  floating popup (TUI `draw_transient_overlay`; GPUI
  `build_transient_gpui`) or a bottom-anchored strip claiming the
  cmdline/candidate rows (TUI `draw_transient_minibuffer_prompt` +
  `draw_transient_minibuffer_candidates`; GPUI
  `build_transient_minibuffer_gpui`) — the exact same flag that already
  decided popup-vs-strip for regular candidate-list pickers. There is
  no separate `PickerDisplay` enum with `Inline`/`Floating`/
  `BottomPopup` variants; it's the one config string, one flag, reused.
- **Transient's job — what.** Both placements call the SAME
  row-windowing function to get the visible group/item lines: TUI
  `transient_group_item_lines`, GPUI `transient_rows_gpui`. Each walks
  `spec.groups`, applies the scroll offset `TransientSpec::scroll_for`
  derives from the current selection, and stops once its visible
  budget is reached (GPUI bounds this at a fixed
  `TRANSIENT_MAX_VISIBLE_ROWS = 24`, since it has no cheap "how many
  rows fit" query the way the TUI can measure its actual terminal
  area). One function per renderer, called by BOTH the popup wrapper
  and the minibuffer wrapper, so the row/scroll computation cannot
  drift between the two placement modes the way it did before (GPUI's
  popup path had its own scroll-aware loop; there was no minibuffer
  loop to drift *from* until this session).

Two smaller fixes landed alongside the split:

- `transient_stack` is now `Vec<(Arc<TransientSpec>, TransientState, usize)>`
  (was a 2-tuple) — entering a submenu pushes the parent's position
  alongside its spec + state and resets it to 0; backing out
  (`BS`/`DEL`) restores it. Previously it leaked across submenu
  navigation: entering a submenu from partway down a parent opened the
  submenu already positioned, and backing out left the parent wherever
  the submenu happened to leave the shared field.
- GPUI's popup container (`build_transient_gpui`) now has
  `.overflow_hidden()` — every sibling popup in that file already
  clips its content; this one didn't, so a transient with enough items
  to exceed its `max_h` bled past the bordered box onto whatever was
  underneath.

### 4bis.6 Keyboard handling

When a transient is open, keystrokes route through
`Action::TransientTrigger` / `TransientToggleFlag` / `TransientDismiss`
instead of the candidate-list actions:

| Key | Action |
|---|---|
| A key matching one of an item's `key` strings | `do_transient_trigger` fires the item: dispatches the action and dismisses (`Action`), toggles the flag in place (`Flag`), pushes the stack and opens the submenu (`Submenu`), or dismisses without acting (`Dismiss`) |
| Select-next / select-prev (same chords the candidate list uses) | Moves `transient_selected` by ±1 over the spec's items, wrapping — the renderer derives its scroll from that (§4bis.5bis) |
| `<CR>` | Fires the selected item, routed through `do_transient_trigger` by the item's own key — one activation path, so submenus / flags / arguments behave identically however the item was reached |
| `q` / `Esc` / `C-g` | `Action::TransientDismiss` → the same `do_picker_dismiss` every picker surface uses |
| `DEL` / `BS` | Pops `transient_stack`, restoring the parent's spec, state, AND selection (§4bis.5) |

#### 4bis.5ter A transient is built for where it was opened

`TransientSourceRegistry`'s builders take a `TransientContext`:

```rust
pub struct TransientContext {
    pub major_mode: Option<String>,   // the `:if-mode` question
    pub minor_modes: Vec<String>,     // the `:if-derived` question
}
```

A menu bound globally has to degrade. Rows that act on the thing under
the cursor are meaningless in a buffer that has no such thing, and a
row whose useful reading depends on which buffer you are in should say
the useful thing. Emacs magit answers both with predicates on its
prefix definitions, and `magit-dispatch` — which under
`magit-define-global-key-bindings 'recommended` is bound to `C-c g`,
exactly our binding — carries three groups gated that way.

**The two mode axes are separate fields on purpose.** A flat list of
active mode ids can only answer one of the two questions, and magit's
dispatch asks both about the same key: `j` is "jump to section" in
`magit-status-mode` and "display status" everywhere else, while its
whole "Applying changes" group is gated on the looser family test.

**Resolved at build time, by the renderer.** Each peer fills the
context from the Editor in its `Effect::OpenTransient` arm
(`Editor::transient_open_context`). The alternative — having whatever
*emits* the effect resolve it — would leave all but the chord path
blind: `ExCommandContext` carries no buffer, so `:magit-dispatch` typed
on the `:` line would always get the ungated menu, whereas in Emacs
`M-x magit-dispatch` is context-aware because `:if-derived` tests the
current mode regardless of how you arrived. Any future plugin-emitted
open gets the same treatment for free.

**What the context deliberately omits:** the buffer id, the cursor and
the selection. A builder produces rows; it does not act. The row's
action receives its own `ActionContext`, which already carries the
underlying buffer, its cursor and any Visual region, resolved at fire
time when they are current — duplicating them here would be
speculative surface that could also go stale between build and fire.

#### 4bis.5bis Why a selection and not a scroll offset

`<C-n>` / `<C-p>` move an **item index**, not a row offset. The
distinction is load-bearing rather than stylistic.

A scroll offset's true maximum is `row_count - visible`, and `visible`
is renderer geometry — the TUI measures its terminal area, GPUI uses a
fixed budget. The host, which owns the state, cannot compute it. The
first shape stored the offset and grew it with `saturating_add`, with
each renderer clamping privately at paint time: the stored value ran
arbitrarily far past anything renderable while the view sat still, and
`<C-p>` then had to walk every phantom step back before anything moved.
Reported in use against magit's file dispatch.

An item index is bounded by `TransientSpec::selectable_count()`, which
the host has. It wraps at both ends, so no out-of-range value is
representable, and each renderer turns it into the scroll offset its
own geometry needs (`TransientSpec::scroll_for`) fresh every frame —
there is no stored scroll left to drift. This is the same reason the
candidate list, which bounds `selected` by `candidates.len()`, never
had the problem.

The row arithmetic (`row_count`, `row_of_item`, `selectable_count`,
`scroll_for`) lives on `TransientSpec` rather than in each renderer:
both peers previously kept their own copy and both undercounted the
per-group separator the same way, which made every multi-group popup
a row per group too short.

The selection is rendered — a `❯` and a bold label, BMP-block so no
patched font is required and one cell wide so no column shifts. Key
presses remain the primary interaction; the selection exists so a menu
taller than its popup can be walked at all, and so `<CR>` has somewhere
visible to land.

There is no reserved `-`-prefix convention for flags — a `Flag` item
fires by pressing its own assigned key, exactly like an `Action` item;
`key` is just `Vec<String>` per item, so a spec author picks whatever
letters make sense (magit's file-dispatch uses plain `s`/`d`, not
`-s`/`-d`). Single-key items fire immediately, no `<CR>` — the core
departure from candidate-list picker mode, where interaction is
`navigate` + `<CR>`.

### 4bis.7 Known gaps

- **`Argument` is defined but not wired.** The type exists
  (`TransientItemKind::Argument { name, default, prompt }`) and
  `transient_initial_state` seeds its `TransientState` entry from
  `default`, but `do_transient_trigger`'s `Argument { .. }` arm is a
  bare no-op — pressing an argument's key does nothing. The
  minibuffer-prompt-then-return-to-transient flow it implies is
  deferred (tracked as MG.8 in the magit slice plan); no ex-command
  currently relies on it.
- **No marginalia.** `TransientItem` has no per-item rich-context
  field, and no renderer draws one. An earlier draft of this section
  described a marginalia mechanism (diffstats, SHAs, ahead/behind
  counts rendered inline) as if implemented; it never was, and no
  current slice plans it. If it's built later, model it as an explicit
  field on `TransientItem` populated when the spec is built (a
  snapshot, not a live watch) — matching how `RawCandidate::marginalia`
  already works for regular pickers (`lattice-picker/src/lib.rs`).
- **`TransientSourceRegistry` has no `unregister`** (§4bis.4) —
  harmless today, would block a future runtime-unloadable transient
  source.

---

## 5. Registry and the `:picker` ex-command

### 5.1 Registration

`PickerRegistry` is a slim wrapper:

```rust
pub struct PickerRegistry {
    sources: HashMap<&'static str, Arc<dyn PickerSourceGenerator>>,
}

impl PickerRegistry {
    pub fn register(&mut self, gen: Arc<dyn PickerSourceGenerator>);
    pub fn get(&self, id: &str) -> Option<&Arc<dyn PickerSourceGenerator>>;
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &PickerSourceSpec)>;
}
```

Each feature crate exposes a registration entry point:

```rust
// in lattice-lsp::picker_sources
pub fn register(reg: &mut PickerRegistry) {
    reg.register(Arc::new(LspReferencesSource));
    reg.register(Arc::new(LspDiagnosticsSource));
    reg.register(Arc::new(LspDocumentSymbolSource));
    reg.register(Arc::new(LspWorkspaceSymbolSource));
    reg.register(Arc::new(LspCodeActionsSource));
}
```

`lattice-ui-tui::App::new` calls each feature crate's
`register` at boot. No `inventory!`-style linker magic;
explicit and ordered. Same shape as `CommandRegistry::populate`.

### 5.2 The `:picker` ex-command

One ex-command in `lattice-grammar`:

```rust
// :picker <source> [args]
let _picker = registry.register_ex_command(
    "ex:picker",
    "Open a picker over the named source.",
    ExCommandSpec {
        latency_class: LatencyClass::Display,
        parse_args: Box::new(parse_picker_args),
        apply: Box::new(|ctx| Ok(Effect::OpenPicker {
            source: ctx.args.pick("source"),
            args:   ctx.args.rest(),
        })),
        args_schema: vec![
            ArgSpec {
                name: "source",
                kind: ArgKind::String,
                doc: "Picker source id (`files`, `recent`, `lsp-references`, …).",
                prompt: "source:",
                // Completion source is wired to the picker registry's
                // source-id iterator (see §5.3).
            },
            // remaining args are source-specific; the cmdline completion
            // path consults the resolved source's args_schema for arg 2+.
        ],
        ..
    },
);
```

One generic `Effect::OpenPicker { source: String, args: Args }`
in `lattice-grammar/src/effect.rs`. The set of valid sources
lives in `PickerRegistry`, looked up at apply time. Adding a
new source — first-party or WASM — never edits the grammar
crate.

### 5.3 Tab completion

Three completion contexts within `:picker`:

| Position | Source feeding the cmdline popup |
|---|---|
| `:picker <Tab>` (arg 1) | `picker-source-completion-mode` in `lattice-picker` — iterates `PickerRegistry::iter()`, emits one candidate per source with `spec.doc` as marginalia. |
| `:picker grep <Tab>` (arg 2+) | The resolved source's `args_schema[N]`. Same `ArgKind`-driven dispatch as every other ex-command. |
| within an open picker | The picker's own query-line matcher. Independent of cmdline completion. |

This is symmetric with the recently-landed mode-driven
completion-source contributions (CSM.K1/K2): each completion
context is a mode that contributes candidates from a
specific provider. `picker-source-completion-mode` slots
into the existing pipeline with no new infrastructure.

### 5.4 Dispatch

App-side, in `lattice-ui-tui/src/app/dispatch.rs`:

```rust
Effect::OpenPicker { source, args } => self.open_picker(source, args),
```

App method:

```rust
fn open_picker(&mut self, source_id: String, args: Args) {
    let Some(gen) = self.picker_registry.get(&source_id) else {
        self.set_message(EchoLevel::Error, format!("picker: unknown source `{source_id}`"));
        return;
    };
    let ctx = self.build_picker_context();             // composes per-feature snapshot
    match gen.init(&ctx, &args) {
        PickerInitResult::Inline(pairs) => self.seat_picker(source_id, gen.clone(), pairs),
        PickerInitResult::Future(fut)   => self.spawn_picker_init(source_id, gen.clone(), fut),
        PickerInitResult::Stream(stream)=> self.spawn_picker_stream(source_id, gen.clone(), stream),
    }
}
```

The dispatch table is **the source id string → generator
lookup**, not a hand-coded `match` per source. New sources
become available the moment they register; the App does not
learn their names at compile time.

Accept path mirrors:

```rust
fn accept_picker(&mut self) {
    let (picker, source_id, gen) = self.take_picker_state();
    let routing = picker.routing_for(picker.selected_candidate()?)?.clone();
    let ctx = self.build_picker_context();
    let outcome = gen.accept(&ctx, &routing)?;
    if let Some(key) = routing_identity(&routing) {
        self.picker_mru.record(&source_id, &key);
    }
    self.apply_picker_outcome(outcome);
}
```

---

## 6. MRU pipeline

MRU is **a property of the picker, not a property of sources**.
No source declares `mru_scope`, no source supplies an
`identity_key` function, no source opts in or out. The picker
runs the same MRU pipeline over every candidate from every
source.

### 6.1 Identity derivation

`RoutingPayload` is owned by `lattice-picker`. Identity is a
function of the payload, defined in `routing.rs`:

```rust
pub fn routing_identity(r: &RoutingPayload) -> Option<String> {
    match r {
        RoutingPayload::OpenFile { path }        => Some(format!("file:{}", path.display())),
        RoutingPayload::Buffer { id }            => Some(format!("buf:{id}")),
        RoutingPayload::InvokeCommand { id, .. } => Some(format!("cmd:{id}")),
        RoutingPayload::PasteRegister { reg }    => Some(format!("reg:{reg:?}")),
        RoutingPayload::ExpandSnippet { id }     => Some(format!("snip:{id}")),
        RoutingPayload::JumpToMark { name }      => Some(format!("mark:{name}")),

        // No stable identity — coordinates drift, indices are per-request.
        RoutingPayload::JumpInBuffer { .. }
        | RoutingPayload::JumpToLocation { .. }
        | RoutingPayload::LspCompletion { .. }
        | RoutingPayload::LspCodeAction { .. }   => None,
    }
}
```

Sources never see this function. They emit a routing payload;
the picker derives identity from the payload's type. If the
payload variant carries a stable identity, MRU happens
automatically. If not, no MRU. **No source had to ask for
this behaviour** — the type system encoded it.

For sources where a stable identity exists but the existing
routing payload doesn't carry it (marks today emit
`JumpInBuffer`; grep / outline emit `JumpToLocation`), the
fix is to extend `RoutingPayload` with a richer variant
(`JumpToMark { name }`, `GrepHit { path, … }` with path
identity, etc.) — not to push the question onto sources.

### 6.2 Scoring

Per-keystroke refilter combines two terms:

```rust
fn combine(match_score: MatchScore, mru_bonus: f64, weight: f64) -> f64 {
    match_score.as_f64() * weight + mru_bonus * (1.0 - weight)
}
```

`weight` is the `picker.mru.match-weight` typed option
(default `0.6` — match dominates, MRU breaks ties strongly).

MRU bonus is a **frecency** value, prescient-style:

```rust
fn frecency_bonus(entry: &MruEntry, now: SystemTime, half_life: Duration) -> f64 {
    let age = now.duration_since(entry.last_used).unwrap_or(Duration::ZERO);
    let decay = (0.5_f64).powf(age.as_secs_f64() / half_life.as_secs_f64());
    let recency = decay;                                    // 0..1
    let frequency = (entry.use_count as f64 + 1.0).ln();    // log-scaled
    recency * 100.0 + frequency * 10.0
}
```

Numbers are tunable; the shape is fixed (recency-dominant,
frequency-tiebreak). Default half-life: 7 days.

### 6.3 Snapshot-at-open

Per-keystroke refilter must not touch the MRU index — every
HashMap lookup on the hot path is 10ns × N candidates and
for the 5000-candidate file picker that's 50μs we don't
need to spend. At picker-open we walk the raw candidate
list once and stamp the bonus into each `RawCandidate`:

```rust
fn snapshot_mru_into_candidates(
    raw: &mut [(RawCandidate, RoutingPayload)],
    mru: &PickerMruIndex,
    source_id: &str,
) {
    let now = SystemTime::now();
    for (cand, routing) in raw.iter_mut() {
        cand.mru_bonus = routing_identity(routing)
            .and_then(|k| mru.lookup(source_id, &k))
            .map(|e| frecency_bonus(e, now, MRU_HALF_LIFE))
            .unwrap_or(0.0);
    }
}
```

After this single O(N) pass, the refilter step reads
`cand.mru_bonus` as a cached field. No HashMap on the
keystroke path.

### 6.4 Persistence

`PickerMruIndex` serializes to `~/.cache/lattice/picker-mru.bincode`
(or `$XDG_CACHE_HOME` equivalent). bincode chosen
deliberately:

- Hot-write file: every accept records an entry, and we
  debounce-write within seconds. TOML's text-encode cost
  dominates at our entry-count target (≤ 1000 per source).
- Opaque to users: this is a derived cache, not config.
  Users tune behaviour via `picker.mru.*` typed options,
  not by hand-editing the cache.
- Forward-compatible: bincode's serde derive handles
  add-only field changes via `#[serde(default)]`.

Schema versioning lives on the top-level record. On
deserialize failure we discard the file and start fresh —
losing MRU is annoying, not catastrophic, and the
alternative (refuse to start when the cache is corrupt)
is worse.

### 6.5 Cap and eviction

Per-source LRU cap (default 1000 entries). On insert past
the cap, evict the entry with the lowest frecency bonus.
This is the prescient strategy — keeps the working set
of items the user actually returns to, drops one-shots.

### 6.6 Events

Two typed events fire on the bus (§5.10):

```rust
PickerOpened   { source: String, ts: SystemTime }
PickerAccepted { source: String, identity: Option<String>, ts: SystemTime }
PickerDismissed{ source: String, ts: SystemTime }
```

`PickerMruIndex` is a subscriber to `PickerAccepted` — it
records on event, not on a direct call. This means MRU
isn't hard-wired into the accept path; it's a typed-event
side-effect, the same pattern plugins use to react to
editor activity.

### 6.7 Typed options

```toml
[picker.mru]
enabled           = true        # global on/off; off = scoring is match-only
recency-half-life = "7d"        # frecency decay
match-weight      = 0.6         # match vs frecency mix (0.0..1.0)
cap-per-namespace = 1000        # LRU cap before eviction
persist           = true        # set false to disable disk write
```

Each surfaces in `:customize` via the typed-option machinery
(§5.12).

---

## 7. Performance budget

Per-keystroke (target sub-frame, < 8.3 ms at 120Hz):

| Stage | Bound |
|---|---|
| Matcher score per candidate | O(query × text) ≤ ~50ns typical |
| MRU bonus lookup per candidate | 0 (cached on `RawCandidate.mru_bonus`) |
| Combine + sort | O(N log N) ≤ 200μs for N=5000 |
| Total refilter | < 1ms for N=5000, < 200μs for N=500 |

Per-pick (open + accept):

| Stage | Bound |
|---|---|
| `source.init` (Inline) | source-specific; files-walker capped at 5000 entries |
| `source.init` (Future) | host spawns on tokio; UI stays interactive |
| MRU snapshot at open | O(N) HashMap lookups ≤ 50μs typical |
| `source.accept` | source-specific; ≤ 1μs typical |
| `routing_identity` + `mru.record` | O(1) ≤ 100ns |
| `apply_picker_outcome` | translates outcome → Effect; Effect path is unchanged |

Bench coverage in `lattice-picker/benches/picker.rs`:

- `open_inline_p99` — 5000-candidate inline source
- `refilter_p99` — keystroke against 5000-candidate set
- `mru_snapshot_p99` — O(N) walk at open
- `mru_record_p99` — accept-path cost

CI gate: `refilter_p99 < 1ms` (loose now, tighten as the
matcher graduates).

---

## 8. Plugin (Phase 7) WIT seam

When the plugin host lands, the WIT interface mirrors the
Rust trait near-1:1:

```wit
package lattice:picker;

interface source {
    record source-spec {
        id: string,
        doc: string,
        args-schema: list<arg-spec>,
        args-hint: string,
    }

    variant accept-outcome {
        open-file(string),
        switch-buffer(u32),
        jump-in-buffer(tuple<u32, u32, u32>),
        invoke-command(tuple<string, list<string>>),
        paste-register(string),
        expand-snippet(string),
        no-op,
        // ...
    }

    resource generator {
        spec: func() -> source-spec;
        init: func(ctx: picker-context, args: list<string>) -> init-result;
        accept: func(ctx: picker-context, routing: routing-payload) -> result<accept-outcome, string>;
    }
}
```

The host wraps each WIT-imported `generator` as a
`Box<dyn PickerSourceGenerator>` and registers it into the
same `PickerRegistry` that holds first-party sources. From
the picker primitive's perspective there is no difference
between a native source and a plugin source — both implement
the same trait. MRU, scoring, rendering all "just work" for
plugin candidates that carry a canonical `RoutingPayload`
variant.

Plugin-custom routing payloads (a `PluginEffect` variant
with opaque bytes) degrade to no-MRU until Phase 7 design
adds an opt-in identity field — punted deliberately.

---

## 9. Test / bench / error story

Per CLAUDE.md heuristic 5 (non-trivial design changes ship
four artefacts together):

### 9.1 Tests

- `lattice-picker/tests/matcher.rs` — fuzzy/substring
  invariants, query-position-irrelevant ranking, case
  folding.
- `lattice-completion/src/orderless.rs::tests` — §3.1
  component parsing (escapes, negation, a half-typed
  trailing backslash), any-order matching, score band,
  and range validity on non-ASCII targets.
- `lattice-picker/src/lib.rs::tests` — §3.1 through the
  picker: a two-component query reaches a row containing
  neither fragment contiguously, `!frag` excludes, a
  single-token query is byte-identical with orderless on
  and off, and a trailing space does not blank the list.
- `lattice-picker/tests/rank.rs` — score combining,
  weight clamping, frecency monotonicity (older entries
  rank lower with all else equal).
- `lattice-picker/tests/mru.rs` — record / lookup
  round-trip, cap eviction picks the lowest-frecency
  entry, persistence load-recovers a partial file,
  schema-version mismatch discards cleanly.
- `lattice-picker/tests/registry.rs` — registration is
  idempotent on id collision (last-wins or error,
  documented), iteration order is stable.
- `lattice-ui-tui/src/app/picker.rs::tests` — end-to-end
  through the App: `:picker files` opens, types narrow
  the candidate list, accept routes through outcome
  translation.

### 9.2 Benches

`lattice-picker/benches/picker.rs` (criterion):

- `open_inline` × {100, 500, 5000} candidate counts.
- `refilter` × {empty query, 1-char, 5-char, 2-component
  orderless} × {500, 5000}. The orderless row is the
  worst per-keystroke shape the matcher sees — every
  component runs the full tier ladder over every
  candidate — so it is the one §7's sub-frame budget has
  to hold for.
- `mru_snapshot` × candidate count.
- `mru_record` (single accept).

CI threshold rows in
[`../operations/benchmarks.md`](../operations/benchmarks.md);
regressions ≥ 20% fail the bench job.

### 9.3 Graceful error handling

- Malformed orderless query (a lone `!`, a trailing `\`)
  → treated as literal text, never an error. The user is
  mid-keystroke; a picker that emptied its list on every
  half-typed escape would be unusable.
- Unknown source id → echo, picker stays closed. No panic.
- `source.init` returns error → echo `"picker: <source>: <error>"`,
  picker stays closed.
- `source.accept` returns error → echo, picker dismisses,
  no outcome applied, no MRU record. Cursor / buffer state
  unchanged.
- MRU index file corrupt → discard, log warning, start
  fresh. Never block boot.
- MRU index file write fails → log warning, retry on next
  accept. Never block accept.
- Async init future cancelled (user dismissed picker
  before results arrived) → drop quietly, no echo.
- Stream init source ends with error mid-flight → keep
  candidates received so far, echo the error, picker
  stays open.

---

## 10. Migration order

This document captures the *target* design. Landing it
involves three independent slices, each shipping the four
artefacts:

1. **Extract `lattice-picker`.** Mechanical move of the
   existing data model out of `lattice-ui-tui::picker`. No
   functional change. One commit, green CI.
2. **Registry + `:picker` ex-command.** Land
   `PickerRegistry`, `PickerSourceSpec`, single
   `Effect::OpenPicker { source, args }`, App-side dispatch
   on source id, migrate `:files` / `:recent` / `:b` to
   register via the new path (keep `:b` and friends as
   short aliases — vim muscle memory).
3. **Trait surface + outcomes.** Land
   `PickerSourceGenerator`, `PickerContext` with per-feature
   handles, `PickerAcceptOutcome`. Migrate the first-party
   sources to trait impls. Cross-link from
   `design.md §5.9.7`.
4. **MRU pipeline.** Land `PickerMruIndex`, identity
   derivation, frecency scoring, snapshot-at-open,
   persistence, typed events, typed options, benches.
5. **Sources P.3–P.10.** Implement each remaining source
   as a `PickerSourceGenerator` impl in its owning crate.
   Each ships with tests; MRU is automatic.

Phase 7 (plugin host) lands the WIT mirror after the
first-party trait surface has been exercised by ≥ 5
concrete sources — exactly the
[`../operations/implementation.md`](../operations/implementation.md)
§ Build order principle.
