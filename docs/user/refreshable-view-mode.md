---
summary: "refreshable-view-mode: gives a generated view the shared `gr` refresh chord; each view declares which of its own actions gr should run."
related: [multibuffer, magit]
---

# refreshable-view-mode

`gr` means "refresh this view" in every generated buffer — magit status,
a project diff, search results, a listing. This mode is where that chord
lives, once.

## Why one mode instead of a chord per view

Because the alternative was tried and drifted. Each view declaring its
own `gr` meant the same binding written out several times, and a view
that was simply forgotten offered no refresh at all — with nothing to
notice it, because there was no single place the chord should have been.
A gap in a copied set does not announce itself.

So the chord is declared here and the *body* is not: the mode carries a
keymap layer and deliberately no handler. Each view declares which of its
own actions refreshes it, and the host resolves that when `gr` fires. The
shared part is shared; the per-view part stays with the view.

## Activation

Manual, never automatic. `gr` in an ordinary source buffer is
[LSP references](help:lsp-references-mode) and has to stay that way, so this mode
attaches only to the generated views that opt in.

## Keybindings

- `gr` — refresh this view.

## Options

None.

## See also

- [`magit`](help:magit) — the family of views this was built for.
