# Fold architecture

Authoritative design for Lattice's fold engine: the provider
abstraction underneath `:set foldmethod=`, how multiple fold
sources compose into one per-buffer fold list, and the
contracts that keep the `z*` grammar surface source-agnostic.

This document is a *companion* to `design.md` (§5.2 modal
engine, §5.9 buffer model), to `diff-system.md` (§6.5 hunk
foldability — first overlay consumer), and to
`multibuffer-views.md` (§6.5 excerpt + file-boundary
foldability — second and third overlay consumers).

## 1. The design goal

Folds in Lattice are **range-based decoration** over a
buffer's line space. The user-facing grammar (`za` / `zo` /
`zc` / `zR` / `zM`, `:set foldmethod=`, `:set foldlevel=N`,
`:foldopen` / `:foldclose`) is intentionally fold-source-
agnostic — it operates on `Fold` entries without caring who
produced them.

That agnosticism is the load-bearing property. It is what
lets the diff subsystem add hunk folds, the multibuffer
subsystem add excerpt and file-boundary folds, and a plugin
add a custom fold source — all without touching the `z*`
arms or growing new ex-commands.

Two saved invariants frame the choices:

> Don't add features beyond what the task requires.
> -- `CLAUDE.md`, design discipline rule

> Buffers must not have kind-specific logic. -- saved
> feedback `feedback_buffers_no_special_case.md`

The first rules out per-source `:hunk-fold` / `:excerpt-fold`
ex-commands. The second rules out branching `recompute_folds`
on `BufferKind`. Both push the same way: one substrate, many
providers, uniform downstream.

## 2. The shape

```
                ┌──────────────────────────────────────┐
                │     FoldRegistry (per Editor)        │
                │                                       │
                │   primaries: HashMap<FoldMethod, P>   │
                │   overlays:  Vec<O>                   │
                └─────────────┬────────────────────────┘
                              │
                  ┌───────────┴────────────┐
                  │                        │
                  ▼                        ▼
       ┌──────────────────┐     ┌──────────────────┐
       │ Primary provider │     │ Overlay provider │
       │  (one runs)      │     │  (all run)       │
       └──────────────────┘     └──────────────────┘
            │                          │
            │                          │
            ▼                          ▼
       Manual / Indent /          Hunk + Unchanged
       Markdown / Syntax /        (D.3.f / D-fix.5),
       Lsp                        Excerpt (M.7),
                                  FileBoundary (M.8),
                                  plugin overlays
```

`:set foldmethod=` picks one primary. Overlays always
contribute. The registry merges, dedupes, and carries
closed-state across recomputes.

## 3. The data model

### 3.1 `Fold`

Unchanged from today (`lattice-core::Fold`): a tuple of
`(start_line, end_line, closed, identity)`. `identity` is
the cache key that survives recompute — adding a provider
doesn't change the shape of a fold, only who emits it.

### 3.2 `ProviderKind`

```rust
pub enum ProviderKind {
    Primary,  // mutually exclusive; :set foldmethod= picks one
    Overlay,  // always composes
}
```

### 3.3 `ProviderId`

```rust
pub struct ProviderId(pub u64);
```

Stable identifier for a registered provider. Used by the
registry for lookup, and by `Fold::identity` so the
recompute can attribute a fold back to its provider when
diagnostics need it. Two distinct providers must produce
distinct `ProviderId`s; a single provider produces the same
id across recomputes.

### 3.4 `FoldProvider` trait

Lives in `lattice-host` (not `lattice-core`) because
providers typically need host-side state (`SyntaxSnapshot`,
LSP fold cache, `HunkIndex`). `lattice-core` stays free of
host dependencies; the trait sits a layer up.

```rust
pub struct FoldContext<'a> {
    pub buffer: &'a Buffer,
    pub buffer_id: BufferId,
    pub path: Option<&'a Path>,
    pub syntax: Option<&'a SyntaxSnapshot>,
    pub lsp_folds: Option<&'a [Fold]>,
    pub diff_hunks: Option<&'a HunkIndex>,
}

pub trait FoldProvider: Send + Sync {
    fn id(&self) -> ProviderId;
    fn kind(&self) -> ProviderKind;
    fn compute(&self, ctx: &FoldContext<'_>) -> Vec<Fold>;
}
```

