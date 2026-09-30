<!-- @generated from wit/ by crates/lattice-plugin-api (render.rs).
     Do not edit: run `UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api`. -->

# `theme`

**Direction:** guest calls into the host through it · **Capability:** none (pure data / dispatch) · **Worlds:** `theme-plugin` (imports)

Mirrors the theme-element registry (`lattice-theme`). A plugin declares the
elements it paints with (name + doc + default style); the host registers each
into the SAME registry builtins live in, under `SourceLayer::Plugin(id)` so
unload reverses it. A plugin-registered element is then indistinguishable
from a builtin: themes override it, `:customize` edits it, `:describe-element`
documents it.

This closes the deferred item in `theme-system.md` — WIT element registration
was designed there and waited for a real consumer, which the sticky-context
plugin is (TC.4/TC.5).

**Why a plugin registers elements rather than naming colours.** The
alternative — the plugin passes literal colours, or names host-owned
`context.*` builtins — puts the palette in the plugin (so a `:colorscheme`
swap cannot touch it) or the element vocabulary in the host (so the plugin
cannot be uninstalled without leaving debris in `:customize`). Registering
the element and letting the theme own what it looks like is the only shape
where both stay where they belong.

## Functions (2)

### `register-element`

```wit
register-element: func(name: string, doc: string, default: style-spec) -> result<_, string>
```

Declare a theme element with its default style.

**Auto-namespaced**, like `config.register-option`: `name` is prefixed
with the plugin's id, so a plugin with id `treesitter-context`
registering `background` contributes `treesitter-context.background`.
The host owns the namespace, so plugins cannot collide with each other
or shadow a builtin.

Idempotent by name (the native registry's contract): re-registering
returns the existing id and leaves its default unchanged, so a reload
is free. `err` when the spec is malformed — never a trap, and never a
partially-registered element.

**Example — Register themeable elements: a palette colour, an inheriting style, a literal RGB** · [`crates/lattice-plugin-host/tests/fixtures/theme-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/theme-guest/src/lib.rs)

```rust
let _ = register_element(
    "background",
    "The context strip backdrop.",
    &StyleSpec {
        inherit: None,
        fg: Some(ColorRef::Palette("overlay".to_string())),
        bg: None,
        modifiers: no_modifiers(),
        scale: None,
    },
);
let _ = register_element(
    "active",
    "The innermost context row.",
    &StyleSpec {
        inherit: Some("treesitter-context.background".to_string()),
        fg: None,
        bg: None,
        modifiers: ModifierSet {
            bold: Some(true),
            italic: Some(false),
            underline: None,
            dim: None,
            reverse: None,
        },
        scale: None,
    },
);
let _ = register_element(
    "separator",
    "The rule under the context strip.",
    &StyleSpec {
        inherit: None,
        fg: Some(ColorRef::LiteralRgb(0x11_22_33)),
        bg: Some(ColorRef::Default),
        modifiers: no_modifiers(),
        scale: None,
    },
);
```

### `set-element-override`

```wit
set-element-override: func(name: string, style: style-spec) -> result<_, string>
```

TK.5: override an element this plugin owns, ABOVE the theme.

`register-element` supplies a *default*, which sits BELOW the active
theme in the resolution stack (`theme-system.md` §5) — so a plugin
cannot express "the user configured this and it must win" with a
default alone. This is that missing step, and it is what lets an
org-shaped `org.todo-keyword-styles` behave the way
`org-todo-keyword-faces` does in emacs.

**Auto-namespaced exactly like `register-element`**, which is what
bounds it: the prefix is the calling plugin's id, so a plugin can only
ever name elements inside its own namespace and cannot restyle a
builtin or another plugin's element. The host re-checks ownership
anyway — namespacing is the mechanism, the check is the guarantee.

`err` for an element this plugin has not registered, so a typo is a
named refusal rather than an override that lands nowhere.

##### Lifetime, which is a real limitation

`:colorscheme` replaces the palette AND the whole override map
atomically, so an override set here does not survive one. Re-applying
after a colourscheme change needs the `theme` import to be reachable
from a path that is alive when the change happens; today this seam's
store is dropped when `register-theme-elements` returns. Documented
rather than worked around.

## Types (3)

### variant `color-ref`

```wit
variant color-ref {
    palette(string),
    literal-rgb(u32),
    default,
}
```

A colour by reference. Mirrors `lattice_theme::ColorRef`.

`palette` is the path a plugin should normally take: it names a key in
the ACTIVE palette (`"blue"`, `"overlay"`, `"text"`), so the element
re-colours when the user swaps colourscheme. `literal-rgb` is the escape
hatch for a colour no palette key expresses; `default` means the
terminal/window default channel.

An unknown palette key resolves to the inherited parent rather than
failing loudly — the same forgiving resolution native elements get. The
symptom of a typo is therefore "everything looks the same", not a crash.

### record `modifier-set`

```wit
record modifier-set {
    bold: option<bool>,
    italic: option<bool>,
    underline: option<bool>,
    dim: option<bool>,
    reverse: option<bool>,
}
```

Tri-state modifiers. Mirrors `lattice_theme::ModifierSet`: `some(true)`
sets, `some(false)` CLEARS an inherited one, `none` leaves it
unspecified. The three-way distinction is load-bearing — an element that
inherits a bold parent must be able to turn bold off, which a plain bool
cannot express.

### record `style-spec`

```wit
record style-spec {
    inherit: option<string>,
    fg: option<color-ref>,
    bg: option<color-ref>,
    modifiers: modifier-set,
    scale: option<f32>,
}
```

How an element is styled, by reference. Mirrors
`lattice_theme::StyleSpec`.

`family` and `weight` are deliberately ABSENT. `family` is an interned
`FamilyId` a plugin cannot produce — crossing it would need a
name-to-id interning contract that no consumer has asked for — and
`weight` is a variable-font axis whose only users are native heading
treatments. Shipping half-designed fields to "size the ABI" is worse
than adding them when something needs them; the WIT is explicitly
unstable until three real plugins have exercised it (plugin-host.md §12).

**Fields**

- `inherit`: `option<string>` — Inherit another element's resolved style; this spec's set fields
  override. The name is resolved at theme-build time, so inheriting an
  element that does not exist yet is fine as long as it exists by the
  time the table is built.
- `fg`: `option<color-ref>`
- `bg`: `option<color-ref>`
- `modifiers`: [`modifier-set`](#record-modifier-set)
- `scale`: `option<f32>` — Relative height ratio (the emacs `:height` float). Quantized to
  fixed-point at resolution.

