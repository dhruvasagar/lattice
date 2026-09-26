# Screenshots — what to capture, and where each one goes

The capture brief for the launch gallery. Every shot below names what it has
to show, how to get the editor into that state, and which surface it lands
on. Read the conventions first — they are pinned, and a shot captured
against a different geometry cannot sit in the same gallery as the others.

**Captured by hand.** A tmux + `screencapture` harness was considered and
declined (2026-09-23): worth building when the shots need regenerating
often, not for the first set. The consequence is that these images rot
silently when the UI changes — so when a status line, gutter or theme
changes shape, recapture is part of that work, not a later cleanup.

**Motion is not here.** Clips come from the demo video
([`demo-script.md`](./demo-script.md)), not from VHS and not from this list
— see [`README.md`](./README.md) for why (headless Chrome's screencast is
broken on this machine, so VHS writes no file). The `.tape` files survive as
verified choreography, and the setup steps below are drawn from them.

**Re-cut 2026-09-20 (L.4b)**, re-scoped 2026-09-23 to carry both renderers,
per-shot setup and destinations. The list itself is derived from
`docs/user/` (one topic page per shipped feature, enforced against
`site/data/nav.toml` by a build that fails when they disagree) rather than
from memory, and ordered around the four differentiators in
[`../dev/architecture/launch-0.9.md`](../dev/architecture/launch-0.9.md) §10.

---

## Conventions — pinned

These supersede the previous "1440×900 / 14px" note, which agreed with
neither the tapes (1400×800 / 16pt) nor the shipped hero (1920×1242). One
convention, so stills and video clips share framing:

|             |                                                                                                                                         |
|-------------|-----------------------------------------------------------------------------------------------------------------------------------------|
| **Window**  | 1400×800 logical. Same as `tapes/*.tape` and `demo-script.md`, so a still and a clip of the same feature line up.                       |
| **Capture** | Native retina — macOS gives ~2800×1600. Do not capture at 1x.                                                                           |
| **Publish** | Downscale to **1920 px wide**, PNG, **under 400 KB**. The hero was made this way (3680×2382 → 1920×1242 / 196 KB) and is the reference. |
| **Font**    | A patched Nerd Font at 16pt, `ui.nerd_fonts=on`.                                                                                        |
| **Theme**   | Default dark. Light only where a shot is explicitly the light variant.                                                                  |
| **Opacity** | None. No terminal transparency — pure dark background.                                                                                  |
| **Frame**   | Editor content only. No window chrome, no desktop, no dock, unless the shot is *about* window management.                               |

**Both renderers.** Every differentiator shot is captured twice — once in
the TUI, once in GPUI. That is not only for the gallery: "TUI and GPUI
parity in lockstep" is a standing rule with no visual verification today,
and a matched pair is the first evidence it holds. Publish whichever makes
the argument better; keep both.

For GPUI there is no `ui.window.width/height` option
(`crates/lattice-ui-gpui/src/window.rs:5446` offers a centred 720×480
default or `ui.window.start-maximized=true`). So: set
`ui.window.start-maximized=true`, capture, and crop to the 1400×800
proportion afterwards.

## Before you capture

- `lattice --version` prints the **released** build, not `cargo run`. The
  gallery should show the thing a visitor can download. GPUI ships a real
  binary for `aarch64-apple-darwin` (`release.yml:125`); `install.sh --gui`
  fetches it.
- `:plugins` lists **four** bundled: `auto-pair`, `treesitter-context`,
  `project`, `comment`. Three means the build is wrong.
- `git status` clean in the capture repo. Shots 1 and 2 run against *this*
  repository — real history is what makes the magit shot honest — and
  shot 1 creates its own scratch file.
- Nothing personal in frame: no real notes, no client paths, no unrelated
  terminal scrollback, no notification banners. Check the whole frame, not
  the editor.
- Full-screen the terminal or window so no desktop bleeds into the edges.

## Naming and destinations

Files are named `<shot>-<renderer>.png`, renderer being `tui` or `gpui`.

**Every published asset is committed in two places.** No script mirrors
them:

- `assets/media/screenshots/<name>.png` — what `README.md` references
  (`./assets/media/screenshots/…`).
- `site/static/media/<name>.png` — what the site references
  (`get_url(path='media/…')`).

Miss the second and the README looks right while the site 404s — the
failure a human reviewer does not catch, because the image renders fine in
the diff. `crates/lattice-cli/tests/every_referenced_screenshot_exists.rs`
binds the references to the files: README refs, site-template refs, every
`gallery.toml` entry present in **both** directories, and every published
PNG inside the 400 KB budget.

