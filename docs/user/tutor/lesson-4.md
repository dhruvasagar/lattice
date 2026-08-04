# Lesson 4: The Mode System and Emacs-Style Help

Lessons 1–3 covered vim's editing model.

Lesson 4 is about what makes Lattice different from a plain vim clone.
Lattice borrows Emacs's self-documentation and extensibility model on top
of vim's modal editing. This lesson covers the mode system, the emacs-style
help map, and the option customization buffer.

---

## 4.1 The Mode System — two orthogonal axes

Lattice has two orthogonal mode axes:

**Vim modal state** — how you interact with the buffer:

```
Normal    navigate, run commands
Insert    type text
Visual    select text
Replace   overtype
Command   the : command line
Search    the / and ? prompt
```

This is what Lessons 1–3 covered. The vim modal state is a per-buffer
state machine. Every buffer has one.

**Content mode** — what the buffer contains:

- **Major mode** — the "identity" of the buffer: `rust-mode`, `markdown-mode`,
  `help-mode`, `terminal-mode`, etc. One per buffer; derived from the file type.
- **Minor mode** — optional feature layers that stack on top of the major
  mode: `lsp-mode`, `spell-mode`, `diff-mode`, etc.
  A buffer can have many active minor modes at once.

Major and minor modes contribute:

- Keybindings (additions to Normal/Insert/Visual — not replacements)
- Completion sources
- Decorations (gutter signs, inlay hints, syntax highlighting)
- Status line items
- Autocmd subscriptions

To see the active mode stack for the current buffer:

```
:describe-active-modes    (or <C-h> m)
```

That lists the major mode and every active minor mode, each with the
chords it contributes. It shows minors as well as the major on
purpose: behaviour shared by several major modes lives in a minor
mode rather than being copied into each one, so a major-only view
would hide those shared chords.

To read about one *named* mode instead — active or not:

```
:describe-mode rust-mode  (or <C-h> M, which prompts for the name)
```

**Exercise:** Open a Rust file with `:e src/main.rs` (or any file) and
watch the major mode indicator update in the status line.

---> Open any Rust file to see the rust major mode activate.

---

## 4.2 The `<C-h>` Help Map — emacs-style self-documentation

Emacs has been self-documenting since 1985. `C-h` (control-h) opens a
"help for help" dispatcher where you press one more key to get the
documentation you need. Lattice adopts the same convention.

The `<C-h>` prefix is available in Normal mode:

```
<C-h> k   :describe-key          — what does this chord do?
<C-h> c   :describe-command      — documentation for a command
<C-h> o   :describe-option       — option description + current value
<C-h> e   :describe-event        — event payload + who subscribes
<C-h> m   :describe-active-modes — the mode stack live here, with chords
<C-h> M   :describe-mode         — prompt for one mode by name
<C-h> b   :describe-buffer       — buffer metadata and flags
<C-h> a   :apropos               — search across all of the above
<C-h> K   :describe-bindings     — chords that fire on this buffer
<C-h> ?   :help-for-help         — this reference
```

Note the lowercase/uppercase pairs. Lowercase is the common case:
`<C-h> m` answers "what is this buffer" without asking you anything,
and `<C-h> K` shows only the chords that actually apply here. The
exhaustive chord table for every mode is still `:keymap`.

**Exercise 1:** Press `<C-h> k J` to see what `J` does.

---> Press <C-h> k J  to see what J does.

**Exercise 1b:** Press `<C-h> m` to see what modes are live in this
buffer, then `<C-h> K` to see what you can press in it.

---> Press <C-h> m  to see this buffer's active modes.

**Exercise 2:** Run `:describe-key n_J` to confirm the Normal-mode binding
(the `n_` prefix scopes the lookup to Normal mode specifically).

---> Run :describe-key n_J  to confirm the Normal-mode binding.

**Exercise 3:** Run `:apropos motion` to find all motion-related commands.

---> Run :apropos motion  and browse the results.

---

## 4.3 `:describe-key` — the mode-prefix syntax

`:describe-key CHORD` shows what a key does in all modes at once.
To narrow to one mode, prefix the chord with the mode tag and `_`:

```
n_CHORD    Normal mode only
i_CHORD    Insert mode only
v_CHORD    Visual mode only
r_CHORD    Replace mode only
c_CHORD    Command mode only
s_CHORD    Search mode only
```

Examples:

```
:describe-key j         — j in all modes
:describe-key n_j       — j in Normal only
:describe-key i_<C-n>   — <C-n> in Insert (completion trigger)
:describe-key v_>       — > in Visual (indent selection)
:describe-key c_<Tab>   — <Tab> in Command (completion)
```

