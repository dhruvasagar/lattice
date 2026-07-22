# Verification checklist

A thorough manual verification of every feature lattice ships
today. Use this for release sign-off, post-refactor regression
sweeps, and onboarding "what does the editor actually do."

Each section is independent; jump to whatever's relevant. Items
are checkboxes you tick as you go.

```sh
# Use a real source tree so motions / search / folds have
# meaningful content.
cargo run --release -- README.md
```

The user docs in [`../../user/`](../../user/) are the canonical
reference for each feature; this file is the runtime
counterpart.

---

## 1. Build, launch, baseline

- [ ] `cargo build --release` succeeds without warnings
- [ ] `cargo test --workspace` passes (recent baseline:
      1475 ui-tui + 738 host + 99 keymap + 25 gpui tests; all crates
      green as of 2026-06-16 — re-snapshot the totals on the next sweep)
- [ ] `cargo clippy --workspace --all-targets` clean
- [ ] `cargo fmt --all -- --check` clean
- [ ] `RUSTDOCFLAGS=-D warnings cargo doc --no-deps --workspace` clean
- [ ] `cargo run --release -- README.md` opens the file in the TUI
- [ ] Modeline shows file path + cursor position (`README.md  1:0`)
- [ ] Status line at the bottom shows the active mode

---

## 2. Modal editing

User doc: [`user/modes.md`](../../user/modes.md).

### Mode transitions

- [ ] Editor starts in **Normal** mode
- [ ] `i` enters **Insert**; modeline reflects this
- [ ] Type a character; it appears at the cursor
- [ ] `<Esc>` returns to Normal; cursor moves left one
- [ ] `a` enters Insert one column to the right
- [ ] `I` enters Insert at first non-blank
- [ ] `A` enters Insert at end-of-line
- [ ] `o` opens a new line below + Insert
- [ ] `O` opens a new line above + Insert
- [ ] `v` enters charwise **Visual**; selection extends with motions
- [ ] `V` enters linewise Visual; whole lines highlight
- [ ] `<C-v>` (or `<C-q>`) enters blockwise Visual; rectangular selection
- [ ] `R` enters **Replace**; typing overwrites instead of inserting
- [ ] `:` enters **Command** mode; cmdline appears at bottom
- [ ] `/` enters **Search**; same cmdline shape, prompt is `/`
- [ ] `?` enters Search backward; prompt is `?`
- [ ] In Visual mode, `o` swaps the selection's anchor and cursor
- [ ] `gv` (in Normal) re-selects the previous Visual selection

### Mode-aware echo

- [ ] In Insert mode, the echo line shows `-- INSERT --`
- [ ] In Visual modes, echo shows the selection shape and char/byte count
- [ ] Recording a macro (`qa`...): echo shows `recording @a`

---

## 3. Motions

User doc: [`user/folding.md`](../../user/folding.md) covers
fold-aware variants. Core motion list:

### Cursor / character

- [ ] `h` / `j` / `k` / `l` — left / down / up / right
- [ ] Arrow keys work in Insert mode
- [ ] `0` — start of line
- [ ] `^` — first non-blank
- [ ] `$` — end of line
- [ ] `g_` — last non-blank
- [ ] `gg` — first line
- [ ] `G` — last line
- [ ] `42G` — line 42
- [ ] `:42<CR>` — same as `42G`

### Word / token

- [ ] `w` — next word start
- [ ] `b` — previous word start
- [ ] `e` — next word end
- [ ] `ge` — previous word end
- [ ] `W` / `B` / `E` / `gE` — same but treats punctuation as part of word

### Paragraph / sentence

- [ ] `(` / `)` — previous / next sentence
- [ ] `{` / `}` — previous / next paragraph
- [ ] `]]` / `[[` — section forward / backward (where applicable)

### Find character on line

- [ ] `fX` — jump to next `X` on the line; cursor lands ON it
- [ ] `FX` — same backward
- [ ] `tX` — jump to char before next `X`
- [ ] `TX` — same backward
- [ ] `;` — repeat last `f` / `F` / `t` / `T`
- [ ] `,` — reverse-repeat
- [ ] `3fX` — jump to the third `X` ahead

### Match / pair

- [ ] `%` — jump to matching `(`/`)`, `[`/`]`, `{`/`}`, `<`/`>`
- [ ] On an unmatched bracket, `%` echoes "no match"

### Viewport

- [ ] `H` — top of viewport
- [ ] `M` — middle of viewport
- [ ] `L` — bottom of viewport
- [ ] `<C-d>` / `<C-u>` — half-page down / up
- [ ] `<C-f>` / `<C-b>` — full page down / up
- [ ] `<C-e>` / `<C-y>` — scroll one line down / up (cursor sticky)
- [ ] `zz` / `zt` / `zb` — center / top / bottom-align cursor row

### Position history

- [ ] `<C-o>` walks back through the position history
- [ ] `<C-i>` walks forward
- [ ] `m{a-z}` sets a named mark; `'a` jumps to it
- [ ] `:marks<CR>` lists every set mark

---

## 4. Operators

User doc: lattice's operator grammar mirrors vim.

