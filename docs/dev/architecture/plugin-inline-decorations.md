# Plugin inline decorations

> Design fragment. The "what" and "why" of letting a WASM plugin style a
> **range of buffer text**. Slice sequencing lives in
> `../operations/slice-plans/plugin-inline-decorations.md` once carved.

## Problem

A plugin can mark a **line** (the `decorations` seam produces gutter
decorations) and it can own a **whole language** (the `language` seam, which
requires the plugin to ship a tree-sitter grammar). It cannot style a range of
text inside a buffer whose language it does not own.

That gap is what blocks three plugins we want, and it blocks them all in the
same place:

| Plugin | Needs | Blocked on |
|---|---|---|
| colorizer | paint `#ff0088` in that colour | a text range styled a literal colour |
| todo-comments | highlight `TODO` / `FIXME` / `HACK` | a text range styled a registered element |
| rainbow delimiters | tint nested brackets by depth | the same |

It also blocks a class rather than three instances — plugin-provided lint
squiggles, spell underlines, a search-hit highlighter, a "highlight other
occurrences of the symbol under the cursor" plugin. Every one of them is the
same sentence: *style this range like that*.

## The native half is already built

This fragment is deliberately small because almost everything it needs exists
and is load-bearing for something else already. What is missing is the
plugin-facing door, not the machinery behind it.

| Piece | Where | Status |
|---|---|---|
| a per-line byte range carrying a style | `lattice_cells::StyledSpan` | shipped |
| a style that names a **registered theme element** rather than a closed enum variant | `lattice_cells::Style::Element(ElementId)` | shipped (DL.1) |
| a plugin registering a theme element, including a literal colour | `wit/theme.wit` — `register-element`, `color-ref::literal-rgb` | shipped |
| extra spans merged over grammar spans, with precedence | `cells_worker::merge_extra_spans` | shipped |
| those spans folded into the matrix cache version | `cells_worker::extra_spans_version` | shipped |
| a per-buffer store for them | the `ExtraHighlights` buffer-local | shipped (help links, PU.1b-2a) |
| an async producer + per-buffer cache + a renderer that reads only the cache | `wasm_decorations.rs`, `decoration_{host,source,task}.rs` | shipped, for gutters |

`Style::Element` says outright why it exists:

> a WASM plugin can register a theme element by name but can **never** add a
> variant to a Rust enum. Without this, themed highlighting is reachable only
> by editing core, which makes it impossible for plugins by construction
> (paramount goal #2).

The variant landed; the seam that would let a plugin *use* it never did. This
fragment is that seam.

## Paramount-goal alignment

**#1 (performance) dictates the shape, and is the reason this is not simply
"call the plugin from the highlighter".** The gutter seam already made this
choice and wrote down why: `Mode::gutter_decorations` is a synchronous trait
the renderer reads every frame, and a WASM mode cannot satisfy it inline
because per-frame WASM is a paramount-#1 violation. Inline decorations are
read on exactly the same path — the cells worker — so they take exactly the
same shape: an **async producer the host calls on a trigger, whose result the
host caches, which the renderer reads**. No WASM on the tick, enforced by the
seam being async and the renderer never holding a guest handle.

**#2 (extensibility)** is the goal being served. Gutter and whole-language are
two thirds of the decoration surface; this is the third.

**#4 (asynchronicity)** — the producer is a plugin actor call like every other
seam, off the UI thread.

**UX (the higher court)** sets the acceptance bar, and it is stricter than
"fast enough": per the keystroke contract, only the edited line may visibly
change, and nothing the user did not edit may change pixel. Colour arriving a
frame or two late is an acceptable eventual-consistency compromise — that is
the same trade syntax highlighting already takes. Content *losing* its colour
for those frames is not, and `incremental-highlight.md` records that exact
regression: the stale path rendered the viewport as plain spans between the
keystroke and the worker republishing, and styled content visibly dropped to
plain and snapped back on every keystroke. **A plugin span must degrade to its
previous value, never to nothing.**

## Contract

### The seam

A new `inline-decorations` interface, a sibling of `decorations` rather than an
extension of it. Two reasons they stay apart: the gutter producer answers
"which lines carry a mark" and this one answers "which ranges carry a style" —
different questions with different invalidation — and a provider should be able
to export one without implementing the other.

	interface inline-decorations {
	    use types.{inline-decoration-context, styled-range};

	    /// Produce the styled ranges for the requested slice of a buffer.
	    /// Async — a produce call suspends the guest, never the render path.
	    /// An `err` string is logged; the cached snapshot KEEPS ITS PRIOR
	    /// VALUE (§UX above — degrade to stale, never to nothing).
	    inline-decorations: func(ctx: inline-decoration-context)
	        -> result<list<styled-range>, string>;
	}

### The context is viewport-scoped, and that is the load-bearing decision

	record inline-decoration-context {
	    buffer: u64,
	    path: option<string>,
	    /// The line window the host wants answered — NOT the whole buffer.
	    first-line: u32,
	    last-line: u32,
	    /// Bumped whenever the host would accept a different answer; echoed
	    /// back in the reply so a late producer's result can be dropped.
	    generation: u64,
	}

The existing `ExtraHighlights` local is whole-buffer and static, and the cells
worker gates on it: a buffer carrying extra spans takes the full rebuild path
and does not reuse rows incrementally. That is correct for help links — seeded
once when the help content is built, never again — and **wrong for a colorizer**,
where it would mean an O(file) rebuild on every keystroke, which is the exact
defect `incremental-highlight.md` was written to remove. So the producer is
asked for a window, the host merges per window, and extra spans must join the
incremental-reuse path rather than opt out of it.

`generation` exists because produce is async and the buffer can move under it.
A reply whose generation is stale is dropped rather than merged; without it a
slow producer can repaint ranges that no longer mean anything, which is a pixel
change to content the user did not edit.

### The style: two cases, and they are genuinely different

	variant range-style {
	    /// A registered theme element, by name. `:colorscheme` retunes it
	    /// like any other element.
	    element(string),
	    /// A literal colour. NOT retuned by a colourscheme, deliberately.
	    literal-rgb(u32),
	}

	record styled-range {
	    line: u32,
	    start-byte: u32,
	    end-byte: u32,
	    style: range-style,
	}

`theme.wit` already argues that a plugin should register elements rather than
pass literal colours, because literal colours put the palette in the plugin and
a `:colorscheme` then cannot retune it. That argument is right for
todo-comments — `TODO` should follow your theme, the way org already registers
one element per TODO keyword — and it is **wrong for a colorizer**, where the
whole point is that `#ff0088` renders as `#ff0088`. A theme that retuned it
would be showing the user a lie about their own file.

So both are offered, and the choice is the plugin's semantic decision rather
than an escape hatch. `literal-rgb` maps to the `color-ref::literal-rgb` the
theme seam already has; `element` resolves through `Style::Element` and the
ordinary `ResolvedTheme` lookup, so it is byte-identical to a builtin category
downstream.

**Unbounded element registration is why `literal-rgb` is not merely
convenience.** Without it a colorizer would have to `register-element` per
distinct colour it encounters, growing the theme registry for the life of the
session with entries nothing can garbage-collect. That is a leak with a
plausible-looking cause, and it is avoided by having the honest variant.

### Precedence

Within one provider, first span wins for a byte — the existing
`style_at_byte` rule, unchanged.

Across providers, and against the grammar: **plugin spans layer over grammar
spans** (what `merge_extra_spans` already does for help links), and providers
layer in **plugin load order**, which is deterministic and inspectable in
`:plugins`. A declared priority number is deliberately NOT offered in v1 —
priority fields invite every provider to claim the top, and no current use case
overlaps another. If two providers genuinely contend, that is the signal to
design the ordering rather than to have guessed it now.

### Invalidation

The host re-asks a provider when, and only when:

- the buffer's text changes within or before the window (an edit shifts byte
  offsets),
