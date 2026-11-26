# Org in lattice — demo script (~8:00)

The second video. [`demo-script.md`](./demo-script.md) introduces the
editor and gives org one short beat; this one is org's own tour, for a
viewer who already knows what org-mode is, or who wants to. It is also
the strongest argument for the plugin host: a whole emacs feature set
shipped as one WebAssembly component, installed from two lines of
`lattice.toml`.

**Lead with the editing, not the feature list.** Every beat is org's own
behaviour driven by vim's grammar, and every surface (the agenda, a
capture, a backlinks list) is a buffer. Say that once in the cold open and
let the rest show it.

Every keystroke below was driven end to end through `tmux` on 2026-11-26,
against a build of `main` plus `[plugins]` support and org v0.6.0 fetched
from its release. The exceptions are the PDF compile in section 7 and the
assistant line in section 9, which are marked. **Do not improvise
commands on camera.**

The verification ran in the terminal renderer; the recording is in the
window. **Dry-run each section once in the window before filming it**, and
section 7 in the window only: the PDF preview is a GUI surface.

---

## Before you record

- `lattice --version` is the first release after 0.10.4, the one that
  reads `[plugins]` from `lattice.toml`. Org v0.6.0 needs plugin API 0.7,
  which 0.10.4 introduced, so nothing older loads it.
- **Network** for the first boot, which fetches org from its GitHub
  release. The setup does that boot off camera.
- **`python3`** on `PATH`, for the babel section.
- **A full enough TeX** for section 7: `latexmk`, `pdflatex` and the
  packages org's LaTeX preamble loads (`wrapfig`, `rotating`, `capt-of`,
  `ulem`, `hyperref`, among others). MacTeX has them all. On Debian or
  Ubuntu that is `texlive-latex-extra`. `kpsewhich wrapfig.sty` printing
  a path is the quick check. Without it the export echoes `Compiling
  latency.pdf…` and stops; the failure is only in `*messages*`.
- Window at 1400×800, 16pt, dark theme, the same geometry as the first
  video, so clips from both line up.
