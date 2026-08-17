# Tree-sitter context — sticky scope headers

Status: design fragment (2026-08-16). Slice plan:
[`../operations/slice-plans/treesitter-context.md`](../operations/slice-plans/treesitter-context.md).

Anchors: [`plugin-treesitter-seam.md`](plugin-treesitter-seam.md) (the ✅ built
structural query surface this consumes), [`plugin-host.md`](plugin-host.md) §5
(the seam spine — the `decorations` producer is the pattern the new `context`
seam copies), [`headerline.md`](headerline.md) (the sticky row this stacks
*under*, never over), [`virtual-rows.md`](virtual-rows.md) (the pinned-row
primitive), [`indent-guides.md`](indent-guides.md) (the worker-resolved
per-pane layer this mirrors), [`theme-system.md`](theme-system.md) §"deferred"
(the WIT element-registration item this closes).

The scopes enclosing the cursor, pinned above the text, so that the `impl` and
the `fn` you are inside stay readable after their headers have scrolled away.
The plugin equivalent everyone arrives with is `nvim-treesitter-context`;
VSCode and Zed ship the same idea as *sticky scroll*.

## Why this exists

Reading code in a long function means holding two facts the viewport has
thrown away: what this block is, and what encloses it. Every reference editor
solves it the same way — pin the enclosing headers to the top of the pane —
and the muscle memory is visual, which puts the *behaviour* under the
UX-follows-convention rule. There is nothing to arbitrate about what the user
sees.

What is not settled by convention is everything else: where the structural
query runs, what crosses the WASM boundary, how the rows keep their syntax
colour, how they compose with a headerline that is already showing something,
and which pane's cursor they answer to. That is the whole of this design.

This ships as a **core bundled plugin**, not native. The tree-sitter seam
(TS.1–TS.3) exists precisely so this class of feature lives outside the host,
and paramount goal #2 is not served by keeping the second structural consumer
native because it was convenient.

## What the user sees

```
┌────────────────────────────────────────────┐
│ ⟳ rust-analyzer: indexing 412/1200         │  ← headerline (unchanged)
│ impl Renderer for TuiRenderer {            │  ← context, outermost
│     fn paint(&mut self, frame: &Frame) {   │  ← context, innermost
├────────────────────────────────────────────┤
│         for row in frame.rows() {          │
│             self.blit(row);                │  ← cursor
│         }                                  │
│     }                                      │
│ }                                          │
└────────────────────────────────────────────┘
```

Three properties are load-bearing and each is argued for below:

1. The headerline keeps the first row. Context starts underneath it, and only
   reaches the top of the pane when there is no headerline to displace.
2. The context rows carry the *same* syntax colour as the source lines they
   mirror — not a re-derivation that can disagree.
3. Outermost scope first, innermost nearest the text. The row adjacent to the
   viewport is the scope the viewport is inside, which is what makes the block
   read as a continuation of the code rather than a list.

## The core concept: a scope

```rust
pub struct ContextScope {
    pub scope_start: u32,
    pub scope_end: u32,
    pub header_start: u32,
    pub header_end: u32,
}
```

A structural range plus the line span that names it. `header_start ..=
header_end` is normally one line; it spans several when a signature wraps.

The property the whole design rests on: **a scope is a pure function of the
parse tree.** No viewport, no cursor, no fold state, no user option. That is
what makes it correct to compute once per parse, cache by parse version, and
resolve against any anchor line afterwards — and it is what a resolved *row
list* is not, since a row list is a function of the cursor and thrashes at
keystroke rate.

### Why scopes are not folds

The overlap is real — `FoldSource` (`lattice-core/src/folding.rs`) is an
exercised native trait, and a scope is nearly a `Fold` with a header span. They
are kept separate deliberately:

- A context set wants `if` / `else` / `match` arms that nobody wants as folds.
- A `Fold` carries user state (`closed`) and an identity for surviving
  recompute; a scope carries neither and never should.
- Fold providers are `foldmethod`-selected *primary* providers, one at a time.
  Context is always-on and additive.

Collapsing them means one concept carries a field the other never uses.
Generalising later from a second real consumer is the cheaper direction than
un-generalising a seam that guessed wrong — and `plugin-host.md` §5's
exercised-trait-first rule says a WIT seam mirrors a native trait that exists,
in the shape it exists.

