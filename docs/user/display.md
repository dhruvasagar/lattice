---
summary: "Display & layout: soft-wrap, tab width, scroll-off margin, and whitespace markers."
related: [wrap, tabstop, scrolloff, whitespace]
---

# Display & layout

How buffer content is laid out on screen: wrapping long lines, how
wide a tab is, how close the cursor gets to the viewport edge, and
whether whitespace is shown. All of these are typed options set with
`:set` (see [options.md](options.md) for the `:set` mechanism and
layered resolution); this page is the deep-dive on each.

> **Status:** all features on this page ship in both the terminal and
> GPUI renderers. `wrap`, `tabstop`, `scrolloff`, and whitespace
> markers are production-ready. One known rough edge under wrap/tabs:
> selection and search **highlight rectangles** can sit a few cells
> off on tab-indented or wrapped lines (the cursor itself is correct)
> — a tracked follow-up.

---

## Quick reference

| Command                          | Default | Meaning                                                        |
|----------------------------------|---------|----------------------------------------------------------------|
| `:set wrap` / `:set nowrap`      | off     | Wrap long lines onto continuation rows vs. clip at the edge    |
| `gj` / `gk`                      | —       | Move **one display row** (wraps count); `j`/`k` always logical line |
| `g0` / `g$`                      | —       | Jump to start / end of the current display row (under wrap)    |
| `:set ui.ligatures` / `:set ui.ligatures=off` | on | Enable / disable OpenType ligatures (GPUI only) |
| `:set ui.window.decorations=none` | `full` | Borderless GPUI window — no titlebar/controls; on macOS it's non-resizable (Raycast/yabai can't move it either), opens pre-sized (GPUI only, next launch) |
| `:set ui.window.decorations=transparent` | `full` | Frameless-looking but **resizable** (Raycast/yabai work); keeps corners/shadow (GPUI only, next launch) |
| `:set ui.window.start-maximized` | off | Open maximized — resizable windows fill the work area; a borderless (`none`) window fills the whole display (GPUI only) |
| `:set tabstop=N`  (`:set ts=N`)  | `4`     | Columns a hard tab occupies                                    |
| `:set scrolloff=N` (`:set so=N`) | `0`     | Minimum lines kept above/below the cursor                      |
| `:set sidescroll=N` (`:set ss=N`) | `0`    | Columns to scroll when the cursor crosses the edge (`0` = jump to centre) |
| `:set sidescrolloff=N` (`:set siso=N`) | `0` | Minimum columns kept left/right of the cursor (`nowrap`)     |
| `zl` `zh` / `zL` `zH` / `zs` `ze` | —      | Scroll right/left N cols · half-screen · cursor to left/right edge |
| `:set list` / `:set whitespace`  | off     | Show whitespace markers (tabs, trailing/leading spaces, …)     |
| `:set display.whitespace.tab=→`  | `→`     | Glyph for a tab when markers are shown                         |
| `:set display.whitespace.trailing=·` | `·` | Glyph for trailing whitespace                                  |
| `:set display.whitespace.leading=·`  | `·` | Glyph for leading indentation                                  |
| `:set display.whitespace.space=`  | *(off)* | Glyph for interior spaces (empty = don't decorate)             |
| `:set display.whitespace.eol=`    | *(off)* | Glyph at end-of-line (e.g. `¬`; empty = don't decorate)        |

---

## Soft-wrap (`wrap`)

With `:set wrap`, a line longer than the pane wraps onto one or more
**continuation rows** instead of running off the right edge. Each
continuation row is marked in the gutter with a dim `↪`; the line
number appears only on the first row. The wrap point follows the pane
width, so resizing the window re-wraps live.

With `:set nowrap` (the default) a long line is clipped at the right
edge and the rest is off-screen.

Wrapping is purely visual — it never changes the file. `j` / `k`
still move by **logical** line (vim's default), so a single long line
is one `j` regardless of how many rows it occupies. Use `gj` / `gk`
to step one **display row** at a time, and `g0` / `g$` to reach the
start or end of the current display row.

The scroll model is wrap-aware: scrolling to the last line brings it
fully into view even when earlier lines wrapped, and the cursor is
positioned on the correct visual row.

## Horizontal scroll (`sidescroll` / `sidescrolloff`)

With `:set nowrap` (the default), a line wider than the pane scrolls
**horizontally** to keep the cursor visible: move the cursor past the
right edge and the view pans right so the rest of the line comes into
view; move back and it pans back. Only the text body pans — the
line-number and sign gutters stay fixed.

- `:set sidescroll=N` — how far to scroll when the cursor crosses the
  edge. `0` (the default, matching vim) jumps so the cursor lands at
  the **centre** of the window; a positive `N` scrolls `N` columns at
  a time for a smoother, continuous feel.
- `:set sidescrolloff=N` — keep at least `N` columns of context
  between the cursor and the left/right edge (the horizontal
  counterpart of `scrolloff`).

Manual scrolling — vim's `z` family (`nowrap` only):

| Chord | Meaning |
|-------|---------|
| `zl` / `zh` | Scroll right / left `[count]` columns |
| `zL` / `zH` | Scroll right / left half the body width |
| `zs` / `ze` | Put the cursor's column at the left / right edge |

Horizontal scroll is mutually exclusive with `wrap`: turning `wrap`
on resets the offset, because a wrapped line has nothing off-screen to
the right.

## Tab width (`tabstop`)

A hard tab (`\t`) renders as advancing to the next multiple of
`tabstop` columns — `:set tabstop=4` (the default) lines indentation
up on 4-column stops, `:set tabstop=8` on 8. This is a *display*
setting; it does not change the bytes in the file (tabs stay tabs).

When whitespace markers are on (`:set list`), the tab shows as the
`display.whitespace.tab` glyph (`→` by default) followed by spaces
filling to the next stop; with markers off it renders as blank space
to the stop.

## Scroll-off margin (`scrolloff`)

`:set scrolloff=N` keeps at least `N` lines visible above and below
the cursor, so the cursor never sits flush against the top or bottom
edge while there's more content to show (vim's `scrolloff`). The
default `0` lets the cursor reach the very edge. The margin is capped
at half the viewport, so on a short window a large value simply keeps
the cursor centred. Near the start or end of the file the margin
naturally shrinks — `G` still puts the last line at the bottom.

## Whitespace markers (`list` / `whitespace`)

`:set list` (alias for `:set whitespace`) makes otherwise-invisible
whitespace visible, vim's `listchars` idea. Each category has its own
glyph option:

- **`display.whitespace.tab`** (`→`) — tabs.
- **`display.whitespace.trailing`** (`·`) — spaces/tabs at end of line
  (rendered in the trailing style so they stand out).
- **`display.whitespace.leading`** (`·`) — indentation at line start.
- **`display.whitespace.space`** (empty) — interior spaces; off by
  default since most people find it noisy. Set to `·` for the
  emacs-`whitespace-mode` look.
- **`display.whitespace.eol`** (empty) — an end-of-line marker; set
  to `¬` for the classic vim `eol` listchar.

Setting any glyph to the empty string disables decoration for that
category. The markers degrade gracefully: the defaults are
plain-BMP glyphs that render in any terminal font, so you don't need
a patched/Nerd font to use `:set list`.

## Ligatures (`ui.ligatures`)

OpenType programming ligatures replace multi-character sequences with
a single presentation glyph: `->` becomes a right arrow, `!=` a
not-equal sign, `=>` a fat arrow, `//` a double slash, and so on.
Common ligature fonts: Fira Code, JetBrains Mono, Cascadia Code,
Iosevka.

**GPUI renderer:** ligatures are **on by default**. If you prefer the
raw characters, `:set ui.ligatures=off` calls
`FontFeatures::disable_ligatures()` before shaping, suppressing both
`liga` (standard ligatures) and `calt` (contextual alternates).

```
:set ui.ligatures=off   " disable — see individual glyphs
:set ui.ligatures       " re-enable (same as =on)
```

Ligatures naturally break at syntax-colour boundaries and at the
cursor position — the same behaviour as VSCode, Helix, and Zed. Most
operator sequences (`->`, `!=`, `=>`, `<=`, `//`, `...`) share a
single syntax token and therefore render as ligatures correctly.

**TUI renderer:** the `ui.ligatures` option is ignored. Whether
ligatures appear in the terminal depends entirely on your terminal
emulator (kitty, WezTerm, Alacritty, iTerm2 etc.) and your
configured terminal font. Lattice does not interfere — it outputs
plain UTF-8 and lets the terminal shape it.

## Window chrome (GPUI)

Two options control the OS window itself rather than buffer content —
**GPUI (`--gui`) only**; both are ignored by the terminal UI, which has
no OS window.

**`ui.window.decorations`** = `full` (default) | `none` | `transparent`.
`full` keeps the normal OS titlebar and window controls (today's
behavior, unchanged). `none` removes them for a borderless window,
matching alacritty's `window.decorations = none`, kitty's
`hide_window_decorations`, or emacs's `undecorated t`. `transparent`
looks frameless but keeps the window **resizable** (see below).

`none` behaves a little differently per platform:

- **Linux (X11):** true borderless window, still resizable via the
  window manager.
- **Windows:** borderless, still edge-resizable.
- **macOS:** no titlebar or traffic-light buttons, but the rounded
  corners and system window shadow remain. Note: a `none` window is
  **non-resizable by any means** on macOS — no edge-resize, and external
  window managers (Raycast, yabai, Rectangle) **cannot** resize or move
  it either (their Accessibility calls are rejected). It is fixed at the
  size it opens with. Use `transparent` if you need Raycast/yabai control.
- **Linux (Wayland / WSLg):** best-effort; a minimal shadow or border
  may still show.

`transparent` is the macOS-friendly frameless option: a transparent,
full-size-content titlebar with the traffic-light buttons hidden (on
macOS they're hidden with `setHidden:`, which — unlike moving them
off-screen — keeps the window controllable by Raycast/yabai). Because it
keeps a titlebar under the hood, the window stays **resizable** —
edge-resize works, and Raycast/yabai can move and resize it. The trade-off
vs `none`: the rounded corners and shadow remain (it is not truly
borderless). On Linux/Windows `none` is already resizable, so
`transparent` is mainly useful on macOS.

`ui.window.decorations` is applied when the window is created, so
changing it (via `:set` or TOML) takes effect on the **next launch**,
not live in the current session.

**`ui.window.start-maximized`** = `true` | `false` (default `false`).
When `true`, the window opens maximized. A `full`/`transparent` window
is resizable, so it maximizes to the work area (keeps the menu bar; not
native fullscreen). A `none` (borderless) window can't be maximized after
the fact on macOS, so it opens pre-sized to fill the whole display.

```toml
# Frameless + resizable + Raycast-controllable (recommended on macOS):
ui.window.decorations = "transparent"
ui.window.start-maximized = true
```
