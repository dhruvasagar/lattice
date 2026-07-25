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

---

## 4bis. Transient mode — action menus on picker substrate

> **Design fragment.** The transient interaction mode extends the picker
> to serve magit-style action menus (dispatch, branch, rebase, file operations).
> The full transient design + magit integration lives in
> [`magit.md`](magit.md) §8. This section covers the picker's role as the
> rendering and interaction substrate.

### 4bis.1 Why the picker

Transients share the picker's core requirements — floating overlay rendering,
keyboard capture, preview pane, scroll, dismiss — but differ in the
interaction model. Instead of filtering candidates by typing a query, the
user presses single-letter keys to fire actions, toggle flags, or open
nested submenus. Rather than building a parallel rendering + input system,
the picker gains a `PickerMode::Transient` variant that switches the display
and input handling.

What the picker already provides that transients reuse verbatim:

| Picker feature | Transient use |
|---|---|
| Floating overlay + styled text rendering (TUI + GPUI) | Renders transient groups, keys, labels, marginalia |
| Preview pane | Live command preview (`git checkout -f main`) |
| Marginalia | Rich per-entry context — diffstats, SHAs, status labels |
| `PickerDisplay::BottomPopup` | Positions the transient at viewport bottom |
| `j`/`k` / `C-n`/`C-p` scroll | Scrolls groups that overflow the viewport |
| `q` / `Esc` / `C-g` dismiss | Closes the transient without action |
| MRU pipeline | Frecently-used transient actions float to the top |

What the transient mode adds on top:

| New capability | Mechanism |
|---|---|
| Grouped entries with section headers | `TransientGroup { label, items }` renders as bold header + indented rows |
| Single-key trigger dispatch | Each `TransientItem` carries a `key`; pressing it fires without cursor nav |
| Flag toggle in-place | `TransientItemKind::Flag` toggles `TransientValue::Bool`, re-renders, updates preview |
| Argument → minibuffer prompt | `TransientItemKind::Argument` opens minibuffer; on confirm, value is set |
| Nested submenus | `TransientItemKind::Submenu(spec)` replaces the current transient; `DEL`/`BS` back |
| Live preview update | `PreviewFn` is called on every flag toggle / argument change, preview pane re-renders |

### 4bis.2 Data model

```rust
/// Lived in `lattice-picker`, extended for transient support.
pub struct TransientSpec {
    pub title: String,
    pub groups: Vec<TransientGroup>,
    pub preview: Option<Box<PreviewFn>>,
    pub footer: Option<String>,
}

pub struct TransientGroup {
    pub label: String,          // "Actions", "Arguments", "Configure"
    pub items: Vec<TransientItem>,
}

pub struct TransientItem {
    pub key: Vec<KeyChord>,     // ["b"], ["c"], ["C-c", "C-c"]
    pub label: String,          // "checkout"
    pub description: String,    // "Check out a branch"
    pub marginalia: Option<String>,  // live context — diffstats, SHAs, status
    pub kind: TransientItemKind,
}

pub enum TransientItemKind {
    Action(ActionId),
    Submenu(TransientSpec),
    Flag { name: String, value: bool },
    Argument { name: String, value: Option<String>, prompt: String },
}

pub type PreviewFn = dyn Fn(&TransientState) -> String + Send;
pub type TransientState = HashMap<String, TransientValue>;

pub enum TransientValue {
    Bool(bool),
    String(String),
}
```

### 4bis.3 API

```rust
impl PickerRegistry {
    /// Open a transient menu. Reuses the picker's rendering pipeline but
    /// switches to grouped, non-filterable display with single-key triggers.
    pub fn open_transient(
        &self,
        spec: TransientSpec,
        display: PickerDisplay,  // BottomPopup, Floating, etc.
    ) -> TransientHandle;
}
```

### 4bis.4 Marginalia — rich context per entry

Marginalia is the transient's information-density mechanism. Each
`TransientItem` carries an optional `marginalia: String` populated by the
caller from live git data (or any domain context) when the transient is
built. The picker renders it as dimmed text right-aligned in the entry row,
the same way it renders marginalia on `RawCandidate`s.

This means the user never presses a key blind — the marginalia tells them
what will happen before they press it.

#### Example: branch transient

```
┌─ Branch ───────────────────────────────────────────────────────┐
│                                                                │
│  Branch                                          on main 3↑ 2↓ │
│                                                                │
│  ▸ Actions                                                     │
│    [b]  checkout           checkout existing branch    ▸ main   │
│    [c]  create             create a new branch                 │
│    [d]  delete             delete a branch              ⚠      │
│    [m]  merge              merge into current branch    main   │
│    [r]  rename             rename a branch                     │
│    [w]  worktree           create a worktree                   │
│                                                                │
│  ▸ Configure                                                   │
│    [-]  [ ] force          force-create or force-delete        │
│    [-]  [x] track          set upstream tracking        main   │
│                                                                │
│  ────────────────────────────────────────────────────────────  │
│  git checkout -b --track origin/main main                      │
│                                                                │
│  q dismiss   DEL back   -f toggle   -t toggle                  │
└────────────────────────────────────────────────────────────────┘
```

