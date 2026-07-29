---
summary: "magit-transient: the dispatch (C-c g) and file-dispatch (C-c f) grouped menus. C-c g opens every magit buffer plus fetch/pull/push, with commit and stash submenus. C-c f stages/unstages/discards and opens diff/log/blame for the current buffer's file. Keys follow Emacs magit's own. The substrate also supports toggleable flags, argument inputs, and live previews, but no shipped magit menu uses them yet."
related: [magit, magit-status, picker]
---

# Magit transient menus

Transient menus are grouped popup menus, Lattice's equivalent of Emacs
magit's transient prefix commands — a "which-key on steroids" overlay.
`C-c g` gives you single-key access to every magit **entry point**
(opening the right buffer from wherever you are) plus the remote
operations and stash-push; `C-c f` covers the six most common
operations on the file you're editing. Neither is yet the full
per-domain action surface (flags, arguments, merge/tag/reset/revert)
that Emacs magit's transients provide — see below for exactly what's
real.

**Key assignments follow Emacs magit's own** wherever lattice has the
corresponding capability, so muscle memory carries across. Magit
entries lattice has no implementation for are deliberately **absent**
rather than present-and-inert — a menu row that does nothing when
pressed is worse than a row that isn't there.

Transients are built on the [picker](help:picker) subsystem's transient-
mode extension — the same rendering and interaction substrate that
powers which-key key hints and command palette drilldown, and (per its
module doc) is meant to power future plugin transients too.

---

## Quick reference

| Chord | Scope | Opens |
|---|---|---|
| `C-c g` | Global (any buffer) | Repo-level dispatch transient |
| `C-c f` | Global (any buffer) | File-level dispatch transient for the current buffer's file |

### Inside a transient

| Key | Action |
|---|---|
| Single letter / chord | Fire the action, toggle the flag, or open the submenu |
| `j` / `k` / `C-n` / `C-p` | Scroll through groups (if they overflow the viewport) |
| `q` / `Esc` / `C-g` | Dismiss the transient |
| `BS` / `DEL` | Return to parent transient (if in a nested submenu) |

---

## Repo dispatch transient (`C-c g`)

Opens from any buffer. Groups, as they actually render today:

```
┌─ Magit dispatch ───────────────────────────────┐
│                                                │
│  ▸ Working tree                                │
│    [s]  status          Open the status buffer │
│    [d]  diff            Diff the working tree  │
│                          against HEAD          │
│    [c]  commit        ▸ Commit changes         │
│                                                │
│  ▸ History                                     │
│    [l]  log             Show commit history    │
│                                                │
│  ▸ Branches                                    │
│    [b]  branch          Open the branch list   │
│                                                │
│  ▸ Stashing                                    │
│    [z]  stash         ▸ Stash operations       │
│                                                │
│  ▸ Remotes                                     │
│    [f]  fetch           Fetch from the remote  │
│                          without merging       │
│    [F]  pull            Fetch + fast-forward   │
│                          merge from the remote │
│    [P]  push            Push to the remote     │
│                                                │
│  ▸ Misc                                        │
│    [r]  rebase          Start an interactive   │
│                          rebase                │
│                                                │
│  q dismiss  BS back                            │
└────────────────────────────────────────────────┘
```

`s`/`d`/`l`/`b`/`r` open the corresponding buffer directly — the same
thing `:magit-status`/`:magit-diff`/`:magit-log`/`:magit-branch`/
`:magit-rebase` do. Once you're in that buffer, use its own direct
chords (`s`/`u`/`x`/`<CR>`/… — see
[`magit`](help:magit)) for the actual operations.

Two submenus drill down:

| Chord | Action |
|---|---|
| `c c` | Open the commit buffer |
| `c a` | Amend the previous commit |
| `z z` | Stash the working tree (`git stash push`) |
| `z l` | Open the stash list |

`c c`/`c a` are the same two keystrokes as magit-status's own `cc`/`ca`
chords, so committing and amending feel identical whether you're in
the status buffer or reaching for the dispatch menu from an ordinary
file. Stash apply/pop/drop are **not** in the `z` submenu — they need
a stash selected, which only the stash-list buffer provides; `z l`
then `a`/`p`/`d` there.

`f` (fetch), `F` (pull), `P` (push), and `z z` (stash push) run git
directly rather than opening a buffer. `f` runs plain `git fetch`
(updates your remote-tracking refs, touches nothing else); `F` runs
`git pull --ff-only` — it will never create a merge commit, and fails
cleanly if your branch has diverged rather than merging; `P` runs
`git push`. All run in the background and fail fast (no hang) if git
needs credentials it doesn't have. **None reports success or failure
back into the menu** — there's no synchronous path from the background
task back to a transient that's already dismissed. Check the
`*messages*` buffer or the debug log for the outcome.

