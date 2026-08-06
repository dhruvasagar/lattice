---
summary: "Themes & colorschemes: :colorscheme to switch (with a live-preview picker), the 21 builtin themes, :customize to tweak options, and registering your own palette."
related: [colorscheme, theme, customize]
---

# Themes & colorschemes

A **theme** is the editor's colour identity — syntax highlighting,
gutter signs, the modeline, diff tints, popups, the canvas itself. In
lattice a theme is a **palette** (a set of named colour roles like
`text`, `green`, `red`, `overlay`) plus an element registry that maps
every styled surface to a role. Swapping the palette recolours the
whole editor at once; there is no per-surface theme wiring to maintain.

> One model, two renderers. The TUI inherits the terminal's own
> background/foreground for the canvas (it can't repaint the terminal),
> so swapping to a light theme there recolours syntax and chrome but
> leaves the terminal's canvas. The GPUI renderer owns its canvas and
> recolours fully, light or dark.

---

## Quick reference

| You want…                              | Do this                              |
|----------------------------------------|--------------------------------------|
| Switch to a known theme                | `:colorscheme tokyonight-dark`       |
| Browse themes with live preview        | `:colorscheme` (no argument)         |
| Vim short for the above                | `:colo gruvbox-light`                |
| Tweak a config group in a buffer       | `:customize ui`                      |
| Pick a group/mode to customize         | `:customize` (no argument)           |
| Toggle the icon palette                 | `:set ui.nerd_fonts` / `:set noui.nerd_fonts` |

---

## Switching themes

`:colorscheme <name>` switches the active theme by name. The whole
editor recolours immediately:

```
:colorscheme dracula-dark
:colo nord-light
```

`:colo` is the vim short. An unknown name echoes an error and leaves
the current theme untouched — nothing changes until a real name lands.
To browse instead of typing a name, run `:colorscheme` with no argument
— the live-preview picker (next section).

### The live-preview picker

`:colorscheme` with **no argument** opens a buffer-backed picker over
every registered theme. As you move the selection, the **whole editor
recolours to that candidate** — you see the real theme, not a swatch
(the cross-editor convention: VSCode, Zed, and telescope all preview
this way).

- `<CR>` (accept) commits the highlighted theme.
- `<Esc>` (cancel) restores the theme that was active when you opened
  the picker — byte-for-byte, no residue.

The picker is the way to shop for a theme; the explicit
`:colorscheme <name>` form is the way to set one you already know.

---

## The builtin catalog

Lattice ships **21 builtin themes**. Catppuccin contributes three
flavours; nine cross-editor families each ship a dark and a light
variant.

**Catppuccin** (the default family)

| Name                    | Tone  |
|-------------------------|-------|
| `catppuccin-mocha`      | dark (default) |
| `catppuccin-macchiato`  | dark  |
| `catppuccin-latte`      | light |

**Cross-editor families** (each `-dark` / `-light`)

| Family       | Dark              | Light              |
|--------------|-------------------|--------------------|
| Gruvbox      | `gruvbox-dark`    | `gruvbox-light`    |
| Tokyo Night  | `tokyonight-dark` | `tokyonight-light` |
| Dracula      | `dracula-dark`    | `dracula-light`    |
| Nord         | `nord-dark`       | `nord-light`       |
| Solarized    | `solarized-dark`  | `solarized-light`  |
| One          | `one-dark`        | `one-light`        |
| Everforest   | `everforest-dark` | `everforest-light` |
| Rosé Pine    | `rosepine-dark`   | `rosepine-light`   |
| Monokai      | `monokai-dark`    | `monokai-light`    |

The default on a fresh start is `catppuccin-mocha`. Markdown headings
get distinct per-level colour and (on the GPUI renderer) per-level
size — h1 down to h6 each pick up their own style, with the leading
`#` markers left at base size.

---

## Domain elements, not just syntax

Most styled things resolve through the `syntax.*` elements — keyword,
string, comment, and so on. Some do not, and deliberately: a commit SHA
is not a "link", the checked-out branch is not a "keyword", and a
keybinding in a help page is not a "type". Those get their own elements
so a theme can retune them without dragging a source-code colour along
with them.

| Element | Where it shows |
|---|---|
| `magit.sha` | commit SHAs in [magit](help:magit) log, blame and rebase views |
| `magit.branch.current` | the checked-out branch in a branch list |
| `magit.ref.decoration` | the `(HEAD -> main, …)` list after a log SHA |
| `magit.rebase.verb` | `pick` / `reword` / `squash` / … in a rebase todo |
| `magit.author` | the author column in a blame |
| `help.key` | a key you press, in a [help](help:help) page (`gr`, `<C-c>g`) |
| `help.command` | a command you type (`:magit-status`) |
| `help.action` | an action id (`action:magit-refresh`) |
| `help.literal` | any other inline literal — a path, a flag, a filename |

The four `help.*` elements are what make a help page scannable: keys
are bold so the thing you are hunting for stands out, commands take the
same colour as the `:` line you will type them on, actions are dimmer
because an action id is machinery you meet rarely, and plain literals
stay quiet so they don't compete with the three that carry meaning.
Every one is retunable — if you want keys in green, that is one element
override.

---

## Customizing options

Themes set colours; many other surfaces (icons, separators, gutter
behaviour) are typed **options**. `:customize` opens a type-aware
editing view that writes back to your TOML config:

```
:customize ui          # every customizable option in the `ui` group
:customize lsp-mode     # what a mode contributes
:customize             # a navigation picker: groups + modes
```

- `:customize` with **no argument** lists every option group and every
  mode that contributes options — pick one to drill in.
- `:customize <group>` or `:customize <mode>` opens the focused view:
  each option shows its name, type, current value, default, and doc.
- Following an option's edit link prefills the `:` command line with
  `set NAME=VALUE`, where you finish the value and accept.

For the concepts behind options (`:set`, layered resolution, where a
value comes from) see [Options](help:options). `:colorscheme` itself is
not persisted across restarts yet — set it from your `init` to make it
stick.

---

## Registering your own theme

The theme catalog is open. A custom palette registered into it gains
everything the builtins have: `:colorscheme <your-name>`, completion,
and a slot in the live-preview picker.

Registration is the `register_theme` seam, reached from your Rust-WASM
`init` (and, later, from a plugin via WIT). You supply a name and a
palette that fills the same **role-key vocabulary** the builtins
reference (`text`, `overlay`, `subtext`, `green`, `red`, `orange`,
`purple`, `cyan`, the `base`/`surface` canvas family, the `ansi.*`
chrome, …). Because every styled element resolves through those roles,
a palette alone recolours the whole editor — you don't wire individual
elements. Registering does not change the active theme; it just adds it
to the catalog, ready for `:colorscheme`.

Re-registering an existing name **replaces** that theme's palette, so
you can override a builtin in place by registering under its name.

---

## Related options

- **`ui.nerd_fonts`** — toggles the icon palette used by the file tree,
  oil, pickers, and gutter. `on` uses Nerd Fonts v3 glyphs (requires a
  patched terminal/GUI font like JetBrains Mono Nerd Font); `off` (the
  default) falls back to a BMP-block palette (`◆ ≡ ◇ ■ ♪ ▶ ·`) that
  renders in any monospace font. Flip it with `:set ui.nerd_fonts` /
  `:set noui.nerd_fonts`. Pick `off` if you see `?` boxes.

## See also

- [Options](help:options) — `:set`, `:customize`, the typed-option model.
- [Modeline](help:modeline) — the status row whose colours follow the
  active theme.
- [Display & layout](help:display) — gutter, whitespace, soft-wrap.
