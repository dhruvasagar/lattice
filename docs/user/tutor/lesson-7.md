# Lesson 7: Customization — Themes, Options, and the Modeline

You now know how to edit. Lesson 7 is about making Lattice *yours*:
picking a colour scheme, setting options, discovering what is
configurable, and shaping the modeline.

Lattice has a single typed option system. Every option has a name, a
type, a description, a default, and a current value. `:set` is just a
front-end to it; the `:options` and `:customize` buffers let you browse
and edit the same registry interactively.

---

## 7.1 Colour Schemes — `:colorscheme`

Switch the active theme with `:colorscheme` (short alias `:colo`):

```
:colorscheme gruvbox       switch to the gruvbox theme
:colo gruvbox              same, abbreviated
```

Run it with **no argument** to open the live-preview theme picker:

```
:colorscheme               open the picker — each candidate recolours
                            the editor live as you move through the list
```

Move through the list with `j` / `k` (or type to filter); the editor
recolours under your cursor so you can see each theme on your real
code. Press `<Enter>` to keep the highlighted theme, or `<Esc>` to
cancel and snap back to the one you started on.

**Exercise 1:** Open the live-preview theme picker.

---> Run :colorscheme  with no argument to open the live-preview picker.

**Exercise 2:** Switch directly to a named theme.

---> Run :colorscheme gruvbox  to switch themes directly.

---

## 7.2 Setting Options — `:set`

`:set NAME` changes an option for this session. The syntax depends on
the option's type:

```
:set number          turn on a boolean option (line numbers)
:set nonumber        turn a boolean OFF (the `no` prefix)
:set relativenumber  relative line numbers in the gutter
:set tabstop=4       assign a value (note the `=`, no spaces)
:set wrap            visual soft-wrap (a boolean)
:set ignorecase      case-insensitive search
:set number?         query — show the option's current value
```

Three forms, by type:

- **Boolean** — `:set NAME` turns it on, `:set noNAME` turns it off.
- **Value** — `:set NAME=VALUE` assigns (e.g. `:set tabstop=4`).
- **Query** — `:set NAME?` prints the current value without changing it.

Many options have vim-style short aliases: `nu` for `number`, `rnu`
for `relativenumber`, `ts` for `tabstop`.

**Exercise 1:** Turn on absolute line numbers.

---> Run :set number  to turn on line numbers.

**Exercise 2:** Set the tab width to 4 columns.

---> Run :set tabstop=4  to set the tab width.

---

## 7.3 Discovering Options — `:options`

You do not have to memorise option names. `:options` opens the full
typed option registry as a buffer:

```
:options             open the full option list
```

Each row shows the option's name, type, current value, and the layer
that set it. It is a normal buffer — navigate with `j` / `k`, search
with `/`, and use `:b` to jump back to it later. This is the reference
for *what exists*; you do not have to guess.

`:apropos` (from Lesson 4) also searches option names, so
`:apropos fold` finds every fold-related option and command at once.

**Exercise:** Browse the typed option registry.

---> Run :options  to browse the typed option registry.

---

## 7.4 The Customize Buffer — `:customize`

`:customize` is the type-aware editor for options — Emacs's
`M-x customize`. Run it bare to browse, or pass a group name to filter
to one area:

```
:customize             browse all customizable option groups
:customize editor      just the core editor options
:customize modeline    just the modeline-layout options
:customize lsp         LSP-related options
```

Inside the customize buffer each option is shown with its
documentation and current value. To change one, run `:set NAME=VALUE`
from the command line — the buffer shows you the exact names to use.

**Exercise 1:** Open the customize browser.

---> Run :customize  to open the customize browser.

**Exercise 2:** Filter the customize buffer to the modeline group.

---> Run :customize modeline  to see the modeline options.

---

## 7.5 The Modeline — `ui.modeline.*`

The modeline is the status bar at the bottom of each pane. Its content
is built from named *elements* placed into three zones — left, center,
and right. The built-in elements:

```
core.mode       the modal state (NORMAL / INSERT / VISUAL …)
core.path       the buffer's file path / name
core.position   the cursor line:column
core.lang       the buffer's language / major mode
```

Lay them out per zone with the `ui.modeline.*` options. On the command
line you give a comma-separated list of element ids:

```
:set ui.modeline.left=core.mode,core.path
:set ui.modeline.right=core.position,core.lang
:set ui.modeline.center=
```

Other modeline knobs:

```
:set ui.modeline.separator=|    glyph drawn between elements in a zone
:set ui.modeline.padding=1      blank columns at each end of the row
```

By default each zone is `auto` — newly registered elements (from a
mode or plugin) place themselves via their own descriptor, so the
modeline grows as you enable features. An explicit list shows exactly
the ids you name, in order.

In your TOML config the same options take an array:

```toml
[ui.modeline]
left = ["core.mode", "core.path"]
right = ["core.position", "core.lang"]
```

**Exercise 1:** Put the cursor position and language on the right.

---> Run :set ui.modeline.right=core.position,core.lang  to lay out the right zone.

**Exercise 2:** Set the element separator.

---> Run :set ui.modeline.separator=|  to separate modeline elements.

---

## Summary

| Feature | Commands |
|---------|----------|
| **Themes** | `:colorscheme NAME` · `:colo` · `:colorscheme` (live picker) |
| **Options** | `:set NAME` · `:set noNAME` · `:set NAME=VALUE` · `:set NAME?` |
| **Discover** | `:options` (full registry) · `:apropos NAME` |
| **Customize** | `:customize` · `:customize GROUP` (e.g. `:customize modeline`) |
| **Modeline** | `:set ui.modeline.{left,center,right}=…` · `.separator` · `.padding` |

One typed option system underneath: `:set` writes it, `:options`
browses it, `:customize` edits it by group, and your TOML config makes
the changes permanent.

---

This is the end of the Lattice Tutor. You now know:

- **Lesson 1:** Modes, movement, basic editing, undo, saving
- **Lesson 2:** The grammar — operators, text objects, counts, dot
- **Lesson 3:** Visual mode, registers, search, substitution, macros
- **Lesson 4:** The mode system, emacs-style help, autocmds, init module
- **Lesson 5:** Splits, buffer list, project search, diff, LSP
- **Lesson 6:** Text objects, narrowing, folding, soft-wrap
- **Lesson 7:** Themes, options, customize, the modeline

Run `:help` to explore the full documentation.
Run `:apropos` to find anything you cannot name.
Press `<C-h> ?` for the help map reference.

*Good luck.*
