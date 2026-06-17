# Theme system

Authoritative design for Lattice's theme/styling layer: a
**two-part model** — named *theme elements* (semantic styleable
roles) and *style specifications* that reference a palette rather
than baking absolute colors — resolved once into a flat table the
renderers read at O(1) per glyph. Modes and plugins register their
own elements and own the defaults for them; reused builtin elements
are referenced-only and styled by the active theme; themes, when
they land, contribute a palette plus overrides of builtin elements.

Companion to `design.md` (§5.6 rendering, §5.12 configuration,
§5.11 introspection), `buffer-local-options.md` (the per-buffer
resolution stack this composes with), `cell-grid-renderer.md` and
`display-line.md` (the read consumers), and `multibuffer-views.md`
§3.8 (whose ad-hoc header theme fields this subsumes). Owns the
*what* and *why*. Sequencing lives in
`docs/dev/operations/slice-plans/theme-system.md`.

---

## 1. Problem — where styling is today

The current model (`crates/lattice-host/src/ui/theme.rs`) is **flat,
semantic-role, palette-less, registry-less**:

- One `Theme` struct, ~40 fields, each a *direct* assignment
  (`pane_status_active: Style`, `diagnostic_error_style: Style`,
  `diff_add_sign_style`, `cursor_line_bg: Color`). Named by role —
  good — but every value is a concrete literal in
  `Theme::default()`, and `syntax_style()` hardcodes a Catppuccin
  Mocha `match`. There is no palette: a new color combination means
  editing ~40 literals across two `Default` impls.
- `Style { fg: Option<Color>, bg: Option<Color>, modifiers }`;
  `Modifiers { bold, italic, underline, dim, reverse }`. The
  vocabulary is **fg/bg + 5 bools** — no font size, no family, no
  box/overline. It cannot express an emacs heading face.
- The **only** element↔style separation that exists is two *closed*
  enums: `GutterDecoration { Diff{kind}, Severity{level} }` (the
  renderer maps `kind`→theme slot) and the syntax `Style` enum
  (mapped by `syntax_style()`). A mode **cannot register a new
  styleable element**; `DecorationProvider` is a literal stub
  reserved for the WIT path (M.10). There is no
  `register_element(name, default)` anywhere.
- A large amount of styling never reached the theme at all — it is
  hardcoded in *both* renderers and has **drifted out of parity**:
  search highlight (TUI `Cyan` vs GPUI `0x6c7086`), visual
  selection, document-highlight read/write, substitute preview,
  inlay color, picker/completion annotation colors. The single
  source of truth `host_theme` is meant to be has holes.
- A telling smell: multibuffer excerpt headers carry no color
  (`Cell` `fg:0/bg:0`, `VirtualRow.bg: None`), so the renderer falls
  them through to `diff_deletion_block_bg`. A *search* header is
  painted with the *diff-deletion* color because there is no element
  for it to claim. `multibuffer-views.md` §3.8 patches this with
  four bespoke `multibuffer_header_*` fields — exactly the
  per-feature field-add this design replaces with registration.

