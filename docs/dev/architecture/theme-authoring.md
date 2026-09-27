# Authoring themes

The practical **how-to** for building a theme, adding one to the built-in
catalog, restyling individual elements, and registering new styleable elements
from a mode or plugin. The *why* — the element/style/palette model and the
decisions behind it — lives in [`theme-system.md`](theme-system.md); read that
for rationale, this for steps. Sequencing/status is in
[`../operations/slice-plans/theme-system.md`](../operations/slice-plans/theme-system.md).

All paths are under `crates/lattice-theme/src/` unless noted.

## The model in one minute

- **Palette** (`palette.rs`) — a named set of base colours the active theme
  owns, keyed by **role key** (`"purple"`, `"base"`, `"diff.add.bg"`). Swapping
  the palette re-colours everything that references it. This is the multi-theme
  primitive.
- **Element** (`element.rs`) — a named, semantic styleable *role*
  (`syntax.keyword`, `modeline.active`, `syntax.code_block`). Carries no colour,
  only identity + owner + a default `StyleSpec`.
- **StyleSpec** (authoring form) — how an element is styled: palette-referenced
  colours + modifiers, optionally inheriting another element. This is what a
  theme override, a mode default, or a user override writes.
- **Style** (resolved form, `lib.rs`) — the concrete, fully-resolved style the
  renderers read (every colour a literal). Produced once at theme-build time and
  read O(1) by `ElementId`.

Dataflow: `ElementName → ElementId` (interned at registration) → resolve each
element's `StyleSpec` (walk `inherit`, look up each palette reference in the
active palette) → a flat `ResolvedTheme.styles` table indexed by `ElementId`.

**The golden rule:** style elements with *palette references*, never baked
literal colours. A bare string in a `StyleSpec` (`fg("purple")`) is a palette
key by default (`element.rs` `From<&str> for ColorRef`); that indirection is
exactly what lets a `:colorscheme` swap re-colour the whole surface.

## Add a new built-in theme (the common case)

A theme is a `NamedTheme { name, palette, overrides }` (`themes.rs`). In the
overwhelming common case `overrides` is empty and *all* the work is the palette
— re-colouring by indirection alone.

**Step 1 — write a palette constructor** in `palette.rs`. Copy an existing one
(`gruvbox_dark_palette`) as the template and fill **every** role key it defines
(the completeness test below enforces this):

```rust
pub fn mytheme_dark_palette() -> Palette {
    use NamedColor as N;
    let rgb = Color::Rgb;
    Palette::new()
        // ---- accents (truecolor) ----
        .with("text", rgb(0xEB, 0xDB, 0xB2))
        .with("overlay", rgb(0x92, 0x83, 0x74))
        // … green/purple/yellow/orange/blue/teal/red/maroon/pink/cyan …
        // ---- ANSI-named chrome (theme-INDEPENDENT — copy verbatim) ----
        .with("ansi.red", Color::Named(N::Red))
        // … ansi.green/yellow/blue/magenta/cyan/darkgray …
        // ---- canvas ----
        .with("base", rgb(0x1D, 0x20, 0x21))   // editor background
        .with("mantle", /* … */)
        // … crust/surface0/surface1/surface2 …
        // ---- one-off tints ----
        .with("diff.add.bg", /* … */)
        // … diff.change/deletion/conflict/*.refine.bg, cursor_line.bg …
}
```

**Step 2 — register it** in the catalog (`themes.rs`): import the fn and push a
`NamedTheme` onto the `builtin_themes()` vec:

```rust
NamedTheme { name: "mytheme-dark", palette: mytheme_dark_palette(), overrides: Vec::new() },
```

**Step 3 — done.** At boot `InMemoryThemeRegistry::with_defaults()` seeds the
catalog from `builtin_themes()`; `:colorscheme mytheme-dark` applies it and
`:colorscheme <Tab>` completes it automatically (the picker reads
`theme_names()`). No renderer or host edit.

**Not a built-in?** `init.rs` (WASM) and future plugins contribute a theme at
runtime via `ThemeRegistry::register_theme(NamedTheme)` (`registry.rs`) —
idempotent by name — without editing the crate.

## Role keys you must define

Every builtin palette must define the same complete set (miss one and
resolution silently falls back with a one-time `tracing::warn!`; the test
`every_builtin_theme_covers_the_full_role_key_set` in `themes.rs` fails). The
canonical set is whatever `default_palette()` (Catppuccin Mocha) defines; the
groups:

- **Accents** (truecolor): `text`, `overlay`, `subtext`, `green`, `purple`,
  `yellow`, `orange`, `blue`, `teal`, `red`, `maroon`, `pink`, `cyan`.