- Read [*Open issues from the dry run*](#open-issues-from-the-dry-run).
  The fold issue shapes several beats: **open every fold (`zR`) before
  anything that makes org edit the file.**
- One take per section. The whole video shares one setup, so the
  **Reset** at the end of a section is only what that section needs to be
  re-filmed; the full reset is at the end.

### Setup, off camera

A throwaway `HOME` holding only the fixture's `lattice.toml` and notes, so
`~/notes` in that config is the fixture and never your own:

```sh
export REPO=$(pwd) && export DEMO_HOME=$(mktemp -d)
mkdir -p $DEMO_HOME/.config/lattice
cp $REPO/docs/media/fixtures/demo-org/lattice.toml $DEMO_HOME/.config/lattice/
cp -r $REPO/docs/media/fixtures/demo-org/notes $DEMO_HOME/notes
cp $REPO/docs/media/fixtures/demo-agenda.org $DEMO_HOME/notes/agenda.org
python3 $REPO/docs/media/fixtures/refresh-agenda-dates.py $DEMO_HOME/notes/agenda.org
cd $DEMO_HOME/notes
HOME=$DEMO_HOME lattice latency.org    # wait for the headlines to fold, then :qa!
```

That boot downloads org into `$DEMO_HOME/.local/share`. Every launch
below is `HOME=$DEMO_HOME lattice …` from `$DEMO_HOME/notes`.

The date script shifts the agenda fixture's timestamps onto today, so the
agenda has a today, an overdue entry and 45 minutes clocked this morning.
Run it on the day you film. It shifts the copy, never the committed file.

Use `HOME`, not `XDG_CONFIG_HOME`, because the config says `~/notes`.
Nothing here runs `cargo`, so the main script's rustup caveat does not
apply.

---

## 0. Cold open — 0:00–0:20

The agenda already open in the window: `HOME=$DEMO_HOME lattice`, then
`<Space>oa` `a`.

**Say:** this is org-mode: the agenda, capture, clocking, running code,
export and a Zettelkasten. It is not Emacs. It is vim's grammar, and every
one of these surfaces is a buffer you edit like a file.

`:qa!`, then section 1.

---

## 1. Two lines to install — 0:20–1:00

**Do:**

```
HOME=$DEMO_HOME lattice ../.config/lattice/lattice.toml
```

The path is relative on purpose: the shell expands `~` with your real
`HOME` before lattice starts, so `~/.config/…` would open your own config
on camera.

**Say, on the file:** `[plugins]` names a GitHub release. No Rust
toolchain and no `init.rs`: lattice downloads the build made for this
editor's plugin API and checks it. Under it, `[org]` is the plugin's own
configuration, in the same file: where my notes are, a capture template,
the languages a code block may run, and how a PDF gets made.

```
:plugins               ← org: release dhruvasagar/lattice-org-plugin@v0.6.0 (lattice.toml)
```

**Say, on the row:** the source column says which file installed it. Pin a
version with `version = "v0.6.0"`; without one it is the newest release
built for this editor.

**Clip O1 (~8s):** the `[plugins]` lines → `:plugins` and the org row.

---

## 2. Outlines and TODOs — 1:00–2:00

**Do:**

```
:e agenda.org
zM                     ← the whole file as an outline
/Editor<CR>
<Tab>                  ← open one level of "* Editor"
zR                     ← open everything before changing anything
/Draft the plugin<CR>
<Space>ot              ← TODO → NEXT
<Space>oS              ← the state menu, a key per state
d                      ← DONE, and a LOGBOOK line records when
```

**Say:** headlines fold with the fold keys you already know. A TODO state
cycles with one key, or comes from a menu when the one you want is three
presses away. These are the states this file declares, and the menu keys
are the ones in the declaration.

**Say, in one line, no beat:** subtrees promote, demote and move
(`<Space>oh`/`ol`, `oH`/`oL`, `oK`/`oJ`), and they are text objects too:
`dar` deletes a subtree and `yar` copies one.

Do not demonstrate moving a subtree with folds closed; see the open
issues.

**Reset:** `:e! agenda.org`

---

## 3. Capture — 2:00–2:45

**Do:**

```
<Space>oc              ← the capture menu, from any buffer
t                      ← the todo template; a draft opens, cursor after TODO
ARecord the org demo<Esc>
<C-c><C-c>             ← filed, and the draft closes
:e inbox.org
```

**Say:** a capture is a draft file of its own, so several can be open at
once, and `:w` keeps one for later. `C-c C-c` files it where the template
says, here under `Tasks`, and `C-c C-k` throws it away.

**Clip O2 (~10s):** `t` → typing → `<C-c><C-c>` → the entry in `inbox.org`.

---

## 4. Clocking — 2:45–3:15

**Do:**

```
:e agenda.org
zR
/Review the rope<CR>
<Space>oxi             ← clock in: ◷0:00 NEXT Review the rope… in the modeline
```

**Say:** the running clock is in the modeline, from every buffer, and
`<Space>oxj` jumps back to it.

```
<Space>oxo             ← clock out: the interval lands in its LOGBOOK
```

**Reset:** `:e! agenda.org`

---

## 5. The agenda — 3:15–4:15

**Do:**

```
<Space>oa              ← the agenda menu
a                      ← this week across every file under ~/notes
```

**Say, on the agenda:** overdue, today, the days ahead, gathered from three
files. And this is not a report. Its rows are the lines of those files.

```
j                      ← onto a row
<Space>ot              ← its state changes here, and in the file
gD                     ← the view menu
r                      ← the clock report: 0:45 this morning, totalled up the outline
```

**Say:** editing a row edits the file, and `:w` saves it. `m` marks rows
and `x` acts on all the marked ones. `gD` switches the span, the log and
the clock report.

**Clip O3 (~10s):** a row's state changing in place.

**Reset:** `:qa!`, then relaunch on `latency.org`.

---

## 6. Babel: running code — 4:15–5:15

**Do:**

```
HOME=$DEMO_HOME lattice latency.org
zR                     ← every fold open; see the open issues
/for hz<CR>            ← inside the python block
<C-c><C-c>             ← "Run python block? (y/n)"
y                      ← #+RESULTS: the budget at 60, 120 and 144 Hz
/echo<CR>
<C-c><C-c>
y                      ← the sh block's line, under it
```

**Say:** the block runs in the background and its output is written under
it, byte for byte as emacs writes it. Nothing runs that the config did not
name: the languages list in `lattice.toml` is the permission, and the
question before each run is on by default.

**Say, in one line, no beat:** `C-c C-v b` runs every block in the buffer,
`:var` hands a block another block's result, `:session` keeps an
interpreter alive between blocks, and `C-c C-v t` tangles blocks out to
files.

**Clip O4 (~10s):** `<C-c><C-c>` → `y` → the results landing.

---

## 7. Export — 5:15–6:30

**Do:**

```
:w
<C-c><C-e>             ← the export dispatcher, emacs's letters
```

**Say, on the dispatcher:** the same letters as emacs. The two rows at the
top are switches, subtree only and body only, and they flip in place.

```
hh                     ← "Exported latency.html"
<C-c><C-e>
lo                     ← LaTeX, compiled to PDF, opened beside the file
```

**Say, as the PDF opens:** that compile ran only the step the config
names, `latexmk`, and the PDF opened in the editor, in a buffer. The
`:noexport:` heading is not in it.

Edit a sentence under `* Why it matters`, then:

```
:w
<C-c><C-e>
lo                     ← the PDF reloads in place, on the page you were reading
```

**Say:** so `l o` while you write is a live preview.

The HTML export was driven in the dry run. The PDF steps were not: the
dry-run machine's TeX lacked `wrapfig`. Dry-run this section in the
window before filming it.

**Clip O5 (~12s):** the edit → `:w` → `l o` → the PDF reloading in place.

---

## 8. Org-roam — 6:30–7:15

**Do:**

```
<Space>onf             ← find a note by title, or make one
Input latency          ← no such note: "Create note: Input latency"
<CR>
Go
Bounded by the <C-c>ni ← link a note, from Insert mode
frame<CR>              ← "Frame budget"
<Esc>:w
<Space>onf
frame<CR>              ← open Frame budget
:org-roam-backlinks    ← "Input latency" links here
```

**Say:** roam is a Zettelkasten over the org files you already have.
Links are by `:ID:`, so they survive a rename, and backlinks are one
command away. `<Space>ondd` opens today's journal.

**Clip O6 (~8s):** `:org-roam-backlinks` listing the note just written.

**Reset:** `:qa!`, then delete the note and relaunch on a file, not bare
(a bare `lattice` restores the session, and with it a buffer for the file
you just deleted):

```sh
rm $DEMO_HOME/notes/roam/*-input_latency.org
HOME=$DEMO_HOME lattice latency.org
```

---

## 9. The assistant — one line, no beat

**Say, only if a model is configured in this `lattice.toml` and you have
dry-run it:** org gives lattice's language models tools over your notes,
the agenda, entries, clocked time and roam, and `:org-plan-day` plans
from what is scheduled, due and overdue, with each change shown as a
diff first. The same tools reach Claude Code over MCP.

Not driven in the dry run: it needs a provider and a key. Leave the line
out rather than say it untested.

---

## 10. Close — 7:15–8:00

**Say:** all of that is one plugin, `lattice-org-plugin`, in its own
repository, built on the same plugin API anyone can use. Each part has
its own manual: `:help org`, `org.babel`, `org.export`, `org.roam`,
`org.assistant`.

**Show on screen:**

```toml
[plugins]
org = "dhruvasagar/lattice-org-plugin"
```

**Reset, the whole video:**

```
:qa!
```
```sh
cd $REPO; rm -rf $DEMO_HOME
```

---

## Clips to cut

| Clip | Section | Length | Goes to |
|------|---------|--------|---------|
| O1 | `[plugins]` → `:plugins` org row | ~8s | org's README, the plugin page |
| O2 | capture → filed into `inbox.org` | ~10s | org's README |
| O3 | an agenda row changing in place | ~10s | org's README; pairs with `org-and-agents-{tui,gpui}.png` |
| O4 | babel results landing | ~10s | org's README, `site/data/plugin-pages/org.md` |
| O5 | PDF reloading in place | ~12s | org's README; the strongest org clip |
| O6 | roam backlinks | ~8s | org's README |

The org README and the site's org page have no motion yet. These are the
clips to give them.

---

## Open issues from the dry run

Found driving this script on 2026-11-26. Each says what it changes on
camera; delete an entry once its fix ships.

- **Folds go stale after org edits the file.** When org inserts or moves
  lines (babel's `#+RESULTS:`, a clock-in's `LOGBOOK`, the state menu's
  log line, `<Space>oK`), the fold ranges keep their old line numbers
  until the next `:w`. A closed fold then hides the wrong lines: a
  headline reads `⋯ 2 lines` with its body showing below, or a whole
  subtree vanishes under a blank row. An edit you type yourself remaps
  folds correctly; only plugin edits do not. Hence `zR` before each such
  beat, and no subtree move on camera.
- **Progress echoes do not clear.** `agenda: scanning…` and
  `Compiling latency.pdf…` stay in the echo area after the work is done,
  and a failed PDF compile never replaces the second one; the error is
  only in `*messages*`. On camera, a stalled compile looks like a hang.
- **The clock report renders inside the first block.** `gD r` puts its
  table between the `Overdue` heading and that heading's own row.
- **`#+TODO:` in a file is read only by export.** The keys that change a
  state read `org.todo-keywords` alone, which is why the demo config
  repeats the fixture's sequence. Without it, `<Space>ot` cycles
  `TODO → DONE` and skips `NEXT` and `WAITING`.
- **`WAITING` asks for a note** (the fixture declares `w@/!`). Cycling
  past `NEXT` opens a `Note (WAITING):` prompt, so section 2 stops at
  `NEXT` and uses the menu for `DONE`.