Providers are pure functions of the context — no
back-references to `Editor`. The dispatcher (the registry
itself) pre-loads what each provider needs, then drives the
compute. This keeps providers cheap to test in isolation
and keeps the threading story simple (no `&mut Editor`
inside compute paths).

### 3.5 `FoldRegistry`

```rust
pub struct FoldRegistry {
    primaries: HashMap<FoldMethod, Arc<dyn FoldProvider>>,
    overlays: Vec<Arc<dyn FoldProvider>>,
}
```

Owned by `Editor`. Constructed at editor boot with the five
built-in `Primary` providers (one per `FoldMethod` variant)
already registered. Overlay providers are added/removed as
their subsystems open/close — `HunkFoldProvider` registers
when `DiffSubsystem::open_session` runs, deregisters when
`drop_session` runs.

## 4. Recompute algorithm

`Editor::recompute_folds()` (called on document edit, after
diff publish, after LSP fold cache update, on
`:set foldmethod=` change):

1. Build `FoldContext` by gathering inputs that any
   registered provider might need: the document snapshot,
   the path, the syntax snapshot (if any), the LSP fold
   cache (if any), the diff hunks (if a session is open).
2. Look up the primary provider keyed on `self.foldmethod`.
   Run it. (`Manual` returns `vec![]`.)
3. Iterate `overlays` in registration order. Run each.
4. Concatenate the results into one `Vec<Fold>`.
5. Carry over closed-state: for each new fold whose
   `identity` matches a fold in the previous `self.folds`,
   adopt the previous `closed` flag. Falls back to
   `(start_line, end_line)` match when `identity` is
   `None`.
6. Carry over user `zf` folds (identity = `None`) from the
   previous `self.folds` verbatim — primary and overlay
   providers don't emit them.
7. Sort: ascending `start_line`, then descending `end_line`
   (existing convention so larger enclosing folds sort
   before their children when start lines match).
8. Store as `self.folds`.

The merge is one pass — O(P + O + F) where P is the primary
result size, O is the union of overlay result sizes, F is
the previous `self.folds` size. At expected scales
(P ≤ 200, O ≤ 100, F ≤ 300) the recompute fits inside the
one-frame keystroke ceiling (8.3 ms at 120 Hz); bench gate enforces this.

## 5. Composition rules

Mostly the rules vim already uses, generalised:

- **Smallest enclosing fold wins on `za`.** When two folds
  cover row R, repeated `za` walks innermost-to-outermost.
  Today's selection helpers (`innermost_fold_idx`,
  `fold_to_close_at`, `outermost_fold_idx` in
  `app/folds.rs`) already implement this — they read
  `self.folds` without caring about source.
- **`zR` opens every fold; `zM` closes every fold.** No
  source distinction.
- **`:set foldlevel=N` honours nesting depth.** The depth
  computation walks `self.folds` and counts enclosing
  folds; source-agnostic.
- **Identity collisions resolve last-write-wins on
  closed-state.** When two providers emit folds with the
  same `(start_line, end_line)` (rare — would require a
  syntactic node coinciding exactly with a hunk), the
  later-registered provider's `closed` flag wins. Identity
  hashes are namespaced per provider so true identity
  collisions across providers don't occur in practice.

### 5.1 Fold-aware viewport scrolling

The host scroll model keys `scroll` on a **source** line and
measures the visible window in **display** rows
(`Editor::bottom_anchored_scroll`). Soft-wrap and virtual rows
(excerpt headers, HUDs) only ever *add* display rows to a source
line, so the historical `1`-per-source-line cost held with an
upward `+segment_count` term bolted on. A closed fold is the one
construct that goes the other way: it *removes* rows — the whole
body collapses onto the single visible head row.

So the bottom-anchor walk skips a closed fold's body entirely. It
hops from a hidden line straight up to the fold head via
`FoldIndex::enclosing_closed_fold`, counting the head as one row
and the body as zero. Counting collapsed lines as one row each
burns the row budget on invisible content and scrolls a buffer
whose unfolded text already fits — `j` over a deeply-folded
document jumping the viewport for no reason. The walk stays
`O(budget · log folds)` regardless of how many lines a fold hides;
`nofoldenable` / no closed folds degrade it to the historical
per-line walk byte-for-byte.

