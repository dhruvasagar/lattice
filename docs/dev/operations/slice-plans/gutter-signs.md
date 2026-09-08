# Gutter signs — slice plan

Design: [`architecture/gutter-signs.md`](../../architecture/gutter-signs.md).

A generic sign mechanism for the gutter: vim's `:sign define` / `:sign place`,
with the host owning what a sign *is* and no built-in opinion about what any
sign *means*.

## Status

| Slice | What | Status |
|---|---|---|
| SG.1 | The sign model — `SignDefinition`, `SignRegistry`, `SignId`, `winning_sign`; `<C-l>` reconciles decorations | ✅ `593e3b49` |
| SG.2a | `GutterDecoration::Sign { line, sign }` — placements, interned; explicit arms in both renderers and at the plugin boundary | ✅ `023b88a9` |
| SG.2b | The mark cell paints a placed sign — service wiring, published `SignsRenderState`, contention with diagnostics, both renderers | ✅ |
| SG.3a | WIT: a plugin **defines** signs (`signs` interface, `sign-plugin` world, drain + teardown) | ✅ |
| SG.3b | WIT: a plugin **places** signs (`gutter-sign` variant arm, name→id resolution at the boundary) | ✅ |
| SG.4 | Signs subsume the severity and diff columns | 📝 |

## SG.1 — the model ✅

`SignDefinition` / `SignRegistry` / `SignId` in `lattice-mode/src/contributions.rs`,
plus `<C-l>` re-asking every decoration producer. No renderer change; compiles
and is green on its own.

Decisions locked here, with reasons in the design doc §1:

