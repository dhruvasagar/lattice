---
summary: "The emacs-keys tribute: a default-on, configurable <C-x> leader binding emacs-style buffer / file / window chords on top of vim modal editing."
related: [mode, emacs-keys-mode]
---

# emacs-keys-mode (the `<C-x>` leader)

Lattice is a vim-modal editor (see
[`modal-editing`](help:modal-editing)). But emacs's `C-x` prefix
map — `C-x C-f` to find a file, `C-x b` to switch buffer, `C-x 2`
to split a window — is muscle memory for a lot of people, and the
two vocabularies don't actually collide: vim owns the alphabetic
Normal-mode keys, emacs's window/buffer verbs live behind a
`Ctrl`-prefix leader. So Lattice ships **both**.

`emacs-keys-mode` is a small **tribute** minor mode. It contributes
one thing: a configurable `<C-x>` leader (prefix key) whose chord
map mirrors the most-used entries of emacs's `C-x` keymap. It does
**not** replace vim modal editing, remap your alphabetic motions,
or touch Insert mode. It layers a handful of `Ctrl`-prefixed
window/buffer chords on top.

> This is an optional convenience, not a paramount feature. If you
> don't want it, one `:set noemacs-keys` reclaims `<C-x>` for plain
> Normal-mode input. The vim grammar is the canonical command API;
> this is a second entry point to commands you already have.

---

## Enabling it

It's **on by default** (`emacs-keys = true`). Two typed options
control it, both live (`:set` re-pushes the leader map immediately —
no restart):

| Option              | Type   | Default  | Effect                                              |
|---------------------|--------|----------|-----------------------------------------------------|
| `emacs-keys`        | bool   | `true`   | Master enable. `:set noemacs-keys` empties the leader. |
| `emacs-keys-prefix` | string | `<C-x>`  | The leader chord that opens the tribute map.        |

```
:set noemacs-keys              " turn the tribute off (reclaim <C-x>)
:set emacs-keys                " turn it back on
:set emacs-keys-prefix=<C-z>   " move the whole leader to <C-z>
```

Disabling doesn't churn the mode set — the mode stays registered and
active; the **layer** carries the gate, so toggling it off just
rebuilds the leader map empty and `<C-x>` falls through to ordinary
Normal-mode resolution. Re-targeting the prefix rebuilds the *whole*
map under the new chord live; the old prefix stops responding. A
malformed prefix degrades to an empty tribute (logged, never a
panic) rather than breaking boot.

The `emacs-keys` *option* and the `emacs-keys-mode` *mode* are
distinct names (the `-mode` suffix is the mode-id convention — see
[`modes`](help:modes)). You toggle the feature with `:set
emacs-keys`, not `:emacs-keys-mode`.

### It's live everywhere — not just code buffers

`emacs-keys-mode` is a **`Universal`-activation** mode: the leader is
live in *every* buffer you can focus, including synthetic buffers
like `*messages*`, help, and the file tree — exactly like emacs,
whose `C-x` map works in `*Messages*` too. (Most minor modes are
document-only; this one is deliberately universal because a
window/buffer leader that died in synthetic buffers would be a
worse tribute.)

---

## The `<C-x>` leader and its chord map

Press the leader (`<C-x>` by default), then a suffix key. The bare
leader is a *partial chord* — Lattice waits for the suffix (or
`<Esc>` to abort), the same partial-chord state any multi-key vim
chord uses.

The map has two tiers. **Tier 1** is buffer / file / save / quit;
**Tier 2** is window (pane) management. Every chord is a second
entry point to an ex-command or action you already have — the
leader introduces no new commands of its own.

### Tier 1 — file / buffer / save / quit

| Chord        | Runs              | Action                                          |
|--------------|-------------------|-------------------------------------------------|
| `<C-x><C-f>` | `:files`          | Find file — open the file picker                |
| `<C-x><C-s>` | `:write`          | Save the current buffer                         |
| `<C-x>b`     | `:buffers`        | Switch buffer — open the buffer picker          |
| `<C-x><C-b>` | `:ls`             | List buffers (the static `:ls` text listing)    |
| `<C-x>k`     | `:bdelete`        | Kill (delete) the current buffer                |
| `<C-x><C-c>` | `:quit-all`       | Quit the whole editor — **dirty-guarded**       |

`<C-x>b` (switch) and `<C-x><C-b>` (list) are distinct chords,
mirroring emacs's `switch-to-buffer` vs `list-buffers`.