- [ ] `dw` deletes a word; cursor lands at the deletion site
- [ ] `de` deletes to end-of-word
- [ ] `dd` deletes a line
- [ ] `2dd` deletes 2 lines (count + linewise)
- [ ] `D` deletes to end-of-line
- [ ] `yw` yanks a word into `"` register
- [ ] `yy` yanks a line
- [ ] `Y` yanks to end-of-line
- [ ] `cw` changes a word (delete + Insert)
- [ ] `cc` changes a line
- [ ] `C` changes to end-of-line
- [ ] `s` substitutes the cursor char (delete one + Insert)
- [ ] `S` substitutes the line
- [ ] `>>` indents the line
- [ ] `<<` dedents the line
- [ ] `2>>` indents 2 lines
- [ ] `gUU` upper-cases the line
- [ ] `guu` lower-cases the line
- [ ] `g~~` toggles case on the line
- [ ] `~` toggles case on the cursor char
- [ ] `=` re-indents using the active formatter (LSP if `lsp-format-mode` on)
- [ ] After any operator, `.` repeats the last change

### Operator + count + motion

- [ ] `d3w` deletes 3 words
- [ ] `5dd` deletes 5 lines
- [ ] `c3l` changes 3 chars
- [ ] `y$` yanks to end-of-line

### Operators on the Visual selection

Every operator acts on the active selection BY DESIGN (one registration
wires both Normal op-pending and Visual). Select a range with `v` / `V` /
`<C-v>`, then:

- [ ] `d` / `x` deletes the selection
- [ ] `c` / `s` changes the selection (delete + Insert)
- [ ] `y` yanks the selection
- [ ] `>` / `<` indents / dedents the selected lines
- [ ] `gU` / `gu` / `g~` upper / lower / toggle-case the selection
- [ ] `zn` narrows to the selection (the narrow operator works in Visual
      too — see §28)
- [ ] After applying, the editor returns to Normal mode

---

## 5. Text objects

- [ ] `daw` — delete a word (with surrounding whitespace)
- [ ] `diw` — delete inner word (no whitespace)
- [ ] `da"` / `di"` — around / inner double-quote string
- [ ] `da'` / `di'` — single-quote
- [ ] `da(` / `di(` — around / inner parens (also `dab` / `dib`)
- [ ] `da{` / `di{` — around / inner braces (also `daB` / `diB`)
- [ ] `da[` / `di[` — around / inner brackets
- [ ] `da<` / `di<` — angle brackets
- [ ] `dap` / `dip` — around / inner paragraph
- [ ] `das` / `dis` — around / inner sentence
- [ ] All also work with `c`, `y`, `>`, `<`, `gU`, `gu`

### Tree-sitter text objects (open a `.rs` / `.py` / `.js` file)

Universal grammar objects owned by `lattice-syntax`; compose with every
operator and with `zn`. Innermost-wins on nesting.

- [ ] `daf` / `dif` — delete a / inner function
- [ ] `dac` / `dic` — delete a / inner class / struct / type
- [ ] `daa` / `dia` — delete a / inner parameter (charwise-accurate, e.g.
      `daa` deletes exactly `x: i32`, not the whole line)
- [ ] `dal` / `dil` — delete a / inner loop
- [ ] `daC` / `diC` — delete a / inner comment (commentstring-driven;
      `aC` keeps the markers, `iC` strips the leader)
- [ ] `vaf` selects the whole function (objects work in Visual too)
- [ ] `yaf` on a nested closure inside a fn targets the innermost (closure)
- [ ] `daf` with the cursor outside any function is a no-op (no edit / bell)

---

## 6. Search & Substitute

User doc: section in [`user/buffers.md`](../../user/buffers.md).

### Search

- [ ] `/foo<CR>` jumps to the next `foo`
- [ ] All matches are highlighted (hlsearch on by default)
- [ ] `n` jumps to the next match; `N` to the previous
- [ ] Matches wrap around the buffer
- [ ] `:noh<CR>` clears the hlsearch overlay
- [ ] `?bar<CR>` searches backward
- [ ] `*` searches forward for the word under cursor
- [ ] `#` searches backward for the word under cursor
- [ ] Multibyte cursor: search lands on the first byte/cell of the match
      (e.g. `§Perf` cursor lands on `P`, not on `e`)
- [ ] Live highlight while typing in `/`: matches glow as you type
- [ ] `<Esc>` cancels search; cursor returns to start position
- [ ] Backref pattern (`/(\w+) \w+ \1`) finds duplicate-word neighbours

### Substitute

- [ ] `:s/foo/bar<CR>` replaces the first match on the current line
- [ ] `:s/foo/bar/g` replaces every match on the current line
- [ ] `:%s/foo/bar/g` replaces every match in the buffer
- [ ] `:'<,'>s/foo/bar/g` (in Visual) replaces within the selection
- [ ] Live preview: as you type `:s/foo/bar/g`, current line's matches
      light up in magenta with strike-through
- [ ] `:%s/foo/bar/g` previews highlight every match in the buffer
- [ ] Cancelling (`<Esc>`) clears the preview without modifying anything
- [ ] Submitting (`<CR>`) performs the substitute; preview clears

---

## 7. Registers & macros

