# Gutter signs

A **sign** is a mark a producer places on a line and the gutter paints. Vim's
`:sign define` / `:sign place`, generalised: the host knows what a sign *is* and
nothing about what any particular sign *means*.

That last sentence is the whole design. A provider's marks, a plugin's
breakpoints, a debugger's current line and a future built-in all place through
one registry and are styled through the ordinary theme registry — so a user
retunes a plugin's signs without either the user or the plugin knowing about the
other. What makes this a *mechanism* rather than a feature is that adding a new
kind of mark requires no host change at all.

Slice sequencing lives in
[`slice-plans/archive/gutter-signs.md`](../operations/slice-plans/archive/gutter-signs.md).

## 1. Definitions and placements are separate, and the split is load-bearing

A **definition** (`SignDefinition`, `lattice-mode/src/contributions.rs`) is
registered once and carries the expensive, reusable parts:

| field | why it is here |
|---|---|
| `name` | what placements refer to; a producer's own namespace by convention |
| `text` | the glyph when `ui.nerd_fonts` is on |
| `fallback` | the BMP glyph when it is off — **the same cell width** |
| `theme_element` | the element the glyph is painted in, resolved through the ordinary theme registry |
| `priority` | which sign wins when two land on one line |

A **placement** is `(line, SignId)` and happens per keystroke, per visible line,
on every refresh.

