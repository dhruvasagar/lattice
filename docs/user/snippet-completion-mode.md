---
summary: "snippet-completion-mode: puts snippet triggers into the completion popup alongside buffer words, tree-sitter symbols, and LSP candidates."
related: [snippet, completion]
---

# snippet-completion-mode

Makes your snippets show up as **completion candidates**. Type a
trigger prefix and the popup offers the snippet next to the other
sources; accept it and it expands.

You never turn this on yourself. It rides with
[`snippet-mode`](help:snippet-mode) — activating that brings this,
because "snippets are enabled here" and "snippet candidates appear in
the popup" should never disagree.

## What it contributes

Exactly one thing: a completion source. It has no chords, no options,
and no behaviour beyond producing candidates.

That narrowness is the design. Expansion is
[`snippet-mode`](help:snippet-mode)'s job, in-flight placeholder
navigation is [`active-snippet-mode`](help:active-snippet-mode)'s, and
this one only answers "what snippets could complete what's been
typed?". Each piece can then be reasoned about — and switched off — on
its own.

## Ranking and display

Candidates flow through the same matcher, ranker, and annotation
pipeline as every other source, so snippets aren't privileged in the
popup — they compete on match quality like anything else. See
[`completion`](help:completion) for how sources combine and how to
tune what appears.

## See also

- [`snippet-mode`](help:snippet-mode) — the gate and direct expansion.
- [`completion`](help:completion) — the popup, its keymap, and its
  other sources.