- [ ] `"ayy` yanks the line into register `a`
- [ ] `"ap` pastes from register `a`
- [ ] `:registers<CR>` (or `:reg`) lists all populated registers
- [ ] `qa` starts recording into register `a`
- [ ] `q` stops recording
- [ ] `@a` plays back the macro
- [ ] `@@` repeats the last-played macro
- [ ] Numbered registers `"0..9` carry recent yanks/deletes per vim convention
- [ ] `"_` (black hole) — `"_dd` deletes without affecting any register

---

## 8. Marks

- [ ] `ma` sets mark `a` at the cursor
- [ ] `'a` jumps to the line of mark `a` (first non-blank)
- [ ] `` `a `` jumps to the exact column of mark `a`
- [ ] `:marks<CR>` lists all set marks
- [ ] Marks survive across mode switches and buffer switches
- [ ] `''` jumps to the previous cursor location

---

## 9. Ex-commands

- [ ] `:w<CR>` writes the buffer to its path
- [ ] `:w foo.txt` writes to `foo.txt`
- [ ] `:q<CR>` quits if the buffer is clean
- [ ] `:q!<CR>` quits unconditionally
- [ ] `:wq<CR>` writes + quits
- [ ] `:e foo.rs<CR>` opens `foo.rs` in the active pane
- [ ] `:e!<CR>` reloads the current file from disk
- [ ] `:bn<CR>` cycles to the next buffer
- [ ] `:bp<CR>` cycles to the previous buffer
- [ ] `:ls<CR>` lists every open buffer
- [ ] `:b 3<CR>` switches to buffer 3
- [ ] `:b<CR>` (no arg) opens the buffer picker
- [ ] `:bd<CR>` closes the active buffer
- [ ] `:set foo=bar<CR>` sets typed option `foo` to `bar`
- [ ] `:set foo?<CR>` echoes the current value
- [ ] `:set nofoo<CR>` (boolean) sets to false
- [ ] `:noh<CR>` clears hlsearch
- [ ] `:reg<CR>` shows registers
- [ ] `:marks<CR>` shows marks
- [ ] Ex-command arg-prompt: `:describe-command<CR>` prefills the cmdline
      with `describe-command ` and the echo shows `command:`
- [ ] After an arg-prompt is armed, `<Esc>` cancels back to Normal

### `:g` / `:v`

- [ ] `:g/^$/d<CR>` deletes every empty line
- [ ] `:g/^/d<CR>` deletes every line
- [ ] `:v/foo/d<CR>` deletes every line that doesn't contain `foo`
- [ ] `:g/foo/<CR>` (empty body) errors with "empty body"
- [ ] `:g/foo/not-a-command<CR>` errors at submit (parsed at submit time)

---

## 10. Buffers & Panes

User doc: [`user/buffers.md`](../../user/buffers.md).

### Buffers

- [ ] Multiple `:e` calls accumulate buffers in the registry
- [ ] `:ls<CR>` (or `:buffers`) shows every entry with `%` for active
- [ ] `:bn` / `:bp` cycle through every buffer kind (doc, file-tree, oil, ...)
- [ ] `:b 1<CR>` activates buffer #1
- [ ] `:b foo<CR>` activates a buffer whose path contains `foo`
- [ ] `:bd` closes the active buffer
- [ ] `:bd!` closes even if dirty
- [ ] Closing a dirty buffer without `!` echoes an error

### Panes / splits

- [ ] `<C-w>s` splits the active pane horizontally
- [ ] `<C-w>v` splits vertically
- [ ] `<C-w>w` cycles focus through panes
- [ ] `<C-w>h/j/k/l` moves focus by direction
- [ ] `<C-w>q` closes the active pane
- [ ] `<C-w>=` equalises pane sizes
- [ ] Each pane has its own cursor + scroll state for its buffer
- [ ] Same buffer in two panes: cursors are independent
- [ ] Closing the last pane shows the empty-buffer state, not a crash

---

## 11. File tree

User doc: [`user/buffers.md`](../../user/buffers.md) (file-tree
section).

- [ ] `:Tree<CR>` opens the file tree in a new pane
- [ ] Tree shows `▸` for collapsed, `▾` for expanded directories
- [ ] File icons appear (when nerd-fonts are on; toggle via theme)
- [ ] `j` / `k` navigate entries
- [ ] `<CR>` on a file opens it in the original pane
- [ ] `<CR>` on a directory toggles expand / collapse
- [ ] Hidden files (`.git`, etc.) shown dimmer than regular files
- [ ] `:TreeClose<CR>` closes the tree pane

---

## 12. Oil buffer

User doc: [`user/buffers.md`](../../user/buffers.md) (oil
section).

- [ ] `:Oil<CR>` opens the current document's parent dir as an oil buffer
- [ ] `:Oil /tmp<CR>` opens `/tmp`
- [ ] Each row shows one entry (file or directory) by name; icons
      render per file type
- [ ] `<CR>` on a file opens it in the original pane
- [ ] `<CR>` on a directory navigates into it (replaces listing)
- [ ] `-` from oil navigates to the parent directory
- [ ] `-` from a Document buffer opens oil at the document's parent
- [ ] Edit a row's name (`cw`) → `:w<CR>` renames the file on disk
- [ ] Delete a row (`dd`) → `:w<CR>` removes the entry
- [ ] Add a new line (`o NAME<Esc>`) → `:w<CR>` creates the file
- [ ] One delete + one create → rename heuristic kicks in (preserves
      file attributes)
- [ ] Two+ of either side → independent deletes / creates execute
      in order (renames → deletes → creates)
- [ ] Echo after `:w` reports `oil: applied changes in <dir>`

---

## 13. Folding

User doc: [`user/folding.md`](../../user/folding.md).

### Manual folds

- [ ] `Vjj` (linewise visual over 3 lines) then `zf` creates a fold
- [ ] Fold renders one line: heading + ` ┄ N lines folded` suffix
- [ ] `zo` opens the fold under cursor
- [ ] `zc` closes it again
- [ ] `za` toggles open/closed
- [ ] `zR` opens every fold
- [ ] `zM` closes every fold
- [ ] `zd` deletes the manual fold
- [ ] `zj` / `zk` jump to next / previous fold start

### Computed folds

- [ ] `:set foldmethod=indent<CR>` switches to indent-based folds
- [ ] `:set foldmethod=markdown<CR>` switches to markdown headings
- [ ] `:set foldmethod=syntax<CR>` switches to tree-sitter cascade
- [ ] `:set foldmethod=manual<CR>` returns to manual-only

### Operator interaction

- [ ] `dd` on a closed fold deletes the entire fold range (every line)
- [ ] `yy` on a closed fold yanks the entire fold range
- [ ] `>>` on a closed fold indents every line in the fold
- [ ] Linear motions (`j` / `k`) skip closed folds (one keystroke crosses)
- [ ] Word motions (`w` / `b`) treat the fold as a single line

### Search vs folds

- [ ] `/foo<CR>` opens the fold containing the match if any
- [ ] `n` / `N` opens enclosing folds as needed

---

## 14. Help system

User docs: every file under [`../../user/`](../../user/).

### `:help`

- [ ] `:help<CR>` opens the topic index
- [ ] `:help folding<CR>` opens the folding topic
- [ ] `:help <Tab>` enumerates registered topics
- [ ] Markdown rendering: headings, code fences, tables, links
- [ ] `<CR>` on a `[topic](help:topic)` link follows to that topic
- [ ] `<C-o>` walks back through navigation history
- [ ] `q` / `<Esc>` dismisses the help popup

### `:describe-*`

- [ ] `:describe-command write<CR>` shows full metadata for `:w`
- [ ] `:describe-command<CR>` (no arg) prefills the cmdline
- [ ] `:describe-key gd<CR>` shows what `gd` does
- [ ] `:describe-buffer<CR>` shows the active buffer's state
- [ ] `:describe-option number<CR>` shows full metadata for `number`
- [ ] `:describe-option-resolution number<CR>` shows the resolver layer view
- [ ] `:describe-events<CR>` shows the typed-event catalogue
- [ ] `:describe-event lsp.buffer-attached<CR>` shows one event
- [ ] `:describe-mode lsp-mode<CR>` shows mode metadata
- [ ] `:apropos buffer<CR>` shows every command/option/event matching `buffer`

### `:list-*`

- [ ] `:options<CR>` lists every customizable option, grouped by group
- [ ] `:list-modes<CR>` lists every registered mode (majors + minors)
- [ ] `:keymap<CR>` lists every default chord binding
- [ ] `:registers<CR>` shows registers
- [ ] `:marks<CR>` shows marks

---

## 15. Modes (major + minor)

User doc: [`user/modes.md`](../../user/modes.md).

### Toggle

- [ ] `:lsp-mode<CR>` toggles lsp-mode on the active buffer
- [ ] `:list-modes<CR>` shows lsp-mode now active (with `*` marker)
- [ ] `:line-numbers-mode<CR>` toggles line-numbers-mode
- [ ] `:wrap-mode<CR>` toggles wrap-mode
- [ ] `:read-only-mode<CR>` toggles read-only-mode
- [ ] `:current-line-highlight-mode<CR>` toggles cursor-line bg
- [ ] `:whitespace-show-mode<CR>` toggles whitespace decoration
- [ ] Toggling an unknown mode echoes "not a registered mode"

### Convergence (M.7.1)

- [ ] `:set number<CR>` activates `line-numbers-mode` (visible in `:list-modes`)
- [ ] `:set nonumber<CR>` deactivates it
- [ ] `:set wrap<CR>` activates `wrap-mode`
- [ ] `:set list<CR>` activates `whitespace-show-mode`
- [ ] `:set cursorline<CR>` activates `current-line-highlight-mode`

### Major-mode swap

- [ ] On a `.rs` file, the major is `rust-mode` (`:list-modes` shows `*`)
- [ ] `:text-mode<CR>` swaps to plain text major
- [ ] Active minors survive the swap

---

## 16. Options & customize

User doc: [`user/options.md`](../../user/options.md).

### `:set` & `:options`

- [ ] `:options<CR>` shows every customizable option grouped by group
      (editor, completion, display, ...)
- [ ] Each row shows name + aliases + type + current value + default + doc
- [ ] `:set tabstop=4<CR>` sets tabstop; visible in subsequent reads
- [ ] `:set tabstop?<CR>` echoes `tabstop=4`
- [ ] `:set tabstop=999<CR>` errors with the validator's range message
- [ ] `:set foldmethod=<Tab>` enumerates `manual`, `indent`, `markdown`, `syntax`
- [ ] `:set rnu<CR>` (alias for `relativenumber`) works
- [ ] `:set noix<CR>` rejected; `:set noignorecase<CR>` works

### `:customize`

- [ ] `:customize<CR>` opens the picker view (groups + customizable modes)
- [ ] Each row is a `[name](customize:name)` link
- [ ] `<CR>` on the `editor` row opens `:customize editor` focused view
- [ ] `:customize editor<CR>` lists every editor-group option
- [ ] `:customize lsp-completion-mode<CR>` lists options that mode contributes
      (with `[mode-shadow]` indicator if active on current buffer)
- [ ] `:customize` unknown name echoes error
- [ ] `<CR>` on an option row prefills `:set NAME=current` and enters Command
- [ ] Editing the value + submit applies via `:set` (cascade fires)

---

## 17. LSP basics

User doc: [`user/lsp.md`](../../user/lsp.md) +
[`user/lsp-mode.md`](../../user/lsp-mode.md).

Open a `.rs` file (or any language with a configured server).

### Auto-attach

- [ ] Modeline shows `[lsp:rust-analyzer]` (or whatever server)
- [ ] `:lsp-status<CR>` shows attached servers + capabilities
- [ ] `:lsp-log<CR>` shows the server's log
- [ ] `:lsp-trace rust<CR>` enables JSON-RPC tracing
- [ ] `:lsp-trace-log rust<CR>` shows the trace log
- [ ] `:lsp-restart rust<CR>` restarts the server
- [ ] `:lsp-server-log<CR>` shows the server's stderr

### Umbrella gate

- [ ] `:lsp-mode<CR>` toggles the umbrella OFF
- [ ] `K` echoes "lsp-mode disabled" (gate fires)
- [ ] Modeline `[lsp:...]` segment hides
- [ ] Diagnostics disappear from the gutter
- [ ] `:lsp-mode<CR>` re-toggles ON; everything resumes

---

## 18. LSP sub-modes (M.6)

User doc: [`user/lsp-mode.md`](../../user/lsp-mode.md) §
"Sub-modes".

With `lsp-mode` ON (cascade activates all 15 sub-modes):

- [ ] `:list-modes<CR>` shows every sub-mode active:
      `lsp-completion-mode`, `lsp-diagnostics-mode`,
      `lsp-hover-mode`, `lsp-signature-mode`, `lsp-format-mode`,
      `lsp-rename-mode`, `lsp-symbols-mode`, `lsp-code-action-mode`,
      `lsp-nav-mode`, `lsp-progress-mode`, `lsp-document-highlight-mode`,
      `lsp-selection-range-mode`, `lsp-folding-mode`,
      `lsp-inlay-hint-mode`, `lsp-semantic-tokens-mode`
- [ ] `:lsp-hover-mode<CR>` toggles only hover off; `K` echoes
      "lsp-hover-mode disabled"
- [ ] `:lsp-diagnostics-mode<CR>` off: diagnostics disappear from gutter,
      but other features still work
- [ ] `:lsp-format-mode<CR>` off: `:lsp-format` echoes "lsp-format-mode disabled"
- [ ] `:lsp-nav-mode<CR>` off: `gd` echoes "lsp-nav-mode disabled"
- [ ] `:lsp-inlay-hint-mode<CR>` off: virtual-text inlay hints disappear
- [ ] `:lsp-semantic-tokens-mode<CR>` off: server-driven highlight reverts to
      tree-sitter colours
- [ ] `:lsp-document-highlight-mode<CR>` off: the soft bg tint on uses of the
      symbol under cursor disappears
- [ ] Cycling `:lsp-mode<CR>` twice (off→on) resets every sub-mode to active

### Per-feature checks (with all sub-modes on)

- [ ] `K` opens the hover popup (cursor-anchored, markdown-rendered)
- [ ] `gd` jumps to definition; `<C-o>` returns
- [ ] `gD` jumps to declaration
- [ ] `gy` jumps to type definition
- [ ] `gI` jumps to implementation
- [ ] `gr` opens the references picker
- [ ] `:lsp-symbols<CR>` opens the document symbol picker
- [ ] `:lsp-workspace-symbol foo<CR>` opens the workspace symbol picker
- [ ] `:lsp-format<CR>` formats the buffer
- [ ] `:lsp-format-range<CR>` (in Visual) formats the selection
- [ ] `:lsp-rename newname<CR>` renames the symbol under cursor
- [ ] `:lsp-code-action<CR>` opens the code-action picker
- [ ] Typing `(` in a function call opens signature help
- [ ] `:complete<CR>` (or insert-mode auto-trigger) opens completion popup
- [ ] Typing inside a function on a server-advertised trigger character
      (e.g. `}` in Rust) fires on-type formatting

### Diagnostics (push + pull)

- [ ] Errors render with `■` glyph (red) in the gutter
- [ ] Warnings with `▲` (yellow)
- [ ] Info with `●` (blue)
- [ ] Hints with `·` (dim gray)
- [ ] Underline overlay on the affected range
- [ ] `]d` jumps to the next diagnostic
- [ ] `[d` jumps to the previous
- [ ] `:diagnostics<CR>` opens the diagnostics picker
- [ ] `:next-error` / `:cnext` walk the **error list** (compiler/tool
      output), NOT diagnostics; an empty list echoes `no error list`
- [ ] On a server advertising pull (`textDocument/diagnostic`), edits trigger
      a refresh without push notifications (verify in `:lsp-trace`:
      `→ textDocument/diagnostic` after a doc-version change)

### Advanced navigation (Phase 4.5)

- [ ] `:lsp-incoming-calls<CR>` opens a picker of callers of the
      function under the cursor (call hierarchy)
- [ ] `:lsp-outgoing-calls<CR>` opens a picker of callees
- [ ] `<C-t>` after either pops the tag stack back to the original site
- [ ] `:lsp-supertypes<CR>` opens supertypes of the type under cursor
- [ ] `:lsp-subtypes<CR>` opens subtypes
- [ ] `:lsp-code-lens<CR>` opens a picker of code lenses (rust-analyzer
      typically surfaces `Run`/`Debug`/`References` actions); accept fires
      the lens command
- [ ] `:lsp-moniker<CR>` echoes the cross-repo moniker(s) at the cursor
      (scheme:identifier (kind) form)
- [ ] `:lsp-color-presentation<CR>` on a color literal in CSS/SCSS opens a
      picker of alternative representations; accept splices the chosen form
- [ ] `gx` on a URL inside a doc-comment / markdown link follows the link
      (file:// opens via `:e`; http:// goes through the OS handler)

### Selection ranges (smart-expand)

- [ ] `:lsp-expand-region<CR>` (or chord, if bound) widens Visual selection
      to the next semantic range (token → expr → statement → block → fn)
- [ ] `:lsp-shrink-region<CR>` walks back inward
- [ ] Moving the cursor out of the chain invalidates it; next expand starts
      fresh

### Inline overlays

- [ ] **Inlay hints**: type / parameter hints appear as italic dim virtual
      text inside the line (e.g. `let x: i32 = ...` shows the `: i32`)
- [ ] Scrolling far outside the cached window triggers a re-fetch (verify
      via `:lsp-trace`: a fresh `inlayHint` round-trip on big jumps, none
      on small scrolls)
- [ ] **Semantic tokens**: server-driven highlight repaints the foreground
      over tree-sitter; e.g. rust-analyzer marks unsafe / mutable / unused
      tokens with distinct colours
- [ ] After several edits, a `→ semanticTokens/full/delta` request appears
      in the trace (proves delta path works; full + delta is in use)
- [ ] **Document highlight**: parking the cursor on an identifier paints a
      soft bg tint on every occurrence in the buffer (read = green-ish,
      write = red-ish, text = blue-ish)
- [ ] **LSP folding**: `:set foldmethod=lsp<CR>` switches to server-driven
      folds; `zc` / `zo` work on server-provided fold regions (function
      bodies, imports, etc.). Falls back to tree-sitter / indent when no
      server advertises foldingRange.

### Server-initiated UX

- [ ] `:lsp-restart rust-analyzer<CR>` echoes "queued" then later reports
      `restarted N actors, replayed N didOpens` in `:lsp-log`
- [ ] An in-flight `cargo check` shows a `[rust-analyzer: <task>]` segment
      in the modeline (progress accumulator)
- [ ] `:lsp-progress-mode<CR>` off hides the modeline segment
- [ ] `:lsp-progress-cancel rust-analyzer<CR>` sends a cancel notification
      (verify via `:lsp-trace`: `→ window/workDoneProgress/cancel`)
- [ ] A server `window/showMessage` lands as an `:echo` with severity
      colour (rust-analyzer rarely fires one — easier to verify against
      a misconfigured server)
- [ ] `window/showMessageRequest` opens an action picker; accepting replies
      with the chosen item; cancelling replies `null`
- [ ] Server `window/showDocument` for a `file://` URI opens it via `:e`;
      `external: true` delegates to `xdg-open` / `open` / `explorer`

### Workspace events

- [ ] Editing `~/.config/lattice/lattice.toml` to set
      `lsp.rust-analyzer.checkOnSave.enable = false` then restarting fans
      out a `workspace/didChangeConfiguration` notification to every
      attached `rust-analyzer` actor (verify in `:lsp-trace`)
- [ ] `:set lsp.rust-analyzer.checkOnSave.enable=true<CR>` cascades the
      same notification at runtime
- [ ] On a server that registers `workspace/didChangeWatchedFiles`
      patterns (rust-analyzer registers `**/Cargo.toml`,
      `**/*.rs`, etc.), externally touching a matched file (e.g.
      `touch Cargo.toml` from a shell) fires a `→
      workspace/didChangeWatchedFiles` notification in the trace
- [ ] `:w` on a buffer at a path that didn't previously exist on disk
      fires `→ workspace/didCreateFiles`
- [ ] Dynamic capabilities: a server that registers e.g.
      `textDocument/formatting` post-init via `client/registerCapability`
      lights up `:lsp-format` even if static caps didn't advertise it
      (verify via `:lsp-trace`: `← client/registerCapability` followed
      by `:lsp-format` succeeding)

### Logging & debugging

- [ ] `:lsp-log<CR>` shows the aggregate subsystem log
- [ ] `:lsp-log rust-analyzer<CR>` shows the per-server log
- [ ] `:lsp-trace rust-analyzer<CR>` toggles JSON-RPC trace recording on;
      a `$/setTrace` with `verbose` lands on the wire
- [ ] `:lsp-trace-log rust-analyzer<CR>` opens the trace buffer; `←` /
      `→` markers distinguish inbound vs. outbound
- [ ] `:lsp-server-log rust-analyzer<CR>` opens the stderr-tail buffer
- [ ] `:lsp-log-level rust-analyzer debug<CR>` flips the per-server level
- [ ] `:lsp-log-clear<CR>` empties the log ring
- [ ] `lsp.log_level = "debug"` in TOML applies at boot (verify with
      `:lsp-log-level`)

---

## 19. Insert completion

User doc: [`user/completion.md`](../../user/completion.md).

In Insert mode in a `.rs` file:

- [ ] Typing a few chars opens the completion popup
- [ ] Popup shows source-tag prefix (`gen:lsp-completion`, `gen:snippet`, ...)
- [ ] `<Tab>` selects the next candidate
- [ ] `<S-Tab>` selects the previous
- [ ] `<C-n>` / `<C-p>` also work
- [ ] `<CR>` accepts the focused candidate
- [ ] `<Esc>` dismisses the popup without accepting
- [ ] Single-candidate auto-insert: typing a unique prefix at popup-open
      time inserts directly (when `completion.auto_insert_single = true`)
- [ ] Ghost text (`completion.ghost_text = true`): top candidate shows
      as dim italic suffix at end-of-line

### Sources

- [ ] LSP completion (rust-analyzer items appear in the popup)
- [ ] Buffer-words: completion of words elsewhere in the buffer
- [ ] Snippets: snippet names appear with `[snip]` tag
- [ ] Tree-sitter symbols: definition-position identifiers
- [ ] Path: when cursor is in a string literal that looks like a path

### Per-language overrides

- [ ] `[completion.per-language.rust]` in TOML can override priorities
- [ ] `:set completion.source.lsp.priority=300<CR>` raises LSP priority

---

## 20. Snippets

- [ ] In Insert mode, type a snippet trigger (e.g. `pln`) + `<C-y>` (or
      whatever the configured expand chord is)
- [ ] Snippet expands; cursor lands on the first tabstop
- [ ] `<Tab>` jumps to the next tabstop
- [ ] `<S-Tab>` jumps backward
- [ ] Editing one tabstop with mirrors updates every mirror
- [ ] Final tabstop ends snippet; cursor stays in Insert
- [ ] `:reload-snippets<CR>` re-reads snippet files from disk

---

## 21. Display options

User doc: [`user/options.md`](../../user/options.md).

### Line numbers

- [ ] `:set number<CR>` shows the gutter
- [ ] `:set relativenumber<CR>` shows distances; cursor row absolute
- [ ] `:set norelativenumber<CR>` returns to absolute
- [ ] `:set nonumber<CR>` hides the gutter

### Wrap

- [ ] `:set wrap<CR>` wraps long lines visually
- [ ] `:set nowrap<CR>` returns to horizontal scroll

### Whitespace decoration (M.7.3)

- [ ] `:set list<CR>` (or `:whitespace-show-mode<CR>`) enables decoration
- [ ] Tabs render as `→` in dim gray
- [ ] Trailing whitespace renders as `·` in red
- [ ] Leading non-tab whitespace renders as `·` in dim gray
- [ ] Mid-text spaces unchanged (default)
- [ ] EOL marker not shown (default)
- [ ] `:set display.whitespace.tab=⇥<CR>` changes the tab glyph
- [ ] `:set display.whitespace.eol=¬<CR>` enables EOL markers
- [ ] `:set nolist<CR>` disables decoration

### Current-line highlight (M.7.3)

- [ ] `:set cursorline<CR>` enables the cursor row's bg highlight
- [ ] Highlight extends to the pane's right edge
- [ ] Inactive panes don't get the highlight
- [ ] Selection still wins per-cell where it overlaps the cursor row
- [ ] `:set nocursorline<CR>` disables it

### Read-only

- [ ] `:read-only-mode<CR>` makes the active buffer read-only
- [ ] Mutating operators (`dd`, `i`, ...) echo "buffer is read-only"
- [ ] `:read-only-mode<CR>` again returns to writable

---

## 22. Tabs / indent

- [ ] `:set tabstop=4<CR>` makes a tab character render as 4 cells
- [ ] `:set tabstop=8<CR>` returns to default
- [ ] In Insert mode, `<Tab>` inserts a real tab (or expands per
      indent settings if those land later)

---

## 23. Position history (jump list / mark ring)

- [ ] After `gg`, `<C-o>` returns to the previous cursor location
- [ ] After multiple jumps (`G`, `<C-o>`, `gd`, ...), `<C-o>` walks back
      through them
- [ ] `<C-i>` walks forward
- [ ] `:jumps<CR>` (when implemented) shows the position history

---

## 24. Help popup geometry (M.7.3 follow-up)

- [ ] `:help<CR>` opens a centered popup that's visibly larger than
      old (up to 120 wide, 40 tall, scaled to terminal size)
- [ ] `:options<CR>`, `:describe-*`, `:apropos`, `:customize` all
      use the larger centered placement
- [ ] `K` opens a hover popup that's small (cursor-anchored, tooltip
      caps unchanged: 30..=80 wide, 5..=20 tall)
