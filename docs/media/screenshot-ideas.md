# Screenshot & Screencast Ideas

This doc lists screenshots and screencasts to collect over time for the Lattice website and README.

## Screenshots

### Hero / Editor (landing page hero)

- **What:** Lattice editing a Rust file with tree-sitter syntax highlighting, LSP completion popup visible, picker open, file tree buffer in a split pane, status line showing mode + filename
- **Why:** The first thing a visitor sees — must show Lattice looks good and real
- **Theme:** Dark theme (matches developer preference, more visually striking)
- **File:** `assets/media/screenshots/hero-dark.png` (1920×1080 or 1440×900)
- **Variant:** Light theme at `assets/media/screenshots/hero-light.png`

### Feature: Modal Editing

- **What:** Visual mode selection active, operator-pending state shown in status line, command line at bottom showing `:s/foo/bar`
- **Why:** Demonstrates vim grammar parity
- **File:** `assets/media/screenshots/modal-editing.png`

### Feature: LSP Integration

- **What:** Completion popup with documentation sidebar, diagnostics (error/warning gutters + inline), signature help
- **Why:** Shows language-aware editing works
- **File:** `assets/media/screenshots/lsp.png`

### Feature: Everything is a Buffer

- **What:** Split pane with file tree buffer (left), code buffer (center), terminal buffer (right-bottom), search results buffer (right-top)
- **Why:** The signature differentiator — shows traditional "sidebar + split" replaced by composable buffer splits
- **File:** `assets/media/screenshots/buffer-splits.png`

### Feature: Multibuffer Search

- **What:** `:search` results showing excerpt lines from multiple files, cursor on a result, preview highlighted
- **Why:** Shows project-wide search and replace workflow
- **File:** `assets/media/screenshots/multibuffer-search.png`

### Feature: Diff & Merge

- **What:** Three-pane diff view (two files + combined), diff sign gutter (add/remove/change), cursor on a hunk
- **Why:** Shows two/three-way diff capability
- **File:** `assets/media/screenshots/diff.png`

### Feature: Picker

- **What:** Fuzzy file picker open, showing file list with scores, preview window
- **Why:** Fast navigation showcase
- **File:** `assets/media/screenshots/picker.png`

### Feature: Help System

- **What:** `:describe-key` result showing keybinding documentation in a help buffer
- **Why:** Self-documenting editor philosophy
- **File:** `assets/media/screenshots/help-system.png`

### Feature: Theme Preview

- **What:** Same Rust file rendered in 3-4 different themes (tiled or carousel)
- **Why:** Shows theme support
- **File(s):** `assets/media/screenshots/theme-{name}.png`

### Feature: Completion Ghost Text

- **What:** Insert mode with ghost text completion inline, before accepting
- **Why:** Shows insert-completion with ghost text
- **File:** `assets/media/screenshots/ghost-text.png`

### Feature: Tutoral

- **What:** Tutor buffer open, interactive lesson with instructions panel
- **Why:** Shows built-in learning experience
- **File:** `assets/media/screenshots/tutor.png`

### Feature: Plugin (WASM)

- **What:** A plugin's custom command running, e.g. `:plugin-mycommand` with output in a buffer
- **Why:** Extensibility showcase
- **File:** `assets/media/screenshots/plugin-wasm.png`

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
- **Format:** PNG for screenshots, WebM/MP4 for screencasts
- **Terminal font:** A patched Nerd Font (e.g. JetBrains Mono Nerd Font) at 14px
- **Theme:** Default dark theme for consistency (light as variant where noted)
- **Opacity:** No transparency/alpha on windows — pure dark background
- **Frame:** no window chrome — just the editor content area unless the screencast shows window management
- **Screencast length:** target 60-120 seconds per clip; < 30s for social-media clips
- **Voiceover:** None — text overlays/annotations instead (international audience)
- **Tool:** Kap (macOS), OBS (cross-platform), or Peek (Linux) for screen capture

## Collection checklist

- [ ] Hero dark
- [ ] Hero light
- [ ] Modal editing
- [ ] LSP integration
- [ ] Buffer splits
- [ ] Multibuffer search
- [ ] Diff & merge
- [ ] Picker
- [ ] Help system
- [ ] Theme preview (3-4 themes)
- [ ] Ghost text completion
- [ ] Tutor
- [ ] Plugin WASM