## Where the work happens

Three layers, and the split is the design.

### The plugin owns the semantics

`plugins/treesitter-context/` — a `wasm32-wasip2` component in its own
workspace, the `auto-pair` shape. It ships per-language `context.scm` queries,
compiles them through the ✅ `tree-sitter` seam, and returns scopes.

The queries run **host-side** (that is what the seam is), so the buffer text
never crosses the boundary. Only `list<context-scope>` does — four `u32`s per
scope.

### The host owns the resolution

`resolve_context(scopes, anchor, opts) -> Vec<u32>` lives in `lattice-cells`,
called by the host on the actor thread when it publishes pane inputs.

It is a **linear scan** over the scope list plus a sort of the (small)
enclosing subset — `O(n + d log d)`, not the `O(log n + depth)` this document
claimed before TC.1 was built. "Which intervals contain line L" is not a binary
search: scopes nest, but the siblings before `L` still have to be rejected one
by one, so pruning needs an augmented interval structure rather than an ordering
trick.

Measured (TC.1 bench, `context_resolve`): **204 ns** at 100 scopes / depth 5,
**2.66 µs** at 5k / depth 20, **21.8 µs** at 50k / depth 20 — ~0.44 ns per
scope. The pathological end is 0.26% of a 120 Hz frame; a 3k-line source file
sits nearer 1 µs. The linear shape is therefore the right trade today, and the
bench is the ratchet that says when it stops being: if a real file ever makes
this hurt, the fix is an augmented interval tree, not a reordering.

The return type is `Vec<u32>` rather than a `SmallVec` because `lattice-cells`
is deliberately near-dep-free (its manifest documents the single exception),
and a dependency is not worth saving one allocation of at most `max_lines`
elements.

The host puts the resulting line list into `PaneCellsInputs`. Two things follow
that are worth stating as guarantees rather than intentions:

- **The reservation cannot disagree with the paint.** The scroll model reserves
  `sticky_context_lines.len()` rows because it *produced* that list. There is
  no second resolution to drift from the first. (The gutter-width mismatch
  between host and renderer is exactly this bug class; here it is designed out
  rather than tested for.)
- **No WASM runs on the scroll or keystroke path.** The guest is called once
  per parse, off-thread, debounced.

### The cells worker owns the pixels

The worker builds the context rows in the same pass that builds the pane's
`DisplayMatrix` and `IndentGuides`, from the same snapshot, stamped with the
same `MatrixVersion` — the IG.2 pattern, and for the same reason: two
projections of one build cannot disagree, so the layer needs no staleness axis
of its own.

Building them there rather than in the renderers is not a preference. The
`CellMatrix` is chunked above `4 × viewport_height` lines
(`CHUNK_SIZE_WHOLE_DOC`, `lattice-cells/src/matrix.rs`), so a header three
thousand lines above the viewport is routinely **not resident**. A renderer
resolving per frame from the published matrix would find nothing to copy and
would have to fall back to unhighlighted text — a visible colour flicker on
scroll, which the UX contract vetoes. The worker holds the rope, the syntax
snapshot and the cell-builder, so it can build a row for any line whether or
not a chunk covers it.

That is also what makes property (2) above true *by construction*: a context
row is built by the same cell-builder as a document row. Highlighting is not
preserved by re-deriving it correctly; it is preserved because there is only
one derivation.

## The pipeline

1. `lattice-syntax` publishes a new `SyntaxSnapshot` for the buffer.
2. The context producer task (`lattice-plugin-host/src/context_task.rs`) wakes,
   debounces, and calls the guest's `context-scopes` export off-thread.
   In-flight work for a superseded parse is cancelled (`cancellation.md`).
3. The guest runs `run-query` against the snapshot with its per-language
   `context.scm` and returns `list<context-scope>`.
4. The host caches `Arc<ContextScopes>` per buffer, stamped with the parse
   version, publishes via `ArcSwap`, and wakes the actor through
   `SubsystemBoot::inbound` — **not** a bare `TickCallback`, so the result
   reaches the screen without waiting for a keypress.
5. On each pane-inputs publish (cursor move, scroll, edit, option change) the
   host runs `resolve_context` per pane and writes `sticky_context_lines` into
   that pane's `PaneCellsInputs`. The scroll model reserves `len()` rows on top
   of the existing `sticky_count`.
