---
summary: "magit-transient: the dispatch (C-c g) and file-dispatch (C-c f) grouped action menus — single-key triggers, toggleable flags, argument inputs, live command previews, submenu navigation."
related: [magit, magit-status, magit-buffers, picker]
---

# Magit transient menus

Transient menus are grouped action popups that give you single-key
access to every magit operation. They are Lattice's equivalent of magit
transient prefix commands — a "which-key on steroids" that shows
available actions, toggleable flags, and a live command preview in one
overlay.

Transients are built on the [picker](picker.md) subsystem's transient-
mode extension — the same rendering and interaction substrate that
powers which-key key hints, command palette drilldown, and future
plugin transients.

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

Opens from any buffer. Groups:

```
┌─ Magit ───────────────────────────────────────┐
│  Magit dispatch                    on main    │
│                                                │
│  ▸ Working tree                                │
│    [s]  stage          Stage changes       3   │
│    [u]  unstage        Unstage changes     2   │
│    [c]  commit         Commit changes          │
│                                                │
│  ▸ History                                     │
│    [l]  log            Show commit history     │
│                                                │
│  ▸ Branches, merging, rebasing                │
│    [b]  branch         Branch operations       │
│    [m]  merge          Merge operations        │
│    [r]  rebase         Rebase operations       │
│                                                │
│  ▸ Stashing                                    │
│    [z]  stash          Stash operations        │
│                                                │
│  ▸ Remotes                                     │
│    [F]  fetch          Fetch from remote       │
│    [P]  push           Push to remote          │
│                                                │
│  q dismiss                                     │
└────────────────────────────────────────────────┘
```

Groups marked `▸` are **submenus** — pressing their key opens a nested
transient specific to that operation (e.g., `b` opens the branch menu).

### Submenu: Branch (`b`)

```
┌─ Branch ───────────────────────────────────────┐
│  Branch                             on main    │
│                                                │
│  ▸ Actions                                     │
│    [b]  checkout          checkout branch       │
│    [c]  create            create new branch     │
│    [d]  delete            delete branch      ⚠ │
│    [m]  merge             merge into current    │
│                                                │
│  ▸ Configure                                   │
│    [-]  [ ] force         force-create/delete   │
│                                                │
│  ────────────────────────────────────────────  │
│  git checkout main                             │
│                                                │
│  q dismiss   DEL back                          │
└────────────────────────────────────────────────┘
```

### Submenu: Stash (`z`)

```
┌─ Stash ────────────────────────────────────────┐
│  Stash                                          │
│                                                │
│  ▸ Actions                                     │
│    [z]  create           create new stash       │
│    [a]  apply            apply stash            │
│    [p]  pop              apply + drop stash     │
│    [d]  drop             drop stash             │
│                                                │
│  ▸ Configure                                   │
│    [-]  [ ] untracked    include untracked       │
│                                                │
│  ────────────────────────────────────────────  │
│  git stash push                                │
│                                                │
│  q dismiss   DEL back   -u toggle               │
└────────────────────────────────────────────────┘
```

### Submenu: Push (`P`)

```
┌─ Push ─────────────────────────────────────────┐
│  Push                                to origin  │
│                                                │
│  ▸ Actions                                     │
│    [p]  push              push current branch   │
│    [u]  push-set-upstream push + set upstream  │
│                                                │
│  ▸ Configure                                   │
│    [-]  [ ] force         force push            │
│    [-]  [ ] all           push all branches    │
│                                                │
│  ────────────────────────────────────────────  │
│  git push origin main                          │
│                                                │
│  q dismiss   DEL back   -f toggle   -a toggle   │
└────────────────────────────────────────────────┘
```

### How submenus work

1. Press a submenu key (e.g., `b` for branch). The current transient is
   pushed onto a stack and the submenu transient replaces it.
2. Press `BS` or `DEL` to pop the stack and return to the parent.
3. Press `q`, `Esc`, or `C-g` to dismiss the entire transient stack.
4. Submenus can nest — from branch you can go to merge, from merge you
   can go to rebase. Each step pushes onto the stack.

