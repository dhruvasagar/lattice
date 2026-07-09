---
summary: "The : line: built-in commands, arg schemas, tab completion, ranges, aliases, :g / :v / :s with live preview."
related: [ex:, substitute, global]
---

# Ex-commands

The `:` line. Type `:` from Normal mode to enter the cmdline,
type a command, hit `<CR>` to run.

```
:w<CR>                  " write
:e foo.rs<CR>           " open foo.rs
:set tabstop=4<CR>      " set option
:%s/foo/bar/g<CR>       " substitute throughout buffer
```

Lattice's ex-command surface is a *typed* command registry. Every
command has an arg schema (with prompt strings + optional default
values + completion); the cmdline parser validates against it.
This means tab-completion, arg-prompts on missing required args,
and `:describe-command` work for every command, including
plugin-supplied ones.

Built-in command count: roughly **130** at the time of writing.
`:apropos<Tab>` enumerates every name; `:describe-command NAME`
shows full metadata.

---

## Quick reference

### Buffer / file

| Command                | Action                                                       |
|------------------------|--------------------------------------------------------------|
| `:w` / `:write`        | Write buffer to its current path                             |
| `:w PATH`              | Write to `PATH`                                              |
| `:wq` / `:x`           | Write + quit                                                 |
| `:q` / `:quit`         | Quit if buffer is clean                                      |
| `:q!`                  | Quit unconditionally                                         |
| `:e PATH` / `:edit`    | Open `PATH` in active pane                                   |
| `:e!`                  | Reload current file from disk                                |
| `:bn` / `:bnext`       | Cycle to next buffer                                         |
| `:bp` / `:bprev`       | Cycle to previous buffer                                     |
| `:b N`                 | Switch to buffer #N                                          |
| `:b PATTERN`           | Switch to buffer matching PATTERN                            |
| `:buffers` / `:b`      | Open the fuzzy buffer switcher (picker)                     |
| `:bd` / `:bdelete`     | Close active buffer                                          |
| `:bd!`                 | Close even if dirty                                          |
| `:ls`                  | List every open buffer as static text                       |

### Splits / panes

| Command          | Action                                                |
|------------------|-------------------------------------------------------|
| `:split` / `:sp` | Horizontal split of active pane                       |
| `:vsplit`/`:vsp` | Vertical split                                        |
| `:close` / `:cl` | Close active pane                                     |
| `:only` / `:on`  | Close every pane except the active one                |

`<C-w>`-prefixed chords (`<C-w>s`, `<C-w>v`, `<C-w>q`, ...) are
the keymap equivalents.

### File tree / oil

| Command           | Action                                                   |
|-------------------|----------------------------------------------------------|
| `:Tree [path]`    | Open file tree rooted at `path` (default: current dir)   |
| `:TreeClose`      | Dismiss file-tree pane                                   |
| `:Oil [path]`     | Open oil-style editable directory listing                |

### Search / substitute

| Command                              | Action                                                          |
|--------------------------------------|-----------------------------------------------------------------|
| `:s/pat/repl/`                       | Substitute first match on current line                          |
| `:s/pat/repl/g`                      | All matches on current line                                     |
| `:%s/pat/repl/g`                     | All matches in buffer                                           |
| `:'<,'>s/pat/repl/g` (in Visual)     | All matches in selection                                        |
| `:noh` / `:nohlsearch`               | Clear hlsearch overlay                                          |
| `:g/pat/CMD`                         | Run `:CMD` on every line matching `pat`                         |
| `:v/pat/CMD`                         | Run `:CMD` on every line *not* matching `pat`                   |

### Options / customize

| Command                                | Action                                                                |
|----------------------------------------|-----------------------------------------------------------------------|
| `:set NAME=VALUE`                      | Set typed option                                                      |
| `:set NAME` (boolean)                  | Set to true                                                           |
| `:set noNAME` (boolean)                | Set to false                                                          |
| `:set NAME?`                           | Echo current value                                                    |
| `:options`                             | List every customizable option                                        |
| `:describe-option NAME`                | Show one option's full metadata                                       |
| `:describe-option-resolution NAME`     | Show which resolver layer provides `NAME`'s value                     |
| `:customize`                           | Open the customize picker                                             |
| `:customize <group>`                   | Open the focused group view                                           |
| `:customize <mode-name>`               | Open the mode's contributed-options view                              |

### Modes

| Command              | Action                                              |
|----------------------|-----------------------------------------------------|
| `:<mode-name>`       | Toggle a registered mode (e.g. `:lsp-mode`)         |
| `:list-modes`        | List every registered mode                          |
| `:describe-mode N`   | Show one mode's metadata                            |

### LSP

See [`lsp`](help:lsp) for the full inventory. Most-used:

| Command                        | Action                                                |
|--------------------------------|-------------------------------------------------------|
| `:lsp-format`                  | Format the buffer                                     |
| `:lsp-format-range`            | Format the Visual selection                          |
| `:lsp-rename NEWNAME`          | Rename symbol under cursor                            |
| `:lsp-symbols`                 | Document symbol picker                                |
| `:lsp-workspace-symbol QUERY`  | Workspace symbol search                               |
| `:lsp-code-action`             | Code-action picker                                    |
| `:lsp-status`                  | Per-server attach + capability status                 |
| `:lsp-log [server]`            | Open per-server log buffer                            |
| `:lsp-trace SERVER`            | Toggle JSON-RPC tracing for SERVER                    |
| `:lsp-trace-log [server]`      | Open the trace log                                    |
| `:lsp-restart SERVER`          | Restart SERVER                                        |
| `:lsp-server-log`              | Open the server's stderr feed                         |

### Diagnostics

| Command                | Action                                                         |
|------------------------|----------------------------------------------------------------|
| `:diagnostics`         | Open the diagnostics picker                                    |
| `:diag-next` / `:cnext`| Jump to the next diagnostic                                    |
| `:diag-prev` / `:cprev`| Jump to the previous                                           |

### Help / introspection

| Command                       | Action                                                     |
|-------------------------------|------------------------------------------------------------|
| `:help [TOPIC]`               | Open help for TOPIC; no arg → topic index                  |
| `:describe-command NAME`      | Show one command's metadata                                |
| `:describe-buffer`            | Show active buffer's state summary                         |
| `:describe-key CHORD`         | Show what CHORD does (in every mode it's bound)            |
| `:describe-events`            | List every typed event                                     |
| `:describe-event NAME`        | Show one event's descriptor                                |
| `:apropos PATTERN`            | Search commands + options + events for PATTERN             |
| `:keymap`                     | List every default chord binding                           |

### State / scratch

| Command           | Action                                          |
|-------------------|-------------------------------------------------|
| `:reg`/`:registers` | List populated registers                      |
| `:marks`          | List set marks                                  |
| `:jumps`          | (When implemented) show position-history ring   |
| `:reload-snippets`| Re-read every snippet file                      |

### Completion

| Command            | Action                                          |
|--------------------|-------------------------------------------------|
| `:complete`        | Trigger LSP completion at cursor                |

### Misc

| Command                | Action                                         |
|------------------------|------------------------------------------------|
| `:hover [TEXT]`        | Open a hover popup with `TEXT` (testing path)  |
| `:HoverClose`          | Dismiss active hover popup                     |
| `:42`                  | Jump to line 42                                |
| `:'a,'b!CMD`           | (Future) Filter range through external `CMD`   |
| `:!CMD`                | (Future) Run external `CMD`                    |

---

## Cmdline UX

The `:` line is itself a **buffer** with a major mode
(`command-line`). Every Normal-mode editing primitive that
makes sense in a single-line context applies — motions,
deletion, undo (within the line), etc.

Specific behaviours:

- `<Tab>` after the command name expands arg completion (paths
  for path-typed args, names for name-typed args).
- `<Tab>` on a partial command name expands the command.
- `<Esc>` cancels and returns to Normal.
- `<C-c>` same as `<Esc>` (cancel).
- `<C-h>` opens `:describe-command` for the partially-typed
  command (when implemented).
- `<Up>` / `<Down>` walk command history.
- `<C-n>` / `<C-p>` same as `<Down>` / `<Up>`.

---

## Arg schemas

Every command's args are typed:

- `string` — bare string
- `path` — file/dir path; `<Tab>` does path completion
- `option-name` — registered typed-option name; `<Tab>` enumerates
- `mode-name` — registered mode; `<Tab>` enumerates
- `command-name` — registered command; `<Tab>` enumerates
- `chord` — keystroke notation; the next chord is captured

Args can be `Required`, `Optional` (with default), or `None`.

### Arg-prompts

Submitting a command without a required arg doesn't error —
the cmdline **prefills** with the command keyword + space and
the echo line shows the schema's prompt:

```
:describe-command<CR>
                  → cmdline shows: describe-command
                  → echo shows:    command:
```

Type the arg, submit, the command runs.

For `chord`-typed args, the prompt arms a chord capture: the
*next* chord you press auto-submits as the argument.

`<Esc>` from a primed arg-prompt cancels back to Normal.

---

## `:g` / `:v` (global / inverse-global)

Run a body command on every line matching (or not matching) a
pattern.

```
:g/^$/d                 " delete every empty line
:g/TODO/p               " print every line containing TODO
:v/foo/d                " delete every line that doesn't contain `foo`
:%g/^/m0                " (future) move-line: reverse every line
```

The body is parsed **at submit time**, before any line is
processed. So a typo'd body errors immediately, not partway
through:

```
:g/foo/notacommand<CR>
                  → "unknown command: notacommand" (immediate)
```

---

## Substitute (`:s`)

Vim's substitute, with live-preview while typing.

```
:s/pat/repl/             " first match on current line
:s/pat/repl/g            " all matches on current line
:%s/pat/repl/g           " all matches in buffer
:'<,'>s/pat/repl/g       " all matches in Visual selection
```

### Live preview

While typing the cmdline, every match the substitute *would*
hit lights up in magenta with a strike-through overlay. Cancel
(`<Esc>`) clears the preview without modifying anything;
submit (`<CR>`) commits.

For `:%s/pat/repl/g`, the preview spans the whole buffer; for
`:s/pat/repl/g`, just the current line.

### Pattern syntax

Lattice uses `regex` crate syntax (`fancy-regex` for backref-
heavy patterns). `\d+` works; `(?P<name>...)` works;
`(\w+) \w+ \1` (find duplicate-word neighbours) works.

### Replacement

Plain text. `\1` ... `\9` reference capture groups. `\0`
(or `&`) references the whole match. (Vim's `~` and case-
modifier escapes — `\u`, `\l` — land in a follow-up.)

---

## Ranges

Most commands accept a range prefix.

| Range          | Meaning                                                 |
|----------------|---------------------------------------------------------|
| (omitted)      | Current line for line-oriented ops; current cursor pos  |
| `42`           | Line 42                                                 |
| `42,50`        | Lines 42 through 50 inclusive                           |
| `'a,'b`        | From mark `a` to mark `b`                               |
| `'<,'>`        | The previous Visual selection                           |
| `%`            | Whole buffer (= `1,$`)                                  |
| `.`            | Current line                                            |
| `$`            | Last line                                               |
| `+5` / `-5`    | Relative to current                                     |

Examples:

```
:5,10d                   " delete lines 5..=10
:%s/foo/bar/g            " substitute throughout buffer
:.,$d                    " delete from cursor to end-of-buffer
```

---

## Aliases

Most ex-commands have multiple surface forms — a long name + a
short alias + sometimes a vim-style abbreviation. The full
alias table is dynamic; `:describe-command NAME` lists each
command's known aliases. Examples:

| Long form            | Aliases                                |
|----------------------|----------------------------------------|
| `:write`             | `:w`                                   |
| `:edit`              | `:e`                                   |
| `:quit`              | `:q`                                   |
| `:buffer-next`       | `:bn`, `:bnext`                        |
| `:nohlsearch`        | `:noh`                                 |
| `:diagnostics-next`  | `:diag-next`, `:dnext`, `:cnext`, `:cn`|
| `:buffer-picker`     | `:buffers`, `:b`                       |

---

## Tab completion

`<Tab>` completion fires on:

1. **Command names.** `:des<Tab>` cycles through `describe-*`
   commands.
2. **Arg names** for `option-name` / `mode-name` / `command-name`
   args.
3. **Paths** for `path` args.
4. **Enum values** for typed-option enum values:
   `:set foldmethod=<Tab>` enumerates `manual` / `indent` /
   `markdown` / `syntax`.

The popup is a vertico-style picker; type to filter,
`<Tab>` / `<S-Tab>` to navigate, `<CR>` to accept, `<Esc>` to
cancel.

---

## Edge cases

### Commands with bang variants

Some commands accept `!` to override safety checks:

- `:q!` quit even if dirty
- `:e!` reload even if dirty
- `:bd!` close even if dirty
- `:w!` write even if read-only (when applicable)

The bang variant is a separate dispatch path; the schema
declares `accepts_bang: true`.

### Empty body for `:g` / `:v`

```
:g/foo/<CR>
                  → error: empty body
```

`:g` with no body has no defined semantics; vim defaults to
`:p` (print) but lattice rejects to keep the surface explicit.

### Range on a non-range command

```
:%set tabstop=4<CR>
                  → error: this command doesn't accept a range
```

Each command's schema declares `accepts_range`. Settings
commands typically don't.

### Multiple commands on one line

Lattice doesn't yet support `|` as a command separator. One
command per `:` invocation. Workaround: use `:g//` for
batched runs.

### History

The cmdline keeps a history ring (in-memory; not persisted to
disk in v1). `<Up>` / `<Down>` walk it. The history is shared
across `:` and `/` (when implemented separately, this
diverges).

---

## See also

- [`modal-editing`](help:modal-editing) — the keymap-side of
  the editor.
- [`buffers`](help:buffers) — buffer / pane commands in
  context.
- [`options`](help:options) — `:set` + `:customize` deep dive.
- [`modes`](help:modes) — `:<mode-name>` toggle + introspection.
- [`lsp`](help:lsp) — every `:lsp-*` command in context.
- [`completion`](help:completion) — cmdline completion mechanics.
- `docs/dev/architecture/design.md` §5.2.1 — unified command /
  grammar dispatcher (developer reference).
