# Lattice — introductory demo script (~7:30)

**Differentiator-led.** It opens on the thing no other editor has and
earns the feature tour afterwards, rather than starting with "here is a
modal editor" — which every viewer has already seen.

Motion clips for the site and README are cut from this recording; there is
no separate GIF pipeline (see `README.md` in this directory). The clip
marks below say which moments to cut, so film them cleanly even if the
surrounding narration rambles.

Every keystroke here is verified — the sequences come from
`tapes/{magit,buffers,config}.tape`, each of which was driven end to end
through `tmux` against a real binary. **Do not improvise commands on
camera.** A demo that shows `unknown command` costs more than a missing
section.

---

## Before you record

- `lattice --version` prints `lattice 0.9.0`. Record the **released**
  build, not `cargo run` — the viewer should be watching the thing they
  can download.
- `:plugins` lists `auto-pair`, `treesitter-context` and `project`, each
  `bundled`. If it does not, the build is wrong and section 3's auto-pair
  beat will silently do nothing.
- Terminal at 1400×800, 16pt, dark theme, Nerd Fonts on if that is your
  daily setup. No transparency.
- `cd` to a clean checkout of this repository. `git status` clean — the
  magit section creates its own scratch file and needs a predictable
  starting point.
- Close anything with personal content. You are filming your real
  machine.
- One take per section. Each section below ends with a **Reset**, so a
  fluffed section costs one section, not the whole recording.

---

## 0. Cold open — 0:00–0:30

Editor already open on a Rust file. No terminal, no title card.

**Say:** what it is, in one breath. A modal, GPU-accelerated, plugin-first
editor written in Rust — vim's grammar, emacs's extensibility, on a core
where the UI thread does no I/O, no parsing and no shaping. Then: "the
fastest way to show you why it exists is this."

**Do:**

```
:magit-status
```

Do not explain magit yet. Let it render, then start section 1.

**Clip:** none. This is narration.

---

## 1. Magit — 0:30–2:00 · the thing nobody else has

**Setup**, before recording this section:

```sh
echo '# demo scratch' > docs/media/.demo-scratch.md
```

That gives the status buffer a real untracked entry to stage, without
touching anything that matters.

**Say:** this is a magit port — not a git plugin, not a status panel. A
real one: hunk-level staging, rebase, blame, transients. Emacs users know
what this is worth; everyone else should know it is the reason a lot of
people never leave Emacs. Zed, Helix and Neovim have nothing equivalent —
fugitive is a fine thing and it is not this.

**Do:**

```
/demo-scratch          ← find the entry (do NOT use jjj; which section it
<CR>                     lands in depends on what else is dirty)
s                      ← stage it
```

Pause on the staged/unstaged split so the viewer sees the file move
between sections. Then open a transient to show the depth:

```
?                      ← the transient for the current context
```

**Say, while the transient is up:** every one of these is a real command
with its own help, and the whole thing is the same vim grammar you use in
a file — `j`, `k`, operators, counts.

**Clip A (~12s):** from `s` through the file moving sections. This is the
README's lead clip.

**Reset:**

```
:q!
```
```sh
git reset -q -- docs/media/.demo-scratch.md; rm -f docs/media/.demo-scratch.md
```

---

## 2. Everything is a buffer — 2:00–3:30

**Say:** most editors give you panels — a file tree pane, a terminal
pane, a search pane, each with its own keys and its own rules. Lattice has
none. Everything is a buffer, so everything takes the same grammar.

**Do:**

```
lattice Cargo.toml
:Tree .
G                      ← navigate the tree with a normal motion
<CR>                   ← open the file under the cursor
:search TODO
```

**Say, on the search results:** this is not a results panel. It is a
buffer. `j` and `k` work, `/` works, you can yank out of it.

```
<CR>                   ← jump to a result
:vsplit
:terminal
echo 'a terminal is a buffer too'
<Esc>                  ← back to normal mode inside the terminal
:ls
```

**Say, on `:ls`:** every one of those — the tree, the search results, the
terminal, the file — is a listed buffer. `:bn`, `:bp`, `:b <name>` move
between all of them identically.

**Clip B (~15s):** `:terminal` → `<Esc>` → `:ls`. This is the site's
"everything is a buffer" clip, and the `<Esc>` beat is the point: modal
editing *inside* a terminal.

**Reset:**

```
:qa!
```

---