---

## File dispatch transient (`C-c f`)

Opens from any buffer. Shows per-file operations for the current
buffer's file. If opened from a magit buffer with a file entry under
the cursor, it resolves the file path from the section index instead.

```
┌─ File ─────────────────────────────────────────┐
│  src/auth/login.rs             modified +12 -3 │
│                                                │
│  ▸ Actions                                     │
│    [s]  stage              stage this file      │
│    [u]  unstage            unstage this file    │
│    [x]  discard            discard changes   ⚠ │
│    [d]  diff               show diff            │
│                                                │
│  ▸ History                                     │
│    [l]  log                show log       23 💬 │
│    [b]  blame              blame this file      │
│                                                │
│  ▸ Filesystem                                  │
│    [r]  rename             rename file          │
│    [D]  delete             delete file       ⚠ │
│    [c]  checkout           checkout from HEAD  │
│                                                │
│  q dismiss   s stage   d diff   l log           │
└────────────────────────────────────────────────┘
```

Marginalia (dimmed right-aligned text) shows live data from git: file
status, diffstat (`+12 -3`), commit count (`23 💬`), and last commit SHA.
The `⚠` glyph marks destructive actions (discard, delete).

---

## Toggleable flags

Flag items (like `[-f] force` in the branch menu) toggle boolean values
in-place. Pressing the flag's key toggles the indicator between `[x]`
and `[ ]` and updates the live command preview.

Flag values accumulate across multiple transients in the same session
— if you toggle `--force` in the push menu, dismiss it, and reopen, the
flag retains its last-set value.

The transient state (flag values + argument values) is held in a
`HashMap<String, TransientValue>` on the mode's Guard.

---

## Arguments

Argument items open a minibuffer prompt for a string value (e.g., a
branch name, a remote name, a commit reference). After confirming the
prompt, the value is set and control returns to the transient. The live
command preview updates to reflect the argument.

Arguments persist across transient invocations — if you set a branch
name argument and dismiss, reopening the transient retains the value.

---

## Live command preview

Each transient has an optional `PreviewFn` — a closure that builds the
preview line from the current transient state (flag values, argument
values). The preview updates every time a flag toggles or an argument
changes.

In the examples above, the `─── git checkout main ───` line is the
live preview — it shows the exact git command that will run when you
press the action key.

---

## Direct chords (advanced-user fast path)

Every transient action also has a direct chord binding registered on
the mode's keymap. Advanced users who have memorised the chords can
press them directly without opening the transient first — the chord
and the transient-submitted action fire the same `ActionId`.

For example, from the magit-status buffer, `s` directly stages the
hunk at cursor — it skips `C-c g` → `s` and fires the identical
handler. The transient is a **discoverability surface** for new
users; the direct chords are a speed path for experienced users.

---

## Options

| Option | Type | Default | Description |
|---|---|---|---|
| `magit.transient.persist-state` | `bool` | `true` | Retain flag and argument values across transient invocations |

---

## How transients work

Transients are a **picker interaction mode** — the picker's rendering
pipeline (floating overlay, keyboard capture, styled text rendering,
TUI + GPUI parity) is the substrate. The transparent mode extension
adds:

1. **Grouped, non-filterable entries** — `TransientGroup` with section
   headers, unlike the standard picker's flat, fuzzy-filterable list.
2. **Single-key triggers** — each item carries a key binding; pressing
   that key fires the item's action without cursor navigation.
3. **Flag toggle indicators** — `[x]` / `[ ]` display for boolean
   flags, updated in-place.
4. **Argument → minibuffer → return** — clicking an argument item opens
   a minibuffer prompt; confirming sets the value and returns to the
   transient.
5. **Submenu stack** — nested transients push onto a stack; `BS`/`DEL`
   pops back to the parent.

The transient data model (`TransientSpec`, `TransientGroup`,
`TransientItem`, `TransientItemKind`, `TransientState`,
`TransientValue`) lives in `lattice-picker`, consumed by
`lattice-magit` for magit-specific transient definitions.