### Magit entries that aren't here

`C-c g` covers what lattice implements. Emacs magit's dispatch also
offers bisect (`B`), merge (`m`), tag (`t`), revert (`V`), reset
(`X`), cherry-pick (`A`), submodule (`o`), remote (`M`), and patch
(`w`/`W`) — none of which lattice has behind them, so none appears in
the menu. Branch *merge* specifically does exist, but only as the `m`
chord inside the branch-list buffer (it needs a branch selected);
`b` then `m` gets you there.

### How submenus work (mechanism)

Pressing a submenu's key pushes the current transient onto a stack and
opens the submenu; `BS`/`DEL` pops back to the parent; `q`/`Esc`/`C-g`
dismisses the whole stack.

---

## File dispatch transient (`C-c f`)

Opens from any buffer via `C-c f`:

```
┌─ File dispatch ────────────────────────────────┐
│                                                │
│  ▸ Stage                                       │
│    [s]  stage           Stage this file        │
│    [u]  unstage         Unstage this file      │
│    [x]  discard         Discard this file's    │
│                          working-tree changes  │
│                          (asks first)          │
│                                                │
│  ▸ Inspect                                     │
│    [d]  diff            Show diff for this file│
│    [l]  log             Show commit history    │
│                          for this file         │
│    [b]  blame           Blame this file        │
│                                                │
│  q dismiss                                     │
└────────────────────────────────────────────────┘
```

Every item acts on the file belonging to **whichever buffer was active
when you pressed `C-c f`** — not an entry at the cursor in some other
buffer. If the active buffer has no file (a synthetic buffer, an
unsaved scratch buffer), or isn't inside a git repository, there's no
path to resolve and the key does nothing.

| Chord | What it runs |
|---|---|
| `s` | `git add <file>` |
| `u` | `git reset HEAD -- <file>` (unstage, keep working-tree changes) |
| `x` | `git checkout -- <file>` — **destructive**, asks `Discard changes to <path>?` first |
| `d` | Opens `*magit:diff:<path>*` ([diff buffer](help:magit-diff-mode)) |
| `l` | Opens `*magit:log:<path>*` — the [log buffer](help:magit-log-mode) scoped to this file's history |
| `b` | Opens `*magit:blame:<path>*` ([blame buffer](help:magit-blame-mode)) |

`x` is the only destructive item, and the only one that confirms
first — same `y`/`n` confirmation dialog magit-status's own `x` uses.
`s`/`u` report optimistically ("magit: staged <path>") and log the
real outcome; they don't block on git.

This transient still can't do what its name in Emacs magit implies —
"act on the entry at cursor in magit-status" — because the ex-command
that opens it has no buffer/cursor context from any *other* buffer to
resolve, only the one that was active. Use the direct chords in
`magit-status` (`s`/`u`/`x`/`d`) for acting on a file other than the
one you're currently in.

Magit's file dispatch also offers stage-all/unstage-all, edit-blob,
trace-definition, and commit-fixup — absent here for the same reason
as the repo menu's gaps: no implementation behind them.

---

## Toggleable flags and the live preview

Some operations take flags, and the menus that offer them stay open
while you toggle, showing the exact git command you're about to run.

### Where they are

| Menu | Key | Flags |
|---|---|---|
| Push (`C-c g` → `P`) | `-f` | `--force-with-lease` |
| | `-u` | `--set-upstream` |
| Fetch (`C-c g` → `f`) | `-a` | `--all` |
| | `-p` | `--prune` |
| Stash (`C-c g` → `z`) | `-u` | `--include-untracked` |
| | `-m` | a stash message (opens a prompt) |

Push and fetch became **submenus** to hold their flags: `P` opens the
push menu, and `P` again (or the listed run key) actually pushes. That
is one extra keystroke than before, and it's the cost of the flags
being reachable at all — a flat menu fires the instant you press its
key, leaving no moment in which to toggle anything.

Pull deliberately has no flags. `--ff-only` isn't optional: a pull that
could silently create a merge commit is the wrong default, so it stays
a direct action rather than a submenu with nothing in it.

### The preview line

While a flag menu is open, the preview shows the command as currently
configured:

```
git push
git push --force-with-lease          # after -f
git push --force-with-lease --set-upstream   # after -u
```

The preview is generated from the same table that builds the argv, so
it cannot show one command and run another.

### `--force-with-lease`, not `--force`

Push offers `--force-with-lease` and does not offer bare `--force`.
The difference matters: `--force-with-lease` refuses when the remote
moved since your last fetch, which is exactly the situation where a
bare force quietly destroys someone else's commits. Emacs magit
defaults the same way.

### The same flags on the `:` line

Every flag works as an ex-command argument too:

```
:magit-push --force-with-lease --set-upstream
:magit-fetch --all --prune
:magit-stash --include-untracked
```

