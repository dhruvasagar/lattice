# Keymap Architecture (developer reference)

Authoritative design for lattice's key-input dispatch. The
plan section at the end lists the slices that take us from the
current state (input.rs hand-rolled match table; keymap.rs
documentation-only) to the architecture design.md §5.2.3 has
spec'd since day one.

## 1. Vision

> *"Keymaps are configuration that bind chord sequences to
> command invocations -- the default vim keymap is itself a
> config file, not hardcoded behavior. Any user or plugin can
> invoke any command, compose commands, or build entirely new
> editing flows."* -- design.md §3 / §5.2.3

The architecture commitments this implies:

- **Bindings are data, not code.** Built-in vim defaults, user
  config, major-mode bindings, minor-mode bindings, and plugin
  bindings all live in the same registry. There is one `bind`
  API; three callers (built-ins, user `init.rs`, plugins).
- **The grammar IS the public command API** (paramount goal
  #3). A plugin that registers a new motion / operator / text
  object can also bind a chord to it. No host changes required.
- **Plugin extensibility is first-class** (paramount goal #2).
  Plugins ship in any WebAssembly Component Model language;
  capability-gated access to `keymap-write` lets them register
  bindings; binding registration cannot violate vim semantics
  (the chord still resolves to a typed `CommandInvocation`).
- **Performance lives on the keystroke path.** Lookup is
  sub-microsecond, allocation-free, wait-free against
  registration writes. The mechanism scales with the
  binding count, not the buffer size.

## 2. Five-layer model (design.md §5.2.3)

Keymap resolution walks layers in priority order:

| Priority | Layer                       | Source                                                                                               |
|----------|-----------------------------|------------------------------------------------------------------------------------------------------|
| 1        | Built-in vim default keymap | host registers at startup; lives in code as `keymap_entry!` macros                                   |
| 2        | Major-mode keymap           | host registers when a major mode activates (rust, markdown, ...)                                     |
| 3        | Active minor-mode keymaps   | pushed/popped as minor modes activate (active-snippet, completion-popup, picker, chord-capture, ...) |
| 4        | User config overrides       | `init.rs` calls `keymap.bind(...)` at boot                                                           |
| 5        | Per-buffer ad-hoc bindings  | `:nmap <buffer>` or plugin per-buffer override                                                       |

Higher priority **overrides**, not shadows: a user-config
binding for `dd` replaces the built-in `dd`; if the user later
unbinds it, the built-in resurfaces.

Within a layer, last-bind-wins. Both bindings retain `source`
provenance so `:describe-key dd` can show "dd → delete-line
(user, init.rs:42; previously vim default,
keymap.rs:213)".

The five layers live in **two physical structures**:

- **`merged` (always-on)** — Builtin + MajorMode + User + Buffer
  layers merged per `BindingMode` at write time. Fires
  unconditionally on every keystroke. Rebuilt by
  `build_always_on_merged` whenever any of those four layers
  change.
- **`minor_mode_tries` (per-buffer overlay)** — one
  `HashMap<BindingMode, KeymapTrie>` per `MinorMode(id)`,
  stored in a second `ArcSwap`. At lookup time,
  `lookup_with_context` starts from the always-on trie and
  folds in only the MinorMode tries whose `ModeId` is in
  `active_modes` for the current buffer, in activation order
  (last-activated minor wins).

Layer mutations are infrequent (mode transitions, config
reload, minor-mode push/pop). Keystroke lookups are fast: the
no-minor path is one `ArcSwap::load` + trie walk (~17 ns
single-chord); the minor-fold path overlays 0–3 tries over the
base before walking (~44 ns typical). Both structures use
`ArcSwap` for wait-free reads; the brief mutex covers only the
source-of-truth layer stack used for rebuilds.

## 3. Data model

### 3.1 `KeyChord`

Canonical, owned representation of one chord token. Stack-only
(no allocation).

```rust
pub struct KeyChord {
    pub key: KeyKind,       // Char(c) | Special(SpecialKey) | Function(u8) | ...
    pub modifiers: KeyMods, // bitfield: ctrl / shift / alt / super
}
```

Notation (lossless round-trip):

- Plain char: `j`, `0`, `$`
- Modifier-prefixed: `<C-d>`, `<S-Tab>`, `<C-S-Right>`
- Special: `<Esc>`, `<CR>`, `<Tab>`, `<BS>`, `<Up>`, `<Down>`, ...
- Function: `<F1>` ... `<F24>`

Multi-key chords are a `[KeyChord]` slice, not a single
`KeyChord`. The trie indexes by chord-by-chord descent, not by
concatenated string.

### 3.2 `KeymapTrie`

Hash-trie indexed by chord prefix. Each node carries:

- `children: HashMap<KeyChord, Node>` (or `Vec<(KeyChord, Node)>` if measurement says small-N is faster).
- `binding: Option<Arc<BoundCommand>>` (Some at terminal nodes; partial nodes carry None).

Lookup:
1. Walk children for the next chord; descend.
2. Terminal node → return `Arc<BoundCommand>`.
3. Internal node with no terminal at this depth → return
   `LookupResult::Partial` (waiting for next chord).
4. No matching child → `LookupResult::Unbound`.

Lookup cost is `O(prefix_length)` hash lookups, not
`O(binding_count)`. Most chords are length 1–2.

### 3.3 `BoundCommand`

```rust
pub struct BoundCommand {
    pub command: CommandInvocation,    // typed, registry-resolved
    pub source: SourceLocation,        // provenance for :describe-key
    pub layer: KeymapLayer,            // priority tag for conflict resolution
}
```

`CommandInvocation` is the same typed type the grammar
dispatcher consumes (DESIGN §5.2.1). The keymap is just a
chord → invocation index; once resolved, dispatch reuses the
existing `execute(invocation)` path. **No special-case
"run an Action" detour for built-ins.**

The legacy `Action` enum in `crate::app` collapses into
`CommandInvocation`s once migration completes. Built-in motions
/ operators / mode transitions are already reachable through
the registry; the `Action` enum exists today only because input.rs
needs an enum to dispatch into App methods. After M3 the
keymap returns `CommandInvocation`s; the App's apply loop
already routes those.

### 3.4 `KeymapRegistry` / `KeymapHandle`

Lives in `crates/lattice-keymap` (extracted from `lattice-host`
during the T1-T9 crate migration; see §13). The App holds a
`KeymapHandle` (cheap `Arc` clone).

```rust
// crates/lattice-keymap/src/registry.rs
pub struct KeymapHandle {
    registry: Arc<KeymapRegistry>,
}

struct KeymapRegistry {
    /// Always-on merged trie: Builtin + MajorMode + User + Buffer.
    /// Wait-free read; rebuilt on writes to those four layer kinds.
    merged: Arc<ArcSwap<MergedKeymap>>,
    /// Per-ModeId MinorMode tries. Wait-free read; rebuilt on
    /// push_layer / pop_layer for MinorMode layers.
    minor_mode_tries: Arc<ArcSwap<HashMap<ModeId, Arc<HashMap<BindingMode, KeymapTrie>>>>>,
    /// Source-of-truth layer list used for rebuilds and telemetry.
    inner: Arc<Mutex<LayerStack>>,
}
```

The two-`ArcSwap` split means write to Builtin/MajorMode/User/Buffer
only rebuilds `merged`; push/pop of a MinorMode layer only rebuilds
`minor_mode_tries`. Neither touches the other structure.

**Keystroke read API:**

- `lookup_with_context(&self, mode: BindingMode, chords: &[KeyChord], active_modes: &[ModeId]) -> LookupResult`
  — hot path; wait-free (no mutex). Folds active MinorMode tries
  over the always-on merged trie in activation order.

**Telemetry / introspection API** (not on the hot path):

- `enumerate_chord_bindings(&self, mode: BindingMode, chords: &[KeyChord]) -> Vec<(KeymapLayer, Arc<BoundCommand>)>`
  — walks the source `inner.layers` under mutex; returns every
  registered binding for the chord regardless of active modes.
- `resolve_trace(&self, mode: BindingMode, chords: &[KeyChord], active_modes: &[ModeId]) -> KeymapResolution`
  — builds a full per-layer trace with `active` flags; used by
  `:describe-key`. See §13.2 for semantics.
- `resolve_trace_all_modes(&self, chords: &[KeyChord], active_modes: &[ModeId]) -> Vec<KeymapResolution>`
  — runs `resolve_trace` for every `BindingMode`; returns only
  modes that have at least one registered binding.
- `layer_label_string(&self, layer: KeymapLayer) -> String`
  — human-readable label from the layer's registered label string.

**Write API:**

- `bind(&self, layer: KeymapLayer, mode: BindingMode, path: &[ChordPattern], cmd: CommandInvocation, source: SourceLocation)`
- `unbind(&self, layer: KeymapLayer, mode: BindingMode, path: &[ChordPattern])`
- `push_layer(&self, kind: PushLayerKind, modes_and_bindings: ...) -> LayerId`
- `pop_layer(&self, id: LayerId)`

After every write the affected structure (`merged` or
`minor_mode_tries`) rebuilds. Rebuild cost is typically
sub-millisecond for the default catalog (~hundreds of bindings
per layer, ~5 layers). The `ArcSwap::store` is one atomic.

Capability-gated variants for plugin/user access:
`try_bind`, `try_unbind`, `try_push_layer` — funnel through a
`capability_allows` check before delegating. See §5.5.

### 3.5 `KeymapEntry` (existing; stays)

The existing `KeymapEntry` in `keymap.rs:134` becomes the
**catalog** of built-in bindings. The `keymap_entry!` macro
constructs entries; a startup pass enumerates the catalog
and registers each into the `KeymapRegistry`. The catalog
gives us:

- Source-location capture for free (`file!()` + `line!()`).
- Doc / mode / chord all in one place.
- The `:describe-key`, `:keymap`, `:apropos` introspection
  surfaces continue to work.

The drift test that catches descriptor / behaviour divergence
becomes obsolete because the descriptor IS the behaviour.

## 4. Lookup path (the keystroke hot path)

```
crossterm::KeyEvent
    ↓  (input thread)
KeyChord normalisation                   (allocation-free; ~50 ns)
    ↓
input.rs::translate
  ├─ overlay claims first?               (picker / completion / snippet / chord-capture / help)
  │   yes → overlay layer's lookup
  │   no  → pending-state lookup
  ↓
KeymapHandle::lookup_with_context(       (K.1.c; ArcSwap::load + trie fold + walk)
    mode,                                 ~17 ns no-minor-modes fast path
    &partial_chord,                       ~44 ns typical (0-3 active minors)
    &active_modes_for_buffer,
)
  ↓
LookupResult
  ├─ Bound(cmd)   → dispatch via grammar's execute(cmd)
  ├─ Partial      → AbsorbPartialChord, wait for next chord
  └─ Unbound      → fall through to literal-text Insert / no-op Normal
```

`active_modes_for_buffer` is read from `App::active_modes[active_buffer_id()]`
— the `ActiveModes { major, minors }` for whichever pane is
focused (Document, Multibuffer, Terminal, Help, …). Using
`active_buffer_id()` rather than a document-specific field is
important: non-Document panes have their own mode stacks and
must not borrow another pane's minor-mode context (see §13.3).

### Performance commitments

- **Lookup p99 < 1 µs** including chord normalisation and trie
  walk. Bench-gated.
- **Zero allocation** on the lookup hot path. `KeyChord` is
  stack-only; `BoundCommand` is reached through `Arc` clone
  (one atomic increment); the dispatch path then invokes the
  grammar with the already-typed invocation.
- **Wait-free reads** via `ArcSwap<KeymapTrie>`. Concurrent
  registration (plugin loading, user `:bind` at runtime,
  minor-mode push/pop) does not stall the input thread. Same
  pattern as `SupervisorSnapshot` (audit slice 1) and
  `DiagnosticsSnapshot` (audit slice 2).
- **Layer-merge on write, not on read.** Pushing a minor-mode
  layer rebuilds the merged trie. With ~hundreds of bindings
  per layer and ~5 layers active, rebuild is sub-millisecond
  and happens on mode transitions (rare). Read latency is
  unaffected by the layer count.

## 5. Registration paths

### 5.1 Built-ins

A startup pass enumerates the existing `KeymapEntry` catalog
in `keymap.rs` and registers each entry into the
built-in layer:

```rust
for entry in BUILTIN_KEYMAP_CATALOG.iter() {
    registry.bind(
        KeymapLayer::Builtin,
        entry.mode,
        &chord_seq_from_str(entry.chord),
        invocation_for(entry.command),
        entry.source,
    );
}
```

The `keymap_entry!` macro stays the source-of-truth construction
path; the catalog stays the documentation surface; what
changes is that the catalog now drives the dispatcher.

### 5.2 Major modes

Major modes (rust, markdown, ...) declare a keymap as part of
their `MajorMode` registration (DESIGN §5.9). The host
registers the major-mode layer when the mode activates, drops
it when the mode deactivates.

```rust
fn rust_mode_keymap() -> Vec<KeymapEntry> { ... }
```

### 5.3 Minor modes

Minor modes (active-snippet, completion-popup, picker,
chord-capture, help-overlay) push a layer with their bindings;
pop on deactivation. Today these are special-case branches at
the top of `input.rs::translate`; after M3 they're plain
layer pushes:

```rust
let layer_id = registry.push_layer(MinorModeLayer {
    bindings: completion_popup_bindings(),
    label: "completion-popup",
});
// ... popup is open ...
registry.pop_layer(layer_id);
```

The branch ordering at the top of `translate` (picker before
snippet before chord-capture before universal `<C-c>`)
becomes layer ordering in the stack.

### 5.4 User config (`init.rs`)

The user's compiled-to-WASM `init.rs` calls the same `bind`
API:

```rust
keymap.bind(KeymapLayer::User, BindingMode::Normal, "<leader>w", ":w<CR>")?;
keymap.unbind(KeymapLayer::Builtin, BindingMode::Normal, "<leader>w")?;
keymap.bind(KeymapLayer::User, BindingMode::Normal, "j", "gj")?;  // nnoremap j gj
```

Same mechanism, different layer.

### 5.5 Plugins

Plugins reach the keymap through the WIT interface
(DESIGN §9):

```wit
interface keymap {
    bind: func(layer: layer, mode: binding-mode, chord: string, cmd: command-id) -> result<binding-id, keymap-error>;
    unbind: func(id: binding-id) -> result<_, keymap-error>;
    push-layer: func(label: string, bindings: list<entry>) -> result<layer-id, keymap-error>;
    pop-layer: func(id: layer-id) -> result<_, keymap-error>;
}
```

**Capability gating.** Plugins must declare `keymap-write` in
their manifest to register bindings. Layer-scoped capabilities
(`keymap-write:minor-mode`, `keymap-write:plugin-only-layer`)
let the host restrict a plugin to its own layer; the host can
deny a plugin's request to bind in `KeymapLayer::Builtin` or
`KeymapLayer::User`. Mirror of the typed-options /
filesystem capability gates.

A plugin that ships a new motion can also ship a binding for
it:

```rust
// in plugin.wit
let motion = registry.register_motion(...);
keymap.bind(KeymapLayer::PluginA, BindingMode::Normal, "]f", motion.id)?;
```

This is the architectural seam paramount goal #3 has been
asking for since day one.

## 6. Conflict resolution + provenance

- **Cross-layer conflict**: top layer wins (priority order in
  §2). The lower-layer binding stays in the registry; if the
  higher binding is unbound, the lower one re-surfaces.
- **Within-layer conflict**: last-bind-wins, but `source`
  provenance keeps both visible to introspection. `:describe-key
  dd` shows the active binding plus a "shadowed by user
  init.rs:42" trail when relevant.
- **Capability-denied**: registration errors with
  `KeymapError::CapabilityDenied`. The plugin's request is
  rejected at the host boundary; lattice never silently
  ignores a registration.

## 7. Counts, operator-pending, and other vim layered concerns

### 7.1 Counts

`3w` is two layered concerns: an integer count, then a chord.
Counts are not bindings; they're an input-thread accumulator
that runs **before** keymap lookup. Today's
`pending_count` in input.rs stays exactly where it is.

After M3:

- Input thread accumulates digits while the chord prefix is
  empty.
- Once a non-digit chord arrives, lookup runs with the
  accumulated count attached to the resulting
  `CommandInvocation`'s count field.
- Dispatch unchanged; `execute(invocation_with_count)` works
  today.

### 7.2 Operator-pending

`d` followed by `w` is a multi-key chord *in the trie*, not a
mode transition. The trie naturally encodes it: `d` is an
internal node with a child `w` (and `e`, `b`, `0`, `$`, `iw`,
`aw`, ...). Lookup of `d` returns `Partial`; the dispatcher
sets `Pending::Operator(d)` and waits for the next chord. On
`w`, lookup of `[d, w]` returns `Bound(delete-with-target=word-forward)`.

Two upgrades over today's hand-rolled state machine:

1. **No special-case Pending branches** for each operator. `d`,
   `y`, `c`, `>`, `<`, `gU`, `gu`, `g~` all encode the same
   way: trie nodes with motion / text-object children.
2. **Plugin-defined operators get plugin-defined chord
   sub-trees for free.** A `sort-lines` operator registered
   by a plugin can register its bindings as `[s, l]` (or
   whatever); the dispatcher walks the trie identically.

### 7.3 Marks, registers, find-char

`'a` (jump to mark line), `"ay` (yank to register a), `fX`
(find char X forward) are all single-character "argument"
chords. They're partial trie nodes whose children are
character-class wildcards. The trie supports a `Wildcard`
child type alongside the literal-chord children.

```rust
enum TrieChild {
    Literal(KeyChord),       // exact match
    CharLiteral,             // any single char (mark name, register, find-target)
}
```

Lookup with `f` followed by `X` walks `Literal(f)` →
`CharLiteral`, binding the char as an arg to the resulting
invocation.

This subsumes the `AfterMark`, `AfterRegister`, `AfterFindChar`
states in today's `BindingMode` enum. Those enum variants
become trie-internal state, not separate dispatch modes.

## 8. Renderer / runtime integration

The keymap registry is held on the App as `KeymapHandle`
(`Arc`-shareable). The input loop's `translate` function takes
`&KeymapHandle` and a `BindingMode`. No other render or
runtime path needs to know about the keymap.

The handle's snapshot pattern means the input loop is
unaffected by registration writes from any thread. The same
guarantee that lets multi-thread render work after audit
slice 7 works for multi-thread input dispatch.

## 9. Migration plan (Slices 8.a -- 8.i)

Each slice ships the four artefacts CLAUDE.md heuristic #5
requires: architecture documentation (this file is the home
for it; per-slice updates as design evolves), benchmark
coverage (`crates/lattice-ui-tui/benches/keymap.rs` -- new
file in 8.a), tests for new scenarios + failure modes, and
graceful error handling.

### Slice 8.a -- Foundation: KeyChord + chord-string normalisation

- New crate-internal type `KeyChord` (stack-only).
- `KeyEvent → KeyChord` (input.rs's existing key normalisation
  becomes a pure function).
- `KeyChord → String` (the `<C-d>` notation already documented
  in `keymap.rs:17-22`).
- `String → [KeyChord]` parser for chord-sequence strings
  (`"gg"`, `"<C-w>j"`, `"dw"`).
- Property tests: round-trip `KeyEvent → KeyChord → String →
  [KeyChord]` for the full key alphabet + modifier matrix.
- Bench: parse cost, normalisation cost.

### Slice 8.b -- KeymapTrie

- Hash-trie data structure with `Literal` + `CharLiteral`
  child kinds (§7.3).
- `insert` / `lookup` / `remove` / `merge`.
- `Arc<ArcSwap<KeymapTrie>>` cell for wait-free reads (mirror
  of `SupervisorSnapshot`).
- Bench: lookup at depths 1, 2, 3; merge cost for 5 layers
  with ~500 bindings each.
- Tests: prefix lookup, partial vs unbound, char-literal
  wildcard, layer merge with shadowing.

### Slice 8.c -- KeymapRegistry + layered resolution

- The five-layer model from §2.
- `bind` / `unbind` / `push_layer` / `pop_layer`.
- `lookup(chord_seq, mode)` walks the merged trie, returns
  `LookupResult`.
- Built-in catalog enumeration → registry registration at
  startup. The existing `keymap_entry!` macro stays.
- Plugin / user-config API surface (skeleton -- capability
  gate lands in 8.h).
- Tests: layer push/pop is observable in lookup; same chord
  rebound at higher layer wins; unbind exposes lower layer.

### Slice 8.d -- Migrate Replace mode (proof of pattern) ✅ landed

- Four binding shapes (`<Esc>`, `<BS>`, `<CR>`, char wildcard)
  registered in `keymap_replace::register_replace_bindings`
  into `KeymapLayer::Builtin` + `BindingMode::Replace`.
- `App::new` constructs a `KeymapHandle`, registers the
  Replace catalog, and the runtime threads `&app.keymap`
  through `TranslateContext`. `input::translate` calls
  `dispatch_replace(ctx.keymap, &event)` for
  `ModalState::Replace` -- the legacy `translate_replace`
  match table is gone.
- Drift test in `keymap_replace::tests` keeps the dispatcher
  honest against a frozen reference of the legacy body across
  the cross-product of {key code} × {modifier set}. Pinned
  end-to-end through `translate` by additional tests in
  `input::tests::*_in_replace*` and the
  `replace_dispatch_reads_from_handle_not_baked_in` /
  `alt_x_in_replace_overwrites_with_x` cases.
- Sets the migration template subsequent slices follow:
  per-mode `register_<mode>_bindings` + `dispatch_<mode>`,
  drift test against a private reference body, and the
  per-mode arm of `input::translate` switches to the
  registry-driven path.

### Slice 8.e -- Migrate Visual mode ✅ landed

- ~30 chord registrations covering exits (`<Esc>` / `v` / `V`),
  motions (`hjkl`, `0$^`, `wbe`, `WBE`, `{}` / `()`, `G`,
  arrow / Home / End aliases), and operators on selection
  (`d` / `x`, `c` / `s`, `y`, `>` / `<`).
- Motions and operators register through
  `BoundCommand::from_invocation` -- the dispatcher returns
  `Action::Invoke(command.clone())`. Only the three
  `ExitVisual` exits and the two block-only `Enter*` paths
  still carry a `legacy_action` (no `CommandInvocation` peer
  today; slice 8.i's bridge retirement closes that gap).
- Block-only `I` / `A` are pre-lookup overrides in
  `dispatch_visual` until the architecture's
  minor-mode-on-Visual layer push lands; the drift test pins
  the kind branch so a future graduation to `push_layer` is
  mechanical.
- `input::translate`'s `ModalState::Visual(kind)` arm now
  calls `dispatch_visual(ctx.keymap, &event, kind)`; the
  legacy `translate_visual` body is gone.
- Drift test in `keymap_visual::tests` walks the cross-product
  of {key code} × {modifier set} × {VisualKind}, asserting
  parity with a frozen reference body of the legacy
  `translate_visual`. Existing `input::tests::*_in_visual_*`
  cover the wiring end-to-end through `translate`.
- The test fixture in `input::tests` now shares one process-
  wide `(Builtins, KeymapHandle)` pair so trie-bound
  `CommandInvocation` ids match the `Builtins` each test
  references.

### Slice 8.f -- Migrate Insert mode ✅ landed

- Base Insert bindings registered by
  `keymap_insert::register_insert_bindings`: `<Esc>`, `<BS>`,
  `<CR>`, `<Tab>`, `<C-Space>`, plus the two-key paths
  `[<C-x>, <C-o>]` and `[<C-x>, <C-s>]`. `<C-x>` itself is a
  partial trie node; `dispatch_insert` translates the `Partial`
  lookup into `SetPending(AfterCtrlX)` and resolves the
  follow-up keystroke via `[<C-x>, normalised(event)]`. The
  next slice (8.g) generalises this to every prefix in Normal
  mode (`g_`, `z_`, `<C-w>`, mark/register/find-char wildcards).
- Literal-text fall-through stays in `dispatch_insert` rather
  than being registered as a char wildcard, per the original
  bullet -- the dispatcher returns `Action::Insert(c.to_string())`
  for any unbound non-CONTROL `Char(c)` lookup. When the popup
  overlay layer is pushed, *its* char wildcard wins, so typing
  routes through `CompletionAcceptThenInsert(c)` instead.
- Completion-popup overlay -> `KeymapLayer::MinorMode` layer
  pushed by `App::sync_keymap_overlays` whenever
  `insert_completion.is_some()`; popped on close.
- Active-snippet overlay -> `KeymapLayer::MinorMode` layer
  pushed whenever `active_snippet.is_some()`. Push order
  (snippet first, popup second) means popup's `LayerId` is
  always higher; popup wins on overlapping chords (preserving
  the legacy "popup precedes snippet" gating). The sync pops
  everything and re-pushes in canonical order whenever overlay
  state changes.
- Modifier-stripping rules in `dispatch_insert` are
  mode-specific (see the table in `keymap_insert.rs`'s module
  docstring): ALT and SUPER are stripped; CTRL and SHIFT are
  preserved so `<C-y>` stays distinct from `y` and `<S-Tab>`
  stays distinct from `<Tab>`. Three documented synthetic-
  modifier drift cases vs. legacy (`<S-Esc>`, `<C-Esc>`,
  `KeyCode::Tab + SHIFT`) -- terminals don't emit these in
  practice; the drift test allow-lists them.
- `input::translate`'s early-out branches for
  `translate_insert_completion_popup` and
  `translate_active_snippet` are gone; the `ModalState::Insert`
  arm now calls `dispatch_insert(ctx.keymap, &event,
  ctx.pending)` and the merged trie handles overlay precedence.
- The test fixture in `input::tests` now picks among shared
  scenario-specific keymaps (`shared_keymap_base`,
  `shared_keymap_with_popup`, `shared_keymap_with_snippet`,
  `shared_keymap_with_both_overlays`) so each test's
  dispatcher sees the right layer stack.

### Slice 8.g -- Migrate Normal mode

The big one. Likely sub-sliced:

- 8.g.i -- single-chord motions and operators (`j`, `w`,
  pseudo-operators `D` / `C` / `S` / `Y` / `x`, mode entry,
  paste, search nav, viewport jumps, etc.). ✅ landed: see
  `keymap_normal::register_normal_bindings` /
  `lookup_normal`. The legacy `input::translate_normal` keeps
  its match arm for the bindings still pending migration
  (operator-leading `d` / `c` / `y` / `>` / `<`,
  pending-prefix `g` / `z`, find-char, marks, register, macro
  control); subsequent sub-slices shrink that arm to nothing.
  Doubled-operator forms (`dd`, `yy`, `cc`, `>>`, `<<`) stay
  with the existing operator-pending state machine until 8.g.iii
  -- the architecture-doc bullet groups them with 8.g.i for
  conceptual completeness, but they ride the operator-pending
  trie expansion in 8.g.iii.
- 8.g.ii -- `g_` and `z_` family. ✅ landed.
  Two-key chord paths registered as `[g, X]` and `[z, X]` in
  the Normal-mode trie. `[g]` and `[z]` themselves stay
  partial nodes (no terminal binding); `lookup_normal`
  translates `LookupResult::Partial` on those chords into
  `SetPending(AfterG)` / `SetPending(AfterZ)`. The second
  key resolves through `keymap_normal::lookup_normal_two_key`,
  which the existing `Pending::AfterG` / `Pending::AfterZ`
  arms in `input::translate_normal` now call. The legacy
  `resolve_after_g` / `resolve_after_z` helpers are gone.
  Bindings covered: `gg` (typed Invoke), `gU` / `gu` / `g~`
  (case-operator pending), `gv`, `gJ`, `g;` / `g,`, `gd` /
  `gD` / `gy` / `gI` / `gr` (LSP), `zz` / `z.` (center),
  `zt` / `z<CR>` (top), `zb` / `z-` (bottom), `zf`, `zo`,
  `zc`, `za`, `zR`, `zM`, `zd`, `zj`, `zk`, `zi`.
- 8.g.iii -- operator-pending → motion / text-object trie
  expansion. ✅ landed.
  Each operator's resolution table is registered under its
  primary chord prefix: `[d, X]` / `[c, X]` / `[y, X]` /
  `[>, X]` / `[<, X]` for the single-chord operators, and
  `[g, U, X]` / `[g, u, X]` / `[g, ~, X]` for the case
  operators (extending the 8.g.ii `g_` paths). Each table
  contains: motion targets (typed `Invoke(op,
  Target::Motion(...))`), the doubled-operator linewise form
  (`Invoke(op, Range::CurrentLine)` -- `dd`, `cc`, `yy`,
  `>>`, `<<`, `gUU`, `guu`, `g~~`), `i_` / `a_` text-object
  pendings (`SetPending(AfterTextObject)`) plus their depth-3
  resolutions (`Invoke(op, Target::TextObject(...))` for
  `diw`, `daW`, `dab`, etc. with all aliases), and `f` /
  `F` / `t` / `T` find-char pendings
  (`SetPending(AfterFindChar { operator: Some(op) })` -- the
  third-key resolution stays in legacy `resolve_after_find_char`
  until 8.g.v). The `Pending::AfterOperator` and
  `Pending::AfterTextObject` arms in `input::translate_normal`
  call `lookup_normal_with_prefix(handle, &prefix, event)`,
  computing the prefix from the operator id via
  `keymap_normal::operator_prefix`. The legacy
  `resolve_after_operator` and `resolve_after_text_object`
  functions are gone, and the operator-leading single keys
  (`d` / `c` / `y` / `>` / `<`) moved out of
  `translate_normal`'s match arm into the trie's depth-1
  layer (terminal Bound nodes that arm
  `Pending::AfterOperator`).
- 8.g.iv -- count accumulator (§7.1). ✅ landed.
  The digit accumulator stays App-side (`pending_count`,
  `op_count` -- updated by the `PushDigit` handler and the
  `SetPending(AfterOperator)` transition). The
  multiplication moves to `keymap_normal::attach_count`,
  applied at the tail of `input::translate_normal` so every
  `Action::Invoke` leaving translate carries the resolved
  count (`op_count * motion_count`, with `motion_count`
  falling back to `inv.count.unwrap_or(1)` -- the
  binding's registered default, e.g. `<PageDown>`'s
  `Count(10)` -- when the user hasn't typed a digit). App's
  existing count-multiplication math in
  `run_document_invocation` / `run_read_only_motion` stays
  for now -- it's idempotent against translate's attach
  (same inputs, same result) and serves the few internal
  callers that build invocations without going through
  translate (`do_repeat_find`, etc.). 8.i can collapse the
  duplication once those callers route through a shared
  attach helper.
- 8.g.v -- mark / register / find-char wildcards (§7.3).
  ✅ landed.
  Each prefix chord (`m` / `'` / `` ` `` / `"` / `q` / `@` /
  `f` / `F` / `t` / `T`) registers a depth-1 terminal binding
  that arms the matching `Pending::After*` state, plus a
  depth-2 child via [`ChordPattern::CharLiteral`] (the
  trie's wildcard primitive). The wildcard binding carries
  a placeholder action (`SetMark('\0')`,
  `SelectRegister(Register::Unnamed)`, `Invoke(find_char_*,
  Args::None)`, ...); `keymap_normal::substitute_normal_capture`
  rewrites the placeholder with the captured char before the
  action leaves translate. Operator-prefixed find-char
  (`d{f|F|t|T}<X>`) extends the same mechanism: depth-3
  wildcards under each operator prefix resolve to
  `Invoke(op, Target::Motion(find_char_*, Args::Char(captured)))`.
  All `Pending::AfterSetMark` / `AfterJumpMark*` /
  `AfterRegister` / `AfterMacroStart` / `AfterMacroPlay` /
  `AfterFindChar` arms in `input::translate_normal` now call
  `lookup_normal_with_prefix` against the appropriate prefix;
  the legacy `resolve_after_*` helpers are gone. The legacy
  match arm in `compute_normal_action` is gone too -- only
  `q` while macro recording stays as a special-case
  short-circuit (state-dependent on App's `recording_macro`).
  One documented drift from legacy: the trie's
  `CharLiteral` only matches bare-printable chords, so e.g.
  `f<C-x>` drops the pending state instead of using `'x'` as
  the find target. Terminals don't typically emit such chord
  combinations.
- 8.g.vi -- `<C-w>` window-management sub-tree. ✅ landed.
  Closes Normal mode out. Every CTRL chord (`<C-d>` /
  `<C-u>` / `<C-f>` / `<C-b>` / `<C-e>` / `<C-y>` / `<C-r>`
  / `<C-o>` / `<C-i>` / `<C-t>` / `<C-l>` / `<C-v>` /
  `<C-q>`) now lives at depth 1 in the Normal trie -- the
  legacy CTRL guard at the top of `compute_normal_action` is
  gone. `<C-w>` is a terminal-with-children: depth-1 binds
  `SetPending(AfterCtrlW)`; depth-2 covers both bare
  (`<C-w>w` / `<C-w>l` / ...) and ctrl-modified
  (`<C-w><C-w>` / `<C-w><C-l>` / ...) second-key forms,
  including the `<Tab>` / `<S-Tab>` (= `BackTab`) /
  `<Backspace>` / arrow aliases vim's lenient `<C-w>` prefix
  accepts. The `Pending::AfterCtrlW` arm in
  `input::translate_normal` calls
  `lookup_normal_with_prefix(handle, &[KeyChord::ctrl('w')],
  event)`; `resolve_after_ctrl_w` is gone.
  After this slice `compute_normal_action` reduces to:
  pending resolution -> digit prefix -> recording-`q`
  short-circuit -> `lookup_normal`. The legacy match arm is
  empty; the function is essentially a thin orchestrator
  around the registry.
  `<C-c>` (universal Quit) is intentionally not registered
  in the trie -- it's intercepted by `input::translate`
  before mode dispatch. The `<C-c>` branch of legacy
  `resolve_after_ctrl_w`'s ctrl table was unreachable in
  practice; its registration in the trie is kept for parity
  with the legacy reference but produces the same
  unreachable behaviour.

Each sub-slice ships independently green; the drift test is
the regression net.

### Slice 8.h -- Plugin / user-config integration ✅ landed

The plugin host (WASM Component-Model) is post-1.0 (see
design.md §13 roadmap), so this slice ships the **registry-side
infrastructure** every future host integration sits on top of:

- **`KeymapCapability` enum** in `keymap_registry.rs` -- the
  privilege bundle a writer presents when calling the gated
  bind APIs. Variants mirror the WIT spec (design.md §5.5):
  - `Full` -- unrestricted; reserved for the host's startup
    catalog enumeration.
  - `User` -- writes to `KeymapLayer::User` only;
    `init.rs` runs with this.
  - `MinorMode` -- writes to any
    `KeymapLayer::MinorMode(_)` / `KeymapLayer::Buffer`
    layer; plugins with `keymap-write:minor-mode` in their
    manifest receive this.
  - `OwnedLayer { layer_id }` -- writes to a single
    specified `MinorMode` layer; mirror of WIT
    `keymap-write:plugin-layer`.
- **`KeymapError`** with `CapabilityDenied { capability,
  layer }` and `InvalidChord(ChordParseError)` variants;
  `Display` + `Error` impls so the future host can surface the
  failure to the plugin / user verbatim.
- **`KeymapHandle::try_bind`**, **`try_unbind`**,
  **`try_push_layer`** -- capability-gated wrappers that funnel
  through a single `capability_allows` check before delegating
  to the un-gated `bind` / `unbind` / `push_layer`. The un-gated
  variants stay public; the host startup pass uses them for the
  built-in catalog.
- **`KeymapHandle::try_bind_chord_string`** -- WIT-shaped
  convenience that parses a chord-sequence string
  (`"<C-w>j"`, `"gd"`) into `Vec<ChordPattern::Literal>` before
  delegating to `try_bind`. The future WIT `bind` host-fn calls
  this; user `init.rs` calls a thin wrapper around it.
- Tests covering the architecture-doc enumeration:
  - `plugin_binds_chord_that_fires_plugin_command`
  - `user_remaps_dd_and_overrides_builtin` (the
    "survives a restart" shape, simulated as
    "user override stays authoritative across intervening
    writes" -- persistence isn't a registry concern; init.rs
    re-runs at boot).
  - `conflicting_plugins_resolve_via_layer_priority` (two
    `OwnedLayer` capabilities binding the same chord; later
    push wins; popping it restores the older).
  - Capability-denial tests for every (capability, layer)
    pair in the matrix: User vs Builtin / MajorMode /
    MinorMode / Buffer; MinorMode vs Builtin / MajorMode /
    User; OwnedLayer vs other-id MinorMode / Builtin.
  - `try_bind_chord_string` parses `<C-w>j` and the binding
    fires; an unterminated angle-bracket surfaces as
    `KeymapError::InvalidChord`.
- **WIT interface** (`wit/keymap.wit`) is **deferred** until
  the WASM host lands. The capability + error types in this
  slice are the in-process shape; once the host arrives, the
  WIT functions translate plugin manifest declarations into
  `KeymapCapability` variants and call through `try_*`.
- **`init.rs` API** is similarly deferred. The shape will be
  a thin wrapper over `try_bind_chord_string` (with command
  resolution via the `CommandRegistry` by name); writing it
  meaningfully requires the WASM init-module loading machinery
  that doesn't exist yet.

### Slice 8.i -- Retire the `bind_legacy` bridge

The full approach memo lives at
[`docs/../archive/8i-approach.md`](../archive/8i-approach.md): goal, the
`Effect::AppAction(AppEffect)` carrier shape (option α), the
type-hoisting decisions, and the sub-slice plan.

**Sub-slice status:**

- **8.i.0 -- Carrier + dispatcher's Action branch.** ✅ landed.
  - Adds `lattice_grammar::AppEffect` (initial variant: `Quit`).
  - Adds `Effect::AppAction(AppEffect)`.
  - Adds `ActionSpec` / `ActionContext` / `register_action` /
    `require_action` parallel to the existing motion / operator /
    text-object / ex-command spec machinery.
  - Replaces `CommandRegistration::Stub` with
    `CommandRegistration::Action(ActionSpec)`.
  - Wires `CommandKind::Action` in
    `dispatcher::execute` (was an `InvalidArgs` stub).
  - `App::apply_effect` gains an `Effect::AppAction(app)` arm
    delegating to a new `App::apply_app_effect(app)` method.
  - Tests: `dispatcher::tests::execute_routes_action_kind_to_action_spec`
    (carrier flows through `execute()` and surfaces the spec's
    Effect) + `action_branch_rejects_non_action_entries`
    (`require_action`'s kind-mismatch path).
  - No call-site changes; bridge stays active. Workspace tests
    green: lattice-grammar 184 → 186; all other crates unchanged.
- **8.i.1 -- Promote no-payload Action variants.** ✅ landed
  across 8 sub-batches (8.i.1.a-h). 41 distinct AppEffect variants
  promoted; ~85 `bind_legacy` call sites swapped to `bind`.
  `register_normal_bindings` / `register_visual_bindings` /
  `register_insert_bindings` / `register_replace_bindings` all
  now take `actions: &ActionIds`. Drift-test reference bodies in
  Visual and Replace updated in lockstep with their respective
  variant migrations. Workspace tests stay green at every batch
  boundary; lattice-ui-tui at 1328 throughout.
  - 8.i.1.a -- `%`, `~`, `o`, `O`, `K` (5 variants).
  - 8.i.1.b -- search + history: `n`, `N`, `<C-o>`, `<Tab>` /
    `<C-i>`, `g;`, `g,`, `<C-t>` (7 variants).
  - 8.i.1.c -- fold ops: `zo`, `zc`, `za`, `zR`, `zM`, `zd`,
    `zj`, `zk`, `zi` (9 variants).
  - 8.i.1.d -- edit history + scroll: `u`, `<C-r>`, `.`,
    `<C-f>`, `<C-b>`, `<C-y>`, `<C-e>` (7 variants).
  - 8.i.1.e -- misc viewport / entry / paste: `<C-l>`, `:`,
    `-`, `gv`, `p`, `P` (6 variants).
  - 8.i.1.f -- LSP go-tos: `gd`, `gD`, `gy`, `gI`, `gr` (5
    variants).
  - 8.i.1.g -- final no-payload: `a`, `zf`, `<BS>` (insert),
    `<C-Space>` / `<C-x><C-o>`, `<C-x><C-s>` (5 variants).
  - 8.i.1.h -- drift-body migration: visual `<Esc>` / `v` /
    `V`, replace `<BS>` (2 variants). `dispatch_replace` gained
    its `Invoke` fallback alongside this batch.
- **8.i.2 -- Promote parameterised Action variants.** ✅ landed
  across 5 sub-batches (8.i.2.a-e). 22 distinct CommandIds
  promoted; ~28 `bind_legacy` call sites swapped to `bind`.
  Encoding convention: distinct `CommandId` per param value;
  AppEffect carries the typed param when the param type lives
  in (or is hoisted into) `lattice-grammar`. App's
  `apply_app_effect` matches a single arm per AppEffect variant
  (e.g. `EnterMode(state)` -> `self.apply(Action::EnterMode(state))`)
  rather than N flat variants. Slice 8.i.2.c hoisted
  `ViewportPos` and `ScrollPos` from `lattice-ui-tui` into
  `lattice-grammar/src/app_effect.rs`; the App keeps `pub use`
  re-exports of both so existing `crate::app::ViewportPos` /
  `crate::app::ScrollPos` callers stay compiling.
  - 8.i.2.a -- mode entry (6 IDs): `EnterMode(Insert/Normal/
    Replace)` + `EnterVisual(Charwise/Linewise/Blockwise)`.
  - 8.i.2.b -- search (4 IDs): `EnterSearch(_)` (`/`/`?`) +
    `SearchWordUnderCursor(_)` (`*`/`#`).
  - 8.i.2.c -- viewport (6 IDs, type hoist): `JumpViewport(_)`
    (`H`/`M`/`L`) + `ScrollCursorTo(_)` (`zt`/`zz`/`zb` and
    aliases).
  - 8.i.2.d -- operators (4 IDs): `JoinLines{with_space}`
    (`J`/`gJ`) + `FindRepeat{reverse}` (`;`/`,`).
  - 8.i.2.e -- insert literals (2 IDs): `InsertNewline` (Insert
    + Replace `<CR>`) + `InsertTab` (Insert `<Tab>`).
- **8.i.3 -- Wildcard-captured variants.** ✅ landed as one
  commit. Seven captured-char wildcards promoted in a single
  batch (the seam was small enough that splitting wouldn't have
  helped review): `OverwriteChar`, `SetMark`, `JumpToMarkLine`,
  `JumpToMarkExact`, `SelectRegister`, `StartMacroRecord`,
  `PlayMacro` (with `PlayLastMacro` as the `@@` branch of the
  same `play-macro` action). Architecture call: validation lives
  in the bound `ActionSpec`, not the per-mode dispatcher. The
  dispatcher just folds `Args::Char(captured[0])` into the
  invocation; each spec's apply closure validates per-variant
  rules and either emits the typed `AppEffect` or returns
  `Effect::None` (the no-op effect IS the "drop pending" signal,
  since `App::apply` clears pending on every non-`SetPending(_)`
  action). `Register::from_input_char` was added to
  `lattice-grammar` as the canonical char-to-Register mapping
  shared by the App and the `select-register` spec. The Replace
  drift comparator's `Invoke` arm now compares `args` so
  captured-char substitution regressions trip the test.
- **8.i.4 -- Retire Pending; finalise.** ✅ landed.
  - 8.i.4.a -- Scaffold + 9 simple-prefix migration. ✅ landed.
    Adds `App::partial_chord: Vec<KeyChord>` and
    `Action::AbsorbPartialChord(KeyChord)`; `lookup_normal`
    emits the latter on every trie `Partial`. The 7 prefix-only
    `bind_legacy([m / ' / ` / " / q / @ / <C-w>], SetPending(After*))`
    sites are gone (the trie's natural `Partial` handles them);
    `g` and `z` likewise stop synthesising `SetPending` from
    `lookup_normal`'s `Partial` arm. `App::apply` clears
    `partial_chord` on every non-`AbsorbPartialChord(_)` action,
    mirroring the existing `pending` reset on every
    non-`SetPending(_)` action. `compute_normal_action` gains a
    top-level partial_chord short-circuit that wins over the
    `match pending` body. The 9 migrated `match pending` arms
    are unreachable post-migration but kept as a defensive
    no-op until 8.i.4.c retires the Pending enum.
  - 8.i.4.b -- AfterCtrlX (Insert mode). ✅ landed.
    `dispatch_insert` grows a `partial_chord` parameter; the
    trie's `Partial` for `<C-x>` (because `[<C-x>, <C-o>]` and
    `[<C-x>, <C-s>]` are bound) emits
    `Action::AbsorbPartialChord(<C-x>)` instead of the prior
    `SetPending(AfterCtrlX)` synthesis.
  - 8.i.4.c -- AfterOperator + AfterTextObject + AfterFindChar.
    ✅ landed. The 10 remaining `bind_legacy(... SetPending(After*))`
    sites retire. AfterOperator (8 sites) needs op_count
    latching that the trie's plain `Partial` doesn't carry, so
    a new `AppEffect::AbsorbOperatorPrefix(OperatorId)` variant
    handles both atomic effects (latch `pending_count` ->
    `op_count`, push prefix to `partial_chord`); each operator
    binds a typed `CommandInvocation` whose `ActionSpec`
    returns this variant. AfterTextObject and AfterFindChar
    delete cleanly -- their `[op, i / a]` and `[op, f / F / t /
    T]` paths are natural `Partial` nodes once the standalone
    binds are gone. `lookup_normal_with_prefix`'s `Partial` arm
    now emits `AbsorbPartialChord(chord)` (was
    `SetPending(None)`), required for nested partials like
    `[d, i]` and `[d, f]`. `compute_normal_action`'s `match
    pending` body collapses to one defensive no-op covering
    all 13 Pending variants. `actions::populate` grew a
    `&Builtins` parameter so the operator-prefix helpers can
    capture the `OperatorId` in their closures.
  - 8.i.4.d -- Pending retirement + `<C-w>` sub-tree.
    ✅ landed. `App::pending: Pending` field, `Pending` enum,
    `Action::SetPending(_)` variant, `compute_normal_action`'s
    `match pending {}` body, and the `pending: Pending`
    parameter on `TranslateContext` /  `translate_normal` /
    `compute_normal_action` / `dispatch_insert` are gone.
    `App::apply`'s "non-SetPending clears pending" guard
    retired. The 2 remaining `bind_legacy(... Action::*Pane*, ...)`
    sites in `register_ctrl_w_sub_tree` migrated to typed
    `CommandInvocation`s with 6 new App-side actions
    (`split_pane_horizontal`, `split_pane_vertical`,
    `close_pane`, `navigate_pane_{left,down,up,right}`,
    `next_pane`, `prev_pane`); `PaneDirection` hoisted from
    `lattice_ui_tui::pane` to `lattice_grammar::app_effect`
    (mirroring the `ScrollPos` / `ViewportPos` hoist in
    8.i.2.c). `run_invocation` gained a `CommandKind::Action`
    short-circuit: action-kind invocations bypass
    `run_document_invocation`'s count multiplication and
    pending-count reset, otherwise the `pending_count = 0;`
    inside it would fire before the dispatched
    `AppEffect::AbsorbOperatorPrefix(_)` could latch op_count,
    breaking `2dw`-style flows.
  - 8.i.4.e -- Retire `bind_legacy` machinery. ✅ landed.
    Migrated the completion-popup and active-snippet
    minor-mode layer builders to bind typed
    `CommandInvocation`s through `bind_invocation`; promoted
    13 popup/snippet variants into `AppEffect`
    (`CompletionNext`, `CompletionPrev`, `CompletionAccept`,
    `CompletionCancel`, `CompletionCancelAndExitInsert`,
    `CompletionToggleDocs`, `CompletionDocsScrollDown`,
    `CompletionDocsScrollUp`, `CompletionAcceptThenInsert(c)`,
    `SnippetNextPlaceholder`, `SnippetPrevPlaceholder`,
    `SnippetLeave`, plus `CompletionTrigger` already migrated
    earlier). The wildcard `<bare-char>` chord in the popup
    layer rides as `Args::Char(c)` on the
    `completion-accept-then-insert` invocation; control chars
    are filtered in the spec. With every binding now carrying
    a real `CommandInvocation`, the `KeymapHandleLegacyExt`
    trait + `bind_legacy` adapter, the
    `BoundCommand::legacy_action` field +
    `from_legacy_action` constructor, the
    `legacy_action_command_id()` placeholder, and the
    `legacy_action.is_some()` branches in `dispatch_replace` /
    `dispatch_visual` / `action_from_bound_with_capture` are
    deleted. Drift reference bodies in
    `keymap_replace::tests::legacy_translate_replace` and
    `keymap_visual::tests::legacy_translate_visual` (and their
    paired `actions_equivalent` comparators + cross-product
    `registry_dispatch_matches_legacy_translate` tests) are
    deleted as well -- with the legacy bridge gone there is no
    second path to drift against; the per-binding `match`
    tests in each module continue to pin the exact
    `Invoke(...)` shape.
  - 8.i.4.f -- Close the count-flow seam. ✅ landed.
    Two latent seam bugs surfaced once an end-to-end
    `key_harness_*` press harness landed in
    `app::tests` (drives `crossterm::KeyEvent` through
    `input::translate` + `App::apply` -- the same path
    `runtime.rs` walks). Both bugs were invisible to the
    per-layer tests: the translate-side tests assert on
    the returned `Action`, the App-side tests
    hand-construct `Action::Invoke(...)`; nothing
    exercised the seam between them.
    - Bug A: dispatcher double-applied count. After
      8.g.iv routed count multiplication into
      `keymap_normal::attach_count` (input-side), the
      App-side math in `run_document_invocation` and
      `run_read_only_motion` was never removed. With
      `attach_count` baking `Count(N)` into the inv and
      the dispatcher then computing
      `op_count.saturating_mul(motion_count)` over that
      already-multiplied count, `2dd` ran with `op*motion
      = 2*2 = 4` and `3dd` saturated the buffer at 9.
      Fix: drop the dispatcher-side multiplication;
      dispatcher now reads `inv.count` directly and
      drains `pending_count` / `op_count` from App
      state. The three App-layer tests that bypassed
      `attach_count` by manually feeding
      `Action::PushDigit + Invoke(operator) + Invoke(motion)`
      were migrated to bake the right count into the
      invocation themselves -- mirroring what
      `attach_count` would produce.
    - Bug B: digit eaten in operator-pending state.
      `compute_normal_action` checked the
      `partial_chord` short-circuit BEFORE the digit
      handler. After `d` absorbed into
      `partial_chord=['d']`, typing `2` routed to
      `lookup_normal_with_prefix(['d'], '2')` -- unbound
      -- which returned `Action::None` and silently
      aborted the operator (App's partial_chord-clear
      guard at the top of `apply` ate the prefix). Vim's
      `d2w`, `2d3w`, `5gg` could never fire because the
      digit never reached the `pending_count`
      accumulator. Fix: hoist the digit handler above
      the `partial_chord` short-circuit in
      `compute_normal_action`. Safe today (no built-in
      chord has a digit as second key); a future plugin
      that wants `[X, digit]` chords would expand the
      rule to "digit handler unless `[partial, digit]`
      is bound", with the registry already carrying the
      data needed. Coordinated change in App's
      partial_chord-clear guard: `Action::PushDigit(_)`
      is now exempt alongside `AbsorbPartialChord(_)`,
      since accumulating a count between chord steps is
      neither a resolution nor an abort of the
      multi-key sequence.
    - The `key_harness_*` press tests now pin both
      fixes: `count_after_operator_d2w_deletes_two_words`,
      `counts_multiply_on_both_sides`, and
      `count_clears_after_motion_fires`. The tests sit
      alongside the original two sanity tests
      (`j_advances_cursor_one_line`,
      `dw_deletes_first_word`) plus coverage for the
      `<C-w>` action-kind short-circuit (slice 8.i.4.d)
      and the Insert mode round-trip.
  - 8.i.4.g -- Linewise-operator newline semantics. ✅
    landed. The press harness `key_harness_count_before_
    operator_dd_deletes_n_lines` (deferred from 8.i.4.f)
    surfaced a third bug -- distinct from the count-flow
    seam -- in the linewise operators themselves. Vim's
    `dd` consumes the line AND its trailing newline so
    the line count drops by `count`; lattice's
    `Range::CurrentLine` returned just the line content
    `[(line, 0), (line, line_len)]`, so `dd` left a
    phantom empty line behind. Same bug for `yy`: the
    yanked content was `"BBB"` instead of vim's
    `"BBB\n"`, breaking paste-after-linewise round-trips.
    `cc`, `>>`, `<<` were already correct -- `cc`'s
    "delete content + enter Insert" produces vim's
    "delete line + reinsert blank line" by byte
    coincidence (`Range::CurrentLine`'s byte span across
    multiple lines does include interior newlines, just
    not the trailing one); `>>` / `<<` deliberately
    operate on content only.
    - Fix lives operator-side, not in the shared
      `Range::CurrentLine` resolver, so `>>` / `<<` /
      `cc` keep their current ranges. New helper
      `extend_linewise_range(buffer, range)` in
      `lattice-grammar::builtins`: extends the range to
      consume the trailing newline; on the last line
      (no trailing newline available) consumes the
      LEADING newline instead so the previous line
      doesn't gain a phantom newline; whole-buffer
      ranges pass through unchanged.
    - `operator_delete` uses the extended range as both
      the slice source and the edit range, so the
      register content and the buffer-side delete agree.
    - `operator_yank` doesn't walk the buffer for the
      newline -- linewise yank content always ends with
      `\n` (vim clipboard convention), so yank just
      appends `\n` to the original slice if missing.
      That gives the same `"BBB\n"` whether the source
      line had a trailing newline or not.
    - Test fallout: 3 grammar tests + 6 UI-layer tests
      pinned the old "leaves an empty line" / `"BBB"`
      shapes; updated to the vim-correct shapes
      (`"aaa\nccc"` and `"BBB\n"` respectively). The
      stale "existing app contract" comment at
      `dd_on_non_fold_line_uses_count_one` is gone --
      the contract is now "matches vim".
  - 8.i.4.h -- Wrap-up: catalog⇄registry test, dispatcher
    end-to-end bench, cross-refs. ✅ landed. Three small
    threads bundled into one slice so the four-artefact
    rule lands as one cohesive unit.
    - Test: new `every_catalog_command_resolves_in_registry`
      in `input.rs`'s test module asserts every
      `default_keymap()` entry with `command: Some(name)`
      resolves to a real registry entry. Catches descriptor
      drift the existing `keymap_descriptors_dont_drift_
      from_translate` test misses -- the chord-side check
      verifies `chord -> Action` round-trip; the new check
      verifies `descriptor.command -> CommandRegistry`
      consistency. They're complementary, not competing.
    - Drift-test retirement deferred. The wrap-up plan
      originally called for retiring the chord-side test
      ("descriptor IS behaviour"). Held off: that's only
      true once `default_keymap()` is the trie's
      source-of-truth, which is the post-1.0 promotion
      slice. Until then, the chord-side test still catches
      bugs the registry-side check can't (chord notation
      drift, mode mis-routing). Both stay; one retires
      when source-of-truth promotion lands.
    - Bench: two new `dispatch_translate_full_*` rows in
      `benches/keymap.rs` measuring full `translate()`
      round-trip through the production-wired keymap
      (real `register_*_bindings` calls -- not the
      synthetic `populated_handle()` the trie-only rows
      use). `_two_chord` covers the partial_chord
      dispatch (`gd`); `_operator_motion` covers the
      operator-prefix flow that 8.i.4.c rebuilt (`dw`).
      Both ~100 ns -- ~10× under the 1 µs keystroke
      commitment, ~3× the bare trie-lookup numbers (the
      remainder is dispatcher fan-out + Action
      construction).
    - `docs/../operations/benchmarks.md`: rows added + a slice-8.i
      narrative paragraph confirming the dispatcher
      rebuild stayed in budget; AbsorbPartialChord /
      AbsorbOperatorPrefix short-circuits don't
      measurably hurt the hot path.
    - `docs/design.md`: cross-references added to §5.2.4
      (Extensibility) and §5.11 (Introspection). §5.2.3
      already pointed at the architecture doc; §5.2.4 now
      points at §5 / §6 here for keymap-side extension
      mechanics, and §5.11 points at §3.5 / §6 for the
      typed-descriptor metadata surface that backs
      `:describe-key`.

## 10. Trade-offs flagged

- **Operator-pending as trie state vs. separate mode.** The
  trie encoding (§7.2) is cleaner architecturally but means
  the dispatcher tracks "I'm at a partial trie node" as state
  instead of "I'm in a `BindingMode::OperatorPending` mode".
  Existing `Pending` enum stays, repurposed: it now carries
  the partial trie cursor + count. Net win in clarity once
  the migration completes; some churn in the App's apply path.
- **Layer-merge cost on every push/pop.** Pushing a minor
  mode rebuilds the merged trie. With ~hundreds of bindings
  per layer and ~5 layers active, that's a sub-millisecond
  hit on mode transitions (rare). The alternative -- walk
  layers separately on each lookup -- pays the cost on every
  keystroke instead. Merge-on-write is the right trade-off.
- **`Action` enum collapse.** Today input.rs returns `Action`
  variants; some encode mode transitions, App-thread effects,
  not just command invocations. Migration may need to keep a
  thin `Action` enum during the transition (slices 8.d-g);
  it dissolves once every binding routes through
  `CommandInvocation`. Track where this lands at slice
  boundaries.
- **Char-literal wildcards in the trie.** Adding `CharLiteral`
  child variants (§7.3) is a small structural extension to the
  trie; it does not affect the literal-chord lookup path. Bench
  on slice 8.b confirms.

## 11. Mode-owned keymap contributions (substrate gap)

Sections 5.2--5.3 describe major / minor mode bindings as
"host registers when a major mode activates" / "pushed when a
minor activates." The *registration* path was specified; the
*declaration* path -- how a `Mode` impl in another crate
expresses what its bindings are -- was never wired. The
`lattice-mode::contributions::Keymap` stub
(`crates/lattice-mode/src/contributions.rs`) currently carries
a `_private: ()` field with a TODO comment: *"the placeholder
lets modes declare intent to contribute a keymap layer without
forcing `lattice-mode` to depend on `lattice-grammar`."*

Until this gap closes, every mode that ships bindings ends up
registering them in `lattice-host` via a hand-rolled
`<mode>_keymap.rs` (`multibuffer_keymap.rs`, the LSP /
Oil / Snippet bindings under `KeymapLayer::Builtin`). This
contradicts the mode-ownership convention
(`feedback_mode_owns_its_surface`) and blocks the MO.1--MO.4
cleanup slices.

### 11.1 Substrate placement

The chord-substrate moves down the dependency graph so modes
in any crate can construct bindings:

| Type | Today (host-owned) | Target crate | Why |
|---|---|---|---|
| `KeyChord`, `KeyKind`, `KeyMods`, `ChordPattern` | `lattice-host::chord` | `lattice-protocol` | Renderer-neutral wire data; same shape class as `BufferId` / `Position` / `Selection`. Renderers produce them, dispatchers consume them. |
| `BindingMode` | `lattice-host::keymap` | `lattice-mode` | Vim-modal-state enum; semantically part of the mode subsystem. |
| `Keymap` contribution type | stub in `lattice-mode::contributions` | `lattice-mode` (real) | Returned by `Mode::keymap()`. |
| `KeymapTrie`, `KeymapLayer`, `BoundCommand` | `lattice-host::keymap_trie` | stay in host | Matcher engine + layer resolution; implementation detail of the lookup hot path. |

`lattice-runtime` was considered and rejected: it owns async
substrate (EventBus, Document spawning, snapshots), and chord
primitives have no runtime characteristics. Coupling them
into runtime would be by accident, not by meaning.

Adding `lattice-grammar` as a `lattice-mode` dep is permitted:
`lattice-grammar` does not depend on `lattice-mode`, so no
cycle. The previously-cited reason for the stub deferral is
resolved by accepting the dep.

### 11.2 The real `Keymap` contribution type

```rust
// crates/lattice-mode/src/contributions.rs
pub struct Keymap {
	pub bindings: Vec<KeymapBinding>,
}

pub struct KeymapBinding {
	pub mode: BindingMode,
	pub chords: Vec<ChordPattern>,
	pub command: CommandInvocation,
	pub source: SourceLocation,
}
```

Modes return `Keymap` from `Mode::keymap()`. The contribution
is *declarative* -- same value returned every call -- so the
host can ask once at registration time and cache the result.
Layer is implicit: every binding contributed by `Mode X` lives
at `KeymapLayer::MinorMode(x.id())` (which covers both majors
and minors per K.1.b convention).

#### 11.2.1 Ergonomic surface: `Keymap::bind_chord`

The raw struct is the floor. The recommended idiom for
mode-contributed keymaps reads like a binding table without
manual chord-vector construction or `SourceLocation` plumbing
per row:

```rust
impl Mode for MultibufferMode {
	fn keymap(&self) -> Keymap {
		Keymap::new()
			.bind_chord(BindingMode::Normal, "]e", self.commands.excerpt_next)
			.bind_chord(BindingMode::Normal, "[e", self.commands.excerpt_prev)
	}
}
```

`bind_chord` is `#[track_caller]` so `Location::caller()`
captures the binding row's own `file:line` into the resulting
[`SourceLocation`]. `:describe-key` shows the chord's
declaration site directly; no
`SourceLocation::builtin_file(file!(), line!())` boilerplate
per row. (Boilerplate that would also rot: a row's
hand-written `line!()` drifts every time a row above is
inserted or deleted.)

The chord string is parsed via the existing
[`lattice_protocol::parse_chord_sequence`] -- same notation
the host's `keymap_entry!` catalog uses. Multi-chord paths
work uniformly across vim and emacs idioms:

| chord string | parsed sequence | typical use |
|---|---|---|
| `"j"` | `[char('j')]` | vim motion |
| `"gd"` | `[char('g'), char('d')]` | vim go-to-def |
| `"]e"` | `[char(']'), char('e')]` | multibuffer next-excerpt |
| `"<C-w>j"` | `[ctrl('w'), char('j')]` | vim split-down |
| `"<C-w>gd"` | `[ctrl('w'), char('g'), char('d')]` | vim split-goto-def |
| `"<C-x>pp"` | `[ctrl('x'), char('p'), char('p')]` | emacs project-switch |
| `"<C-x><C-s>"` | `[ctrl('x'), ctrl('s')]` | emacs save-buffer |
| `"<C-c><C-c>"` | `[ctrl('c'), ctrl('c')]` | mode-confirm |
| `"<S-Tab>"` | `[shift(Tab)]` | snippet-prev |
| `"<lt>"` | `[char('<')]` | literal `<` |

The trie indexes the chord sequence directly. Pressing
`<C-x>` after a `bind_chord(Normal, "<C-x>pp", …)` gives
`Partial`; pressing the second `p` gives `Bound` and fires.
No "after-C-x" binding-mode state is required -- the matcher
walks the sequence generically. (This is the same trie
discipline that has always supported vim's `gg`; the K.2.3
surface just makes the emacs-prefix shape ergonomic to
declare from a mode crate.)

**Boot-fail on malformed chord strings.** Mode bindings live
at compile-time-static call sites with constant strings; a
parse error is a bug in the mode impl, not a runtime
condition. `bind_chord` panics with a message naming the
chord string and the caller location. The editor refuses to
boot rather than silently dropping the binding -- same
discipline the host's `keymap_entry!` catalog uses for the
identical reason.

**What `bind_chord` deliberately does not express:** wildcard
slots (`'a` mark target, `"a` register name, `fX` find-char
target, `qa` macro-name, `@a` macro-play target). The
shorthand has no notation for "any single typed char"; the
rare mode that needs `ChordPattern::CharLiteral` calls
[`Keymap::bind`] with an explicit `chords` vector. Today
every wildcard-bearing binding lives in the host's built-in
catalog (Normal-mode marks / registers / find-char / macro
prefixes), so mode crates almost never need the lower-level
API.

Plugin-loaded bindings, runtime `:bind` invocations, and
test fixtures use the lower-level [`KeymapBinding::new`]
constructor directly with whatever `SourceLocation` is
appropriate (`SourceLayer::Plugin(...)`,
`SourceLayer::Runtime`, …). `bind_chord` is the affordance
for the built-in-Rust idiom; it does not own the contract.

#### 11.2.2 Table-form contribution: `Keymap::from_entries`

The chain form is ergonomic for 1-5 bindings. Mode crates
that ship 5-20+ bindings (LSP majors / minors, multibuffer,
oil, snippet, file-tree, …) read better as a **table** the
eye can scan top-down. K.2.4.A.0 promoted the host's
`keymap_entry!` macro out of `lattice-host` and into
`lattice-mode` so any mode crate can declare bindings as
static-catalog rows:

```rust
use lattice_mode::{
    keymap_entry, BindingMode, Keymap, KeymapEntry,
    LifecycleFuture, Mode, ModeContext, ModeKind, ModeId,
};
use std::sync::OnceLock;

fn multibuffer_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! { mode: Normal, chord: "]e",
                doc: "Jump to next excerpt",
                cmd: "multibuffer:excerpt-next" },
            keymap_entry! { mode: Normal, chord: "[e",
                doc: "Jump to previous excerpt",
                cmd: "multibuffer:excerpt-prev" },
            keymap_entry! { mode: Normal, chord: "]E",
                doc: "Jump to next file boundary",
                cmd: "multibuffer:file-next" },
            keymap_entry! { mode: Normal, chord: "[E",
                doc: "Jump to previous file boundary",
                cmd: "multibuffer:file-prev" },
        ]
    })
}

impl Mode for MultibufferMode {
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(multibuffer_keymap_entries())
    }
    // ...id(), kind(), on_activate(), ...
}
```

Each `keymap_entry!` row carries `mode`, `chord` (notation
string), `doc` (one-line summary), and `cmd` (canonical
command name registered in the `CommandRegistry`). The macro
captures `file!()` + `line!()` per row via a `#[doc(hidden)]
pub` constructor (`KeymapEntry::__new`) so the row's
declaration site flows through to `:describe-key`'s source
link without any per-row boilerplate.

The table form expands the [`Keymap`] contribution shape with
a second field:

```rust
pub struct Keymap {
    pub bindings: Vec<KeymapBinding>,           // chain form
    pub entries: Vec<&'static KeymapEntry>,     // table form
}
```

Both fields populate the same matcher trie; what differs is
the resolution timing.

**Chain form**: `bindings` already carries a typed
`CommandInvocation`, and `bind_chord` parsed the chord string
at call time. The host pass inserts directly into the trie.

**Table form**: each `entry` carries a `&'static str`
canonical command name (`"multibuffer:excerpt-next"`); the
host pass walks `entries` at registration time, looks up each
name in the `CommandRegistry` to mint a `CommandId`, parses
the chord string into a `Vec<ChordPattern::Literal>`, builds
a `KeymapBinding` carrying the entry's `doc` + `source`, and
inserts it into the trie alongside the chain-form bindings.

The two paths compose freely:

```rust
fn keymap(&self) -> Keymap {
    Keymap::from_entries(multibuffer_keymap_entries())
        .bind_chord(BindingMode::Normal, "<C-r>", self.refresh_cmd.clone())
}
```

The `<C-r>` binding doesn't fit the catalog because
`self.refresh_cmd` is built at mode-construction time (not a
static name), so it routes through the chain form; the rest
of the bindings ride the table.

**Per-entry resolution behaviour** (translation pass
`crates/lattice-host/src/keymap_mode_contributions.rs`):

| Entry shape | Pass behaviour |
|---|---|
| `command = None` | Silent skip. Synthetic catalog row (PushDigit / SetPending / find-char prefix / register prefix / mark-name prefix) — informational for `:describe-key` and `:keymap`, not dispatchable. |
| `command = Some(name)`, registry lookup miss | `tracing::warn!` and skip. Catalog drift — the binding names an invocation no command implements. Matches existing catalog drift-test convention. |
| Chord string fails `parse_chord_sequence` | `tracing::warn!` and skip. Defensive — the `keymap_entry!` macro doesn't validate chord strings at expansion; a typo lands here. |
| Otherwise | Build `KeymapBinding::new(mode, parsed_chords, CommandInvocation::of(cmd_id), entry.source().clone()).with_doc(entry.doc)` and feed into the same grouping pass the chain form uses. |

**When to pick which form:**

| Form | Picks up | Best for |
|---|---|---|
| Chain form (`bind_chord`) | Typed `CommandInvocation` (no registry lookup), no per-binding doc, `#[track_caller]` source capture | 1-5 bindings; dynamically-named commands the mode itself constructs; per-binding parameterisation (counts, args) |
| Table form (`from_entries`) | `&'static str` command name (looked up at registration), per-row `doc` string surfaced by `:describe-key`, macro-captured source per row | 5-20+ bindings against the standard command vocabulary; reads as a binding-table; static catalog for `:keymap` |

The two paths converge at the same `KeymapTrie` at the same
`KeymapLayer::MinorMode(mode.id())`, so they're
indistinguishable from the matcher's point of view.

Sub-arc landed (2026-06-02): K.2.4.A.0.1 substrate move
(commit `4f763d5`); K.2.4.A.0.2 `Keymap::from_entries` +
`KeymapBinding.doc` (commit `6461f56`); K.2.4.A.0.3 host
translation pass entry resolution (commit `81c4600`);
K.2.4.A.0.4 chain + table composability test
(commit `eaf9e33`).

### 11.3 Host translation

Host gains one new pass in the boot path (and on every
dynamic `ModeRegistry::register` after boot):

```rust
for mode in mode_registry.iter() {
	let layer = KeymapLayer::MinorMode(mode.id());
	for binding in mode.keymap().bindings {
		let bound = BoundCommand::from_invocation(
			binding.command,
			binding.source,
			layer,
		);
		host_keymap.trie_for(binding.mode).insert(&binding.chords, Arc::new(bound));
	}
}
```

The existing `KeymapTrie::insert` is the only matcher touch
point; the K.2 work is plumbing *to* it, not changing it.

### 11.4 Effect on §2's five-layer model

No change to the layer model. Mode-contributed bindings still
land at layer 2 (major) / layer 3 (minor). The substrate
moves *who builds them* (host today → owning mode crate
tomorrow); resolution is unchanged.

### 11.5 Migration sequencing

See [slice plan: keymap-substrate](../archive/keymap-substrate.md)
for the K.2.1--K.2.7 carving. The K.2 substrate unblocks the
MO.1--MO.4 cleanup slices (LSP, Oil, Snippet bindings move
out of host) and the `multibuffer_keymap.rs` deletion that
M.6 left in host as known debt.

## 12. Help-prefix bindings (`<C-h>` map)

Lattice ships an emacs-style help prefix on `<C-h>` in
**Normal mode only**. The prefix is **Layer 1 (built-in
defaults)**, not a mode contribution -- help is universal,
not a state-machine transition. Lives in a host-owned helper
module (`crates/lattice-host/src/keymap_help.rs`) per the
slice plan.

### 12.1 Binding table (Normal mode) — landed (K.3, 2026-06-02)

K.3.2 (commit `ed2a1bf`) registers these bindings at
`KeymapLayer::Builtin`, `BindingMode::Normal`, from
`crates/lattice-host/src/keymap_help.rs`. The
`:help-for-help` row is an alias of `:help` added by K.3.1
in the host's ex-command alias table.

| Chord | Command | Notes |
|---|---|---|
| `<C-h> <C-h>` | `:help-for-help` | Explicit form. Two presses of `<C-h>` open the help-for-help index. |
| `<C-h> ?` | `:help-for-help` | Easier-to-type alias of `<C-h><C-h>` — single keypress after the prefix. |
| `<C-h> k` | `:describe-key` | Prompts for chord; shows the bound command + provenance. |
| `<C-h> c` | `:describe-command` | Letter `c` chosen over emacs's `f` (function) -- matches Lattice vocab. |
| `<C-h> o` | `:describe-option` | Letter `o` chosen over emacs's `v` (variable) -- matches Lattice vocab. |
| `<C-h> e` | `:describe-event` | Lattice-native; emacs has no first-class event introspection. |
| `<C-h> m` | `:describe-mode` | Active majors + minors on the current buffer. |
| `<C-h> b` | `:describe-buffer` | Buffer metadata (kind, flags, mode stack, ...). |
| `<C-h> a` | `:apropos` | Cross-cutting search across commands / options / events. |
| `<C-h> K` | `:keymap` | Keymap listing for the current state. Capital K because lowercase `k` is `:describe-key`. |

**No bare `<C-h>` leaf binding** — see §12.3 for the rationale.

### 12.2 Out-of-scope binding modes

- **Insert mode**: `<C-h>` retains vim's backspace
  semantics. No help-prefix in Insert.
- **Cmdline** (the rich minibuffer per design.md §5.9.10):
  retains its existing `<C-h>` (cmdline-level backspace).
  No help-prefix at the `:` line.
- **Visual / OperatorPending**: no help-prefix. The two
  binding modes already carry vim's most chord-dense
  grammars; keeping the help-prefix Normal-only avoids
  surprise rebindings and chord collisions in pending state.
  Users who need help mid-visual escape to Normal first --
  vim's mental model.

### 12.3 No bare `<C-h>` leaf — Option 2 from K.3.0 (2026-06-02)

The original §12.3 envisioned `<C-h>` as both a fireable
leaf (`:help-for-help`) and a prefix to deeper bindings,
resolved via `timeoutlen` — same machinery vim uses for
general chord ambiguity. The K.3.0 trie audit landed a
different decision.

**K.3.0 finding.** Today's
[`crates/lattice-host/src/keymap_trie.rs`](https://github.com/dhruvasagar/lattice)
`lookup()` at lines 251-263 returns `Bound` immediately when
a node has a binding, even if it also has children. The
dispatcher has no `timeoutlen` machinery anywhere — partial-
chord state is handled by an `AbsorbPartialChord(c)` Action
that absorbs the chord and waits for the next, but only when
the trie returns `Partial` (no leaf binding at this node).
There's no mid-state "Bound-but-also-has-children" signal
the dispatcher could time-out on.

**Option 2 decision (user-confirmed, 2026-06-02).** Skip the
bare `<C-h>` leaf binding. `<C-h>` stays a pure prefix node
(returns `Partial`). The slice plan's already-listed
alternatives `<C-h><C-h>` and `<C-h>?` serve as the
explicit `:help-for-help` entry points. One keystroke of
extra friction; zero new dispatcher infrastructure.

The discarded alternatives were:

- **Option 1**: Implement full `timeoutlen` mechanism.
  Adds a `Bound { is_also_prefix: bool }` (or a new
  `BoundOrPartial`) variant + timer-driven dispatch in the
  App. ~150-200 LOC across `keymap_trie.rs`,
  `keymap_registry.rs`, `input.rs`, App state.
  First piece of timer-driven dispatch in the project.
- **Option 3**: Reverse "leaf wins" — return `Partial` when
  a node is both leaf and prefix. Vim-adjacent but not
  vim-exact (no actual timeout; user must press a deeper
  chord OR `<Esc>` to abort to the leaf). Medium effort.

Option 1 is the correct long-term answer when timer-driven
dispatch arrives for other reasons (a future `j10j` count-
absorbing slice, for example). At that point bare `<C-h>`
can land alongside, and this section retires. Until then,
the explicit form is the convention.

### 12.4 Sequencing — landed (2026-06-02)

Full carving in
[slice plan: help-prefix](../archive/help-prefix.md).
K.3 was gated on K.2 landing (it did, commit `0938572` closing
the K.2 arc) and shipped as a thin slice on top: substrate
unchanged, just the new bindings + Option-2 design lock-in +
mode-scope tests + docs.

- K.3.0 ✅ trie audit + Option 2 decision (this section).
- K.3.1 ✅ `:help-for-help` ex-command alias
  (commit `ed2a1bf`).
- K.3.2 ✅ `keymap_help.rs` registers the binding table at
  `KeymapLayer::Builtin`, `BindingMode::Normal`
  (commit `ed2a1bf`).
- K.3.3 ✅ mode-scope enforcement tests (Visual /
  OperatorPending / Cmdline / Search / Replace)
  (commit `05c2e25`).
- K.3.4 ✅ doc artefacts (this slice).

## 13. `lattice-keymap` crate + layer-trace API (T1-T13, K.1.c / K.1.d)

### 13.1 Crate extraction (T1-T9) ✅ landed

All keymap types were extracted from `lattice-host` and
`lattice-mode` into a dedicated `crates/lattice-keymap` crate.
The extraction is a pure mechanical migration — same types, same
behaviour, only module paths changed. Every test remained green
throughout.

**Post-extraction dependency graph:**

```
lattice-protocol
      ↓
lattice-grammar
      ↓
lattice-keymap          ← owns: ModeId, BindingMode, KeymapEntry, keymap_entry!,
      ↓                           Keymap, KeymapBinding, KeymapTrie, KeymapLayer,
lattice-mode            ←         BoundCommand, LookupResult, KeymapRegistry,
      ↓                           KeymapHandle, KeymapCapability, KeymapResolution,
lattice-host            ←         LayerHit, parse_describe_key_arg
```

Types that stayed in `lattice-host` (circular-dep barrier):

| Type | Reason |
|---|---|
| `KeymapReverseLookupHandle` | Implements `lattice-completion::KeymapReverseLookup`; pulling `lattice-completion` into `lattice-keymap` would create a cycle. |
| `keymap_mode_contributions.rs` | Needs both `ModeRegistry` (from `lattice-mode`) and `KeymapHandle` (from `lattice-keymap`); only `lattice-host` can see both. |

`lattice-mode` re-exports all moved types from `lattice-keymap`
for backward compatibility (callers that `use lattice_mode::ModeId`
continue to work).

### 13.2 Layer-trace API (T10-T13, K.1.d) ✅ landed

`:describe-key` uses a **sparse layer-trace** model: instead of a
combined static-catalog + runtime-lookup split, one API call walks
every registered layer and marks each hit active or not.

**Types** (`crates/lattice-keymap/src/resolution.rs`):

```rust
/// One registered layer's binding for the queried chord.
pub struct LayerHit {
    pub layer: KeymapLayer,
    pub command: Arc<BoundCommand>,
    /// `true` if this layer fires for the current buffer.
    /// Always `true` for Builtin / MajorMode / User / Buffer;
    /// set by cross-referencing MinorMode id against active_modes.
    pub active: bool,
}

/// Full trace for one (chord, BindingMode) pair.
/// `hits` ordered lowest → highest priority (Builtin first, Buffer last).
pub struct KeymapResolution {
    pub mode: BindingMode,
    pub hits: Vec<LayerHit>,
}

impl KeymapResolution {
    /// Last active hit (highest-priority active layer). None when
    /// no active layer has a binding.
    pub fn winner(&self) -> Option<&LayerHit> { ... }
    pub fn is_bound(&self) -> bool { self.winner().is_some() }
}
```

**Mode-prefix parser** (also in `resolution.rs`):

```rust
/// Parse a `:describe-key` argument with optional mode prefix.
/// Returns (Some(mode), chord_without_prefix) or (None, original).
///
/// Prefix table: n_ Normal · i_ Insert · v_ Visual ·
///               r_ Replace · c_ Command · s_ Search
pub fn parse_describe_key_arg(s: &str) -> (Option<BindingMode>, &str)
```

**`KeymapHandle` methods** (in `registry.rs`):

```rust
// Full per-layer trace for one mode + chord sequence.
pub fn resolve_trace(
    &self,
    mode: BindingMode,
    chords: &[KeyChord],
    active_modes: &[ModeId],
) -> KeymapResolution

// Run resolve_trace for every BindingMode; omit modes with no hits.
pub fn resolve_trace_all_modes(
    &self,
    chords: &[KeyChord],
    active_modes: &[ModeId],
) -> Vec<KeymapResolution>
```

Both are telemetry paths; they take the `inner` mutex to walk the
full layer list and are never called on the keystroke hot path.

### 13.3 K.1.c — per-buffer minor-mode context filter ✅ landed

The dispatch loop passes the active buffer's `MinorMode` ids into
`lookup_with_context`; only those overlays fire. This is K.1.c:
the per-keystroke minor-mode gate that prevents a minor mode
active on buffer A from intercepting keystrokes while buffer B is
focused.

**Rule**: any context-sensitive keymap query — whether for dispatch
(`lookup_with_context`) or for introspection (`resolve_trace`) —
must read the active-mode set from `App::active_modes[active_buffer_id()]`,
not from any document-specific field. Non-Document panes
(Multibuffer, Terminal, etc.) have their own `ActiveModes` entries
in `App::active_modes` and must not borrow a Document pane's state.

**Two bugs found and fixed** during the T10-T13 implementation
(commit `eb219c5`):

1. **Wrong buffer id in `build_describe_key_content`**
   (`crates/lattice-host/src/dispatch.rs`). The initial
   implementation used `self.document_buffer_id` to build
   `active_modes`. Non-Document panes (Multibuffer, Terminal,
   Help) have a different `active_buffer_id()`; their minor
   modes were silently excluded, making `:describe-key` show
   all minor-mode bindings as inactive when focus was elsewhere.
   Fix: use `self.active_buffer_id()`.

2. **MajorMode treated as mode-gated in `resolve_trace`**
   (`crates/lattice-keymap/src/registry.rs`). The initial
   implementation checked `active_modes.contains(&id)` for
   `KeymapLayer::MajorMode(id)`. But `lookup_with_context`
   places MajorMode bindings in `build_always_on_merged`
   — they fire unconditionally, like Builtin. So `resolve_trace`
   must also mark `MajorMode(_)` as `active = true` regardless
   of `active_modes`, to match hot-path semantics. Fix:
   `MajorMode(_)` joins `Builtin | User | Buffer` in the
   always-active match arm.

**Invariant:** the `active` flag on a `LayerHit` must faithfully
reflect what `lookup_with_context` would return for the same
`(mode, chords, active_modes)` triple. Divergence between the
two paths produces `:describe-key` output that contradicts actual
dispatch behaviour.

## See also

- [slice plan: lattice-keymap crate + layer-trace](../operations/slice-plans/keymap-impl-plan.md) -- T1-T13 sequencing.
- [slice plan: keymap-substrate](../archive/keymap-substrate.md) -- K.2 sequencing.
- [slice plan: help-prefix](../archive/help-prefix.md) -- K.3 sequencing.
- [slice plan: mode-ownership-cleanup](../operations/slice-plans/mode-ownership-cleanup.md) -- MO.1--MO.4, gated on K.2 landing.
- [design.md §5.2.3](design.md) -- canonical spec.
- [design.md §5.2.4](design.md) -- extensibility (matches §5.5
  here).
- [design.md §9](design.md) -- plugin WIT interfaces.
- `crates/lattice-ui-tui/src/keymap.rs` -- the catalog (to be
  promoted to source-of-truth in slice 8.c).
- `crates/lattice-ui-tui/src/input.rs` -- the legacy
  hand-rolled dispatcher (to be replaced incrementally
  through slices 8.d-g).
- `docs/../archive/m3-binding-census.md` -- inventory of every existing
  built-in binding (one-time migration checklist; produced
  during planning).