6. The cells worker builds one row per line in that list and publishes the
   pane's `StickyContext` layer.
7. Both renderers paint: matrix sticky rows first, context rows second, scroll
   window third.

Step 5 runs at keystroke rate and is the only part of this on the hot path. It
is a binary search over a sorted array and a walk bounded by `max-lines`
(default 3). Step 6 is skipped entirely when the resolved line list is
unchanged, which is the common case for a cursor moving within one scope.

## Stacking with the headerline

The sticky strip, top to bottom:

1. `VirtualRowMatrix::sticky_rows()` — the existing headerline. **Never
   overwritten, never reordered, never dropped.**
2. The `StickyContext` rows — outermost scope first.

When the headerline provider returns `None`, (1) is empty and context begins at
the top of the pane. That is the whole rule: appending after an existing
producer rather than competing for row 0. No `sticky_rank` field, no ordering
negotiation between providers, no kind-branch in the renderer.

**Budget guard.** The strip may occupy at most
`context.max-viewport-fraction` of the pane height (default 33%), and the
headerline's rows come out of that same share — context stacks under it and
never displaces it, so `ContextOptions::reserved_rows` carries what the
headerline already holds. Over budget, context rows are dropped from the
**outermost inward** — the innermost scope is the one you are actually in, so
it is the last to go. Headerline rows are never dropped; a pane too short for
even one context row simply shows none (a 3-row pane at 33% yields zero, which
is the honest answer).
This is the same intent as `nvim-treesitter-context`'s `min_window_height`,
expressed as a fraction so it scales with the split rather than assuming one.

## Per-pane correctness

`PaneCellsInputs` output cells are keyed by `buffer_id` today —
`Editor::indent_guides_for(buffer_id)`, `Editor::display_matrix_for(buffer_id)`
— so two panes showing one buffer share a layer and the second publish wins.
`IndentGuides` sidesteps this by publishing block *extents* and letting each
renderer pick the active block per frame from its own cursor row.

Context cannot sidestep it: the rows themselves differ per pane, not just which
one is emphasised. So the sticky-context output cell is keyed by **`pane_id`**
— a new `Editor::sticky_context_for(pane_id)` map, contained to this layer, and
leaving the existing buffer-keyed cells alone.

The test that matters: one buffer open in two splits with cursors in different
scopes shows different context in each. It is the only test that can fail if
the keying regresses to `buffer_id`, and it should be written first.

## The resolver, precisely

Given the anchor line `A` and the pane's first visible line `T`:

1. Collect every scope with `scope_start <= A <= scope_end` — the scopes the
   anchor is inside. A scope encloses its own header line, which is what makes
   `[u` terminate (see below).
2. Keep only those with `header_end < T` — the ones that actually scrolled
   away. A header still on screen is not pinned; duplicating a line the user
   can already read spends a row, and on a short pane that is the row the
   innermost scope needed.
3. Sort by `scope_start` ascending (outermost first). Nesting makes this a
   total order in practice; ties break on `scope_end` descending.
4. Expand each scope to its header span, capped at
   `context.multiline-threshold` lines (default 1). A three-line signature with
   the threshold at 1 shows its first line only.
5. Truncate to **rows**, not scopes — a multi-line header consumes its rows
   from the same budget. The budget is the tighter of `context.max-lines`
   (default 3) and the viewport-fraction guard, minus the rows the headerline
   already holds. `context.trim-scope` picks the end: `outer` (default) drops
   outermost, `inner` drops innermost. A group that does not fit **stops** the
   walk rather than being skipped: keeping a further-out scope after dropping a
   nearer one renders a stack with a hole in it, which reads as simply wrong.

> **Corrected during TC.1.** This section previously folded steps 1 and 2 into
> one predicate, `header_end < A`, glossed as "the enclosing scopes whose header
> has scrolled past". Those are different things, and the gloss was the correct
> one: with the cursor at line 30, an `impl` header at line 10 and the view
> starting at line 5, `header_end < A` holds while the header is plainly on
> screen. The resolver needs the viewport top, so `ContextOptions` carries
> `viewport_top` and the two conditions are separate steps.

## Anchor: cursor, and why not topline