- [ ] In a 60-row terminal, centered popup uses ~40 rows
- [ ] In a 20-row terminal, centered popup uses ~15 rows (3/4 of buffer)

---

## 25. Themes / appearance

- [ ] `:set ui.dim_inactive=false<CR>` makes inactive panes look identical
      to the active one (no DIM modifier overlay)
- [ ] `:set ui.dim_inactive=true<CR>` returns to dimmed inactive
- [ ] Diagnostic glyphs render with the configured per-severity colors

---

## 26. Performance smoke

Open a large file (`crates/lattice-ui-tui/src/render.rs` is 3k+ lines):

- [ ] `gg` lands instantly
- [ ] `G` lands instantly
- [ ] `j`/`k` motion feels responsive (no perceptible latency)
- [ ] `/foo<CR>` finds matches without freezing
- [ ] `:%s/foo/bar/g` completes within a frame for typical patterns
- [ ] LSP request (`K`) returns quickly; no UI freeze

Latest measured numbers:
[`benchmarks.md`](benchmarks.md).

---

## 27. Cross-cutting

### Error handling

- [ ] Submitting an unknown ex-command echoes "unknown command: foo"
- [ ] `:set unknown=1<CR>` echoes E518
- [ ] `:bd` on the only buffer closes the editor (or echoes a message)
- [ ] Opening a non-existent file with `:e missing.txt` creates a new buffer
- [ ] Opening a binary file (try `cargo run -- /bin/ls`) doesn't crash

