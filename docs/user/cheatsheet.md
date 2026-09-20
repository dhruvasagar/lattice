---
summary: "One-page keystroke reference: modes, motions, operators, git, LSP."
related: [modal-editing, ex-commands]
---

# Cheatsheet

## Normal mode

| Key | Action |
|---|---|
| `h` `j` `k` `l` | Move cursor |
| `w` `b` `e` | Word motions |
| `0` `^` `$` | Line start / first non-whitespace / line end |
| `f{char}` `F{char}` | Find forward / backward on line |
| `t{char}` `T{char}` | Until forward / backward on line |
| `%` | Matching bracket |
| `gg` `G` | First line / last line |
| `{N}gg` `{N}G` | Go to line N |
| `/` `?` | Search forward / backward |
| `n` `N` | Next / previous match |
| `*` `#` | Search word under cursor forward / backward |

### Operators

| Key | Action |
|---|---|
| `d{motion}` | Delete |
| `c{motion}` | Change (delete + enter insert) |
| `y{motion}` | Yank (copy) |
| `~` | Toggle case |
| `gu{motion}` | Lowercase |
| `gU{motion}` | Uppercase |
| `> {motion}` | Indent right |
| `< {motion}` | Indent left |
| `=` | Format (lsp-format or ={motion}) |
| `gq{motion}` | Format / hard-wrap |

### Text objects

| Key | Selects |
|---|---|
| `iw` `aw` | Inner / a word |
| `iW` `aW` | Inner / a WORD (non-whitespace) |
| `is` `as` | Inner / a sentence |
| `ip` `ap` | Inner / a paragraph |
| `it` `at` | Inner / a tag block (HTML/XML) |
| `i(` `a(` | Inner / a paren block |
| `i{` `a{` | Inner / a brace block |
| `i[` `a[` | Inner / a bracket block |
| `i<` `a<` | Inner / a angle bracket block |
| `i'` `a'` | Inner / a single-quoted string |
| `i"` `a"` | Inner / a double-quoted string |
| `i\`` `a\`` | Inner / a backtick-quoted string |

### Counts

Prefix motions and operators with a number: `3j` (down 3), `d5w` (delete 5 words).

## Visual mode

| Key | Action |
|---|---|
| `v` | Character-wise visual |
| `V` | Line-wise visual |
| `<C-v>` | Block-wise visual |
| `o` | Move cursor to other end of selection |
| `d` `x` | Delete selection |
| `c` | Change selection |
| `y` | Yank selection |
| `~` | Toggle case |
| `>` `<` | Indent / outdent |

## Insert mode

| Key | Action |
|---|---|
| `<Esc>` | Return to normal |
| `<C-h>` | Delete character before cursor |
| `<C-w>` | Delete word before cursor |
| `<C-u>` | Delete to start of line |
| `<C-r>{reg}` | Insert from register |
| `<C-n>` `<C-p>` | Completion next / previous |
| `<C-e>` | Cancel completion |

## Command-line mode (`:`)

| Command | Action |
|---|---|
| `:w` | Write (save) |
| `:q` | Quit current buffer |
| `:wq` | Write and quit |
| `:q!` | Force quit (discard changes) |
| `:e {file}` | Open file |
| `:bn` `:bp` | Next / previous buffer |
| `:ls` | List buffers |
| `:bd` | Delete buffer |
| `:split` `:vsplit` | Split horizontal / vertical |
| `:tabnew` | New tab |
| `:set {option}` | Set option |
| `:colorscheme {name}` | Change theme |
| `:{range}s/old/new/g` | Substitute |
| `:g/pattern/command` | Global command |
| `:help {topic}` | Open help |

## Picker

| Command | Opens |
|---|---|
| `:picker {source}` | A named source (`files`, `recent`, `buffers`, `lines`, `outline`, `grep`, `jumps`, `marks`, `registers`, `commands`, `colorscheme`, ...); `<Tab>` after `:picker ` lists them |
| `:files [root]` | File picker |
| `:recent` | Recently-edited files |
| `:buffers` `:b` | Buffer switcher |
| `:colorscheme` | Theme picker, with live preview |

## Picker (within picker)

| Key | Action |
|---|---|
| `<C-n>` `<Down>` `<Tab>` | Next result |
| `<C-p>` `<Up>` `<S-Tab>` | Previous result |
| `<CR>` | Accept selected candidate |
| `<Esc>` `<C-c>` | Dismiss picker |
| `<C-s>` | Accept, open in horizontal split |
| `<C-v>` | Accept, open in vertical split |
| `<C-t>` | Accept, open in new tab |

## Multibuffer / search results

| Key | Action |
|---|---|
| `:search {pattern}` | Search project |
| `<CR>` | Jump to source location |
| `]e` `[e` | Next / previous match |
| `gr` | Refresh (re-run the same search) |

## Diff mode

| Key | Action |
|---|---|
| `:diffthis` | Start diff |
| `]c` `[c` | Next / previous hunk |
| `do` | Diff obtain (get from other buffer) |
| `dp` | Diff put (put to other buffer) |

## Buffers and navigation

| Key | Action |
|---|---|
| `<C-w> h/j/k/l` | Navigate panes |
| `<C-w> w` | Cycle panes |
| `<C-w> s/v` | Split horizontal / vertical |
| `<C-w> c` | Close pane |
| `:only` | Close every pane except the active one |

## LSP

| Key | Command | Action |
|---|---|---|
| `K` | — | Show hover documentation |
| `gd` | — | Go to definition |
| `gD` | — | Go to declaration |
| `gI` | — | Go to implementation (lowercase `gi` is vim's "go to last insert") |
| `gr` | — | Find references (picker) |
| `[d` `]d` | `:diag-prev` / `:diag-next` | Previous / next diagnostic |
| — | `:lsp-references` | Open the editable references view |
| — | `:lsp-rename` | Rename symbol |
| — | `:lsp-format` | Format buffer |

## Macros

| Key | Action |
|---|---|
| `q{reg}` | Start recording into register |
| `q` (in recording) | Stop recording |
| `@{reg}` | Execute macro |
| `{N}@{reg}` | Execute macro N times |
| `@@` | Repeat last macro |

## Registers

| Register | Content |
|---|---|
| `"` | Unnamed (default yank/delete) |
| `0` | Last yank |
| `1`-`9` | Last 9 deletes |
| `a`-`z` | Named registers |
| `+` `*` | System clipboard |
| `_` | Black hole (discards) |

## Marks

| Key | Action |
|---|---|
| `m{a-z}` | Set mark |
| `'{a-z}` | Jump to mark line |
| `` `{a-z} `` | Jump to mark line + column |
| `<C-o>` `<C-i>` | Walk the jump list back / forward |

## Options

| Option | Default | Description |
|---|---|---|
| `tabstop` | 4 | Spaces per tab |
| `shiftwidth` | 4 | Spaces per indent |
| `expandtab` | true | Use spaces for tabs |
| `number` | true | Show line numbers |
| `relativenumber` | false | Show relative line numbers |
| `wrap` | false | Soft-wrap long lines |
| `scrolloff` | 0 | Lines visible above/below cursor |
| `sidescrolloff` | 0 | Columns visible left/right of cursor |
| `ui.mouse` | true | Mouse support |
| `clipboard` | true | Yank/paste through the system clipboard (bool, not vim's `unnamed` string) |

## See also

- [Full modal editing reference](help:modal-editing)
- [Ex-commands reference](help:ex-commands)
- [All options](help:options)