**Screenshots never go in `docs/user/`.** That tree is the offline `:help`
corpus embedded in the binary (`crates/lattice-help/build.rs` reads it), so
an image there renders as dead markup in a help buffer. Supporting shots go
on *site* pages — `site/content/plugins/*.md` and the landing template —
which is also why `site/content/docs/` is off limits: it is synced from
`docs/user/`.

**The hero is being recaptured**, and `hero-dark.png` retires with it.
Replacing it means updating three references: `README.md:13`,
`site/templates/index.html:20` (`.hero-shot`), and the mirrored file in
`site/static/media/`.

---

## The five differentiator shots

These lead the README gallery and the landing page. Each exists to answer
"why this and not Zed / Helix / Neovim / VS Code", not "what features does
it have".

### 1. Magit — a real porcelain inside a modal editor

**Shows:** staged and unstaged hunks in the status buffer with a transient
dispatch menu open.
**Absent from:** Zed, Helix, Neovim (fugitive is not magit), VS Code.
**Files:** `magit-tui.png`, `magit-gpui.png`

**Setup** (from `tapes/magit.tape`, keystroke-verified):

```
# shell, before launching — a real untracked file to stage
echo '# magit demo scratch file' > docs/media/.magit-demo-scratch.md

lattice
:magit-status
/magit-demo-scratch      <CR>      # find it; a fixed `jjj` is NOT reliable
s                                  # stage the hunk
C-c g                              # repo dispatch transient (docs/user/magit.md:37)
```

Capture with the transient open over a status buffer showing **both** a
staged and an unstaged section — that contrast is the shot. Afterwards:

```
git reset -q -- docs/media/.magit-demo-scratch.md
rm -f docs/media/.magit-demo-scratch.md
```

**Goes to:** README gallery (lead), landing gallery, and it is the strongest
candidate for the README hero if the recaptured hero does not beat it.

### 2. Everything is a buffer

**Shows:** a four-way split — file tree, code, project-search results, and
a terminal — with `:ls` listing all of them uniformly.
**Absent from:** all of them. The others have panels; these are buffers.
**Files:** `buffer-splits-tui.png`, `buffer-splits-gpui.png`

**Setup** (from `tapes/buffers.tape`):

```
lattice Cargo.toml
:Tree .                  <CR>      # `.` explicitly — the no-arg form errors
G                                  # last entry is a real file, not a dir
<CR>
:search TODO             <CR>      # results are a real multibuffer
:vsplit                  <CR>
:terminal                <CR>
echo 'a terminal is a buffer too'  <CR>
<Esc>                              # Terminal-Insert would send `:` to the shell
:ls                      <CR>
```

Two captures are worth taking here: one with the four panes live, one with
`:ls` open over them. The `:ls` frame is the one that proves the claim —
different kinds, one list.

**Goes to:** README gallery, landing gallery.

### 3. Config is Rust, compiled, and programmable

**Shows:** `init.rs` open beside the editor, defining a real ex-command,
then `:reload-config` and the command working.
**Absent from:** Zed (JSON), Helix (TOML), Neovim (Lua), VS Code (JSON+TS).
**Files:** `config-init-rs-tui.png`, `config-init-rs-gpui.png`

It must show something **genuinely programmatic** — a custom command or a
hook, not a keybinding one-liner. A remapped key looks like every other
editor's config; a compiled function does not.

**Setup** (from `tapes/config.tape` — runs under a throwaway `$HOME`, never
touches a real `~/.config/lattice`):

```
export REPO=$(pwd) && export DEMO_HOME=$(mktemp -d)
HOME=$DEMO_HOME lattice --scaffold-init
cp $REPO/docs/media/tapes/fixtures/config-demo-plugin.toml $DEMO_HOME/.config/lattice/init/plugin.toml
cp $REPO/docs/media/tapes/fixtures/config-demo-init.rs     $DEMO_HOME/.config/lattice/init/src/lib.rs
cd $DEMO_HOME/.config/lattice/init
cargo build --release --target wasm32-wasip2 -q
cp target/wasm32-wasip2/release/lattice_init.wasm init.wasm

HOME=$DEMO_HOME lattice src/lib.rs
gg
:reload-config           <CR>
:hello Lattice           <CR>
```

`config-demo-init.rs` is a real `grammar`-seam component registering
`:hello <name>`, and it is the annotated example in `docs/user/init.md`'s
"Custom grammar" section. Capture with the Rust source visible **and** the
command's output on screen — one without the other makes half the point.
Clean up with `rm -rf $DEMO_HOME`.

**Goes to:** README gallery, landing gallery. Per `demo-script.md` this is
also the strongest video clip, so frame the still to match clip C.

### 4. Org-mode and agents, as buffers