### Recovery

- [ ] `<C-z>` (or terminal suspend) suspends the editor; `fg` resumes
      cleanly without losing state
- [ ] Resizing the terminal: layout reflows; popup re-positions
- [ ] Closing a popup with `q` always returns control to the document

### Unicode

- [ ] Lines with `§` / `中` / emoji render correctly
- [ ] Cursor lands on the first cell of a wide glyph after motion
- [ ] Search / substitute correctly handles multibyte ranges

---

## 28. Narrow mode (`zn`)

User doc: design fragment
[`../architecture/narrow-mode.md`](../architecture/narrow-mode.md).

Open a `.rs` file with a few functions.

- [ ] `znip` narrows to the current paragraph (one-excerpt view opens)
- [ ] `zni{` narrows to the surrounding brace block
- [ ] `znG` narrows from the cursor to end-of-file
- [ ] `3znj` narrows the cursor line plus 3 below
- [ ] `znaf` narrows to the whole function (tree-sitter object + operator)
- [ ] The narrow view's gutter shows the SOURCE line numbers
- [ ] Syntax highlighting is live inside the narrow view
- [ ] Editing a line in the narrow view propagates to the source buffer
- [ ] `:w` inside the narrow view saves the underlying source file on disk
- [ ] `:widen` closes the narrow view
- [ ] `:42,67narrow<CR>` narrows to an explicit line range
- [ ] In Visual mode, selecting lines then `zn` narrows the selection
- [ ] `:'<,'>narrow` (from Visual `:`) also narrows the selection
- [ ] From inside a narrow view, `znac` opens a new narrow on the struct in
      the ORIGINAL file (stacked narrow stays one hop from the source)
