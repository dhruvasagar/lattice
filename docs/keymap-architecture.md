# Keymap Architecture (developer reference)

Authoritative design for lattice's key-input dispatch. The
plan section at the end lists the slices that take us from the
current state (input.rs hand-rolled match table; keymap.rs
documentation-only) to the architecture DESIGN.md §5.2.3 has
spec'd since day one.

## 1. Vision

> *"Keymaps are configuration that bind chord sequences to
> command invocations -- the default vim keymap is itself a
> config file, not hardcoded behavior. Any user or plugin can
> invoke any command, compose commands, or build entirely new
> editing flows."* -- DESIGN.md §3 / §5.2.3

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

## 2. Five-layer model (DESIGN.md §5.2.3)

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

The five layers are physically merged into ONE trie at
layer-stack-mutation time (push minor mode, pop minor mode,
load user config). Runtime lookup walks one structure. Layer
mutations are infrequent (mode transitions, config reload);
keystroke lookups are constant.

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

### 3.4 `KeymapRegistry`

Public type the App holds.

```rust
pub struct KeymapRegistry {
    /// Wait-free read cell. Built once per layer-stack
    /// mutation; lookups walk this tree.
    merged: Arc<ArcSwap<KeymapTrie>>,
    /// One trie per layer. The layer stack is materialised
    /// into `merged` on every push/pop.
    layers: Mutex<LayerStack>,
}

pub struct KeymapHandle {
    inner: Arc<KeymapRegistry>,
}
```

The handle is what App, plugins, and tests see. Mirror of the
`SupervisorSnapshot`/`DiagnosticsSnapshot` pattern from the
audit slices: wait-free reads, mailbox-style writes (or just a
brief lock for layer mutations, since they're infrequent).

Read API:

- `lookup(&self, chord_seq: &[KeyChord], mode: BindingMode) -> LookupResult`

Write API:

- `bind(&self, layer: KeymapLayer, mode: BindingMode, chord_seq: &[KeyChord], cmd: CommandInvocation, source: SourceLocation)`
- `unbind(&self, layer: KeymapLayer, mode: BindingMode, chord_seq: &[KeyChord])`
- `push_layer(&self, layer: MinorModeLayer)`
- `pop_layer(&self, layer_id: LayerId)`

After every write, the merged trie rebuilds (cheap — typically
a few thousand entries; the Arc swap is one atomic store).

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
KeymapHandle::lookup(chord_seq, mode)    (ArcSwap::load + trie walk; ~200 ns)
  ↓
LookupResult
  ├─ Bound(cmd)   → dispatch via grammar's execute(cmd)
  ├─ Partial      → set Pending, wait for next chord
  └─ Unbound      → fall through to literal-text Insert / no-op Normal
```

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
- 8.g.ii -- `g_` and `z_` family (`gg`, `gd`, `gD`, `gI`, `gy`,
  `gr`, `g~`, `gu`, `gU`, `zt`, `zb`, `zz`, `zo`, `zc`, `zR`,
  `zM`, `zd`, ...).
- 8.g.iii -- operator-pending → motion / text-object trie
  expansion (`d{motion}`, `y{motion}`, `c{motion}`,
  `d{i,a}{textobj}`, ...).
- 8.g.iv -- count accumulator (§7.1) lives in input.rs;
  attaches to the resolved invocation.
- 8.g.v -- mark / register / find-char wildcards (§7.3).
- 8.g.vi -- `<C-w>` window-management sub-tree.

Each sub-slice ships independently green; the drift test is
the regression net.

### Slice 8.h -- Plugin / user-config integration

- WIT interface (§5.5).
- Capability gating: `keymap-write` (full),
  `keymap-write:minor-mode`, `keymap-write:plugin-layer`.
- `init.rs` API mirrors the WIT shape.
- Tests: plugin binds chord that fires plugin command; user
  remaps `dd` and the rebinding survives a restart; conflict
  test (two plugins try to bind the same chord); capability
  denial.

### Slice 8.i -- Drift test retirement + close-out

- The `keymap.rs` drift test becomes obsolete (descriptor IS
  behaviour). Replace it with a "every catalog entry resolves
  to a real `CommandInvocation`" test.
- Bench rollup in `BENCHMARKS.md`.
- This file finalised; cross-references added to
  `docs/DESIGN.md §5.2.3` so the spec doc points to the
  authoritative architecture reference.

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

## See also

- [DESIGN.md §5.2.3](DESIGN.md) -- canonical spec.
- [DESIGN.md §5.2.4](DESIGN.md) -- extensibility (matches §5.5
  here).
- [DESIGN.md §9](DESIGN.md) -- plugin WIT interfaces.
- `crates/lattice-ui-tui/src/keymap.rs` -- the catalog (to be
  promoted to source-of-truth in slice 8.c).
- `crates/lattice-ui-tui/src/input.rs` -- the legacy
  hand-rolled dispatcher (to be replaced incrementally
  through slices 8.d-g).
- `docs/m3-binding-census.md` -- inventory of every existing
  built-in binding (one-time migration checklist; produced
  during planning).
