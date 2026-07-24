+++
title = "Picker & marginalia"
+++



The picker is one vertico-style fuzzy finder that works over
**every** source. You type `:picker <source>`, the picker walks
that source's candidate set, and the same query line, ranking,
preview, and keymap apply no matter what you picked over — files,
grep hits, buffers, symbols, registers, commands. Recently-used
candidates float to the top automatically across every source.

Each row can carry **marginalia**: typed, colour-coded annotation
columns (a file's size and mtime, a command's keybinding and
latency class, a grep hit's `path:line:col`) that live *beside* the
matchable text rather than baked into it. Rows that contain source
code — grep hits, `:picker lines`, `:picker outline` — render that
code in the buffer's own syntax colours.

> **Status:** shipped — all first-party sources below, marginalia
> across every source, syntax-highlighted previews for
> `lines` / `outline` / `grep`, and frecency (MRU) ranking with
> disk persistence. LSP-backed pickers (references, symbols, code
> actions) live in [lsp.md](../lsp/); the theme picker lives in
> [themes.md](../themes/). Plugin-contributed sources are Phase 7.

---

## Quick reference

### Opening a picker

| Command | Opens |
|---|---|
| `:picker <source> [args]` | The named source (see the table below). `<Tab>` after `:picker ` lists sources. |
| `:files [root]`           | File picker (alias for `:picker files [root]`) |
| `:recent`                 | Recently-edited files (alias for `:picker recent`) |
| `:buffers` / `:b`         | Buffer switcher (fuzzy picker; `:ls` is the static text listing) |
| `:colorscheme`            | Theme picker, with live preview (see [themes.md](../themes/)) |

### Inside an open picker

| Key | Action |
|---|---|
| *(type)* | Filter the candidate list (fuzzy match) |
| `<C-n>` / `<Down>` / `<Tab>` | Move selection down |
| `<C-p>` / `<Up>` / `<S-Tab>` | Move selection up |
| `<CR>` | Accept the selected candidate |
| `<Esc>` / `<C-c>` | Dismiss (nothing happens; state restored) |
| `<C-s>` | Accept, opening the file in a horizontal split |
| `<C-v>` | Accept, opening the file in a vertical split |
| `<C-t>` | Accept, opening the file in a new tab |
| `<BS>` | Delete the last query character |
| `<C-u>` | Clear the query |

`<C-s>` / `<C-v>` / `<C-t>` only change *where* a file-opening
accept lands; for sources that don't open a file (registers,
commands, …) they behave like `<CR>`.

---

## The sources

Every id below is valid as `:picker <id>`. Sources marked "active
buffer" read the buffer you were in when you opened the picker.

