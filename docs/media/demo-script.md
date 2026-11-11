# Lattice — introductory demo script (~9:00)

**Differentiator-led.** It opens on the thing no other editor has and
earns the feature tour afterwards, rather than starting with "here is a
modal editor" — which every viewer has already seen.

Motion clips for the site and README are cut from this recording; there is
no separate GIF pipeline (see `README.md` in this directory). The clip
marks below say which moments to cut, so film them cleanly even if the
surrounding narration rambles.

Every keystroke here is verified. Sections 1–3 come from
`tapes/{magit,buffers,config}.tape`, each driven end to end through `tmux`
against a real binary; section 3 was re-run against the 0.10.0 release.
Section 4 was driven the same way against the 0.10.0 release, with a
local mock standing in for the provider so the keys could be checked
without a key. **Do not improvise commands on camera.** A demo that shows
`unknown command` costs more than a missing section.

The verification ran in the terminal renderer; the recording is in the
window. Both renderers share one dispatcher, so the keys are the same, but
pane widths and timing are not — **dry-run each section once in the window
before filming it.**

---

## Before you record

- `lattice --version` prints the **released** version — `0.10.0` or later,
  never `cargo run`. The viewer should be watching the thing they can
  download.
- `:plugins` lists **five** bundled plugins: `auto-pair`,
  `treesitter-context`, `project`, `comment` and `lighthouse` (plus `init`
  once a config is loaded). Fewer means the build is wrong, and section
  3's auto-pair beat will silently do nothing. The authoritative list is
  `CORE_PLUGINS` in `xtask/src/main.rs`; a test (`lattice-cli`'s
  `the_demo_script_names_every_bundled_plugin`) fails if this line and
  that constant disagree, so trust the constant over this sentence if they
  ever do.
- **Film in the window.** A bare `lattice` has opened the GPU window since
  0.9.8, so it is what a viewer gets on download; the terminal appears in
  section 6 as `lattice --tui`. Window at 1400×800, 16pt, dark theme, Nerd
  Fonts on if that is your daily setup. No transparency. Same geometry as
  the stills (`screenshot-ideas.md`, the `-gpui` variants), so a clip and a
  screenshot of the same feature line up.
- `cd` to a clean checkout of this repository. `git status` clean — the
  magit section creates its own scratch file and needs a predictable
  starting point.
- **Section 4 needs `ANTHROPIC_API_KEY`** exported in the shell you launch
  from. Set it off camera, and not where shell history or a scrollback
  will show it. Then send one ask before filming: it proves the key and
  the model id in `fixtures/demo-llm/lattice.toml`, and warms the
  connection so the first on-camera answer does not stall.
- Close anything with personal content. You are filming your real
  machine.
- One take per section. Each section below ends with a **Reset**, so a
  fluffed section costs one section, not the whole recording.

---

## 0. Cold open — 0:00–0:30

Editor already open on a Rust file, in the window. No terminal, no title
card.

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
<C-c>g                 ← the dispatch transient (NOT `?` — that is backward
                         search here, as it is in every other buffer)
```

**Say, while the transient is up:** every one of these is a real command
with its own help, and the whole thing is the same vim grammar you use in
a file — `j`, `k`, operators, counts.

**Clip A (~12s):** from `s` through the file moving sections. This is the
README's lead clip.

**Reset:**

```
<Esc>                  ← dismiss the transient
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
export XDG_CONFIG_HOME=$DEMO_HOME/.config
lattice --scaffold-init
cp $REPO/docs/media/tapes/fixtures/config-demo-plugin.toml $XDG_CONFIG_HOME/lattice/init/plugin.toml
cp $REPO/docs/media/tapes/fixtures/config-demo-init.rs $XDG_CONFIG_HOME/lattice/init/src/lib.rs
cd $XDG_CONFIG_HOME/lattice/init && cargo build --release --target wasm32-wasip2 -q \
  && cp target/wasm32-wasip2/release/lattice_init.wasm init.wasm