- definitions and placements split (the render-path cost argument),
- placements carry an interned `SignId`, not a name (`Copy`, no per-line clone),
- ids **retire** rather than being reused (a stale placement paints nothing
  instead of some later sign's glyph),
- redefinition **keeps** the id (a reloading plugin does not orphan placements),
- ties break on `name` (or the glyph depends on hash seeding).

## SG.2a — placements ✅

`GutterDecoration::Sign { line, sign: SignId }`. Both renderers and the plugin
boundary got **enumerated** arms rather than a `_`, so the next variant still
forces a decision at each site — aligned by fallback, not by silence. The
boundary returns an explicit `Err` naming SG.3 rather than dropping a placement
it cannot yet spell.

## SG.2b — the mark cell paints ✅

`gutter.sign` (the fallback tone) landed first, in `b5560bcf`.

The rest:

- `SignRegistryHandle` registered as a boot service, on the alias it is looked
  up on (the ServiceRegistry Arc/TypeId rule).
- `RenderState::signs` (`SignsRenderState`) — the definition snapshot plus a
  `SignId → ElementId` map resolved **at publish**, so the render path neither
  hashes a string nor looks an element up by name.
- Both renderers partition placements into a `sign_map`, resolving contention as
  placements arrive via `winning_sign`.
- The mark cell is **shared** with diagnostics; `sign_beats_severity` is
  strictly-greater so a tie leaves the error visible. Design §3 records the
  three alternatives and why each lost.
- `SignDefinition::glyph_char` truncates to one cell rather than letting a
  definition widen the gutter and push every line of content sideways.

Tests: `lattice-mode` covers truncation, the empty glyph, the strictly-greater
tie and `iter` skipping retired ids; `lattice-ui-tui` covers the painted glyph,
the diagnostic winning a tie, a high-priority sign taking the cell, priority
deciding between two signs, a retired id painting neither its own glyph nor its
successor's, and the content column not moving when a sign arrives.

## SG.3a — a plugin defines signs ✅

Modelled on `wit/theme.wit`, which solved the same problem: `wit/signs.wit`
declares a `signs` interface with `define-sign` and a `sign-plugin` world
exporting `register-signs`, drained once at load and auto-namespaced by plugin
id so plugins cannot collide with each other or shadow a native producer's
sign.

- `sign_host.rs` mirrors `theme_host.rs`: the conversion and the copy-on-write
  registry write are free functions, unit-testable without a `Store`.
- Drains at **rank 2**, after `theme`, so a plugin that registers the element
  its signs name has already done so — its signs paint in their own colours on
  the first frame instead of falling back to `gutter.sign` until something
  republishes. Not a correctness requirement, since the fallback exists.
- The `signs` host func is wired into the linker **unconditionally**. An
  unwired import fails instantiation of the whole component, not just the call.
- Teardown records the namespaced NAMES rather than the namespace prefix: the
  seam's store is dropped when `register-signs` returns, so a plugin cannot
  declare a sign later in its life, which makes the list complete by
  construction and gives the report an exact count. `undefine_prefix` (added by
  SG.1) is what to switch to if that stops being true.
- The reversal writes the `ArcSwap` **once for the whole list**. Storing per
  name would make N intermediate snapshots visible to the render path, each
  with a different subset of the plugin's signs still painting.

Tests: `sign_host` unit tests cover namespacing, redefinition keeping the id,
whole-namespace removal, and the spec crossing without losing the fallback
palette. `lattice-plugin-loader/tests/sign_drain.rs` drives a real
`wasm32-wasip2` `sign-guest` component end-to-end — declaration, redefinition
keeping the id across the boundary, and unload retiring ids so a later
definition cannot inherit a retired slot.

Its rig wires `theme_registry` even though nothing in it declares an element:
the loader gates the whole reversal on one all-or-nothing tuple of registry
handles, so a rig missing any of them turns unload into a silent no-op and the
teardown assertions pass vacuously. That cost one red test to find, which is
the point of asserting the report COUNT rather than only the absence.

Without this slice SG.3b would be inert — a guest could only place a name
nothing had defined.

## SG.3b — a plugin places signs ✅

`record gutter-sign { line: u32, name: string }` and a `sign(gutter-sign)` arm
on the `gutter-decoration` variant. A guest has no `SignId` to carry — ids are
interned by the host — so the wire carries the NAME and the resolution happens
once, at the boundary and off the render path, which is what lets the native
placement stay `Copy` with no per-line `String`.

- Context-free `WitBoundary::to_wit` / `from_wit` cannot spell a sign, and say
  so by naming `decoration_to_wit` / `decoration_from_wit` rather than dropping
  it. The boundary's contract is that a new arm forces a decision at every
  site; "needs the registry" is a decision worth being told about.
- The registry-aware pair returns `Ok(None)` for an unresolvable name — a skip,
  not an error. An `Err` fails the whole batch and would take the plugin's diff
  and severity marks down with it over one unregistered name. A definition that
  has not registered yet is recoverable; a malformed record is not, and those
  still fail. Logged at `debug!`, because a decoration producer runs on every
  refresh and one bad name at `warn!` would flood at keystroke rate.
- `WasmDecorationSource` holds the registry HANDLE and loads per call. A
  snapshot captured at construction would keep answering from the registry as
  it was when the source was built, so every later plugin's signs would
  silently skip.
- `decoration_to_wit` exists for a direction with no consumer yet, so the round
  trip is testable AS a round trip. Testing one direction would not catch an
  id↔name mapping that silently disagreed with itself.

Tests: the boundary round trip, the unknown name skipping while its batch
neighbour survives, a retired id having no name to send, and the registry-free
conversions refusing by name. End-to-end, the `decorations-guest` fixture now
emits two sign placements — one defined, one not — so the positive path runs
through a real component rather than only through `None` answers, and the
existing unwired-harness test pins that a producer with no registry loses its
own sign marks and nothing else.

## SG.4 — signs subsume severity and diff 📝

Severity and diff marks become built-in sign definitions placed through the
registry; the host paints generic sign columns and owns no hardcoded gutter
semantics. Design §3 argues this is the right end state (closest to the
Helix/Zed gutter-as-a-list shape) and why SG.2b is its first increment rather
than a detour. Deferred because it rewrites two well-covered paint paths in
both renderers, and that risk wants its own slice.
