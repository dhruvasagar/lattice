# Screenshot & Screencast Ideas

This doc lists screenshots and screencasts to collect for the Lattice website
and README.

**Re-cut 2026-09-20 (L.4b).** The previous version of this list was
feature-organised and predated most of what Lattice now ships — it never
mentioned magit (~22 topic pages), org, agents, narrowing, folding,
multibuffer, which-key, surround, table mode, snippets, compilation, the
REPL, or dashboard. It was re-derived from `docs/user/` (one topic page per
shipped feature, enforced against `site/data/nav.toml` by a build that fails
when they disagree — see the inventory diff in
`.superpowers/sdd/launch-0.9/task-L.4b-report.md`) rather than re-cut from
its own contents, and re-ordered around the four differentiators in
[`../dev/architecture/launch-0.9.md`](../dev/architecture/launch-0.9.md) §10.

## Priority shots — the differentiators

These are the shots the README gallery and the site lead with. Each exists
to answer "why this and not Zed / Helix / Neovim / VS Code", not "what
features does it have". See `../dev/architecture/launch-0.9.md` §10.

| # | Shot | Shows | Absent from | File |
|---|---|---|---|---|
| 1 | Magit status with staged + unstaged hunks and a transient popup open | a real magit port inside a modal editor | Zed, Helix, Neovim (fugitive is not magit) | `assets/media/screenshots/magit.png`, `assets/media/demos/magit.gif` |
| 2 | Four-way split: file tree, code, terminal, search results — all real buffers | everything is a buffer; the same grammar works in all of them | all of them; the others have panels | `assets/media/screenshots/buffer-splits.png`, `assets/media/demos/buffers.gif` |
| 3 | `init.rs` beside the editor, defining a custom command and a hook, then `:reload-config` applying it live | config is Rust compiled to WASM, and it is programmable — not a settings file | Zed (JSON), Helix (TOML), Neovim (Lua), VS Code (JSON+TS) | `assets/media/screenshots/config-init-rs.png`, `assets/media/demos/config.gif` |
| 4 | Org agenda beside a coding-agent buffer under interactive diff review | org-mode and agents-as-editable-buffers, in one editor | everything outside Emacs; Zed's agent is not a buffer | `assets/media/screenshots/org-and-agents.png` |
| 5 | The same file in the TUI and the GPU window, side by side | one core, two first-class renderers | Zed (no TUI), Helix (no GPU) | `assets/media/screenshots/two-renderers.png` |

Shot 3 must show something genuinely programmatic — a custom command or a
hook — not a keybinding one-liner. A remapped key looks like every other
editor's config; a compiled function does not. `docs/media/tapes/config.tape`
demonstrates this with a real `:hello <name>` ex-command registered from
`init.rs`, built to WASM, and invoked after `:reload-config`.

Shot 4 (org + agents) is not yet a rendered demo — see
`.superpowers/sdd/launch-0.9/task-L.4b-report.md` for why (a live agent
session is not reproducible/deterministic enough for a committed tape, and
a real org agenda would leak personal data). It stays on this list as a
screenshot to capture by hand with a fixture org file, same convention as
the GPUI shots below.

## Supporting shots

Used on feature pages and in the docs, not the landing gallery. Grouped by
area; each names the `docs/user/` page(s) it documents.

**Magit, beyond status** (`docs/user/magit-*.md`, ~22 pages)
- Interactive rebase (`magit-rebase-mode`) — the todo-list buffer, reorder/edit/squash
- `magit-log-mode` — commit graph, `<CR>` to a revision
- `magit-blame-mode` — inline blame gutter, jump to the commit
- `magit-diff-mode` side-by-side (`dv`) two/three-way diff
- A transient dispatch menu open (`C-c g`) — the popup itself

**Org-mode** (`docs/user/org.md` + agenda/capture pages)
- `org-capture` template picker mid-capture
- `org-agenda` composite view (day/week agenda + TODO list)
- Clocking (`org.agenda-log.md` / clock report) in the modeline
- Org-roam backlinks pane

**Coding agents as buffers** (`docs/user/claude-code-mode.md`, `opencode-mode.md`, `ai-*-mode.md`)
- `:claude` / `:opencode` conversation buffer mid-response
- `:diff-accept` / `:diff-reject` side-by-side review of an agent's edit
- The `*ai:<provider>:<index>*` log buffer (`:ai-log`)

**Everything-is-a-buffer, beyond the hero split**
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
- Completion popup with docs sidebar + diagnostics gutter (kept from the old list)
- `lsp-code-action-mode` — the action picker
- `lsp-references-mode` / `lsp-symbols-mode` — a references/symbols buffer

**Kept from the previous list, still worth shooting**
- Modal editing: visual-mode selection, operator-pending status, `:s/foo/bar`
- Picker: fuzzy file picker with frecency-sorted results + preview
- Help system: `:describe-key` result in a help buffer
- Theme preview: the same file across 3–4 themes
- Ghost-text completion (insert mode, before accepting)
- Tutor: the interactive lesson buffer
- A plugin's custom command running (extensibility, general case — differentiator 3 is the sharper version of this)

## Skip (real, but not visually distinctive)