None of this supports the two goals driving the redesign:
**multi-theme** (swap a palette, not 40 literals) and **plugin
extensibility** (a markdown/org mode registers and owns its
heading elements; a theme styles them or they fall back to the
mode's palette-referencing defaults). See [[feedback_mode_owns_its_surface]].

---

## 2. The model — element + style + palette

Three concepts, cleanly separated:

1. **Theme element** — a named, semantic styleable role
   (`editor.cursor_line`, `syntax.keyword`, `diff.add.sign`,
   `multibuffer.excerpt_header`, `markdown.heading.1`). Identity
   only; carries no color. Builtin elements ship with the core;
   modes/plugins **register their own**.
2. **Style spec** — how an element is styled, expressed by
   **reference**: colors name a palette entry (not an absolute
   RGB), and a spec may **inherit** another element and override
   specific attributes (emacs `:inherit`, vim `:hi link`). This is
   the authoring form.
3. **Palette** — a named set of base colors owned by the active
   theme (`base`, `surface0`, `red`, `mauve`, …). Swapping the
   palette re-colors every element that references it. This is what
   a future "theme" mostly *is*.

Resolution composes them: for each element, walk its inherit chain,
resolve each `ColorRef` against the palette, and produce a concrete
**resolved style** — done **once at theme-build time** into a flat
table keyed by an interned `ElementId`. The renderer reads
`resolved[id]` — an array index, no string hashing, no walk, on the
hot path. The registry/palette/inheritance machinery lives entirely
at *build* time; the *read* stays O(1) (§7).

```
  registration          authoring                resolve-once            hot path
  (mode + core)          (theme + mode default)   (theme-build)           (per glyph)

  ElementId ◄── name     StyleSpec {              ResolvedStyle {         resolved[id]
   "syntax.keyword"        fg: Palette("mauve"),    fg: Rgb(cba6f7),       └─ array index
   "markdown.heading.1"    inherit: None,           bold: true,              O(1), no walk
   "diff.add.sign"         bold: true,              scale: None,
   ...                     scale: Some(1.6),        ...
                         }                        }
                              │                         ▲
                              └── Palette { mauve = #cba6f7, ... } ──┘
```

This is **emacs faces' semantics** (named faces, `:inherit`,
rich attributes, plugin `defface` + theme override) expressed in
**Helix's palette+scope file shape** (explicit `[palette]`,
elements reference palette names, hierarchical fallback), on a
**GPU-capable substrate** (the GPUI peer can honor a richer
attribute set than a terminal grid). See §9 for why those are the
right references.

---

## 3. Data model

Renderer-neutral, in a new `lattice-theme` crate (§6.4 for the
crate-location rationale). The existing `host_theme` types
(`Color`, `Style`, `Modifiers`, `NamedColor`) move here; host
re-exports for continuity.

### 3.1 Element identity

```rust
	/// Interned, process-stable index for a registered theme
	/// element. Allocated at registration; small integer so the
	/// hot-path read is `resolved[id.0 as usize]`.
	#[derive(Copy, Clone, PartialEq, Eq, Hash)]
	pub struct ElementId(u32);

	/// A registered theme element. Identity + metadata + the
	/// owner-supplied default. Carries NO resolved color — the
	/// default is itself a `StyleSpec` (reference form).
	pub struct ThemeElement {
		pub id: ElementId,
		pub name: ElementName,        // "syntax.keyword", dotted, hierarchical
		pub owner: ElementOwner,      // Core | Mode(ModeId) | Plugin(PluginId)
		pub default: StyleSpec,       // owner's default; theme may override
		pub doc: &'static str,        // self-documenting help (§8, design.md §5.11)
	}

	pub enum ElementOwner { Core, Mode(ModeId), Plugin(PluginId) }
```

`ElementName` is dotted and **hierarchical with fallback** (Helix
scopes, neovim treesitter captures): `markdown.heading.1` falls back
to `markdown.heading` falls back to `markdown` if the more-specific
element is unstyled by the active theme. Fallback is resolved at
build time, not read time.

### 3.2 Style spec — the authoring (reference) form

```rust
	/// How an element is styled, by reference. The form a theme
	/// file, a mode default, or a buffer-local remap is written in.
	#[derive(Clone, Default)]
	pub struct StyleSpec {
		/// Inherit another element's resolved style; this spec's
		/// own set fields override. Emacs `:inherit` / vim `:hi
		/// link`. Resolved by walking the chain at build time.
		pub inherit: Option<ElementName>,
		pub fg: Option<ColorRef>,
		pub bg: Option<ColorRef>,
		pub modifiers: ModifierSet,   // set / unset / inherit per modifier
		// ---- rich vocabulary (§5); honored by GPUI, degraded by TUI ----
		pub scale: Option<f32>,       // relative height ratio (emacs :height float); resolves to FontScale (§3.4)
		pub family: Option<FamilyRef>,// proportional / monospace family ref
		pub weight: Option<Weight>,   // finer than the bold bool
		// box / overline / underline-style land here when a renderer reads them
	}

	/// A color BY REFERENCE — never an absolute literal in the
	/// common path. `Palette` is the normal case; `Literal` is the
	/// escape hatch (a one-off a palette entry would over-generalize);
	/// `Default` means the terminal/window default channel.
	pub enum ColorRef {
		Palette(PaletteKey),
		Literal(Color),               // existing Color enum, escape hatch
		Default,
	}
