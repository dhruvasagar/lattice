---
summary: "repl-mode: makes i/a/o/A/I/O jump to the prompt and enter Insert, so 'start typing' always lands at the input line of a transcript buffer."
related: [repl, prompt]
---

# repl-mode

A minor mode for buffers shaped like a REPL: a read-only transcript
with an editable prompt at the bottom.

It changes one thing, and it's the thing that matters in such a buffer:
the Normal-mode insert-entry keys don't insert *where the cursor is*.

| Key | Normally | Here |
|---|---|---|
| `i` `a` `o` `A` `I` `O` | Enter Insert at the cursor | Move to the prompt, then enter Insert |

Because the transcript above the prompt is read-only, "start typing" in
a REPL always means "type at the prompt". Without this you'd press `i`
somewhere in the scrollback, get Insert on a read-only region, and have
your keystrokes rejected — technically correct and completely useless.

## Where you meet it

Any buffer with the transcript-plus-prompt shape activates it — the AI
conversation buffers are the current consumers. You don't toggle it
yourself; the buffer's major mode brings it.

## Why a minor mode

It used to live on a major mode's keymap, and that was the wrong place.
A major-mode keymap is gated by the buffer's active major, and binding
the six most common Normal-mode keys through that gate means any gap in
the gating resurfaces them *everywhere* — `i` jumping to end-of-buffer
on the dashboard was the symptom that produced this rewrite.

As a minor mode scoped per buffer, the binding is live exactly where
the mode is and nowhere else.

## See also

- [`modes`](help:modes) — majors, minors, and how activation is scoped.
- [`modal-editing`](help:modal-editing) — what those keys do everywhere
  else.