This prefix convention mirrors Neovim's `nnoremap`/`inoremap` naming,
so the mental model transfers.

---

## 4.4 `:apropos` — find what you don't know the name of

`:apropos PATTERN` searches command names, option names, event names,
mode names, and help topic summaries in one sweep.

```
:apropos lsp         — everything LSP-related
:apropos search      — search commands, options, and events
:apropos completion  — completion surface
:apropos fold        — fold-related commands and options
:apropos indent      — indentation options and commands
```

Results open in a help buffer with live cross-links.

---

## 4.5 `:options` — the customize buffer

Lattice has a typed option system. Every option has a name, type
(bool, integer, string, enum, colour, ...), description, default value,
and the current value with the layer that set it.

Options can be set three ways:

**1. Temporarily at runtime:**

```
:set wrap on
:set tabstop 4
:set colorscheme gruvbox
```

**2. Interactively via the customize buffer** (Emacs's `M-x customize` equivalent):

```
:options          — opens the full option list
:options search   — filters by name or description
```

Inside the `:options` buffer, navigate with `j`/`k`, press `<Enter>` on
any option to edit it inline, and `:w` to save back to your TOML.

**3. Statically in your TOML config file:**

```toml
[editor]
wrap = true
tabstop = 4
```

**Exercise:** Run `:options` to explore the full option tree.

---> Run :options  to see the typed option registry.

---

## 4.6 Autocmds — hooks as typed event subscriptions

Autocmds in Lattice are typed event subscriptions. The event bus carries
structured payloads; autocmds subscribe with an optional filter.
Vim's `:autocmd` and Emacs's `add-hook` desugar to the same mechanism.

From the command line:

```
:autocmd BufEnter *.rs  lsp-start
```

This says: when a `BufEnter` event fires for any buffer whose path
matches `*.rs`, run the `lsp-start` command.

Useful events:

```
BufEnter      buffer becomes active in a pane
BufLeave      buffer leaves the active pane
BufWritePre   before a buffer is written to disk
BufWritePost  after a buffer is written to disk
InsertEnter   entering Insert mode
InsertLeave   leaving Insert mode
ModeChanged   vim modal state changed
LspAttach     LSP server attached to a buffer
FileType      buffer's major mode was detected
```

**Exercise:** Run `:describe-event BufWrite` to see the event schema.

---> Run :describe-event BufWrite  to see the event schema.

---

## 4.7 The Init Module — programmable config in Rust/WASM

Static settings live in your TOML file. Anything programmable —
custom keymaps, autocmds, custom commands, hooks — lives in your
init module: a Rust file compiled to WebAssembly and loaded at startup.

Your init module has access to the full plugin API:

```rust
fn init(ctx: &mut InitContext) {
    // bind a key in Normal mode
    ctx.keymap.bind_normal("<leader>f", "file-picker");

    // register an autocmd
    ctx.autocmd.on(Event::BufWritePre, |e| {
        if e.path.ends_with(".rs") {
            ctx.cmd.exec("lsp-format");
        }
    });

    // define a custom command
    ctx.cmd.register("my-command", |args| {
        // ...
    });
}
```

Because the init module is WASM, it is sandboxed (capability-gated and
fuel-limited) like any other plugin. You get first-class extensibility
with the same safety guarantees the plugin host enforces for third-party
plugins. TOML covers static option overrides; everything programmable is
code. One toolchain, one substrate.

**Exercise:** Read about the init module in the help system.

---> Run :help init  to open the init-module documentation.

---

## Summary

**Two mode axes:**

```
Vim modal state:  Normal / Insert / Visual / Replace / Command / Search
Content mode:     Major mode (file-type identity) + Minor modes (features)
```

**`<C-h>` help map (Normal mode):**

```
<C-h> k   :describe-key     (n_/i_/v_/r_/c_/s_ prefix to narrow mode)
<C-h> c   :describe-command
<C-h> o   :describe-option
<C-h> e   :describe-event
<C-h> m   :describe-active-modes   (this buffer's mode stack)
<C-h> M   :describe-mode           (one mode by name)
<C-h> b   :describe-buffer
<C-h> a   :apropos
<C-h> K   :describe-bindings       (chords that fire here)
<C-h> ?   :help-for-help
```

`:keymap` remains the exhaustive chord table for every mode.

```
:options        interactive option customize buffer  (like emacs M-x customize)
:set NAME VAL   runtime option change
:autocmd        typed event subscription (hook) — same as add-hook
```

**Init module:** Rust compiled to WASM; keymaps, autocmds, custom commands.

Continue with Lesson 5: Splits, buffers, multibuffer search, diff, and LSP.
