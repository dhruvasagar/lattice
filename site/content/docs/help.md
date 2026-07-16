+++
title = "Help system"
+++


# Help system

Lattice is self-documenting. Every command, option, mode, event, and
key binding carries metadata that the help system surfaces on demand.
No internet connection; no man pages; no searching the source. The
answer is in the editor.

---

## Quick reference

| Keystroke / command            | What it does                                          |
|-------------------------------|-------------------------------------------------------|
| `<C-h> k`                     | `:describe-key` — what does this chord do?            |
| `<C-h> c`                     | `:describe-command` — metadata for a named command    |
| `<C-h> o`                     | `:describe-option` — description + current value      |
| `<C-h> e`                     | `:describe-event` — event payload + who subscribes    |
| `<C-h> m`                     | `:describe-mode` — active modes on the current buffer |
| `<C-h> b`                     | `:describe-buffer` — buffer kind, flags, mode stack   |
| `<C-h> a`                     | `:apropos` — cross-cutting search                     |
| `<C-h> K`                     | `:keymap` — full chord table for the current state    |
| `<C-h> <C-h>` or `<C-h> ?`   | `:help-for-help` — this index                         |
| `:help [topic]`               | Open a topic by name (`:help modal-editing`, etc.)    |
| `:help`                       | Open the topic index                                  |

The `<C-h>` prefix is available in **Normal mode only** — the same
scope vim uses for the help key. In Insert mode `<C-h>` is backspace
(vim convention); in Command and Search it clears the preceding
character.

---

## `:describe-key` — what does a chord do?

```
:describe-key CHORD
:describe-key MODEPREFIX_CHORD
```

Shows every layer that has a binding for CHORD, marks which layer
fires under the current buffer's active modes, and links to the
source location where the binding was declared.

### Mode-prefix syntax

By default `:describe-key` queries **all binding modes** at once. To
narrow to one mode, prefix the chord with a two-character mode tag
followed by `_`:

| Prefix | Mode    | Example                      |
|--------|---------|------------------------------|
| `n_`   | Normal  | `:describe-key n_gd`         |
| `i_`   | Insert  | `:describe-key i_<C-n>`      |
| `v_`   | Visual  | `:describe-key v_>`          |
| `r_`   | Replace | `:describe-key r_x`          |
| `c_`   | Command | `:describe-key c_<Tab>`      |
| `s_`   | Search  | `:describe-key s_<C-r>`      |

Without a prefix, all modes that have a binding for the chord are
shown; modes with no binding are omitted to keep the output compact.

```
:describe-key j          -- shows Normal, Visual, and Replace hits
:describe-key n_j        -- shows Normal hits only
:describe-key i_<C-n>    -- shows the Insert completion-trigger
:describe-key v_>        -- shows what > does in Visual mode
```

The mode prefix follows the same convention as Neovim's `:map`
commands (`nnoremap`, `inoremap`, etc.) — `n_` for normal, `i_` for
insert — so muscle memory transfers.

### Output format

```
j — 3 registration(s) across 3 mode(s):

[Normal mode]
  -> motion:line-down  (fires now)
     layer: Built-in
     source: crates/lattice-host/src/keymap_normal.rs:1620

[Visual mode]
  -> motion:line-down  (fires now)
     layer: Built-in

[Replace mode]
  -> action:overwrite-char  (fires now)
     layer: Built-in
```

