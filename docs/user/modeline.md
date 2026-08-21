---
summary: "The per-pane status row: zones, the modal tag, and configuring the layout."
---

# Modeline

The **modeline** is the one-row status footer at the bottom of every
pane: the modal tag, buffer path, cursor position, language, and any
mode/plugin badges (LSP progress, diff counts, …). It is a registry of
**elements** placed into three **zones** — you choose which elements go
where, and in what order.

> Not the *headerline* (the row at the **top** of a buffer for
> async/multibuffer progress). This doc is the bottom status row.

## Quick reference

| You want…                          | Do this                                              |
|------------------------------------|------------------------------------------------------|
| Move an element to another zone    | `:set ui.modeline.left=core.mode,core.path,lsp`      |
| Restore a zone's default layout    | `:set ui.modeline.right=auto`                        |
| Blank a zone                       | `:set ui.modeline.center=` (empty)                   |
| Change the separator (auto-spaced) | `:set ui.modeline.separator=·` → shows ` · `         |
| Add/remove start/end margin        | `:set ui.modeline.padding=2` (or `0` to flush)       |
| Edit it all in a buffer            | `:customize modeline`                                |
| See the full mode name             | it flashes in the echo area on every mode change     |

## Zones

Three zones, laid out left → right across the row:

- **Left** — flush-left (`core.mode`, `core.path` by default).
- **Right** — flush-right as a block (`lsp`, `core.position`,
  `core.lang` by default).
- **Center** — centred in the gap; empty by default, the home for
  custom / plugin elements.

Within a zone, elements render in the order you list them (or, by
default, in their built-in priority order). When the row is too narrow,
Center is sacrificed first, then Right, then Left.

## Built-in elements

| Id              | Shows                                  | Default zone |
|-----------------|----------------------------------------|--------------|
| `core.mode`     | lean modal tag (`NOR`, `INS`, …)       | Left         |
| `core.path`     | buffer path + `[+]` when modified      | Left         |
| `core.position` | cursor `line:col`                      | Right        |
| `core.lang`     | detected language                      | Right        |

Modes contribute their own ids — `lsp` (server / progress badge),
`diff` (`+N ~M` hunk counts), `claude-code` (the IDE server status on
the agent terminal: `claude :PORT` + connection count, see
[`claude-code-mode`](help:claude-code-mode)) — and plugins will register more. Any
registered id can be placed in any zone.

## The modal tag and the echo

The modeline shows a **lean 3-letter tag** for the current mode, no
brackets:

| Mode            | Tag   | | Mode             | Tag   |
|-----------------|-------|-|------------------|-------|
| Normal          | `NOR` | | Command          | `CMD` |
| Insert          | `INS` | | Search           | `SEA` |
| Visual          | `VIS` | | Replace          | `REP` |
| Select          | `SEL` | | Operator-pending | `OPN` |
| Terminal        | `TRM` | | Terminal-insert  | `TIN` |

The colour (the `modeline.mode` theme role) carries the rest. When you
**change** mode, the **full** name flashes in the echo area, vim-style
(`-- INSERT --`, `-- VISUAL LINE --`). That echo is transient — it never
clutters `:messages`, and returning to Normal clears it.

## Configuring the layout

Five typed options under the `modeline` group:

```toml
[ui.modeline]
left      = ["core.mode", "core.path"]
center    = []                              # custom / plugin zone
right     = ["lsp", "core.position", "core.lang"]
separator = "|"                             # shown as " | " (auto-padded)
padding   = 1                               # blank margin at row start/end
```

- **Omit a key** → that zone keeps its **default** (descriptor-driven)
  layout. This is why a newly-installed plugin's badge shows up without
  you editing config: unset zones auto-include registered elements.
- **List ids** → exactly those, in that order. Unknown ids are silently
  skipped (logged at `debug`).
- **`[]`** (empty list) → an explicitly-blank zone.
- Moving an id into an explicit zone removes it from any default zone,
  so it never shows twice.

From the cmdline, lists use commas and `auto` is the keyword that
restores the default:

```
:set ui.modeline.right=lsp,core.position,core.lang
:set ui.modeline.left=auto
```

`:customize modeline` opens a buffer where you can edit all five at
once.

### Spacing

Two knobs control breathing room:

- **`separator`** — the glyph between elements. You give just the glyph;
  the modeline pads it with a space on each side automatically, so
  `:set ui.modeline.separator=|` shows ` | `. (That's why `|` and `" | "`
  look the same — the surrounding spaces are added for you.) A blank
  separator is a single space.
- **`padding`** — columns of blank margin at the row's start and end.
  Default `1`; set `0` to flush content to the pane edges, or a larger
  number for a roomier bar.

## Clicking modeline elements

An element can declare a command to run when you click it. Built-in
elements declare none — nothing in the default modeline is clickable —
but modes and plugins can, and when they do, both the terminal and the
GPUI window honour it.

In the terminal you have to opt in first:

```
:set ui.mouse
```

It's off by default on purpose. While the editor is reading the mouse,
your **terminal** isn't — click-drag text selection and middle-click
paste stop working inside Lattice. Some terminals let you hold Shift to
get them back; not all do. Rather than take that away from everyone for
a feature few use yet, you turn it on when you want it. `:set noui.mouse`
turns it back off immediately, so it's reasonable to flip on when you
need it and off when you want to copy something.

The GPUI window ignores this option entirely — it owns its own input, so
listening for clicks costs you nothing there.

Only the left button acts. Clicking a separator, the padding, or an
element that declared no command does nothing at all. An element that
got shortened to fit a narrow pane is still clickable — half a label is
still that element.

In the GPUI window, elements that declare hover text also show a tooltip.
Terminals have no hover, so there is nothing to show there.

## See also

- [Options and configuration](help:options) — `:set`, `:customize`, the
  typed-option system these keys live in.
- [Display & layout](help:display) — soft-wrap, gutter, whitespace.
- [Modal editing](help:modal-editing) — the modes the tag reflects.
