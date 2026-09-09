---
summary: "which-key-mode: the *which-key* hint popup — hold a prefix and see what can follow it, read from the live keymap rather than a static list."
related: [which-key, describe-key, keymap, describe-bindings]
---

# which-key-mode

The major mode of the `*which-key*` buffer: the hint that appears when
you hold a chord prefix.

Press `g` and pause. After a short delay a panel appears along the
bottom of the pane listing every key that can follow `g` right now, what
each one does, and how many bindings hide under the ones that open a
deeper prefix.

    g

    d  go to definition       s  +4
    r  find references        {char}  go to mark

Keep typing and the panel narrows to match. Finish the chord and it
disappears. Nothing you press behaves differently while it is up.

## It reads the live keymap

The list is built from the same composite keymap the dispatcher walks
when it resolves your keystroke — your major mode's chords, every active
minor mode's, your `init.rs` bindings, and any plugin's, folded in the
same priority order.

That matters when something shadows something else. If a mode rebinds
`gd`, the panel shows the mode's binding, because that is the one that
will fire. It cannot advertise a chord that would not run.

`:describe-bindings` composes its answer differently and can drift; this
panel cannot.

## It never steals a key

The popup is passive. Your buffer keeps focus, the cursor stays where it
was, and every keystroke resolves against the keymap exactly as it would
with no popup on screen. There is nothing to dismiss and nothing to
navigate — finishing or abandoning the chord takes the panel with it.

This is deliberate: a hint with its own navigation keys would change
what a chord means, differently for each prefix. Under `<C-w>` it would
eat `n`; under `g` it would eat `j`.

## What the rows mean

| Row | Meaning |
|---|---|
| `d  go to definition` | pressing `d` runs that command |
| `s  +4` | `s` opens a deeper prefix with 4 bindings under it |
| `{char}  go to mark` | the next key is taken literally (marks, registers, `f`) |
| `g alone: …` | the prefix is *also* a binding on its own |

Labels come from the binding's own documentation, so a plugin's chords
are described as well as the built-in ones.

## Options

| Option | Default | What it does |
|---|---|---|
| `which-key.enabled` | `true` | Turn the popup off entirely. |
| `which-key.delay` | `300` | Milliseconds to wait before showing. `0` shows it immediately. |
| `which-key.max-height` | `12` | Most content rows to show. Capped at half the pane regardless. |
| `which-key.max-columns` | `6` | Most grid columns. |
| `which-key.sort` | `key` | `key` or `label`. |

The delay is what separates a hint from a stutter. If you know the chord
you are typing, you finish it well inside the window and never see the
panel at all — it only appears when you hesitate, which is exactly when
it helps.

Set `which-key.delay=0` if you would rather have it immediately, or
`which-key.enabled=false` to turn it off:

    :set which-key.delay=0
    :set which-key.enabled=false

## When it does not appear

- **The prefix has nothing under it.** An empty panel is worse than no
  panel, so nothing opens.
- **The pane is very narrow.** Below about 20 columns a single column of
  truncated labels helps nobody.
- **You finished the chord.** That is the intended case.

## See also

- `:describe-key` — ask about a specific chord, including one you cannot
  press right now, and including a bare prefix.
- `:keymap` — the full binding catalog.
- `:describe-bindings` — what can fire in this buffer.