Both surfaces resolve to the same arguments and run the same code —
the transient is the discoverable path, the ex-command the scriptable
one. Full spellings only; there are no abbreviations. An unrecognised
token is ignored rather than failing the command, since the flags are
additive: the worst case is an operation that does slightly less than
you asked, never something you didn't ask for.

### Arguments that take a value

Some entries take text rather than a yes/no. `-m` in the stash menu is
the first: press it, type a message, press `<CR>`, and you're back in
the menu with the message filled into the preview.

```
git stash push                         # before
git stash push -m "wip: parser"        # after -m
git stash push --include-untracked -m "wip: parser"
```

An unlabelled stash is findable only by position, and positions
renumber whenever you drop one — so a message is the difference
between "stash@{2}, probably" and knowing.

Things worth knowing about the round-trip:

- **`<Esc>` cancels the argument, not the menu.** You come back to the
  menu with the value unchanged, so a typo costs a keystroke rather
  than your place.
- **Flags you toggled before opening the prompt survive it.** The menu
  is parked whole and restored whole, including which submenu you were
  in.
- **Re-selecting an argument seeds the prompt with its current value**,
  so editing beats retyping.
- **Submitting an empty value clears the argument.** That's how you
  unset one you set by mistake — the empty string never reaches git,
  because `git stash push -m ""` would label the stash with nothing,
  which is worse than no label.

On the `:` line, a value argument takes the rest of the line, so it
needs no quoting and must come last:

```
:magit-stash --include-untracked -m wip: parser and tests
```

The branch-create wizard's name prompt (see
[`magit-branch-mode`](help:magit-branch-mode)) is a different
mechanism — a picker-triggered follow-up, not an argument field inside
a menu.

### Still to come

Log's `--all` / count / path-filter. A log buffer's scope rides its
*name* (`*magit:log:<path>*`), so its arguments need that channel
rather than this one.

---

## Direct chords vs. the dispatch transient

Every magit buffer's own operations (`s`/`u`/`x`/`<CR>`/… in
magit-status, `s`/`u` in magit-diff, `a`/`p`/`d`/`z` in magit-stash, …)
are ordinary chords on that buffer's keymap — see
[`magit`](help:magit) and
[`magit-status-mode`](help:magit-status-mode). They are **not** exposed as items
in the `C-c g` dispatch transient: for its buffer-opening items
(`s`/`d`/`l`/`b`/`r`, plus `c c`/`c a`/`z l`) the dispatch transient
only opens buffers, it doesn't reach into a buffer and stage a file
for you. So `C-c g` then `s` opens the status buffer (equivalent to
`:magit-status` or `C-x g`) — it does not stage anything, even though
plain `s` inside that same status buffer does. The letters coincide by
mnemonic convenience, not because they fire the same action.

The direct-action exceptions are `f`/`F`/`P` (fetch/pull/push) and
`z z` (stash push), which fire the git operation straight from the
transient — there's no equivalent buffer to open first. Every `C-c f`
item acts directly too, on the current buffer's file.

So the repo dispatch's job is primarily **discoverability of entry
points** (with the remote/stash-push exceptions), while the file
dispatch is a genuine **action menu** for the file you're editing.
Once you're in a magit buffer, its own direct chords are how you do
the rest of the work.

---

## How transients work

Transients are a **picker interaction mode** — the picker's rendering
pipeline (floating overlay or minibuffer strip depending on
`picker.display`, keyboard capture, styled text rendering, TUI + GPUI
parity) is the substrate. See [`picker`](help:picker) for that shared
rendering machinery. The transient mode extension adds:

1. **Grouped, non-filterable entries** — `TransientGroup` with section
   headers, unlike the standard picker's flat, fuzzy-filterable list.
2. **Single-key triggers** — each item carries a key binding; pressing
   that key fires the item's action without cursor navigation.
3. **Flag toggle indicators, argument → minibuffer → return, and a
   live preview line** — supported by the data model
   (`TransientItemKind::Flag`/`Argument`, `TransientSpec::preview`),
   but not exercised by any shipped magit menu yet — see above.
4. **Submenu stack** — nested transients push onto a stack; `BS`/`DEL`
   pops back to the parent. Exercised today by the commit (`c`) and
   stash (`z`) submenus under `C-c g`.

The transient data model (`TransientSpec`, `TransientGroup`,
`TransientItem`, `TransientItemKind`, `TransientState`,
`TransientValue`) lives in `lattice-picker`, consumed by
`lattice-magit` for magit's dispatch/file-dispatch menus, and by the
generic `x`-discard confirmation dialog (a two-item `y`/`n` transient
built from `confirm_transient_spec`, reused wherever the editor needs
a yes/no confirmation — magit-status's discard prompt is the first
consumer).
