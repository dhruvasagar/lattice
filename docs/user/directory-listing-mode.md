---
summary: "directory-listing-mode: the shared presentation minor for directory listings — icons, alignment and styling for oil and the file tree alike."
related: [oil, file-tree]
---

# directory-listing-mode

The shared presentation layer for buffers that list a directory. Both
[oil](help:oil-mode) and the [file tree](help:file-tree-mode) show the same kinds
of thing — a folder, a symlink, an executable, a file with a type — and
this is the one mode that decides how those look.

## Why the presentation is its own mode

The two listings are different *interactions*: oil is a directory you
edit as text, the file tree is a persistent navigation pane. Their
presentation is not different at all. Keeping the icon set, the
alignment and the styling here means a change to how a symlink renders
lands in both, rather than in one and eventually the other.

## Icons

Two palettes, and both occupy the same cell width so the columns do not
shift when you switch:

- **Nerd Fonts** glyphs when `ui.nerd_fonts=on`.
- A **plain-Unicode** fallback otherwise, which is the default — the
  first frame has to work in whatever font the terminal already has.

Turn the richer set on with:

```
:set ui.nerd_fonts=on
```

## Keybindings

None of its own. The interaction chords belong to whichever listing you
are in.

## Options

- `ui.nerd_fonts` — which icon palette to draw.

## See also

- [`oil`](help:oil-mode) — editing a directory as text.
- [`file-tree`](help:file-tree-mode) — the navigation pane.