`context.anchor` takes `cursor` (default) or `topline`.

`nvim-treesitter-context` defaults to the cursor; VSCode and Zed's sticky
scroll are topline-based. They genuinely disagree, so convention does not
settle it, and the differentiator is what the feature answers. Sticky scroll
answers *what am I looking at*; context answers *where am I*. Lattice defaults
to `cursor` because the second question is the one a modal editor's user is
asking — the cursor is where the next operator lands.

There is also a structural reason to prefer it, and it is the sharper argument:

> Reserved sticky rows shrink the visible window. With a **topline** anchor
> that closes a loop — reserve a row, the window shrinks, the topline moves,
> the resolved scope set changes, reserve differently. With a **cursor**
> anchor there is no loop: the cursor line is set by motions, never by
> reservation.

`anchor = topline` therefore resolves against the topline *as of before* this
frame's reservation, breaking the cycle at the cost of one frame of lag when
the context height changes. That is a documented, tested property of the
non-default mode, not an accident.

## The `context` WIT seam

A new `wit/context.wit`, mirroring the `decorations` producer shape exactly —
an **async producer whose result the host caches**, never a synchronous call on
a render or scroll path.

```wit
interface context {
    use types.{context-request, context-scope};

    /// Produce the structural context scopes for a buffer. Async — the call
    /// suspends the guest, never the render path. An `err` is logged and the
    /// buffer keeps its previously cached scopes (graceful degradation: a
    /// failed refresh must not blank the strip).
    context-scopes: func(req: context-request) -> result<list<context-scope>, string>;
}

world context-plugin {
    import host-services;
    import tree-sitter;
    import logging;
    export context;
}
```

`context-request` carries the owned projection the §4.2 rule requires — buffer
id, path, line count, language id, parse version. Bulk text does not ride the
request; the guest reaches structure through the `tree-sitter` seam.

Host side, four files mirroring the decoration quartet:
`context_source.rs` (the native `ContextSource` trait the wrapper implements),
`context_task.rs` (the debounced off-thread driver), `context_host.rs` (the
registry insert), `boundary_context.rs` (the round-trip type mirror).

Gated on the existing **`tree-sitter`** editor capability. No new capability:
the seam grants no reach the guest does not already have — it is a place to
*return* structure, not a new source of it.

## The `theme` WIT seam

`theme-system.md` lists WIT element registration as deferred, with the note
that the host had to land first. It has. This plugin is its forcing function,
the way lighthouse is host-services'.

```wit
interface theme {
    use types.{style-spec};

    /// Register an element and its default style. Called at load; the host
    /// inserts into the same registry builtins live in, under
    /// `SourceLayer::Plugin(id)` so unload reverses it.
    register-element: func(name: string, doc: string, default: style-spec) -> result<_, string>;
}
```

The alternative considered and rejected was for the host to register
`context.*` as builtins and have the plugin name role strings — the existing
`ui-segment.role` precedent. It ships faster and it is wrong for the same
reason the mode-ownership rule exists: it leaves half the surface (the element
names, their docs, their defaults) with the host, so the plugin cannot be
uninstalled without leaving debris in `:customize`, and the next plugin that
wants a themed surface needs a host edit. That is the half-migration shape the
standing rule names.

A plugin-registered element is otherwise indistinguishable from a builtin:
themes override it, `:customize` edits it, `:describe-*` documents it.

## Configuration

Registered by the plugin through the ✅ `config` seam, so `:set`,
`:describe-option`, `:customize` and completion treat them like core options.

| Option | Type | Default | Meaning |
|---|---|---|---|
| `context.enabled` | bool | `true` | Master switch; buffer-local override allowed |
| `context.anchor` | enum | `cursor` | `cursor` \| `topline` |
| `context.max-lines` | u32 | `3` | Maximum context **rows** |
| `context.trim-scope` | enum | `outer` | Which end to drop when over budget |
| `context.multiline-threshold` | u32 | `1` | Max rows one scope's header may use |
| `context.max-viewport-fraction` | u32 | `33` | Percent of pane height the strip may occupy |
| `context.separator` | string | `""` | Rule glyph below the block; empty = off |
| `context.line-numbers` | bool | `true` | Show source line numbers in the context gutter |
| `context.disabled-languages` | list | `[]` | Language ids to skip |
| `context.max-file-lines` | u32 | `100000` | Skip the query above this; feature silently off |