- **ANSI-named chrome**: `ansi.red/green/yellow/blue/magenta/cyan/darkgray` —
  **theme-independent**; keep them `Color::Named(...)` in every palette (copy
  verbatim), so they degrade correctly on 16-colour terminals. Never flip these
  to RGB.
- **Canvas**: `base` (editor background), `mantle`, `crust`, `surface0`,
  `surface1`, `surface2`.
- **One-off tints**: `diff.add.bg`, `diff.change.bg`, `diff.deletion.bg`,
  `diff.conflict.bg`, `diff.add.refine.bg`, `diff.remove.refine.bg`,
  `cursor_line.bg`.

`base` and `text` are the light/dark canvas seam: `editor.background → base`,
`editor.foreground → text`. Get them right or the GPUI canvas won't invert.

## Light vs dark

There is **no `is_dark` flag** — polarity is expressed by palette values alone
(a light theme's `base` is light and `text` dark; a dark theme is the reverse).
Convention: name themes `*-dark` / `*-light` (`*-latte` for Catppuccin). The
test `light_themes_are_light_and_dark_themes_are_dark` (`themes.rs`) asserts a
`*-light`/`*-latte` theme's `base` channel-sum exceeds its `text`, and the
reverse for dark. **Hand-tune light diff/cursor-line tints** — the dark
near-black tints (`rgb(0,50,0)`-style) read as black blocks on a light canvas;
see `catppuccin_latte_palette`.

Ship both a light and a dark variant of a new theme family.

## Restyling specific elements (overrides)

When re-tinting the palette isn't enough — a theme wants a *particular* element
bold, italic, or a specific colour independent of the palette — populate
`NamedTheme.overrides: Vec<(ElementName, StyleSpec)>`. Build the `StyleSpec` with
the `spec()` helper and its chainable builders (`element.rs`):

```rust
overrides: vec![
    ("syntax.comment".into(), spec().fg("overlay").italic()),
    ("syntax.keyword".into(), spec().fg("purple").bold()),
],
```

`StyleSpec` builders: `.fg(ref)`, `.bg(ref)`, `.bold()`, `.italic()`,
`.underline()`, `.dim()`, `.reverse()`, `.no_bold()` (clears an inherited bold),
`.inherit("other.element")`, `.scale(1.6)`, `.weight(Weight::Bold)`. A colour
argument is a palette key by default; wrap a literal only as an escape hatch
(`ColorRef::Literal(Color::Rgb(...))`). Modifiers are tri-state — an override
sets only the fields it names and leaves the rest inherited.

## The element catalog

The canonical list of styleable elements is the `register_builtins()` body
(`registry.rs`) — every element the core registers with its default `StyleSpec`
and doc string — surfaced to the renderers through the `BuiltinElementIds`
struct (`registry.rs`), captured once at boot. Introspect a live build with
`:describe-element <name>` / `:apropos`. Major groups:

- **Chrome**: `pane.status.active/inactive`, `pane.separator`,
  `pane.inactive_overlay`; `modeline.active/inactive/mode/path/position/lang`.
- **Canvas**: `editor.background/foreground/cursor/cursor_line`.
- **Gutter/fold**: `gutter.sign`, `gutter.fold.open/closed/summary`.
- **Diff**: `diff.{add,change,remove,conflict}.{sign,line}`,
  `diff.{add,remove}.refine.bg`, `diff.deletion_block`.
- **Diagnostics / search / selection**: `diagnostic.{error,warning,info,hint}`,
  `search.match`, `search.current`, `selection`, `doc_highlight.{read,write,text}`.
- **Terminal**: `terminal.ansi.0`…`terminal.ansi.15`.
- **UI surfaces**: `ui.popup.{background,title,hint}`, `picker.{title,prompt,count,root}`,
  `file_tree.{dir,hidden,file}`, `whitespace[.trailing]`, `indent.guide[.active]`,
  `messages.{timestamp,trace,debug,info,warn,error}`, `sticky.context.*`,
  `transient.*`, `help.*`, `completion.annotation.*`.
- **Mode-owned** (registered by their mode, not core): e.g. `magit.*`.
- **Syntax**: see below.

## Syntax highlighting elements

Tree-sitter capture → concrete style is two stages:

1. **capture name → syntax `Style` category** (`lattice-syntax/src/style.rs`):
   `CAPTURE_NAMES` maps nvim-treesitter convention names (`keyword.control`,
   `string.escape`, `text.title.1`, `text.literal`, …) to a `Style` variant;
   `name_to_style()` matches on the dotted head, `capture_priority()` gives
   overlap precedence.
2. **`Style` → `ElementId` → resolved `Style`** (`lattice-syntax/src/theme_style.rs`):
   `syntax_element_id(ids, style)` maps each variant to a `syntax.*` element
   (`Style::Keyword → syntax.keyword`); `resolve_syntax_style(resolved, ids,
   style)` reads it.

The `syntax.*` elements: `default`, `comment`, `line_comment`, `string`,
`keyword`, `type`, `number`, `function`, `constant`, `variable`, `operator`,
`punctuation`, `attribute`, `heading.1`…`heading.6`, `bold`, `italic`, `link`,
`url`, `markup_raw`, `code_block`, `markup`. Heading elements carry `scale` +
`weight` (GPUI renders variable row height; TUI degrades to bold+colour).

**`syntax.code_block`** is a background-only element — the full-row tint behind a
fenced/indented (or org) code block. Which lines get it is grammar-declared via
a `@codeblock` capture in the folds query, not node kinds — see
[`plugin-languages.md`](plugin-languages.md) §2.2.1. A theme styles the block
background purely by setting `syntax.code_block`'s `bg`.

**TK.1 — a capture may name a registered element.** If a capture name isn't a
builtin category and a theme registry is threaded in,
`name_to_style_with_theme()` returns `Style::Element(id)`, so a plugin/mode query
capture (e.g. `@org.todo.WAITING`) paints with a registered element. Builtin
names always win; element captures inherit `keyword`'s overlap priority so they
don't silently lose overlaps.

## Registering NEW elements (modes & plugins)

An element is registered once, then referenced by `ElementId`:

- **Native (mode / in-tree):** `ThemeRegistry::register(name, owner, default,
  doc) -> ElementId` (`registry.rs`), idempotent by name. `owner` is
  `ElementOwner::{Core, Mode(id), Plugin(id)}`. Modes reach the registry through
  the `ThemeRegistryHandle` service, never `&mut Editor`.
- **WASM plugin:** the `theme` WIT interface (`crates/lattice-wit/wit/theme.wit`)
  — `register-element(name, doc, default)` and `set-element-override(name,
  style)`. The host **auto-namespaces**: a plugin `treesitter-context`
  registering `background` gets `treesitter-context.background`
  (`lattice-plugin-host/src/theme_host.rs`); registration is idempotent so
  reload is free. Two limits to know: the WIT `style-spec` omits `family`/`weight`
  (colour + modifiers only), and a plugin's `set-element-override` does **not**
  survive a `:colorscheme` swap (the new theme replaces the palette + override
  map atomically) — documented, not worked around.

A new theme rarely registers elements; it re-tints existing ones. Registering is
for a *mode/plugin that introduces a new styleable role* (magit's `magit.sha`,
org's TODO faces).

## User overrides

- `:set ui.*` typed options (`lattice-host/src/ui/theme_options.rs`) drive
  theme-global overrides on a few chrome elements (`pane.separator`,
  `pane.status.*`); `Editor::sync_host_theme_from_config()` applies them via
  `set_override`.
- `parse_color` (`lib.rs`) accepts the 16 ANSI names, `default`/`reset`, and
  6-digit hex (`#cba6f7` or `cba6f7`).
- TOML covers static palette/element overrides; anything programmable
  (conditional theming) lives in the `init.rs` WASM module.

## Pitfalls checklist

1. Cover **both** light and dark (naming + polarity test).
2. Fill the **entire** role-key set (completeness test; a miss is a silent
   mis-theme, not a crash).
3. Reference the palette; don't bake literals (that's what makes swaps work).
4. Keep `ansi.*` terminal-named across all palettes.
5. Hand-tune light diff/cursor-line tints.
6. `editor.background→base`, `editor.foreground→text` — the canvas-inversion seam.
7. Hold `ElementId`s on any hot path (via `BuiltinElementIds`); never look up by
   name per frame. Resolution is O(elements) at build time; reads are O(1).
8. Rich vocab (`scale`/`family`/`weight`) degrades gracefully — GPUI honours it,
   TUI ignores scale/family and maps heavy weight to bold. Not a defect.

## Testing

`themes.rs` auto-covers a new theme: role-key completeness and light/dark
polarity. Add a targeted assertion if the theme makes a specific claim (a
hand-tuned tint, a non-empty `overrides`). A palette-key typo resolves to the
inherited parent (forgiving) — so "everything looks the same" is the failure
signature to watch for, not a panic.

## See also

- [`theme-system.md`](theme-system.md) — the element/style/palette design and
  rationale (the *why*).
- [`plugin-languages.md`](plugin-languages.md) §2.2.1 — the `@codeblock` /
  `@fold` folds-query capture conventions and `syntax.code_block`.
- `:describe-element` / `:apropos` / `:colorscheme` — live introspection.