- [ ] Two narrows over the same file, side by side: editing one updates the
      other on the next tick

---

## 29. Decoration retention across focus (DR.x)

- [ ] Split (`<C-w>v`) with two `.rs` buffers, both syntax-highlighted +
      inlay hints + diagnostics visible
- [ ] `<C-w>w` to the other pane: the now-inactive pane KEEPS its full
      syntax / inlay / diagnostic decorations (only opacity dims if
      `ui.dim_inactive=true`)
- [ ] Focusing back does NOT require a keystroke to repaint — decorations
      were never torn down
- [ ] Editing in one pane leaves the other pane's decorations stable
      (unchanged lines never lose their cues)
- [ ] `:redraw<CR>` is the deliberate clean-slate escape hatch (forces a
      full rebuild)

---

## What to do if a check fails

1. Confirm latest `main`: `git log --oneline -1`.
2. Rebuild clean: `cargo clean && cargo build --release`.
3. Capture exact keystrokes + visual output. Include the commit
   SHA.
4. Open a GitHub issue with the above + `cargo --version` + OS /
   terminal.

---

## See also

- User-facing reference: [`../../user/`](../../user/) — every
  feature has a deep-dive doc.
- Implementation status:
  [`implementation.md`](implementation.md) — what ships today
  vs. what's spec'd.
- Architecture diagrams:
  [`../architecture/diagrams.md`](../architecture/diagrams.md)
  — visual entry points to the design.