```

`ModifierSet` is tri-state per modifier (`Set` / `Unset` / `Inherit`)
so inheritance can *clear* a parent's bold, not only add — emacs
faces distinguish "unspecified" from "off".

### 3.3 Palette

```rust
	/// A named set of base colors. Owned by the active theme.
	/// Swapping the palette re-colors every element that references
	/// it — the multi-theme primitive.
	pub struct Palette {
		colors: HashMap<PaletteKey, Color>,   // "base", "mauve", "red", ...
	}
	pub struct PaletteKey(SmolStr);
```

A `PaletteKey` that an element references but the active palette
lacks resolves to the element's fallback (its `inherit` chain, then
a hard default) and logs once — never panics (graceful degradation,
paramount-goal-aligned).

### 3.4 Resolved style — the read form

```rust
	/// The concrete, fully-resolved style the renderers read. This
	/// is today's `host_theme::Style`, GROWN with the rich-vocab
	/// fields. Every field concrete; no references, no inherit.
	#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, Hash)]
	pub struct Style {                // re-exported as host_theme::Style
		pub fg: Option<Color>,
		pub bg: Option<Color>,
		pub modifiers: Modifiers,     // existing bools
		pub scale: Option<FontScale>, // None ⇒ 1.0 (no-op on TUI)
		pub family: Option<FamilyId>, // None ⇒ buffer default
		pub weight: Option<Weight>,
	}