```

`XDG_CONFIG_HOME`, **not** `HOME`. `:reload-config` recompiles `init.rs`
with `cargo`, and with `HOME` pointed at an empty directory a
rustup-managed `cargo` cannot find its toolchain: the rebuild fails
(exit 126, reported only in `*messages*`) and the reload silently keeps
the old config. Pre-building here also warms the target directory, which is
what makes the on-camera rebuild take about three seconds rather than
forty.

Since 0.9.11 the scaffold writes its manifest into `src/lib.rs` rather
than a `plugin.toml`. The fixture replaces `lib.rs` and supplies the
`plugin.toml` itself, and that still loads — re-checked against 0.10.0.

**Say:** there is no Lua here, no vimscript, no elisp, and no JSON. One
substrate: your config is Rust, compiled to WebAssembly, loaded by the
same plugin host that loads plugins. Which means config is not a settings
file — it is a program.

**Do:**

```
lattice src/lib.rs     ← same shell, so XDG_CONFIG_HOME is still exported
gg
```

**Say, on the source:** this is not a keybinding. It is a function that
registers a new ex-command through the grammar seam. A remapped key looks
like every other editor's config; this does not.

```
:hello Lattice         ← "Hello, Lattice! (from init.rs, compiled to WASM)"
/Hello<CR>
cwHowdy<Esc>           ← change the compiled function, live
:w
:reload-config         ← wait ~3s: it rebuilds, then swaps the config in
:hello Lattice         ← "Howdy, Lattice! …"
```

**Say:** I just changed a compiled Rust function, and the running editor
picked it up — no restart, no interpreter.

**Do not say** "that command did not exist when the editor started". It
did: `init.wasm` is loaded at boot (and when only the source is present,
boot builds it). The edit is what makes the reload visibly do something;
without it, `:hello` prints the same line before and after and the clip
shows nothing.

**Clip C (~12s):** the edit → `:reload-config` → `:hello Lattice` → the new
output. The single most differentiating ten seconds in the video.

**Reset:**

```
:q!
```
```sh
cd $REPO; rm -rf $DEMO_HOME; unset XDG_CONFIG_HOME
```

---

## 4. Language models, through the grammar — 5:00–6:30

**Setup**, off camera. A throwaway config holding only the fixture's
provider, so nothing from your own config is on camera, and a copy of a
three-line crate that fails to build on purpose (E0382: `greeting` is
moved into a closure, then used again). `ANTHROPIC_API_KEY` must already
be exported (see *Before you record*).

```sh
export REPO=$(pwd) && export DEMO_HOME=$(mktemp -d)
export XDG_CONFIG_HOME=$DEMO_HOME/.config && mkdir -p $XDG_CONFIG_HOME/lattice
cp $REPO/docs/media/fixtures/demo-llm/lattice.toml $XDG_CONFIG_HOME/lattice/
cp -r $REPO/docs/media/fixtures/demo-llm/project $DEMO_HOME/demo
echo 'API_TOKEN=not-a-real-secret' > $DEMO_HOME/demo/.env
cd $DEMO_HOME/demo && cargo build -q 2>/dev/null; true   # fails on purpose; warms the build
```

**Say:** in most editors the AI is a sidebar with its own rules. Here a
model is reached the way everything else is — an operator, a motion, a
buffer — and what it proposes goes through the same review as any other
change.

**Do:**

```
lattice src/main.rs    ← same shell, so XDG_CONFIG_HOME is still exported
```

Wait for `lsp ✓` in the modeline before going on. Until rust-analyzer has
settled, its own `cargo check` holds the build lock, and `*compilation*`
fills with `Blocking waiting for file lock` lines.

```
:compile cargo build   ← fails: error[E0382], borrow of moved value
```

Wait for `✗ "cargo build"` in the header line — searching before the
output has landed finds nothing.

```
/E0382<CR>             ← the error (search, don't count: the line moves)
<Space>aaip            ← ask about the error's paragraph
what is wrong, and how do I fix it?<CR>
```

**Say, as the chat opens:** that was a read-only build buffer, and asking
about it was an operator and a text object — `<Space>aa` takes any motion,
from any buffer. The chat opened beside it; my cursor did not move, and
the error went with the question, folded. The chat is a buffer too: the
same grammar edits it.

```
:only
:e src/main.rs
6G                     ← anywhere inside main
<Space>araf            ← rewrite: an operator again, `af` the function
fix the borrow error the compiler reported<CR>
```

**Say, as the diff opens:** nothing has been written. This is the same
diff review lattice uses for every proposed change — the file as it is on
one side, the proposal on the other.

```
]c                     ← to the changed hunk
:diff-accept
:w
```

**Say:** one change, and one `u` would undo it. Then the guard rails,
before the viewer asks:

```
:llm-context-add .env  ← "llm: .env matches llm.exclude (`.env`); not attached"
:recompile             ← ✔ "cargo build" ok
```

**Say, on the refusal:** `.env`, keys and certificates are excluded by
default, and a project can add to that list but never take from it.

```
<Space>aq              ← *llm-requests*: every request since the editor started
G<CR>                  ← the ask (newest is listed first), as it went over the wire
```

**Say, on `*llm-inspect*`:** this is the request exactly as it was sent —
headers, body, the compiler output it carried — with the key shown as
`<redacted>`. Nothing the editor sends is hidden from you.

**Say, in one line, no beat:** this was Anthropic; a local server such as
Ollama is three lines of config and no key, and a `model=` word on one ask
switches it for that ask.

**Be accurate here, on camera:**

- **Narrate the mechanism, never the answer.** The model's wording changes
  every take. If the rewrite proposes anything other than a fix that
  builds (dropping `move`, or cloning before it), `:diff-reject` and run it
  again — never accept a wrong fix on camera.
- **Tools are off.** This model only saw what was sent; it cannot read the
  project or run commands until `llm.tools` turns them on, and then each
  edit opens as a diff and each command asks first. Say so if you mention
  agents; do not imply it browsed the code.

**Clip F (~15s):** the diff opening → `]c` → `:diff-accept` → `:recompile`
going ✔. The LLM section's clip.

**Reset:**

```
:qa!
```
```sh
cd $REPO; rm -rf $DEMO_HOME; unset XDG_CONFIG_HOME
```

---

## 5. Org-mode, as buffers — 6:30–7:15

**Setup:** the org plugin is not in the bundled core set. Install it from
its release — `require_release` in `init.rs` (0.9.11) downloads the build
for this plugin API, no toolchain needed — and check `:plugins` lists it
before filming. Use the fixture `docs/media/fixtures/demo-agenda.org`,
**not** your real notes.

**Say:** org-mode — agenda, capture, clocking — in a modal editor that is
not Emacs, and the agenda is a buffer like everything else in this video.

**Do:** open the agenda, move around it with normal motions. This section
has no verified keystroke list — dry-run it with the plugin version you
film.

**Say, in one line, no beat:** coding agents run as buffers too — Claude
Code and opencode each attach with their own CLI installed. If you show
one, say which: `:opencode` runs opencode's own TUI in a terminal buffer,
and its ACP conversation buffer is the alternative, not the default. The
release notes were corrected for exactly this, so do not undo it in the
video.

**Clip D (~10s):** moving around the agenda with normal motions.

**Reset:** `:qa!`

---

## 6. One core, two renderers — 7:15–8:00

**Say:** everything so far was the GPU window, which is what `lattice`
opens. The terminal build is not a fallback: it is a first-class
renderer, which is why this works over SSH on a machine with no GPU.

**Do:** the same file open in the window and in `lattice --tui` in a
terminal, side by side. Scroll both.

**Say:** Zed has no terminal UI. Helix has no GPU renderer. Nobody else
ships both from one core.

**Clip E (~8s):** the side-by-side scroll.

---

## 7. Close — 8:00–8:45

**Say, plainly and without hedging:** this is 0.10 — usable, and still
pre-1.0. Binaries are unsigned, there is no Homebrew or `cargo install`,
and org-mode is a separate plugin rather than part of the download. All of
that is written down — point at the known-limitations page rather than
listing it.

**Show on screen:**

```
curl -fsSL https://raw.githubusercontent.com/dhruvasagar/lattice/main/install.sh | sh
```

**Say:** feedback is the point of this release. Issues and discussions are
open, and the bug form asks for your platform and whether the plugins
loaded, because those are the two things that make a report actionable.

---

## Clips to cut

| Clip | Section                       | Length | Pairs with the still            | Goes to                                |
|------|-------------------------------|--------|---------------------------------|----------------------------------------|
| A    | magit staging                 | ~12s   | `magit-{tui,gpui}.png`          | README lead, gallery shot 1            |
| B    | `:terminal` → `<Esc>` → `:ls` | ~15s   | `buffer-splits-{tui,gpui}.png`  | gallery shot 2                         |
| C    | edit → `:reload-config` → `:hello` | ~12s | `config-init-rs-{tui,gpui}.png` | gallery shot 3, and the strongest clip |
| F    | rewrite diff → `:diff-accept` → `:recompile` | ~15s | none yet          | the LLM section; a gallery candidate   |
| D    | org agenda motions            | ~10s   | `org-and-agents-{tui,gpui}.png` | gallery shot 4                         |
| E    | window + TUI side by side     | ~8s    | `two-renderers.png`             | gallery shot 5                         |

Each clip has a still of the same moment — same geometry, same theme — so
frame them alike. The stills are specified in
[`screenshot-ideas.md`](./screenshot-ideas.md); the landing gallery reads
`site/data/gallery.toml`, which renders nothing until the files exist.
Clip F has no still yet: if it becomes a gallery shot, specify one there
first.

Keep each under 4 MB if committed; prefer linking a hosted video and
committing only the stills.
