---
summary: "completion-popup-mode: active only while the completion popup is open — owns C-n / C-p / C-y / Tab / CR and the rest of the popup keymap."
related: [completion, complete]
---

# completion-popup-mode

Active **only while the completion popup is open**. It owns the keys
that mean something exclusively inside that popup:

`<C-n>` / `<C-p>` (next / previous), `<C-y>` (accept), `<Tab>`,
`<CR>`, `<Esc>`, `<C-e>`, `<C-d>`, `<C-Space>`, `<C-f>` / `<C-b>`,
plus the per-source filter chords.

## Why a mode and not a flag

This replaced an imperative "is a popup open?" boolean the dispatcher
used to consult. As a mode, the popup's keymap is scoped by the same
machinery as every other mode's, so `<C-n>` binds in one place and is
live exactly when the popup is — instead of the dispatcher branching on
a flag before deciding what a key means.

It is the transient half of a pair;
[`completion-mode`](help:completion-mode) is the persistent gate that
decides whether a popup can open here in the first place.

## Options

None.

## Keybindings

`<C-n>` / `<C-p>` next / previous, `<C-y>` accept, `<Tab>`, `<CR>`, `<Esc>`, `<C-e>`, `<C-d>`, `<C-Space>`, `<C-f>` / `<C-b>`, plus the per-source filter chords. Live only while the popup is open.

## See also

- [`completion`](help:completion) — the popup, its sources, and
  configuration.