`context.separator` defaults to empty rather than a glyph on purpose: any
non-empty default would have to satisfy the Nerd-Fonts-degrade rule, and a
separator is a preference rather than an affordance. A user who sets one gets
the BMP-safe treatment (`─`, Box Drawing, present in every terminal font).

`context.max-file-lines` exists because a whole-buffer query is `O(file)`. On a
generated or minified file past the cap the feature turns itself off and says
so at `debug` — a missing strip, never a stall.

## Theme elements

| Element | Styles |
|---|---|
| `context.background` | The strip's backdrop |
| `context.separator` | The rule, when `context.separator` is set |
| `context.line-number` | Line numbers in the context gutter |
| `context.active` | The innermost row, for optional emphasis |

Deliberately four, and none of them a foreground for code. Text colour comes
from the source lines' own highlighting; these compose the backdrop and the
gutter around it. A theme that wanted to recolour the code in the strip would
be overriding syntax highlighting from a place nobody would think to look.

## Grammar surface

**`treesitter-context-mode`** — a **minor** mode with an `ActivationPolicy`
spanning every major that has a tree-sitter grammar.

> **It declares NO required capabilities, and cannot.** Declaring
> `TREE_SITTER` made the mode fail to activate on every buffer, Rust files
> included. The mode-capability gate is half-built editor-wide: the enforcement
> side exists (`ModeRegistry` rejects on `required - buffer_caps`), but nothing
> ever POPULATES a buffer's capability set — every activation site in
> `lattice-host` passes `CapabilitySet::empty()`, and no native mode had
> declared a requirement before, so the gate had never been exercised. Any mode
> declaring any capability today is unsatisfiable.
>
> It would be the wrong requirement even if the buffer half were built: a buffer
> gains its tree when the first parse lands, so gating on it would make `[u`
> unavailable until then and require re-activation afterwards — activation churn
> for a chord that already no-ops gracefully with no tree. The capability that
> genuinely matters is the manifest's `editor_capabilities = ["tree-sitter"]`,
> which gates the tree handle itself. Not a chord copied into
each major: the shared-behaviour-is-a-minor-mode rule exists because a copied
set silently develops gaps (`magit-diff-mode` got `s` and `u` but not `x`, and
nothing announced it).

Its keymap lives at `KeymapLayer::MinorMode(treesitter-context-mode)`. Never
`Builtin` — `Builtin` is universal vim grammar and fires in every buffer,
including ones with no tree.

### `[u` — jump up the context stack

`nvim-treesitter-context` suggests `[c`, which is unavailable here twice over:
TSM.4 owns `[c` / `[C` (previous class start / end) and vim convention gives
`[c` to diff hunks, which `lattice-diff` already holds. The convention slot is
occupied, so the choice falls back to fitting the existing `[`-prefix
structural family. `[u` — "up" — is free in both vim and lattice.

The handler body lives in the plugin. Given the cursor line `A`, it targets the
header of the innermost scope whose `header_end < A` — the same predicate the
resolver uses in step 1. A count prefix walks `N` levels.

**Why repeated presses walk outward without a forward command.** Landing on
scope `S`'s header puts the cursor *inside* `S`, but `S`'s own header is no
longer strictly above the cursor, so the next `[u` skips it and finds `S`'s
parent. The stack pops by construction. That is why there is no `]u`: the
inverse of walking up is `<C-o>`.

`<C-o>` works because the jump pushes
`push_position_history(cursor, PositionSource::PluginPush)` before moving — the
same path other plugin-initiated jumps already take. Nothing about `<C-o>`'s
existing behaviour changes; the context jump is just another entry in the ring.

### Ex-commands

`:context-toggle` only — dashed and namespaced per the naming rule, with no one-
or two-letter short (those slots are scarce and reserved for vim-canonical
commands).