An option, a keybinding nicety, or internal plumbing — nothing a still image
or short clip can carry on its own: `emacs-keys-mode`, the individual
language-mode pages (~25 of them; the hero shot already proves syntax
highlighting), `whitespace-show-mode` / `wrap-mode` / `*-line-numbers-mode` /
`current-line-highlight-mode` (display toggles), `command-line-expand-mode` /
`path-completion-mode` / `prompt-line-mode` (minibuffer plumbing under the
hood of shots already listed above), `notifications-mode`, `cancellation`,
`modes`, `options`, `plugins-mode`, `pi-mode`, `troubleshooting-keys`.

## Screencast Ideas

### 1. Getting Started (60s)

- Open Lattice from terminal
- Open a Rust file
- Basic normal-mode navigation (j, k, w, b, f, t)
- Search (`/`)
- Save (`:w`)
- Exit (`:q`)

**Purpose:** New user onboarding — "here's how to do the basics in 60 seconds"

### 2. Modal Editing Power (90s)

- Operators + motions: `ciw`, `daw`, `yit`
- Text objects: `ci(`, `da[`, `cit`
- Visual mode + `:norm`
- Macros (record + replay)

**Purpose:** Show that vim grammar is fully implemented; convince vim users they won't lose muscle memory

### 3. The Everything-is-a-Buffer Workflow (120s)

- Open file tree buffer
- Open files from tree
- Run `:search` — results in buffer
- Jump from search result to source
- Open terminal buffer in split
- Navigate between buffers with `:b` / `:bn` / `:bp`

**Purpose:** Show the composable buffer paradigm; this is Lattice's unique selling point

### 4. LSP in Action (90s)

- Open Rust file with error → see diagnostics in gutter
- Trigger completion with `.` in insert mode
- Navigate with `]d` / `[d`
- Rename symbol with `:lsp-rename`
- Format with `:lsp-format`

**Purpose:** Show LSP integration is first-class

### 5. Plugin Extensibility (120s)

- Install a WASM plugin
- Show plugin's new commands available
- Run plugin command
- Show plugin's UI / buffer integration
- Discuss `init.rs` config approach

**Purpose:** Show the extensibility model; differentiate from editors with limited plugin APIs

### 6. Picker + Frecency (60s)

- Open picker (`<space>` or `:pick`)
- Type partial filename → fuzzy matched
- See frecency-sorted results
- Preview file before selecting

**Purpose:** Fast navigation showcase

### 7. Diff and Merge (90s)

- `:diffthis` on two buffers
- Navigate hunks with `]c` / `[c`
- Apply changes with `do` / `dp`
- Three-way merge demo

**Purpose:** Show diff capability in action

### 8. Custom Configuration (120s)

- Show `init.rs` (Rust compiled to WASM)
- Add a custom keybinding
- Add a custom command
- Reload config with `:reload-config`
- New command works immediately

**Purpose:** Show the WASM config model — without Lua, without vimscript

## Technical Notes

- **Resolution:** 1440×900 for screenshots (clear on retina+non-retina)
- **Format:** PNG for screenshots, WebM/MP4 for screencasts, GIF for the
  README/site demo clips (see `README.md` in this directory for the VHS
  tapes that generate those)
- **Terminal font:** A patched Nerd Font (e.g. JetBrains Mono Nerd Font) at 14px
- **Theme:** Default dark theme for consistency (light as variant where noted)
- **Opacity:** No transparency/alpha on windows — pure dark background
- **Frame:** no window chrome — just the editor content area unless the screencast shows window management
- **Screencast length:** target 60-120 seconds per clip; < 30s for social-media clips
- **Voiceover:** None — text overlays/annotations instead (international audience)
- **Tool:** VHS for TUI demo GIFs (declarative, regenerable — see `README.md`); Kap (macOS), OBS (cross-platform), or Peek (Linux) for manual GPUI screenshots/screencasts

## Collection checklist

### Priority (differentiators)
- [ ] Magit status + transient (screenshot + `magit.gif`)
- [ ] Buffer splits: tree + code + terminal + search (screenshot + `buffers.gif`)
- [ ] `init.rs` custom command + `:reload-config` (screenshot + `config.gif`)
- [ ] Org agenda + agent buffer under diff review
- [ ] TUI + GPU renderer side by side

### Supporting
- [x] Hero dark — captured (L.5, 2026-09-20), downscaled from a 3680×2382 retina
      capture to 1920×1242 / 196 KB, committed at
      `assets/media/screenshots/hero-dark.png` and mirrored to
      `site/static/media/hero-dark.png` for the site hero. Wired into
      `README.md` and `site/templates/index.html` (`.hero-shot`).
- [ ] Hero light
- [ ] Modal editing
- [ ] LSP integration
- [ ] Multibuffer search
- [ ] Diff & merge
- [ ] Picker
- [ ] Help system
- [ ] Theme preview (3-4 themes)
- [ ] Ghost text completion
- [ ] Tutor
- [ ] Plugin WASM (general case)
- [ ] Magit rebase / log / blame / side-by-side diff
- [ ] Org capture / clocking / roam
- [ ] `:claude` / `:opencode` conversation + diff review
- [ ] Oil-mode directory edit
- [ ] Dashboard
- [ ] Compilation + error list
- [ ] REPL
- [ ] Which-key
- [ ] Surround
- [ ] Table mode
- [ ] Narrow / widen
- [ ] Folding