`(fires now)` marks the binding that would actually execute given the
current buffer's active modes. A hit marked `(registered but not
active)` exists in the registry but is shadowed or gated by an
inactive minor mode.

The **layer** field shows which layer the binding lives in:
`Built-in` (the default vim keymap), `User config` (your `init.rs`),
`Major mode: rust` (a language-mode contribution), or
`Minor mode: lsp-mode` (a minor-mode contribution). Higher-priority
layers shadow lower ones; `:describe-key` shows the full stack so
you can see both the winner and what it shadows.

Source links are live: pressing `<CR>` on a source line opens the
file at that line in a new split.

### Via the `<C-h>` map

In Normal mode, press `<C-h> k` — a prompt appears. Type the chord
you want to describe (with optional mode prefix) and press `<Enter>`.

---

## `:describe-command` — metadata for a command

```
:describe-command NAME
```

Shows the command's documentation string, argument spec, aliases,
current keybindings (every chord bound to it, across all modes),
source location, and links to related help topics.

Aliases resolve correctly: `:describe-command write` and
`:describe-command w` both resolve to `ex:write`.

Via `<C-h>`: `<C-h> c` prompts for a command name with completion.

---

## `:describe-option` — option documentation

```
:describe-option NAME
```

Shows the option's type, current value, default, resolver chain
(which layer set it: TOML / `:set` / mode override / built-in
default), and a human-readable description.

Via `<C-h>`: `<C-h> o`.

See also `:options` for an interactive listing of all options with
in-place editing (the emacs `M-x customize` equivalent).

---

## `:describe-event` — event payload and subscribers

```
:describe-event NAME
```

Shows the event's payload type, when it fires, and the list of
current subscribers: autocmds, mode lifecycle hooks, plugin hooks.
Useful when writing plugins or autocmds and you want to know what
fires on a given trigger.

Via `<C-h>`: `<C-h> e`.

---

## `:describe-mode` — the active mode stack

```
:describe-mode [NAME]
```

Without an argument: shows the major mode and all active minor modes
on the current buffer — name, version, brief description, contributed
keybindings, and whether each minor mode is currently active.

With a name: describes that mode whether or not it is active on the
current buffer.

Via `<C-h>`: `<C-h> m`.

---

## `:describe-buffer` — buffer metadata

```
:describe-buffer
```

Shows the current buffer's ID, kind (Document / Multibuffer /
Terminal / Help / …), flags (`listed`, `hidden`, `modified`,
`read-only`), the file path if any, and the full mode stack.

Via `<C-h>`: `<C-h> b`.

---

## `:apropos` — search across everything

```
:apropos PATTERN
```

Pattern-matches against command names, option names, event names,
mode names, and help topic summaries in a single sweep. Results open
in a help buffer with cross-links. Use this when you know what you
want to do but not what it is called.

Via `<C-h>`: `<C-h> a`.

Examples:
```
:apropos lsp          -- everything LSP-related
:apropos search       -- search commands, options, and events
:apropos fold         -- fold-related surface
:apropos completion   -- completion triggers and options
```

---

## `:keymap` — the full chord table

```
:keymap [MODE]
```

Opens a buffer showing the complete chord table for the current
state, or for the specified mode. Grouped by layer: Built-in, User
config, each active major/minor mode contribution. Each row links to
its source via `<CR>`.

Via `<C-h>`: `<C-h> K` (capital K — lowercase `k` is `:describe-key`).

---

## `:help` — topic documentation

```
:help [TOPIC]
:help-for-help
```

Opens the named topic as a help buffer. `<Tab>` after `:help `
lists all registered topics with their summaries. Without an
argument, opens the topic index.

`:help-for-help` (also `<C-h> <C-h>` or `<C-h> ?`) opens this page.

### Help buffer navigation

Inside any help buffer:

| Key     | Action                                              |
|---------|-----------------------------------------------------|
| `j`/`k` | Move cursor (Normal mode motion)                    |
| `<CR>`  | Follow a link (topic, source file, or chord link)   |
| `<Esc>` | Dismiss the help overlay                            |
| `gg`/`G`| Jump to top / bottom                                |
| `/`     | Search within the topic                             |
| `<C-o>` | Go back (position history)                          |

Three link kinds exist in help buffers:
- **Topic links** (`help:modal-editing`) — jump to another topic
- **Source links** (file:line pairs from `:describe-*`) — open the
  source in a split
- **Chord links** (chord notation) — run `:describe-key` for that chord

---

## Inline help at the command line

At the `:` command line, press `<C-h>` while the cursor is on a
partially-typed command name to open `:describe-command` for it
inline. This gives you documentation without losing your place in
the command you are building.

---

## See also

- [`modal-editing`](../modal-editing/) — the vim grammar; mode
  entry/exit; registers, marks, macros.
- [`modes`](../modes/) — major + minor modes; how modes contribute
  keymaps and what each mode offers.
- [`options`](../options/) — `:set`, `:customize`, TOML config.
- [`ex-commands`](../ex-commands/) — the full `:` command reference.