> **`:context-up` was designed and then dropped (TC.6).** The plan was for it to
> share the chord's handler so the two could not drift. The seam cannot deliver
> that: `apply-ex-command` receives no `borrow<tree-snapshot>` — only
> `apply-action` does — so an ex-command cannot compute a jump target, and no
> `Effect` re-dispatches a command in a way that would borrow the action's tree
> (`invoke-command` is a picker routing payload, not an effect). A `:context-up`
> that silently did nothing would be worse than an absent one, so the chord is
> the jump's only surface.
>
> `:context-toggle` survives because it needs no structure — it reads and writes
> an option. If a second consumer ever wants structure from an ex-command, the
> fix is a tree parameter on `apply-ex-command`: a seam change worth making for
> two consumers and not for one.

## Language coverage

Queries ship in the plugin as `queries/<language>/context.scm`, embedded with
`include_str!` so the component is self-contained. A capture named `@context`
marks a node whose header should be pinned; an optional `@context.end` narrows
the header span when the node's first line is not the whole of it.

First wave: Rust, Python, Go, JavaScript, TypeScript, C, Markdown. A language
with no query contributes nothing and logs at `debug` — most languages will not
have one at first, and that is a normal state rather than a defect worth
warning about.

## Error handling

- **Query compile failure** — `warn` once per (plugin, language), that language
  contributes nothing. Compiling once and caching the result means a broken
  query costs one log line, not one per parse.
- **Guest trap** — the existing dead-until-reinstantiation discipline. The
  cached scopes stay; the strip keeps its last good value.
- **Refresh failure (`err` return)** — cached scopes retained, `debug`. Blanking
  the strip on a failed refresh would make a transient error look like the
  feature breaking.
- **Stale scopes vs. current parse** — keep painting. Eventual consistency is
  permitted for structure the user did not edit; a blank frame followed by a
  repopulated one is not.
- **File over `max-file-lines`** — skip, `debug`.
- **Missing query** — no context, `debug`.

The through-line: every failure mode degrades to *fewer rows*, never to wrong
rows and never to a flicker.

## Paramount-goal alignment

- **#1 Performance.** Zero WASM on the keystroke or scroll path. The guest runs
  once per parse, off-thread, debounced, cancellable. The per-keystroke cost is
  one binary search and a walk bounded by `max-lines`; the worker's row build
  is skipped when the resolved list is unchanged. Element fan-out stays
  `O(max-lines)`, independent of document size.
- **#2 Extensibility.** The structural half of the feature lives entirely in a
  component. Two general seams land with it — a context producer and theme
  element registration — and both are available to every subsequent plugin. The
  acid test holds: zero `Editor::do_context_*` methods, zero new host `Action`
  variants; the plugin contributes ActionIds and owns the handler bodies.
- **#3 Vim semantics.** `[u` sits in the existing `[`-prefix structural family,
  takes a count, and pushes position history so `<C-o>` unwinds it. The
  behaviour is contributed by a minor mode that owns its keymap *and* its
  handlers.
- **#4 Asynchronicity.** The producer is an async task woken through
  `SubsystemBoot::inbound`, so results reach the screen with no keypress. Parse
  and query both run off the actor thread.
- **UX (higher court).** Nothing the user did not edit changes colour or
  position: context rows are the buffer's own cells, failures degrade to fewer
  rows, and the headerline is never displaced. The one accepted eventual
  consistency is a scope set that catches up a reparse later — the same latitude
  syntax colour already has.

## Rejected alternatives

- **Plugin resolves rows per scroll.** The guest returns finished rows and the
  host calls it on every viewport change. Simpler host side, no scope cache —
  and a WASM round-trip at scroll rate, plus a per-pane call, plus a new scroll
  event. A paramount-#1 regression for a cache that thrashes by construction.
- **Native, with the seam later.** Fastest to a working feature, and it closes
  the structural-UI class to plugins for however long "later" lasts. The
  tree-sitter seam was built for this.
- **A general `scopes` seam feeding folds too.** One concept instead of two, and
  it mirrors no native trait in the shape needed, forces `Fold` to carry header
  spans it never reads, and refactors a working path that feeds the render hot
  path. Reachable later from a real second consumer.
- **Push scopes over the `events` seam.** Zero new WIT — and no versioning or
  staleness contract, no capability story specific to the data, and the host
  would have to invent the cache discipline anyway. The seam in all but name.
- **Renderers resolve per frame from the published `CellMatrix`.** Symmetrical
  with how `IndentGuides` picks its active block, and broken by chunking: a
  header above the resident chunks has no cells to copy, so it would paint
  uncoloured or not at all.