Folding the two together would re-carry a glyph and a theme key across the
boundary for every marked line of every refresh, to restate something that was
already true at load. The split is what keeps the render path cheap
(paramount #1).

### Placements carry an id, not a name

`GutterDecoration` is `Copy`, and one placement exists per visible marked line
per refresh. A `String` there would be both a clone per line and the end of
`Copy` for every consumer. So the name→id resolution happens **once, where a
placement is produced** — for a plugin, at the WASM boundary; for a native
provider, wherever it builds its marks — never on the render path.

### Ids retire; they are never reused

`SignRegistry::undefine` leaves a `None` hole rather than freeing the slot. A
placement produced by a producer that ran *before* the removal therefore paints
**nothing**. Reusing the slot would have made that stale placement paint some
*later* sign's glyph — a wrong answer in place of the right one, and the silent
kind. A blank cell is a visible absence.

Redefinition, by contrast, **keeps** the id: a plugin reloading with a new glyph
does not orphan placements already in flight, they simply start painting the new
glyph. That is what "redefine" should mean.

### Ties break on name

`winning_sign` breaks equal priorities on `name`, not on iteration order. Without
that, the painted glyph depends on `HashMap` seeding: the same buffer renders
differently between runs, and a test that passes today fails when the map
reseeds.

## 2. The glyph is a font question; the colour is a theme question

`SignDefinition::glyph` picks `text` or `fallback` from `ui.nerd_fonts`;
`theme_element` decides the colour. Conflating the two is how a themed editor
renders tofu.

Both palettes must occupy the same cell width, per the icon-degradation rule, so
toggling `ui.nerd_fonts` cannot shift the gutter's geometry.

`glyph_char` truncates to one character rather than trusting a producer to have
obeyed the one-cell rule. Widening a cell would push every line of content
sideways — a pixel change to content the user did not edit, which costs far more
than the lost tail of a glyph.

## 3. Columns, and why the gutter has two of them

The gutter is `[mark][diff][line numbers]`. **Every mark in both columns is a
sign** (SG.4b): `diagnostic.{error,warning,info,hint}` and
`diff.{add,change,remove,conflict}` are definitions in the same registry a
plugin writes, registered at boot, named by their producers. The host owns no
hardcoded gutter semantics — the renderers cannot tell a diagnostic from a hunk
mark from a plugin's breakpoint, and do not need to.

`SignDefinition.column` is what makes that possible, and it was forced rather
than chosen. Collapsing both columns into one contended cell — the obvious
reading of "signs subsume both" — would drop the git gutter on exactly the lines
a diagnostic touches, which are the lines a user is most likely to be looking
at. Vim's single `signcolumn` accepts that trade; Helix and Zed do not, and
UX-over-architecture does not permit it here. So contention is **per column**,
the host owns the column ORDER (a gutter whose columns moved per buffer would be
unreadable) and nothing about what goes in them. A definition naming a column
the host does not paint falls back to the leftmost rather than vanishing.

The column list is not user-configurable yet. That is the obvious next step and
is deliberately deferred: it changes the gutter's WIDTH, which every scroll,
wrap and cursor-column calculation reads. `BUILTIN_SIGN_COLUMNS` is the single
place the count lives, so a third column widens the gutter by construction
rather than by someone remembering to update a constant.

### Priorities carry the severity order

`max()` on `GutterSeverityLevel` was the retired `Severity` arm's semantics, and
it survives the unification as priority: **hint 10, info 20, warning 30, error
40**. `10` is vim's default sign priority and the floor, so a plugin shipping the
vim default ties with a hint (broken by name, deterministically) and loses to
everything above it.

Displacing an error means exceeding `DIAGNOSTIC_ERROR_PRIORITY`. That is a
stricter bar than SG.2b's, where any priority above 10 did it — and the stricter
reading is the right one: a sign that hides a compiler error had better mean it.
A debugger stopped on this very line qualifies.

### The glyph options write through the registry

`ui.diagnostic-*-glyph` are live options the renderers used to read per frame. A
definition is static by design — that is what keeps the render path free of
per-line option reads — so `rebuild_option_cache` re-registers the built-ins when
a glyph moves. Redefinition keeps the id (§1), which is what makes that safe with
placements already in flight. The write is skipped when no glyph actually
changed, because the registry is read on the render path and an unconditional
store would hand the renderer a fresh `Arc` on every `:set` of any kind.

## 4. The render path

Per pane, per frame, in both renderers (`lattice-ui-tui/src/render.rs`,
`lattice-ui-gpui/src/window.rs`):

1. The decoration walk partitions every `GutterDecoration::Sign` into ONE map
   per column, resolving contention **as placements arrive** via `winning_sign`
   — not by whoever paints last.
2. Each column's winning sign becomes one cell, left to right.
3. The glyph comes from `glyph_char(nerd_fonts)`; the colour from the
   pre-resolved element.

There is exactly one cell renderer. The separate severity and diff-sign paths
are gone with the variants they served.

Nothing on this path hashes a string or looks a theme element up by name.
`RenderState::signs` (`SignsRenderState`) carries the definition snapshot **and**
a `SignId → ElementId` map resolved on the actor thread at publish. It is rebuilt
per publish rather than version-cached deliberately: the map is keyed by
*definitions*, of which there are single digits, not by placements. Caching it
would need an invalidation axis folding the sign registry's version *and* the
theme's, and a missed bump there paints a stale glyph in the wrong colour — a
silent failure bought with a handful of string hashes on the publish path.

### The fallback tone

A definition names its own theme element. When that element is not registered —
a plugin that shipped a sign without one, or a theme that has not been reloaded —
the renderer falls back to **`gutter.sign`**, not to no style at all. A sign with
nowhere to get its colour still has to be visible: it was placed to tell the user
something, and painting it invisibly is the one outcome that loses the
information entirely rather than merely showing it in the wrong tone.

`gutter.sign` defaults to `text`, and the contrast with the fold markers beside
it is deliberate: a fold marker is always-present chrome and is muted so it does
not clutter, while a sign is placed on purpose and earns ordinary foreground
presence.

## 5. Reconciliation

`<C-l>` re-asks every decoration producer. `:redraw` already re-derives every
visible pane from a clean slate, and a producer's signs are part of that display
— but neither of the decoration pump's triggers moves on a redraw, so a stale
sign was precisely what survived the key pressed to clear it. This is the whole
decoration path, not just signs.

## 6. Paramount-goal alignment

- **#1 (performance).** Definitions are registered once; placements are `Copy`
  and carry an interned id; theme elements resolve at publish, not at paint. The
  render path adds one `HashMap<u32, SignId>` probe per visible line.
- **#2 (extensibility).** A plugin's sign is a first-class occupant of the same
  cell as a built-in mark, styled through the same theme registry, retunable by
  a user who has never heard of the plugin.
- **#4 (asynchronicity).** Plugin producers run off the render path; the
  renderer reads a host-written cache and never enters WASM on the tick.

## 7. A plugin's signs (SG.3a)

`wit/signs.wit` — a `signs` interface with `define-sign`, and a `sign-plugin`
world exporting `register-signs` that the host drives once at load. The shape
is `theme.wit`'s, because the problem is the same one: a plugin that passed
literal glyphs and colours per placement would put the palette in the plugin
(so `:colorscheme` could not touch it) and re-cross the same glyph and theme
key for every marked line of every refresh.

Names are **auto-namespaced by plugin id**, so a plugin can neither collide
with another nor shadow a native producer's sign. Unload reverses the
declaration, and because ids retire (§1) a placement still in flight from an
unloaded plugin paints nothing rather than inheriting a later sign's glyph.

The seam drains at rank 2, after `theme`, so a plugin that registers the
element its signs name has already done so and its signs paint in their own
colours on the first frame. That is a nicety, not a requirement — the
`gutter.sign` fallback is what makes it safe either way.

A guest **places** signs by name too (SG.3b): `gutter-decoration` carries a
`sign(gutter-sign)` arm whose payload is `(line, name)`. A guest has no id to
carry, so the host interns the name at the boundary — off the render path,
which is the whole reason a native placement stays `Copy` with no per-line
`String`.

A name nothing has defined is **skipped**, not an error. An error fails the
producer's whole batch and would take the plugin's diff and severity marks down
with it over one unregistered name; a definition that has not registered yet is
recoverable, and the skip matches what the render path already does with an
unknown id. The context-free `WitBoundary` conversions cannot spell a sign at
all and refuse by naming the registry-aware pair, so a new arm still forces a
decision at every site rather than being dropped silently.

## 8. Open

- **A user-configurable column list** (§3). The list is a host constant today;
  making it configurable changes the gutter's width, which every scroll, wrap
  and cursor-column calculation reads.
- **The diff row TINT** is still a separate path — it reads `diff.sign_maps`
  directly rather than going through the registry. That is arguably correct: a
  tint is a property of the row, not a mark in a column, and a sign has no
  background. Worth revisiting only if a plugin ever needs to tint a row.