| Source | Lists | `<CR>` does | Args |
|---|---|---|---|
| `files` | Files under the workspace root | Open the file | `[root]` — directory to walk (default: document's parent / cwd) |
| `recent` | Recently-edited files (MRU) | Open the file | — |
| `buffers` | Every open buffer | Switch to it | — |
| `lines` | Lines of the active buffer | Jump to the line | — |
| `outline` | Tree-sitter symbols in the active buffer | Jump to the symbol | — |
| `grep` | Live recursive text search (`rg` / `ag` / `grep`) | Jump to the hit | `[pattern]` — seeds the prompt; omit to start empty |
| `jumps` | Position-history ring (jump list + mark ring, newest first) | Jump to the entry | — |
| `marks` | Vim marks | Jump to the mark (same as `` ` ``) | — |
| `registers` | Vim registers (unnamed, numbered, named) | Paste the register at the cursor | — |
| `commands` | The ex-command palette | Invoke the command | — |
| `snippets` | Snippets for the active buffer's language | Expand the snippet at the cursor | — |
| `colorscheme` | Registered themes (live preview) | Commit the theme | — (usually reached via bare `:colorscheme`) |

`:picker grep` is a **live** source: it re-runs the search backend
as you type and streams hits in. It jumps to a single chosen hit —
for a persistent, editable multibuffer of *all* matches, use
[`:search`](../project-search/) instead.

### Tab completion inside `:picker`

- `:picker <Tab>` lists every source id, each with its one-line
  summary as marginalia.
- `:picker files <Tab>` (and other sources that take an arg)
  completes the argument through the same machinery `:e <Tab>`
  uses — e.g. path completion for `files`.

---

## Navigating & accepting

Type to narrow. Matching is fuzzy: characters must appear in
order, but not adjacently, and the matched characters are
highlighted in each row. Ranking blends the fuzzy-match score with
a frecency bonus (see [MRU ranking](#mru-ranking)), so a strong
match wins but recent picks break ties.

**Live preview.** Moving the selection previews the candidate. For
`lines`, `grep`, and `outline` the preview scrolls the underlying
buffer to (and centres) the target line, so you see the result in
context before committing. For `colorscheme` the *whole editor*
recolours to the highlighted theme; `<Esc>` restores exactly what
you had. Previews never touch the file on disk — dismissing leaves
everything as it was.

**Where accepts land.** By default a picked buffer or location
opens in the active pane. `:set picker.result.display=split-h` (or
`split-v`) makes every picker accept open in a new sibling pane
instead; the per-accept `<C-s>` / `<C-v>` / `<C-t>` chords override
it for one pick.

---

## Marginalia

Marginalia are the typed, colour-coded columns to the right of a
row's matchable text. Instead of a source hand-padding metadata
into one flat string, each source emits *typed segments* and the
renderer resolves each segment's colour from the theme. The row's
`display` (the part fuzzy match scores against) stays clean; the
metadata rides alongside.

What each picker surfaces:

| Source | Matchable text | Marginalia columns |
|---|---|---|
| `commands` | command name | bound keybinding · argument hint · doc summary · latency class |
| `buffers` | path | buffer id (`#N`) · status (`•` active / `+` dirty) · kind |
| `files` | path | permissions · size · mtime (eza-style) |
| `recent` | path | permissions · size · mtime |
| `grep` | matched line (preview) | `path:line:col` |
| `jumps` | buffer label | `line:col` · source tag (`auto` / `mark` / `plugin` / `'x`) |
| `outline` | symbol name | line number |
| `lines` | line text | line number |
| `marks` | mark name | `line:col` |
| `registers` | register contents | register name (`"a`, `"0`, …) |
| `snippets` | prefix (trigger) | snippet name (kind) · description |

Unbound commands, un-stattable paths, and other missing values
simply leave the cell blank — no placeholder, no error.

Marginalia is the same mechanism the insert-mode completion popup
uses for its annotation column; see
[completion.md](../completion/) for that surface.

### Colouring the columns

Every column resolves through the theme under the
`completion.annotation.*` namespace, so `:colorscheme` recolours
picker marginalia and buffer syntax in lockstep. The slots
(with default dark tones):

| Slot | Default | Used for |
|---|---|---|
| `completion.annotation.location.path` | dim | grep hit path |
| `completion.annotation.location.line` | yellow | line number |
| `completion.annotation.location.col`  | dim | column number |
| `completion.annotation.status.dirty`  | red | dirty-buffer `+` |
| `completion.annotation.status.active` | green | active-buffer `•` |
| `completion.annotation.latency.reflex` | green | `[reflex]` commands |
| `completion.annotation.latency.display` | blue | `[display]` commands |
| `completion.annotation.latency.background` | orange | `[background]` commands |
| `completion.annotation.args` | subtext | command argument hint |
| `completion.annotation.buffer-id` | dim | buffer `#N` |
| `completion.annotation.register` | purple | register / mark name |

The file-metadata columns (permissions, size, mtime) in the
`files` and `recent` pickers reuse the permission / size / mtime
slots documented in [filetree-oil.md](../filetree-oil/). Picker
rows don't carry a file-type icon column today — icons appear in
the file tree and multibuffer headers, not here.

---

## Syntax-highlighted previews

Rows that contain source code render it in the buffer's real
syntax colours rather than one flat tone:

- `:picker lines` and `:picker outline` colour the active buffer's
  line/symbol text using the tree it has already parsed — no extra
  parsing, no cost on the UI thread.
- `:picker grep` colours each hit's preview by detecting the file's
  language from its extension and parsing just that one line, off
  the UI thread alongside the search.

The colours come from the same `syntax.*` theme slots as the
editor, so `:colorscheme` recolours previews too. Where the fuzzy
match overlaps a syntax span, the **match highlight wins** — you
always see what matched first. Files with no supported grammar
fall back to a plain single-colour preview.

---

## MRU ranking

The picker keeps a per-source **frecency** history (recency ×
frequency): candidates you accept float toward the top next time,
decaying over days. This is a property of the picker itself — every
source gets it for free, including future plugin sources, as long
as the candidate has a stable identity (a file path, a buffer, a
command, a register, a mark). Coordinate-only picks (a specific
grep line, a jump entry) have no stable identity and are not
ranked by history.

The index persists to `~/.cache/lattice/` (or the `$XDG_CACHE_HOME`
equivalent) between sessions. It's a derived cache, not config — if
it's ever corrupt the picker discards it and starts fresh rather
than refusing to open.

### Configuration

| Option | Default | Meaning |
|---|---|---|
| `picker.mru.enabled` | `true` | Frecency ranking on/off. Off = rank by pure match score. |
| `picker.mru.recency-half-life-days` | `7` | Days for a pick's recency weight to halve. |
| `picker.mru.cap-per-namespace` | `1000` | Max history entries per source before the lowest-frecency one is evicted. |
| `picker.mru.persist` | `true` | Write the index to disk between runs. |
| `picker.display` | `minibuffer` | `minibuffer` (vertico-style, list above the `:` line) or `popup` (centred overlay). |
| `picker.result.display` | `active-pane` | Where an accepted buffer/location opens (`active-pane`, `split-h`, `split-v`). |
| `picker.grep.backend` | `auto` | Grep binary: `auto` picks the first of `rg` / `ag` / `grep` on `PATH`; or force one by name. |
| `picker.grep.max-hits` | `2000` | Cap on hits surfaced per `:picker grep`. |

Set any of these with `:set <name>=<value>`, or edit them through
`:customize`. See [options.md](../options/).

---

## Related pickers

Several features open the same picker over their own sources:

- **LSP** — `gr` (references), `:lsp-symbols` (document symbols),
  `:lsp-workspace-symbol`, `:lsp-code-action`. See [lsp.md](../lsp/).
- **Themes** — bare `:colorscheme` opens the live-preview theme
  picker. See [themes.md](../themes/).
- **Buffers** — `:b` opens the buffer switcher; `:ls` / `:buffers`
  print a flat listing. See [buffers.md](../buffers/).

They all share this keymap, ranking, and marginalia machinery —
the muscle memory carries across.

---

## Not yet

- **Plugin-contributed sources.** The source trait is designed to
  be mirrored over WASM in Phase 7; today all sources are
  first-party.
- **A theme colour *swatch*** in the `colorscheme` picker (the live
  full-editor preview ships; a per-row colour chip does not).
- **Keybinding marginalia for non-command pickers** (e.g. showing
  the open-in-split chord on file rows).
