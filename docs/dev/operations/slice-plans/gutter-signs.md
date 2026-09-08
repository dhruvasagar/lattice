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
| SG.3a | WIT: a plugin **defines** signs (`register-signs`, teardown via `undefine_prefix`) | 📝 |
| SG.3b | WIT: a plugin **places** signs (`gutter-sign` variant arm, name→id resolution at the boundary) | 📝 |
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

## SG.3a — a plugin defines signs 📝

Modelled on `wit/theme.wit`, which solved the same problem: a `signs` interface
with `define-sign`, a `sign-plugin` world exporting `register-signs`, drained
once at load, auto-namespaced by plugin id so plugins cannot collide or shadow a
builtin. Teardown calls the `undefine_prefix` SG.1 added for exactly this.

Without this slice SG.3b is inert — a guest could place a name nothing has
defined.

## SG.3b — a plugin places signs 📝

`record gutter-sign { line: u32, name: string }` and a `sign(gutter-sign)` arm
on the `gutter-decoration` variant.

The name→id resolution needs the registry, which context-free
`WitBoundary::from_wit` does not have — so it happens at the
`DecorationSource::gutter_decorations` call site, off the render path, which is
where the design says placements are produced. An unresolvable name skips that
placement and logs at `debug!` (per-refresh producer ⇒ not `info!`), matching
the native path's "an unknown id paints nothing" rather than failing the whole
batch: a definition that has not registered yet is recoverable, a malformed
record is not.

## SG.4 — signs subsume severity and diff 📝

Severity and diff marks become built-in sign definitions placed through the
registry; the host paints generic sign columns and owns no hardcoded gutter
semantics. Design §3 argues this is the right end state (closest to the
Helix/Zed gutter-as-a-list shape) and why SG.2b is its first increment rather
than a detour. Deferred because it rewrites two well-covered paint paths in
both renderers, and that risk wants its own slice.