`<C-x><C-c>` is emacs's `save-buffers-kill-emacs`. It routes to the
dirty-guarded `:qa` (quit every pane and tab), **not** the brute
`<C-c>` quit — unsaved changes are honoured exactly as `:qa` honours
them. Plain `<C-c>` (no leader) keeps its usual meaning.

### Tier 2 — window (pane) management

| Chord     | Runs    | Action                                         |
|-----------|---------|------------------------------------------------|
| `<C-x>2`  | (split) | Split the pane **horizontally** (new pane below) |
| `<C-x>3`  | (split) | Split the pane **vertically** (new pane right) |
| `<C-x>0`  | (close) | Close this pane (`delete-window`)              |
| `<C-x>1`  | (only)  | Close every *other* pane (`delete-other-windows`) |
| `<C-x>o`  | (focus) | Move focus to the next pane (`other-window`)   |

These mirror emacs's `C-x 2` / `C-x 3` / `C-x 0` / `C-x 1` / `C-x o`
and reuse the same pane actions the vim `<C-w>` family binds — the
leader is just a second doorway to them.

> **Digit suffixes don't become counts.** `<C-x>2` and `<C-x>3` are
> matched as literal second chords after the leader, so the digit
> never enters vim count accumulation. A *bare* digit (no leader
> pending) still starts a count as usual — `2j` is two-lines-down,
> untouched.

### `<C-g>` is *not* part of this mode

`<C-g>` (emacs's `keyboard-quit`) cancels a running operation and
returns you to Normal — see
[modal-editing](help:modal-editing). It is a **builtin** binding, not
part of the tribute, so `:set noemacs-keys` does not take it away.

It is called out here because it is the one emacs chord you get
regardless of this setting, and because it is *not* under the `<C-x>`
leader.

---

## The window/buffer ex-commands

The pane operations the Tier-2 chords reach also exist as plain
ex-commands, with vim-conventional names — useful from the `:` line,
in macros, and in config:

| Ex-command            | Short      | Does                                              |
|-----------------------|------------|---------------------------------------------------|
| `:split`              | `:sp`      | Split the current pane horizontally               |
| `:vsplit`             | `:vsp` / `:vs` | Split the current pane vertically             |
| `:close`              | `:clo`     | Close the current pane                            |
| `:only`               | `:on`      | Close every pane except the active one (`C-x 1`)  |
| `:qa`                 | `:qall` / `:quitall` | Quit the whole editor (every pane + tab) |

`:qa` is the editor-wide quit: it ignores pane and tab count and
shuts everything down (subject to the dirty guard). Contrast plain
`:q`, which closes a single pane — or the current tab page if it's
the tab's last pane — and only quits when it's the last pane of the
last tab.

---

## How it coexists with vim modal editing

The tribute is **additive**. It does not change the vim grammar:

- **The leader layer is Normal-mode only.** It binds chords in
  Normal mode and nowhere else. Insert mode is untouched — `<C-x>`
  there keeps whatever Insert-mode meaning it has (e.g. completion
  triggers), and so does every other key. Visual / Operator-Pending
  / Replace / the `:` and `/` lines are all unaffected.
- **No alphabetic motion is remapped.** `h` `j` `k` `l`, `w` `b` `e`,
  operators, text objects, counts — all exactly as
  [`modal-editing`](help:modal-editing) describes. The tribute lives
  behind a `Ctrl`-prefix leader, off in its own corner of the keymap.
- **It's one minor mode among many.** It shows up in `:list-modes`,
  carries metadata in `:describe-mode emacs-keys-mode`, and its
  chords appear in `:describe-key` / `:keymap` with their source —
  the same introspection every mode gets. `:describe-key <C-x>b`
  tells you exactly what the chord does under your active modes.

If a chord ever feels like it's shadowing something, that's what the
introspection is for: `:describe-key` replays the live precedence
fold and names the binding that actually fires. And the escape hatch
is always one option away — `:set noemacs-keys`.

---

## See also

- [`modal-editing`](help:modal-editing) — the vim grammar this
  layers on top of (Normal / Insert / Visual, operators, motions).
- [`modes`](help:modes) — major + minor modes, `:<mode-name>`
  toggles, the option-resolution model, and the keymap-precedence
  rules the leader plays by.
- [`options`](help:options) — `:set` + `:customize`, the typed
  options `emacs-keys` and `emacs-keys-prefix` live in.
- `docs/dev/architecture/emacs-keys.md` — the design fragment
  (the *what* and *why*); `docs/dev/operations/slice-plans/archive/emacs-keys.md`
  is the implementation record.