- the window changes (scroll, resize, fold),
- the theme changes — element resolution moves, though the ranges do not,
- the provider calls `refresh-decorations` (the existing host-services call,
  reused rather than duplicated),
- the provider is loaded, reloaded or unloaded.

Unload must drop that provider's spans by provenance, the way every other
registry contribution reverses (`unregister_plugin`). A span outliving its
plugin is a pixel the user cannot explain and cannot clear.

## Rejected alternatives

**Let a plugin add highlight captures to an existing language.** Shaped right
— colorizer really is "one more highlight layer" — but `register-language`
requires the plugin to ship `grammar: list<u8>`, so extending Rust or CSS would
mean either shipping a duplicate grammar or inventing a capture-injection
mechanism into someone else's query. Both are larger than this seam, and
neither helps the non-tree-sitter cases (a colour inside a comment, a spell
underline) which have no capture to attach to.

**Extend the gutter `decorations` seam with a range field.** Smaller diff,
worse contract: it fuses two invalidation lifetimes (a line mark survives a
horizontal edit; a byte range does not) and forces every existing gutter
provider to have an opinion about a concern it does not have. `StyledSpan` and
`RefineSpan` were split for precisely this reason and the note on `RefineSpan`
says so.

**Let the span carry a full style-spec (fg, bg, modifiers) inline.** Rejected
for v1: it is a second palette in the plugin for the themeable case, and
`literal-rgb` already covers the case where literal is correct. Background and
modifiers are a separate axis — `RefineSpan` is the precedent that backgrounds
resolve differently — and adding them later is additive.

**Make it synchronous and call it from the cells worker.** This is the one that
looks cheapest and is forbidden: the cells worker runs per rebuild, a guest call
there is WASM on the render path, and paramount #1 is not negotiable. The gutter
seam already rejected the same shortcut.

## Foldability and grammar surface

None. Inline decorations add no motions, no text objects, no operators, and no
chords: they change how existing text is painted and nothing about what the
grammar can address. A provider that also wants commands contributes them
through the `grammar` seam as any plugin does.

Folds interact only through the window: a fold changes which source lines are
visible, so it invalidates the window like a scroll.

## What this unblocks

`comment` is **not** on this list — it needs `register-operator`, which
`grammar.wit` already has, and is independent of this seam.

- **colorizer** — `literal-rgb`, one provider, no theme elements registered.
- **todo-comments** — a fixed set of registered elements (`todo.todo`,
  `todo.fixme`, …), themeable, exactly the shape org already uses for TODO
  keywords. Its *listing* half needs nothing from here: the
  `scanned-excerpt-source` and `picker-source` seams already ship.
- **rainbow delimiters**, plugin lint squiggles, spell underlines, occurrence
  highlighting — the same sentence with different ranges.

## Open questions

- **Does `ExtraHighlights` become the storage, or a peer?** The local is
  whole-buffer `Vec<Vec<StyledSpan>>` indexed by source line; a windowed
  producer wants a sparse, window-keyed cache closer to
  `PerBufferCache<WasmGutterDecorationCache>`. Resolving this is the first
  slice's job, and it decides whether help links and plugin spans share a
  merge path or merely share a merge *function*.
- **What is the cost floor on a large file?** The producer is windowed, but the
  merge and the matrix-version fold are per rebuild. Needs a bench beside
  `cells_worker` before the seam is called done, per the four-artefact rule.
- **Should a provider declare which languages it applies to?** A colorizer is
  language-agnostic; a todo highlighter arguably is too. Deferred until a
  provider wants the filter, rather than guessing a `languages: list<string>`
  field now.