**Shows:** an org agenda beside a coding-agent buffer under interactive diff
review.
**Absent from:** everything outside Emacs; Zed's agent is not a buffer.
**Files:** `org-and-agents-tui.png`, `org-and-agents-gpui.png`

The only shot with no tape, for two reasons that hand-capture resolves: a
real agenda would leak personal notes, and a live agent session is a
non-deterministic model conversation no fixed timeline can script. With a
human holding the shutter, a **real** short agent session is fine and is
what should be shown — do not stage a fake transcript for a launch gallery.

**Needs a fixture** that does not exist yet: `docs/media/fixtures/demo-agenda.org`
— a handful of scheduled and deadlined TODOs, dated relative to capture day
so today's section is populated, with no personal content. Write it before
capturing.

**Setup:**

```
lattice docs/media/fixtures/demo-agenda.org
# org agenda for the fixture file, then:
:vsplit
:claude          (or :opencode)
# ask for a small, real edit in this repo; when it proposes a diff, leave
# the side-by-side review on screen — `:diff-accept` / `:diff-reject` visible
```

Org is **not** a bundled plugin, so this capture needs it installed. That is
true and the caption should not imply otherwise.

**Goes to:** README gallery, landing gallery, and `site/content/plugins/org.md`.

### 5. Two first-class renderers

**Shows:** the same file, same state, in the TUI and the GPU window side by
side.
**Absent from:** Zed (no TUI), Helix (no GPU).
**File:** `two-renderers.png` (a composite — no `-tui`/`-gpui` pair)

Now that every shot above is captured twice, this one is a **composite of an
existing pair** rather than a separate session: pick the pair that reads
best at small size and join them with `magick`. It still earns its own slot
— the parity claim lands in one image, which is what a landing page needs.

```
magick montage <shot>-tui.png <shot>-gpui.png -tile 2x1 -geometry +8+8 two-renderers.png
```

**Goes to:** landing gallery. Optional in the README — it is the one shot
that reads poorly at README width.

### The hero (recapture)

`hero-dark.png` was captured at L.5 and Dhruva chose to recapture it.
**Files:** `hero-tui.png`, `hero-gpui.png`; the published one replaces the
three `hero-dark.png` references listed under *Naming and destinations*.

Current alt text describes the shot to match: a buffer being edited, with
the project file tree and a `describe-buffer` popup open. Keep that shape or
update the alt text with it.

---

## Where the shots land

| Surface                     | Slot                                                                                                                                        | State                                                                                |
|-----------------------------|---------------------------------------------------------------------------------------------------------------------------------------------|--------------------------------------------------------------------------------------|
| `README.md`                 | Hero at line 13; a **new gallery section** after *What works today* (lines 32-42), before `## Org-mode, and what a plugin can be` (line 44) | gallery does not exist yet                                                           |
| `site/templates/index.html` | `.hero-shot` at line 20; a **new `<section class="gallery">`** between `features` (line 26) and `principles` (line 54)                      | gallery does not exist yet                                                           |
| `site/content/plugins/*.md` | One shot per plugin page: `auto-pair`, `treesitter-context`, `project`, `comment`, `org`                                                    | slots deliberately left empty at L.11 — "a broken image is worse than an absent one" |

The README gallery should stay compact — the file is 164 lines against a
143-line discipline, so thumbnails in a two-column table linking to the full
images, not five inline `<img>` tags at full width.

**The landing gallery is scaffolded and renders nothing.**
`site/data/gallery.toml` ships with `shots = []`, and `index.html` wraps the
section in `{% if gallery.shots %}` — the same trick the plugin index uses
for its unpopulated "community" group, so nothing is referenced until the
file exists. Each shot's entry is written out commented in that file, alt
text included; uncomment as the capture lands.

The **README** gallery cannot work that way — markdown has no conditional —
so it lands in the **same commit** as the assets. The guard above is what
makes that safe: add a reference to an image that is not there and the test
fails before it can be pushed.

---

## Supporting shots

Not the landing gallery — these fill plugin pages and future feature pages.
Each names the `docs/user/` page(s) it documents, for the caption to be
accurate. Capture in whichever renderer shows it best; pairs are only
required for the five above.

