+++
title = "Quick Reference"
description = "One-page cheatsheet for Lattice keybindings and commands"
+++

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
| `J` | Join lines |

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
| `:!{shell cmd}` | Run shell command |
| `:help {topic}` | Open help |

## Picker (`<space>` or `:pick`)

| Key | Action |
|---|---|
| `<C-p>` | File picker |
| `<C-g>` | Grep / text search |
| `<C-b>` | Buffer picker |
| `<C-o>` | Outline / symbol picker |
| `<C-r>` | Recently opened |
| `<C-k>` | Pick a picker |
| `<Esc>` | Close picker |

## Picker (within picker)

| Key | Action |
|---|---|
| `<C-n>` `<Down>` | Next result |
| `<C-p>` `<Up>` | Previous result |
| `<C-c>` `<Esc>` | Dismiss picker |
| `<Tab>` | Preview selection |

## Multibuffer / search results

| Key | Action |
|---|---|
| `:search {pattern}` | Search project |
| Enter on a result | Jump to source location |
| `:cnext` `:cprev` | Next / previous result |
| `:copen` `:cclose` | Open / close results |

## Diff mode

| Key | Action |
|---|---|
| `:diffthis` | Start diff |
| `]c` `[c` | Next / previous hunk |
| `do` | Diff obtain (get from other buffer) |
| `dp` | Diff put (put to other buffer) |
| `:diffupdate` | Refresh diff |

## Buffers and navigation

| Key | Action |
|---|---|
| `<C-w> h/j/k/l` | Navigate panes |
| `<C-w> w` | Cycle panes |
| `<C-w> s/v` | Split horizontal / vertical |
| `<C-w> c` | Close pane |
| `<C-w> o` | Close others |

## LSP

| Key | Command | Action |
|---|---|---|
| `K` | `:lsp-hover` | Show documentation |
| `gd` | `:lsp-definition` | Go to definition |
| `gD` | `:lsp-declaration` | Go to declaration |
| `gi` | `:lsp-implementation` | Go to implementation |
| `gr` | `:lsp-references` | Find references |
| `[d` `]d` | `:lsp-diagnostic-prev/next` | Previous / next diagnostic |
| `<C-w> d` | `:lsp-peek-definition` | Peek definition |
| `:lsp-rename` | Rename symbol |
| `:lsp-format` | Format buffer |

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
| `%` | Current file name |
| `#` | Alternate file name |
| `/` | Last search pattern |
| `:` | Last command |
| `.` | Last inserted text |
| `=` | Expression register |

## Marks

| Key | Action |
|---|---|
| `m{a-z}` | Set mark |
| `'{a-z}` | Jump to mark line |
| `` `{a-z} `` | Jump to mark line + column |
| `'.` `` `. `` | Jump to last change position |
| `''` `` `` `` | Jump to previous position |

## Options

| Option | Default | Description |
|---|---|---|
| `tabstop` | 4 | Spaces per tab |
| `shiftwidth` | 4 | Spaces per indent |
| `expandtab` | true | Use spaces for tabs |
| `number` | true | Show line numbers |
| `relativenumber` | false | Show relative line numbers |
| `wrap` | true | Soft-wrap long lines |
| `scrolloff` | 3 | Lines visible above/below cursor |
| `sidescrolloff` | 3 | Columns visible left/right of cursor |
| `mouse` | true | Mouse support |
| `clipboard` | unnamed | Clipboard integration |

## See also

- [Full modal editing reference](./docs/modal-editing/)
- [Ex-commands reference](./docs/ex-commands/)
- [All options](./docs/options/)