- **Host registers `context.*` theme builtins.** Ships without a `theme` seam,
  and leaves the element names, docs and defaults with the host — the
  half-migration the mode-ownership rule names.
- **Overlay the context on top of the text instead of reserving rows.** What
  `nvim-treesitter-context` does with a float window, and it removes the
  reservation feedback loop entirely. Rejected because lattice's sticky rows
  reserve, uniformly, for every producer: an overlay would hide content the user
  has no way to know is hidden, and it would make this the one pinned surface
  the scroll model does not account for.

## Risks

- **Three seams before one row appears.** The `context` WIT, the `theme` WIT and
  the pane-keyed layer all land before the plugin can show anything. This is the
  lighthouse shape — the host extension is the real cost, the plugin is small.
  The lever if it needs to ship sooner is deferring the `theme` seam to last and
  carrying hard-coded defaults through the earlier slices.
- **`Effect::CursorMoveIn` and the position-history push may not be in the WIT
  effect mirror.** `boundary_effect.rs` is large and this has not been verified.
  If either arm is missing, the mode slice grows by a small effect-mirror
  addition. Verify before starting that slice, not during it.
- **Whole-buffer query cost on large files.** Bounded by `max-file-lines` and
  run off-thread, but the tail is real on generated code. The bench records it;
  the cap is the mitigation.
- **Per-keystroke resolver cost.** Small by construction, but it is genuinely on
  the hot path and therefore genuinely ratchetable. It gets a bench so a later
  change that makes it `O(scopes)` fails CI rather than a review.
- **Reservation churn.** A cursor crossing a scope boundary changes the reserved
  row count, which moves the text under the strip by a row. Unavoidable — every
  implementation of this feature has it — but worth a test that the movement is
  exactly the reservation delta and never more.

## Testing strategy

- **Unit — the resolver** (`lattice-cells`): nesting depth; `header_end < A`
  exclusion when the header is still visible; trim `outer` and `inner`;
  multi-line headers consuming row budget; anchor exactly on a header line;
  empty scope list; the viewport-fraction guard; `topline` anchor resolving
  pre-reservation.
- **Host**: reservation count equals published `sticky_context_lines.len()` for
  a matrix of cases; scroll geometry with headerline and context both present;
  **one buffer, two panes, different cursors → different context** (the
  pane-keying proof).
- **Renderer parity**: TUI and GPUI both paint headerline-then-context in that
  order, and context row cells are asserted **identical** to the source rows'
  cells — the highlighting-preservation proof, and the reason it is an
  assertion rather than a hope.
- **Async landing**: scopes reach the screen with **no intervening keypress**. A
  test that dispatches an action first passes on the broken version, which is
  the hole `test_helpers::settle` exists for.
- **Seam**: a fixture component returning canned scopes, so the seam is testable
  without a real grammar — and a trapping fixture proving the strip keeps its
  last good value.
- **Grammar**: `[u` walks outward on repeat and terminates at top level; a count
  jumps `N` levels; `<C-o>` returns to the pre-jump position; `[u` is inert in a
  buffer with no tree-sitter grammar (the minor is not active there).
- **Benches**: `resolve_context` at depth 20 over 50k scopes; the worker's
  context-row build; the seam round-trip against the async-producer budget (the
  `decorations` gate, not the sync typed-call gate).

## Cross-references

- [`plugin-treesitter-seam.md`](plugin-treesitter-seam.md) — the query surface
  the plugin consumes.
- [`plugin-host.md`](plugin-host.md) §5 — the seam spine; the `decorations`
  producer this copies.
- [`headerline.md`](headerline.md) — the row this stacks under; §9's per-pane
  masking open question is adjacent but not resolved here.
- [`virtual-rows.md`](virtual-rows.md) — `VirtualRowKind::Sticky` and the pinned
  pre-pass.
- [`indent-guides.md`](indent-guides.md) — the worker-resolved per-pane layer
  this mirrors.
- [`theme-system.md`](theme-system.md) — element registration; this closes the
  deferred WIT item.
- [`cancellation.md`](cancellation.md) — superseded-parse cancellation for the
  producer task.
- [`../operations/slice-plans/treesitter-context.md`](../operations/slice-plans/treesitter-context.md)
  — sequencing.