**Plugin pages** (highest value after the differentiators — these slots exist
and are empty)
- `auto-pair` — a bracket/quote pair completing in insert mode
- `treesitter-context` — the sticky context header on a scrolled function
- `project` — the switch menu / remembered project list
- `comment` — `gcc` toggling a line, `gc` over a visual selection
- `org` — agenda, capture, and the outline (shares shot 4's fixture)

**Magit, beyond status** (`docs/user/magit-*.md`, ~22 pages)
- Interactive rebase (`magit-rebase-mode`) — the todo-list buffer, reorder/edit/squash
- `magit-log-mode` — commit graph, `<CR>` to a revision
- `magit-blame-mode` — inline blame gutter, jump to the commit
- `magit-diff-mode` side-by-side (`dv`) two/three-way diff
- The file dispatch transient (`C-c f`)

**Org-mode** (`docs/user/org.md` + agenda/capture pages)
- `org-capture` template picker mid-capture
- `org-agenda` composite view (day/week agenda + TODO list)
- Clocking (`org.agenda-log.md` / clock report) in the modeline
- Org-roam backlinks pane

**Coding agents as buffers** (`docs/user/claude-code-mode.md`, `opencode-mode.md`, `ai-*-mode.md`)
- `:claude` / `:opencode` conversation buffer mid-response
- `:diff-accept` / `:diff-reject` side-by-side review of an agent's edit
- The `*ai:<provider>:<index>*` log buffer (`:ai-log`)

**Everything-is-a-buffer, beyond shot 2**
- `oil-mode` — editing a directory listing as text, `:w` renders the diff as filesystem ops
- `dashboard-mode` — the splash buffer, every row a followable link
- `multibuffer-mode` / `:search` results — excerpts from several files in one buffer
- `compilation-mode` + the error list — `:compile`, `gr` to rerun, `<CR>` to jump
- `repl-mode` — a REPL transcript where `i`/`o` jump to the prompt line

**Editing power beyond vim-parity**
- `which-key-mode` — the hint popup, read live off the keymap
- `surround-mode` — `ys`/`cs`/`ds` before/after
- `table-mode` — a markdown/org pipe table mid-edit, `<Tab>` walking cells
- `narrow-mode` — `zn` narrowed to a region, `:widen` restoring
- `folding` — a computed fold collapsed, `zo`/`zc`
- Macros + the yank ring as editable data (not a hidden register)

**LSP** (`docs/user/lsp*.md`, ~20 submodes)
- Completion popup with docs sidebar + diagnostics gutter
- `lsp-code-action-mode` — the action picker
- `lsp-references-mode` / `lsp-symbols-mode` — a references/symbols buffer

**Still worth shooting**
- Modal editing: visual-mode selection, operator-pending status, `:s/foo/bar`
- Picker: fuzzy file picker with frecency-sorted results + preview
- Help system: `:describe-key` result in a help buffer
- Theme preview: the same file across 3–4 themes
- Ghost-text completion (insert mode, before accepting)
- Tutor: the interactive lesson buffer
- Hero light variant

## Skip

Real, but nothing a still image can carry: `emacs-keys-mode`; the individual
language-mode pages (~25 — the hero already proves syntax highlighting);
`whitespace-show-mode` / `wrap-mode` / `*-line-numbers-mode` /
`current-line-highlight-mode` (display toggles); `command-line-expand-mode` /
`path-completion-mode` / `prompt-line-mode` (minibuffer plumbing already
under the hood of shots above); `notifications-mode`; `cancellation`;
`modes`; `options`; `plugins-mode`; `pi-mode`; `troubleshooting-keys`.

## Motion

Screencasts are planned in [`demo-script.md`](./demo-script.md) — seven
sections with reset points, and a *Clips to cut* table mapping five moments
to the surfaces that embed them. That file is the authority for motion; this
one does not duplicate it.

---

## Checklist

Differentiators — each needs both renderers:

- [X] 1. Magit status + transient — `magit-tui.png`, `magit-gpui.png`
- [ ] 2. Buffer splits + `:ls` — `buffer-splits-tui.png`, `buffer-splits-gpui.png`
- [ ] 3. `init.rs` + `:reload-config` — `config-init-rs-tui.png`, `config-init-rs-gpui.png`
- [ ] 4. Org agenda + agent diff review — `org-and-agents-tui.png`, `org-and-agents-gpui.png`
- [ ] 5. Two renderers composite — `two-renderers.png`
- [ ] Hero recapture — `hero-tui.png`, `hero-gpui.png`

Blocking work before capture:

- [X] Write `docs/media/fixtures/demo-agenda.org` (shot 4)
- [ ] Install the org plugin in the capture environment (shot 4)

After capture:

- [ ] Mirror every published file into `site/static/media/`
- [ ] Uncomment each shot's entry in `site/data/gallery.toml`
- [ ] Build the README gallery section (same commit as the assets)
- [ ] Retire `hero-dark.png` — three references
- [ ] Fill the five plugin-page slots
- [x] Landing `<section class="gallery">` — scaffolded, renders nothing until
      `gallery.toml` has entries
- [x] Asset existence + size guard —
      `crates/lattice-cli/tests/every_referenced_screenshot_exists.rs`

Supporting shots are tracked by the lists above, not as checkboxes — they
land opportunistically as pages need them.
