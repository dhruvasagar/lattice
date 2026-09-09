---
summary: "A key does nothing: how to tell a missing binding from a keystroke the terminal never sent."
related: [describe-key, keymap, map]
---

# When a key does nothing

A key that appears dead has two very different causes, and they need
opposite fixes:

1. **lattice never bound it** — the chord is unbound, or bound only in a
   mode this buffer is not in.
2. **lattice never received it** — your terminal consumed the keystroke,
   translated it into something else, or sent nothing at all.

The second is invisible from inside the editor unless you go looking,
and it is the more common surprise on macOS. Diagnose which one you have
before changing any configuration.

## Step 1 — ask what the key is bound to

Type the chord as **text**:

	:describe-key <M-k>

This never touches your keyboard: it parses the chord string and asks the
keymap directly. It reports every mode with a binding, which layer each
one came from, and which fires now.

- **It names a command** → the binding exists. Your problem is step 2.
- **It names a command marked `[inactive]`** → the binding exists, but the
  mode that owns it is not active on this buffer. An `org-mode` chord
  reported from a `.txt` file looks like this; open the org file and it
  becomes `[active]`.
- **"is a PREFIX — N continuation(s)"** → the chord starts a sequence but
  is not a binding on its own. The listed continuations are the chords
  that finish it; press `<CR>` on one to describe it.
- **"is not bound in any mode"** → genuinely unregistered, everywhere.

The report covers **every key, not just the keys active here** — a
binding on an inactive mode is listed and flagged, never hidden. That is
deliberate: "does this exist" and "does this fire here" are different
questions, and a chord that answers `[inactive]` tells you to switch
buffers, while one that answers "not bound" tells you to bind it.

`:describe-key` accepts a mode prefix (`n_` Normal, `i_` Insert, `v_`
Visual, `r_` Replace, `c_` Command, `s_` Search) to narrow the report:
`:describe-key i_<C-n>`. `<leader>` is expanded for you, so
`:describe-key <leader>ff` works as written.

## Step 2 — ask what the editor actually received

Press `<C-h> k` and then press the key itself.

Capture reserves nothing, so every key describes itself — including
`<CR>`, `<Esc>` and `<BS>`. Multi-key chords work by pressing each key in
turn; the keymap ends the sequence on its own, so there is no terminator
to press. This works for chords of any length and from any buffer:
`<C-c> <C-x> <C-b>` reads all three keys whether or not you are in an org
buffer, because where a sequence ENDS is a fact about the keymap, not
about where you are standing.

One consequence: a chord that merely *starts* a sequence — `<Space>`,
`g`, `<C-w>` — cannot be captured on its own, because capture is still
waiting for the rest of it. Use the string form from step 1 for those;
it answers with the list of chords that continue the prefix.

Read the result against what you pressed:

| What you see | What it means |
|---|---|
| The chord you pressed, "not bound" | The keystroke arrived. Bind it. |
| A **different** chord or a stray character | Your terminal rewrote the keystroke. |
| **Nothing at all** — no help buffer opens | Your terminal swallowed it. Nothing reached lattice. |

The last two rows are terminal problems. No lattice setting can fix
them, because by the time the editor is reading input the original
keystroke is already gone.

## Alt / Option keys on macOS

This is the usual culprit, and it produces a confusing asymmetry:
`<M-Up>` works while `<M-k>` does nothing.

Arrow keys are sent as escape sequences that carry a separate modifier
field, so Alt survives them. **Letters are not.** On macOS, Option+letter
is by default a *character composition* key — Option+k is the dead key
`˚` (ring above), which emits nothing at all until you type the next
character. So `<M-k>` is not "bound and ignored"; it is never sent.

In **kitty**, `~/.config/kitty/kitty.conf`:

	macos_option_as_alt yes

Use `left` or `right` instead of `yes` to keep one Option key for typing
accented characters — that is what you trade away.

Other terminals spell it differently: iTerm2 has a per-profile
*Left/Right Option key → Esc+* setting; Terminal.app has *Use Option as
Meta key*; Alacritty has `option_as_alt`.

To see exactly what your terminal transmits, independent of lattice:

	kitten show_key -m kitty

## Other keys terminals commonly intercept

- **The terminal's own shortcuts** win before any application sees them.
  Check your terminal's keybindings if a chord is missing entirely.
- **`<C-i>` and `<Tab>`**, and **`<C-m>` and `<CR>`**, are historically
  the same byte. A terminal that does not enable a disambiguating
  keyboard protocol cannot tell them apart, so binding one may appear to
  bind the other.
- **`<C-S-…>`** combinations are unavailable in many terminals for the
  same reason: the classic encoding has nowhere to put the Shift bit.
- **The GPUI renderer is not affected** by any of this — it reads
  keyboard events directly. A chord that works under `--gui` and not in
  the terminal is strong evidence the terminal is the variable.

## See also

- `:keymap` — the full binding catalog.
- `:describe-bindings` — what can fire in *this* buffer right now.
- `:map` — user-level bindings.
