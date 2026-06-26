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

## 6. Grammar surface impact

**None.** This is the whole point. The `z*` family, the
`:foldopen` / `:foldclose` ex-commands, the
`foldmethod=` / `foldlevel=` options — all unchanged. The
refactor is purely under-the-hood.

`:set foldmethod=` still parses to `FoldMethod` (the
existing enum); the registry resolves the enum to its
registered primary provider. Adding a new primary in the
future means adding a `FoldMethod` variant *and* a
`FoldProvider` impl; the enum stays the user-facing surface.
Adding an overlay needs no enum or option work — the
subsystem owns lifecycle.

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
- `crates/lattice-ui-tui/src/app/folds.rs` — unchanged;
  reads `self.folds` source-agnostically.
- `docs/dev/architecture/diff-system.md` §6.5 — first
  overlay consumer (hunk folds).
- `docs/dev/architecture/multibuffer-views.md` §6.5 — M.7
  and M.8 overlay consumers (excerpt + file boundary).