### 5.2 Gutter marker rendering

Both renderers draw a fold marker on **every foldable head row**
while `foldenable` is on — `▾` on an open (expanded) head, `▸` on a
closed (collapsed) one — so the affordance is visible before you
fold, not only after. The host answers the per-row question with
`FoldIndex::fold_start_kind_at(line) -> Option<FoldMarker>`
(`Open` / `Closed` / `None`); each renderer maps that to its glyph.
The TUI's `fold_glyph_for` and the GPUI gutter-meta build call the
same predicate, so the two peers never disagree on which rows carry
a marker.

**Placement.** The marker sits **between the line number and the
code** (`… 99 ▸ code`), not in a leading fold column — a separator
space before the glyph and a trailing gap after it. Both gutters
reserve that three-cell trailing slot unconditionally (even with
`nonumber`), so toggling line numbers or folding a region never
shifts the body column.

**Colour is themed, muted by convention.** The glyphs resolve
through two theme elements — `gutter.fold.open` (default `overlay`,
the dim gutter tone) and `gutter.fold.closed` (default `subtext`,
the line-number tone, a step brighter so a collapsed fold reads as
"content hidden here"). This follows the cross-editor convention:
VS Code, Zed, JetBrains, Sublime, and Neovim all render fold
controls as a low-emphasis gray, never an accent. A theme that
leaves the elements unset falls back to the historical dim gutter
colour, so the marker never vanishes. The `⋯ N lines` collapsed
summary (TUI today; GPUI parity pending) carries the rest of the
"hidden content" signal via text rather than a loud glyph colour.

## 6. Grammar surface impact

**None — from the provider refactor.** That is the whole point. The
`z*` family, the `:foldopen` / `:foldclose` ex-commands, the
`foldmethod=` / `foldlevel=` options — all unchanged. The
refactor is purely under-the-hood.

`:set foldmethod=` still parses to `FoldMethod` (the
existing enum); the registry resolves the enum to its
registered primary provider. Adding a new primary in the
future means adding a `FoldMethod` variant *and* a
`FoldProvider` impl; the enum stays the user-facing surface.
Adding an overlay needs no enum or option work — the
subsystem owns lifecycle.

(The one *later* feature that did add `z`-grammar — org-mode
cycling — is §6.1. It adds keys/commands but no new provider,
method, option, or `Fold` field.)

### 6.1 Org-mode visibility cycling

`za` toggles a fold between two states (open / closed). Org-mode
**cycling** walks a heading — or the whole buffer — through *three*
progressive states with one key. It is an **operation over the fold
list**, not a new provider or `FoldMethod`: it lives host-side beside
`do_set_fold_state_at_cursor` (`za`) / `do_set_all_folds` (`zR`/`zM`)
as `Editor::do_cycle_fold_at_cursor` / `do_cycle_folds_global`, wired
through the same `AppEffect → Action → Editor::do_*` path. Universal
`z*` grammar (Builtin layer), so no mode owns it.

**No new `Fold` state — hierarchy by containment, state by inference.**
This is the load-bearing design choice:

- `Fold` is unchanged (`{start_line, end_line, closed, identity}`). There
  is **no depth field**. Parent/child is derived on demand by *range
  containment* — `A` is `B`'s ancestor iff `A.start ≤ B.start && B.end ≤
  A.end` — the same derivation `:set foldlevel=N` already does. Direct
  children of a fold are its descendants not contained by any *other*
  descendant; a *leaf* is a fold containing no other fold.
- The cycle **state is inferred from the current `closed` flags**, not
  stored. The local cycle reads the root + its descendants; the global
  cycle reads the whole list. Statelessness is the win: it needs no
  reconciliation across recompute (the providers re-emit folds every
  edit; a stored cycle-state would fight the `identity` carry-over), and
  it composes cleanly with `za`/`zo`/`zR` — a manual tweak just changes
  what the next cycle press infers, exactly like emacs.

**The states** (each is a `closed`-flag pattern the operation *writes*):

- **Local** (`z<Space>` / `:fold-cycle`), on the innermost fold under the
  cursor (the "root"): **FOLDED** (root closed) → **CHILDREN** (root
  open, every descendant closed) → **SUBTREE** (root + descendants open).
  A leaf root degenerates to FOLDED ↔ open (a toggle), matching org.
- **Global** (`z<Tab>` / `:fold-cycle-global`), over every fold:
  **OVERVIEW** (all closed) → **CONTENTS** (each fold closed *iff* it is a
  leaf — structural headings open, leaf bodies folded) → **SHOW-ALL** (all
  open). With no nesting (a flat list) OVERVIEW and CONTENTS coincide, so
  the cycle degenerates to OVERVIEW ↔ SHOW-ALL.

`z<Space>` is **contextual**: when the cursor is not inside any fold it
falls back to the global cycle, so one key serves both; `z<Tab>` is the
explicit-global escape hatch (the contextual key does the *local* cycle
while on a heading).

**Outline navigation companion.** `zp` / `:fold-goto-parent`
(`do_goto_parent_fold`) moves the cursor *up* one level — emacs
`outline-up-heading` — using the same containment derivation: the
innermost fold containing the cursor, then the start of the innermost
fold strictly containing *that*. It's a cursor move (no fold-state
change), distinct from `zj`/`zk` (which step to a sibling fold edge), and
wired the same `AppEffect → Action → Editor::do_*` way.

**The one deviation from emacs.** Because the providers emit
*subtree-spanning* folds (a heading's fold covers its whole subtree, not
just its body), CHILDREN / CONTENTS leave the heading's own intro text
(above its first child) visible, where org hides it. Closing that gap
would need the providers to emit a separate *body* fold per heading — a
deliberate non-goal (more folds, more recompute) for a cosmetic
difference.

**Performance.** The cycle is a **one-shot per keypress**, off the render
path (a command, like `za` — not per-frame). Cost is `O(n²)` containment
math over `n` = folds *in the buffer*, which §4 bounds at ≈≤300 even at
extreme scale — microseconds, the same class as the existing
`innermost_fold_idx` / `outermost_fold_idx` scans and the `foldlevel`
depth walk. A fold op already forces a one-shot display-matrix recompute
(fold elision) that dwarfs it. (If `n` ever justified it, the global
`is_leaf` could drop to `O(n log n)` with a sort + stack; premature for
≤300.)

**Rejected alternative — store depth / cycle-state on `Fold`.** It would
make the inference trivial but adds per-fold bookkeeping that must be
recomputed and reconciled with `identity` on every provider re-emit, and
a stored state desyncs from reality the moment the user runs a plain
`za`. Heuristic #1: the genuinely-better long-term design is the
stateless one; the derivation cost is negligible (above).

## 7. Plugin path (forward-looking)

A WIT-registered fold provider falls naturally out of this
shape. The plugin's `FoldProvider` impl is a thin shim that
calls into wasmtime; the registry treats it the same as a
built-in. `kind: Primary` plugin providers register a new
`FoldMethod` variant at boot (the enum gains an `Extension`
arm carrying the plugin-supplied label); `kind: Overlay`
plugin providers register/deregister around their lifecycle
events. Both reuse the merge pipeline.

Plugin host enforcement (fuel limits, crash isolation, etc.)
applies uniformly because `compute()` is a single typed
call. No diff-aware or excerpt-aware plumbing on the plugin
side.

## 8. Open questions

- **Per-overlay enable flag**. Should `:set
  diffopt-=folds` (or similar) let a user disable hunk-fold
  overlay without closing the diff session? Probably yes
  for v1 — the option lives on the diff subsystem, not the
  fold engine. Deferred to D.3.f.1's decision moment.
- **Provider ordering deterministic vs. registration-
  order**. Today's plan uses registration order. If two
  overlays produce identity collisions, that order
  determines who wins. Alphabetical-by-provider-id may be
  more deterministic; revisit if it bites.
- **Async compute**. All providers today are sync because
  the budget fits. LSP fold provider is async-fed (cache
  lookup is sync against an async-populated cache);
  hypothetical plugin providers that need async resolution
  will need a Promise-shaped variant. Deferred until a
  real consumer asks.

## 9. Slice plan

Sequencing lives in
[`docs/dev/operations/slice-plans/fold-architecture.md`](../operations/slice-plans/fold-architecture.md);
authoritative status per slice lives in
[`docs/dev/operations/implementation.md`](../operations/implementation.md).
This fragment owns *what* and *why*; the slice plan owns
*when* and *in what order*.

## 10. Testing strategy

- **Unit tests on the registry**: primary swap reproduces
  today's behaviour exactly (existing fold tests stay
  green); overlay add → recompute → fold appears; overlay
  drop → recompute → fold gone; closed-state survives
  primary swap; closed-state survives overlay re-emit;
  identity-collision last-write-wins resolves predictably.
- **Per-provider tests** (already exist for indent /
  markdown / syntax in `lattice-host::folds`): preserved
  verbatim — the providers wrap today's pure functions.
- **End-to-end** (D.3.f.1): open a file with a hunk, `za`
  on a row inside the hunk collapses it; `zR` reopens;
  `:set foldmethod=indent` then `:set foldmethod=syntax`
  swaps primaries without losing hunk overlay.
- **Bench** (D.3.f.2): `fold_recompute_p99_us` at 100
  hunks overlaid on top of a syntax primary. CI gate
  catches registry-indirection regression.
- **Org-cycle** (§6.1): on a nested parent + children fold set, the
  local cycle walks FOLDED → CHILDREN → SUBTREE → FOLDED via the
  `closed`-flag patterns; a leaf root toggles; the global cycle walks
  OVERVIEW → CONTENTS → SHOW-ALL; off-fold `z<Space>` falls back to the
  global cycle. (Tests in `lattice-ui-tui::app::folds`.)

## 11. Risks

- **Refactor blast radius.** `recompute_folds()` is called
  from many sites (document edit, foldmethod change,
  document open). Each site must still work after the
  refactor. Mitigation: the trait wraps the existing
  `compute_*_folds` functions unchanged; the dispatch
  shape stays the same; tests stay green.
- **Identity-hash collision across providers.** A syntax
  fold and a hunk fold sharing `(start_line, end_line)`
  could collide on identity if both providers hash the same
  inputs. Mitigation: providers namespace their identity
  hashes (`hash(("hunk", start, end))`,
  `hash(("syntax", node_kind, line_text))`); the
  `ProviderId` salt makes accidental collision essentially
  impossible.
- **Overlay registration leaks.** A subsystem that registers
  an overlay but never deregisters leaves stale folds.
  Mitigation: subsystem lifecycle owns registration —
  `DiffSubsystem::drop_session` is the symmetric point.
  Editor close drains the registry.

## 12. Cross-references

- `lattice-core/src/folding.rs` — `Fold`, `FoldMethod`,
  `ProviderKind`, `ProviderId` (data types only).
- `lattice-host/src/fold_provider.rs` (new) —
  `FoldProvider`, `FoldContext`, `FoldRegistry`.
- `lattice-host/src/folds.rs` — primary provider impls
  wrapping `compute_indent_folds` /
  `compute_markdown_folds` / `compute_syntax_folds`.
- `lattice-host/src/dispatch.rs` — `Editor::recompute_folds`
  drives the registry; LSP fold cache plumbing stays put.
  `do_cycle_fold_at_cursor` / `do_cycle_folds_global` (§6.1)
  live here beside the `za` / `zR` handlers.
- `crates/lattice-ui-tui/src/app/folds.rs` — unchanged for the
  provider model (reads `self.folds` source-agnostically); also
  holds the org-cycle tests (§6.1).
- `docs/user/folding.md` — the user-facing folding guide,
  including the "Org-style cycling" section (§6.1's surface).
- `docs/dev/architecture/diff-system.md` §6.5 — first
  overlay consumer (hunk folds).
- `docs/dev/architecture/multibuffer-views.md` §6.5 — M.7
  and M.8 overlay consumers (excerpt + file boundary).