Each marginalia value is resolved from live git data when the `TransientSpec`
is built:

| Item | What marginalia shows | Source |
|---|---|---|
| `[b] checkout ▸ main` | The current branch name | `Repository::head_name(repo)` via `lattice-vcs` |
| `[d] delete ⚠` | Warning for destructive action | Static — `⚠` glyph from the severity icon system |
| `[m] merge main` | The default merge target | `Repository::head_name(repo)` |
| `[-t] [x] track main` | The configured upstream | `Reference::resolve(repo, "HEAD@{upstream}")` |
| Title `on main 3↑ 2↓` | Ahead/behind counts | `WorkingTree::ahead_behind(repo)` |

#### Example: file-dispatch transient

```
┌─ File ────────────────────────────────────────────────────────┐
│                                                                │
│  src/auth/login.rs                         modified  +12 -3   │
│                                                                │
│  ▸ Actions                                                     │
│    [s]  stage               stage this file          modified  │
│    [u]  unstage             unstage this file        staged    │
│    [x]  discard             discard changes           ⚠ +12 -3 │
│    [d]  diff                show diff for this file   +12 -3   │
│                                                                │
│  ▸ History                                                     │
│    [l]  log                 show log for this file    23 cmts  │
│    [b]  blame               blame this file           a1b2c3d  │
│                                                                │
│  ▸ Filesystem                                                  │
│    [r]  rename              rename/move this file              │
│    [D]  delete              delete this file            ⚠     │
│    [c]  checkout            checkout from HEAD          clean   │
│                                                                │
│  ────────────────────────────────────────────────────────────  │
│  file: src/auth/login.rs (+12 -3)                              │
│                                                                │
│  q dismiss   s stage   d diff   l log                          │
└────────────────────────────────────────────────────────────────┘
```

| Item | What marginalia shows | Source |
|---|---|---|
| `[s] stage modified` | Current file status | `WorkingTree::path_status(repo, path)` |
| `[x] discard ⚠ +12 -3` | What would be lost | Diffstat from `git diff --stat <path>` |
| `[l] log 23 cmts` | Commit count for this file | `git log --oneline <path> \| wc -l` |
| `[b] blame a1b2c3d` | Last commit touching this file | `git log -1 --format=%h <path>` |
| `[c] checkout clean` | File status vs HEAD | `WorkingTree::path_status(repo, path)` |
| Title `+12 -3` | Diffstat | `git diff --stat <path>` |

**Performance note:** Marginalia is populated when the `TransientSpec` is
built (on transient-open), not per-frame. Flag toggles update the preview
line but do not re-resolve git data. The marginalia is a snapshot of the
file or repo state at the moment the transient opens — consistent with
magit's own behaviour (transients show cached state, not live-watching).

### 4bis.5 Display preferences

The existing `PickerDisplay` variants control where the transient renders:

```rust
pub enum PickerDisplay {
    Inline,         // Embedded in the active buffer (not appropriate for transient)
    Floating,       // Modal overlay, centered
    BottomPopup,    // Bottom-anchored, viewport-width — default for transients
}
```

Magit transients default to `BottomPopup` — the magit convention of a
transient appearing at the bottom of the screen.

### 4bis.6 Keyboard handling

When a transient is open, the picker captures all keystrokes until dismiss:

| Key | Action |
|---|---|
| Letter key matching an item's `key` | Fires the item's action / toggles flag / opens submenu / opens minibuffer |
| `j` / `k` / `C-n` / `C-p` | Scroll groups that overflow the viewport |
| `q` / `Esc` / `C-g` | Dismiss without action |
| `DEL` / `BS` | Return to parent transient (when in a submenu) |
| `-` followed by flag letter | Toggle a Flag item (e.g., `-f` for force) |

Single-letter Action keys fire immediately — no `<CR>` needed. This is the
core departure from candidate-list picker mode, where all interactions are
`navigation` + `<CR>`.

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
- `refilter` × {empty query, 1-char, 5-char} × {500, 5000}.
- `mru_snapshot` × candidate count.
- `mru_record` (single accept).

CI threshold rows in
[`../operations/benchmarks.md`](../operations/benchmarks.md);
regressions ≥ 20% fail the bench job.

### 9.3 Graceful error handling

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