```

**`Style` must stay `Eq + Hash`** (T.1): the host folds a
content-hash of the `Theme` into `lattice_cells::MatrixVersion::theme`
so a palette change rebuilds the cell matrix. `f32` is neither `Eq`
nor `Hash`, so the resolved scale is fixed-point —
`FontScale(u16)`, hundredths (`100` = 1.0×). The authoring
`StyleSpec.scale` (§3.2) stays an `f32` ratio; resolution quantizes
it to `FontScale` here. `Weight` is an enum and `FamilyId` an
interned `u32`, both `Hash`.

`Style` stays `Copy` and cheap — the renderers keep adapting it to
ratatui (`From<&Style>`) and to GPUI (`to_rgb_u32` + the new
`scale`/`family`/`weight` reads). The new fields are additive;
existing call sites compile unchanged (the added `Option`s default
`None`).

### 3.5 Registry + resolved table

```rust
	/// The element registry — registration + resolution. Lives as a
	/// ServiceRegistry service (the MultibufferRegistry precedent,
	/// see multibuffer-views.md §3.7). Modes reach it through the
	/// service handle, never through `&mut Editor`.
	pub trait ThemeRegistry: Send + Sync {
		/// Register a custom element with its owner-supplied default.
		/// Idempotent by name; returns the interned id.
		fn register(&self, name: ElementName, owner: ElementOwner,
		            default: StyleSpec, doc: &'static str) -> ElementId;
		fn id(&self, name: &ElementName) -> Option<ElementId>;
		/// The flat, resolved read table for the active theme.
		fn resolved(&self) -> Arc<ResolvedTheme>;
	}
	pub type ThemeRegistryHandle = Arc<dyn ThemeRegistry>;

	/// Flat table: `styles[id]` is the resolved style for element id.
	/// Rebuilt on theme/palette change, published via ArcSwap so the
	/// renderer's read is lock-free. Hashable so the cell-grid
	/// renderer folds it into MatrixVersion (existing pattern,
	/// theme.rs S2.3.a) — a theme change bumps the version, cells
	/// rebuild with fresh styles.
	pub struct ResolvedTheme {
		styles: Box<[Style]>,         // indexed by ElementId.0
		version: u64,
	}
```

`ResolvedTheme::get(id)` is the only thing the hot path calls. The
syntax highlighter, gutter renderer, virtual-row renderer, etc. each
hold their `ElementId`s (interned once at startup) and read by index.

---

## 4. Ownership boundary

The crux, and the standing-rule check ([[feedback_mode_owns_its_surface]],
the substrate-vs-mode distinguishing rule in CLAUDE.md):

| Concern | Owner | Why |
|---|---|---|
| Register a **custom** element + its default `StyleSpec` | **the mode/plugin** | The element only exists because the mode produces it; it owns production AND default styling. |
| Reference a **builtin** element when emitting content | the mode (reference only) | "Reuse existing element, theme styles it." The mode does not carry a style for it. |
| The **palette** + overrides of **builtin** element styles | **the theme** (a contributor) | A theme is a palette + a set of element-style overrides; nothing more. |
| Element registry service, resolution, the resolved table, the renderer read | **the host** | Generic, uniform-host machinery — the renderer reads `resolved[id]` for *every* element across *all* buffers. Uniform-host consumer ⇒ host-side, exactly like `Document::display_line_numbers` (CLAUDE.md substrate-vs-mode rule). |

This is **not** a half-migration: the mode contributes the element
*and* its default *and* references builtins by name — it never
leaves the host to hardcode a style for it. The acid test from the
mode-ownership rule holds: a new provider crate that needs styling
adds **zero** `Style` fields to `host_theme` and **zero** match arms
in the renderer — it registers elements and references them. The
host's resolved-table read is generic and stays generic.

> **Standing-rule check (mode ownership):** ✅ — the binding (which
> element a row/glyph carries) AND the handler (the element's default
> style) both live with the mode that owns the buffer. The host owns
> only the generic resolve+read. No chord/handler/data half lands in
> the host.

Concretely, this **deletes** the `multibuffer_header_*` fields that
§3.8 of `multibuffer-views.md` adds to `host_theme`: the multibuffer
mode registers `multibuffer.excerpt_header[.path|.count]` with
palette-referencing defaults and the renderer reads them by id. The
faint-red deletion-block accident is gone because the header has a
real element instead of borrowing `diff.deletion_block`.

---

## 5. Override scopes — "override builtins wherever applicable"

Three composable override seams, in priority order. The user
requirement ("modes/plugins should be able to override builtins
wherever applicable") needs all three; the second is the one that
makes the markdown/org example work without disturbing other
buffers.

1. **Theme-global override.** A theme (or `:set ui.*`, user TOML,
   init.rs) re-styles a builtin element. Global; affects every
   buffer. Emacs: theming a builtin face. Vim: `:hi` a builtin
   group.
2. **Buffer-local / mode-local remap.** A *mode* overrides how
   elements resolve **for its own buffers only**, layered on top of
   the global theme. This is emacs `face-remap-add-relative` —
   exactly how `variable-pitch-mode` and org/markdown make *their*
   buffers proportional and scale headings without touching global
   state. It composes through the **`FrameView::for_buffer`
   resolver seam** ([[project_per_buffer_options_direction]],
   `buffer-local-options.md` §3): theme resolution slots into the
   same per-buffer resolution stack the options system already
   owns.
3. **Default fallback.** An element with no override resolves
   through its `inherit` chain to its owner's default (which
   references the palette, so it harmonizes with any theme out of
   the box).

The resolution stack (extending `buffer-local-options.md` §3 to
styling):

```
	1. Buffer-local element remap   — mode-local face-remap (markdown
	                                  → proportional family + scaled headings)
	2. Theme override               — active theme's element-style overrides
	3. Element default (owner)      — mode/core default StyleSpec (palette-ref)
	4. Inherit chain / dotted fallback — markdown.heading.1 → markdown.heading → markdown
	5. Hard default                 — last-resort concrete style; logs if reached via missing palette key
```

Buffer-local remaps resolve into a **per-buffer resolved table**
(or a small overlay over the global table) so the read stays O(1) —
the renderer reads the buffer's table, not a merge at paint time.
Because a markdown buffer's richness comes from *its* resolved table
(selected by the buffer's view, never by `match buffer_kind`),
[[feedback_buffers_no_special_case]] holds: the renderer never
branches on kind; it reads the element the row carries.

---

## 6. Renderer-peer asymmetry + rich rendering (markdown/org)

The reason the vocabulary is *open* (§3.2/§3.4 carry `scale`,
`family`, `weight`): emacs-grade markdown/org rendering. The honest
decomposition is **two layers**, and only the first is this design.

### 6.1 Layer 1 — the face/style vocabulary (this design)

Emacs renders bigger headings via the face attribute `:height` (a
float multiplier) and proportional text via `:family`. To match
that, the resolved `Style` carries `scale`/`family`/`weight`, and an
element default can set them (`markdown.heading.1 → {scale: 1.6,
weight: bold, fg: Palette("red")}`). This is squarely in scope.

- **GPUI peer honors it.** gpui `TextRun`s carry a `Font`
  (family/weight/style) and a per-run size; rendering different font
  sizes is how Zed renders. `scale`/`family`/`weight` map directly
  onto the run shaping the GPUI peer already does.
- **TUI peer degrades gracefully.** A fixed cell grid cannot do
  variable font size — `scale`/`family` are no-ops; the heading
  renders bold + colored + (optionally) underlined, exactly like
  terminal emacs or `glow`. This is the correct outcome, not a
  defect ([[feedback_icon_palette]] degradation principle generalizes:
  vocabulary the substrate can't honor degrades, it never breaks).

### 6.2 Layer 2 — display/layout transformation (NOT this design)

The truly rich org/markdown experience — variable **row height**,
proportional reflow, inline images, folded/replaced markup,
prettified entities — is driven in emacs by overlays + `display`
text-properties, and it breaks the **uniform-line-height cell-grid**
model the renderer is built on (the same model the soft-wrap work
counted display rows against; variable height changes the scroll
math). On GPUI this is where "real UI components" live — genuine
element trees for a rendered block, an image element, a flex table —
and it is *more* than a font-size attribute.

This is a **separate, larger renderer initiative**, gated behind the
soft-wrap display-row model and `design.md` §5.6.7 "Path 4 — full
inline media blocks" (already post-1.0). This design **enables and
does not foreclose it**: open vocabulary now, renderer reads a
resolved per-element `Style`, so when the GPUI display pipeline
learns variable row height + components, a markdown mode's registered
elements + buffer-local remap (§5) light up richly on GPUI and
degrade on TUI with **no theme-layer change**.

### 6.3 The guardrail (paramount #1, non-negotiable)

"Real UI components" on GPUI stay **O(viewport-lines) fan-out**,
produced from a *prepared display model*, **off the per-frame hot
path**. The forbidden pattern is explicit ([[feedback_no_ui_thread_work]]):
per-char `div()` cells, element trees rebuilt every frame
proportional to content. A markdown view rendered as components is
fine; a component tree recomputed per keystroke is the line. The
markdown mode produces a richer *display model* (off-thread, like
every provider); the renderer fans it out bounded by viewport.

### 6.4 Renderer-neutral, crate location

The element/style/palette types are **renderer-neutral**: an element
resolves to a semantic identity + a `Style` in the open vocabulary.
It never says "cell" or "component". Each peer interprets it with its
own richness. The theme layer's one job is to **not bake terminal-grid
assumptions into the element/style model** — which is why `scale`/
`family` exist in the type even before the TUI can honor them.

Types live in a new **`lattice-theme`** crate (depended on by
`lattice-host`, `lattice-mode`, `lattice-cells`, both renderers).
Rationale mirrors the `lattice-multibuffer` extraction
(`multibuffer-views.md` §3.6): modes can't depend on `lattice-host`,
so the registration types + registry trait must live in a crate both
host and modes share — `lattice-mode` is the minimum, a dedicated
`lattice-theme` is cleaner and matches the one-tree-per-concern
precedent. The registry itself is a **ServiceRegistry service**
([[feedback_servicesregistry_arc_typeid]]: register and look up the
same `ThemeRegistryHandle = Arc<dyn ThemeRegistry>` type).

---

## 7. Performance contract

Paramount goal #1. Stated as invariants CI pins:

- **Resolution is O(elements), at theme-build time only.** Never on
  the per-keystroke or per-frame path. Triggered by theme load,
  palette change, `:set ui.*`, `:colorscheme`, or a buffer-local
  remap — all rare, user-initiated, not per-keystroke.
- **The read is O(1): `resolved[id]`** — an array index into
  `Box<[Style]>`, no string hashing, no inherit walk, no palette
  lookup. Elements intern their `ElementId` once at startup and hold
  it.
- **Theme change re-resolves + re-renders affected buffers** (a full
  re-render is acceptable: theme change is not a hot path; the
  nerd-font toggle already does this). Decoration-retention
  ([[project_decoration_retention_status]]) is unaffected — a theme
  change legitimately invalidates cells; a *focus* change still
  recomputes nothing.
- **`ResolvedTheme` is `Hash`**, folded into `MatrixVersion`
  (existing `theme.rs` S2.3.a pattern) so a palette change bumps the
  cell-matrix version and rebuilds with fresh styles — no separate
  invalidation path.
- **Bench coverage**: resolve-cost vs element-count (must be flat
  per-glyph regardless of registry size); a `theme_read_is_index`
  assertion; the keystroke→glyph ratchet must not move on theme
  changes that don't occur mid-typing.

---

## 8. Configuration + introspection surface

- **`:set ui.*`** continues to write palette entries + builtin
  element overrides; the `parse_color` path grows hex support
  (deferred today behind a truecolor check — now unblocked since the
  palette is the indirection point).
- **`:colorscheme <name>`** swaps the active palette + override set.
  Multi-theme = a registry of named `(Palette, overrides)` pairs.
- **User TOML** declares a palette + builtin overrides (the
  static-settings half, per CLAUDE.md "TOML covers static option
  overrides only"); programmable theming (computed palettes,
  conditional overrides) lives in init.rs WASM.
- **Self-documenting help** (design.md §5.11): every element carries
  a `doc` string and owner. `:describe-element <name>` /
  `:describe-face` opens a buffer-backed view of the element, its
  resolved style, its owner, and its inherit chain. `:customize`
  opens a type-aware editing buffer over the palette + overrides
  (design.md §5.12). This falls out of the registry for free —
  elements are introspectable data, not scattered literals.

---

## 9. Cross-editor reference

Weighted per [[feedback_editor_design_references]] — Helix/Zed for
the substrate (resolved-table, gpui rich runs), Vim/Emacs for the
customizability/extensibility model (this is the extensibility axis;
the user correctly anchored on emacs):

| Editor | Element model | Palette | Reference-not-absolute | Plugin registers element | Rich vocab |
|---|---|---|---|---|---|
| **Emacs faces** | named faces, `defface` defaults | modern themes (modus/ef) define named palette + role map | `:inherit` (compose/override) | **yes** — plugins `defface`, themes override | **yes**: `:height` float, `:family`, `:box`, `:overline` + overlays |
| **Vim/Neovim hi-groups** | named groups | by convention only | `:hi link` | yes — plugins define + `default` | terminal-bound |
| **Helix** | dotted scopes, hierarchical fallback | **explicit `[palette]`** + scope refs | scope → palette name | no plugin system yet | terminal-bound |
| **Zed** | grouped fixed schema + syntax map | absolute hexes | no | no (closed schema) | gpui *can*; schema doesn't expose |

The design takes emacs's semantics (the ceiling for customizability,
incl. `:inherit` and rich attrs), Helix's file shape (palette +
dotted elements + fallback — Rust, similar substrate, proves it
works), and Zed's evidence that a gpui renderer honors rich per-run
styling. None of the terminal editors can do Layer-1 variable font
size; emacs (GUI) and a gpui renderer can — which is the whole point
of the open vocabulary.

---

## 10. Why this shape — options + rejected alternatives

> **UX (higher court):** all options keep the keystroke contract —
> resolution is off the hot path; a theme switch re-renders affected
> buffers (existing nerd-font-toggle discipline), introduces no
> per-keystroke cost. Neutral on UX *iff* resolution stays off the
> hot path — a hard §7 requirement, not a default.

**Option A — keep the flat struct; hoist the scattered literals into
new named fields.**
> **Paramount goals:** protects #1 (zero indirection). Sacrifices #2
> — a struct field-add is a *core code change per element*; a
> markdown/org plugin can never register `markdown.heading.1`.
> **Heuristic #1 (merit):** the risk-averse "it works, keep extending
> it" path the heuristic forbids as a tiebreaker. Pays the parity
> debt but forecloses the stated requirement.
> **Heuristic #2:** anchored on nothing but "smaller diff."

**Option B — element registry + palette + inheritance (this design).**
> **Paramount goals:** *serves* #2 (the registry IS the extensibility
> seam, WIT-shaped). Protects #1 by construction via
> resolve-once-into-flat-table (§7). Reinforces everything-is-a-buffer
> — elements keyed by semantic role, never `match buffer_kind`;
> *removes* the multibuffer-header-borrows-diff-color smell.
> **Heuristic #1 (merit):** genuinely-better long-term design with a
> concrete, nameable merit — a closed struct *cannot* let a plugin
> contribute a styleable element; a registry can. Not novelty; the
> only shape that satisfies #2.
> **Heuristic #2:** anchored on goal #2, not "emacs does it."
> **Standing-rule (mode ownership):** ✅ (§4) — registration + default
> stay with the mode; host owns only the generic resolve+read.
> **Cost, not soft-pedaled:** an interning + resolution layer, a
> registration service, a resolved-table rebuild on theme change,
> migrating ~40 fields + scattered literals + the syntax `match` into
> elements, and getting the WIT registration shape right (deferred to
> the plugin phase but not foreclosed). A real multi-slice build; if
> resolution ever leaks onto the hot path it regresses #1 — mitigated
> by the §7 flat-table discipline, which CI pins.

**Option C — Zed-style grouped fixed schema (ThemeColors /
SyntaxTheme / StatusColors), absolute colors.**
> **Paramount goals:** protects #1; better-organized than A. Still
> sacrifices #2 (closed schema, no plugin-registerable element).
> Absolute colors fight the "reference the palette" requirement.
> **Heuristic #1:** a cleaner A; doesn't clear the extensibility bar.
> **Heuristic #2:** "Zed does it" — and Zed *can't* do plugin-defined
> UI elements, the exact requirement.

**Recommendation: Option B**, because it protects paramount-#2 (the
element registry is the extensibility seam — modes register and own
custom elements; themes own palette + builtin overrides) while
resolve-once-into-flat-table keeps paramount-#1 intact. A and C fail
the requirement that modes register and own custom elements.

Locked parameters inside B: **palette (named colors, Helix) AND
inherit (emacs) both** — orthogonal, each carries weight (palette →
multi-theme by swap; inherit → "reuse builtin, theme styles it" with
no duplication). **ElementIds interned to small integers**, never
strings on the read path. **Full rich vocabulary specced now**
(`scale`/`family`/`weight`), **implemented incrementally** as each
renderer learns to read it — open vocabulary guarantees the
markdown/org future without shipping attrs no renderer honors yet
(respecting "don't add beyond what the task requires" while not
designing a closed struct that forecloses Layer 1).

---

## 11. Deferred / not in scope

- **WIT plugin registration** of elements (a WASM plugin calling
  `register`) — the trait surface is designed for it (`ElementOwner::Plugin`),
  but the WASM host lands in the plugin phase (mirrors
  `multibuffer-views.md`'s M.10 deferral). In-tree modes use the
  Rust registry now.
- **Layer 2 display/layout** (variable row height, inline media,
  proportional reflow, real-component blocks) — separate renderer
  initiative (§6.2), gated behind the soft-wrap display-row model +
  Path 4.
- **Box / overline / underline-style / blink** attributes — land in
  `StyleSpec`/`Style` when a renderer reads them; the vocabulary is
  additive.
