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
| `:set tabstop=N`  (`:set ts=N`)  | `4`     | Columns a hard tab occupies                                    |
| `:set scrolloff=N` (`:set so=N`) | `0`     | Minimum lines kept above/below the cursor                      |
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
