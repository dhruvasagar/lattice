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
| SG.4a | The built-in signs — `SignDefinition.column`, diagnostics + diff registered at boot, ids interned | ✅ |
| SG.4b | Producers emit `Sign`; renderers paint columns generically; `GutterDecoration::{Diff,Severity}` deleted | ✅ |

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

## SG.4a — the built-in signs ✅

Diagnostics and diff marks become definitions in the same registry a plugin
writes: `diagnostic.{error,warning,info,hint}` and
`diff.{add,change,remove,conflict}`, registered at boot, with their ids
interned into `BuiltinSignIds` (the `BuiltinElementIds` shape — a producer
emitting a mark per visible line reads a field rather than hashing a name per
line).

**`SignDefinition.column` is the forcing change, and it is not optional.**
Severity and diff occupy separate cells today. Collapsing them into one
contended cell would drop the git gutter on exactly the lines a diagnostic
touches — the lines a user is most likely to be looking at — so the
unification only works if a sign says which column it belongs to. Vim's single
`signcolumn` accepts that trade; Helix and Zed do not, and the UX rule (no
regression in service of architecture) does not permit it here either. A column
the host does not paint falls back to the leftmost one rather than vanishing,
on the same principle as the `gutter.sign` theme fallback.

**Priorities carry the severity order.** `max()` on `GutterSeverityLevel` was
the old `Severity` arm's semantics and it has to survive: hint 10, info 20,
warning 30, error 40. `10` is vim's default sign priority and the floor.
Outranking an error now means exceeding `DIAGNOSTIC_ERROR_PRIORITY` — a real
change from SG.2b, where any priority above 10 did it, and the stricter reading
is the right one.

**The glyph options write through the registry.** `ui.diagnostic-*-glyph` are
live options the renderers used to read per frame. A definition is static by
design — that is what keeps the render path free of per-line option reads — so
`rebuild_option_cache` re-registers the built-ins when a glyph moves.
Redefinition keeps the id (SG.1), which is what makes that safe with placements
already in flight; the property was built before anything needed it and this is
the thing that needed it. The write is skipped when no glyph actually changed,
because the registry is read on the render path and an unconditional `store`
would hand the renderer a fresh `Arc` on every `:set` of any kind.

`BuiltinSignIds::default()` is every id `SignId(u32::MAX)`, which resolves to
nothing — so a fixture that never registered the built-ins paints no marks
rather than whatever sits at id 0, which would be some plugin's sign, silently.

The WIT `sign-spec` gains `column`; an empty string means the mark column, so a
guest that does not care lands where every sign landed before columns existed.

## SG.4b — the switch ✅

`GutterDecoration` has ONE variant now. The three native producers
(`lsp-mode`, `compilation-mode`, `diff-mode`) emit `Sign` placements naming
built-ins through the interned `BuiltinSignIds` the renderers inject into the
decoration context; both renderers partition into one map per column and paint
one cell each; the two hardcoded cell renderers and the two width constants are
deleted.

Atomic by necessity: removing the variants forces every producer, both
renderers, the boundary and their tests at once, and any smaller step leaves the
half-migration shape the mode-ownership rule exists to prevent.

- **The WIT keeps its `diff` / `severity` arms as sugar.** A guest saying "line
  4 is an addition" should not have to know the host spells that `diff.add`,
  and deleting the arms would break every decoration plugin for a change
  entirely internal to the host. The boundary maps them to built-in sign names,
  so the native side has exactly one kind of decoration.
- **The context-free `WitBoundary` impl is kept, not deleted.** It refuses both
  directions by naming the registry-aware pair. That keeps the refusal a
  compiler-checked total function: a future arm still has to decide there.
- **A stripped harness now contributes NO marks**, not even sugar ones, because
  every arm needs the registry to resolve a name. In production the registry is
  a boot service and always present; inert is the same degradation every other
  unwired seam takes, and it is honest — the alternative is marks resolving to
  whatever sits at id 0.
- **`sign_beats_severity` and `SEVERITY_SIGN_PRIORITY` are deleted.** They
  encoded "does this sign outrank a diagnostic", which is now just a priority
  comparison between two signs in one column. A public helper with no
  production consumer, named after a concept that no longer exists separately,
  is the orphan the conversion rule forbids leaving behind.

**One contract change, and it is real.** SG.2b let any sign above priority 10
displace an ERROR, because a diagnostic held the cell at one priority whatever
its severity. Now the severities span 10..40 and displacing an error means
beating 40. `a_higher_priority_sign_takes_the_cell_from_a_diagnostic` was
updated to say so — the only test that had to change meaning rather than shape.

Tests: the three producers each pin that they emit the built-in for their kind
AND that they contribute nothing without the interned ids (rather than
placements resolving to id 0); the boundary pins that the sugar arms resolve to
built-ins and that an unregistered built-in skips; `decoration_drain` and
`decoration_source` pin the same end-to-end through a real component; the TUI's
existing diagnostic-glyph and gutter-width tests pass unchanged, which is the
evidence the unification is invisible to the user.

The boundary benchmark was rewritten rather than deleted: the conversion is
registry-aware now, so its cost includes a name→id `HashMap` probe. Measuring it
at the boundary is precisely what keeps it from quietly moving onto the render
path.

## SG.4 — remaining

A user-configurable column list. Deferred deliberately: it changes the gutter's
WIDTH, which every scroll, wrap and cursor-column calculation reads.
`BUILTIN_SIGN_COLUMNS` is the single place the count lives, so a third column
widens the gutter by construction rather than by someone remembering a constant.