## 3. Config is a program — 3:30–5:00

The section that needs the most care, because the payoff is subtle and
easy to undersell.

**Setup**, off camera (the build takes ~25s — do not film it):

```sh
export REPO=$(pwd) && export DEMO_HOME=$(mktemp -d)
HOME=$DEMO_HOME lattice --scaffold-init
cp $REPO/docs/media/tapes/fixtures/config-demo-plugin.toml $DEMO_HOME/.config/lattice/init/plugin.toml
cp $REPO/docs/media/tapes/fixtures/config-demo-init.rs $DEMO_HOME/.config/lattice/init/src/lib.rs
cd $DEMO_HOME/.config/lattice/init && cargo build --release --target wasm32-wasip2 -q \
  && cp target/wasm32-wasip2/release/lattice_init.wasm init.wasm
```

**Say:** there is no Lua here, no vimscript, no elisp, and no JSON. One
substrate: your config is Rust, compiled to WebAssembly, loaded by the
same plugin host that loads plugins. Which means config is not a settings
file — it is a program.

**Do:**

```
HOME=$DEMO_HOME lattice src/lib.rs
gg
```

**Say, on the source:** this is not a keybinding. It is a function that
registers a new ex-command through the grammar seam. A remapped key looks
like every other editor's config; this does not.

```
:reload-config
:hello Lattice
```

**Say:** that command did not exist when the editor started.

**Clip C (~10s):** `:reload-config` → `:hello Lattice` → the output. The
single most differentiating ten seconds in the video.

**Reset:**

```
:q!
```
```sh
rm -rf $DEMO_HOME
```

---

## 4. Org-mode and agents as buffers — 5:00–6:15

**Setup:** a fixture org file with a few scheduled items — **not** your
real notes. The org plugin is not in the bundled core set, so install it
first and check it loads before filming.

**Say:** two things that exist nowhere else together. Org-mode — agenda,
capture, clocking — in a modal editor that is not Emacs. And coding agents
that are *buffers*: you edit the conversation with the full grammar, and
review their diffs with the same diff subsystem you use for your own
changes.

**Do:** open the agenda, move around it with normal motions, then bring up
an agent buffer with a diff under review and navigate hunks with `]c` /
`[c`.

**Be accurate here, on camera:** `:opencode` runs opencode's own TUI in a
terminal buffer; the lattice-owned ACP conversation buffer with diff
review is the alternative, not the default. Say which one you are showing.
The release notes were corrected for exactly this, so do not undo it in
the video.

**Clip D (~12s):** hunk navigation in the agent's diff review.

**Reset:** `:qa!`

---

## 5. One core, two renderers — 6:15–7:00

**Say:** the terminal build is not a fallback. It is a first-class
renderer, which is why this works over SSH on a machine with no GPU. And
the same core drives a GPU-rendered window.

**Do:** the same file open in the TUI and in `--gui`, side by side.
Scroll both.

**Say:** Zed has no terminal UI. Helix has no GPU renderer. Nobody else
ships both from one core — which is why `--gui` is a peer here rather
than a fallback, even though it is opt-in at 0.9.

**Clip E (~8s):** the side-by-side scroll.

---

## 6. Close — 7:00–7:30

**Say, plainly and without hedging:** this is 0.9, an alpha. The editor is
usable; the distribution is new. Binaries are unsigned, LSP servers are
installed by hand, syntax colours are not fully themeable yet, and the GPU
renderer is not at parity with the terminal one. All of that is written
down — point at the known-limitations page rather than listing it.

**Show on screen:**

```
curl -fsSL https://raw.githubusercontent.com/dhruvasagar/lattice/main/install.sh | sh
```

**Say:** feedback is the point of this release. Issues and discussions are
open, and the bug form asks for your platform and whether the plugins
loaded, because those are the two things that make a report actionable.

---

## Clips to cut

| Clip | Section | Length | Goes to |
|---|---|---|---|
| A | magit staging | ~12s | README lead, site differentiator 1 |
| B | `:terminal` → `<Esc>` → `:ls` | ~15s | site differentiator 2 |
| C | `:reload-config` → `:hello` | ~10s | site differentiator 3, and the strongest clip |
| D | agent diff review | ~12s | site differentiator 4 |
| E | TUI + GPU side by side | ~8s | site differentiator 5 |

Keep each under 4 MB if committed; prefer linking a hosted video and
committing only the stills.
