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
| `j` / `k` / `C-n` / `C-p` | Move the selection one item, wrapping at either end; the view scrolls to follow when the menu overflows |
| `<CR>` | Fire the selected item — the same thing its key does |
| `BS` / `DEL` | Return to parent transient (if in a nested submenu) |
| `q` / `Esc` / `C-g` | Return to parent; at the top level, dismiss |

The selected row is marked with a `❯` and a bold label. Pressing an
item's key is still the primary way to use a transient — the selection
exists so a menu taller than the popup can be walked, and so there is
somewhere visible for `<CR>` to land.

---

## Repo dispatch transient (`C-c g`)

Opens from any buffer. Groups, as they render **outside** a magit
buffer (see [below](#what-changes-inside-a-magit-buffer) for what it
adds inside one):

```
┌─ Magit dispatch ───────────────────────────────┐
│                                                │
│  ▸ Working tree                                │
│    [s]  status          Open the status buffer │
│    [d]  diff          ▸ Diff the working tree  │
│                          against HEAD          │
│    [c]  commit        ▸ Commit changes         │
│                                                │
│  ▸ Applying changes                            │
│    [S]  stage all       Stage every tracked    │
│                          modification          │
│    [U]  unstage all     Unstage everything,    │
│                          keeping your tree     │
│                                                │
│  ▸ History                                     │
│    [l]  log           ▸ Show commit history    │
│    [A]  cherry-pick   ▸ Copy or move commits   │
│                          onto this branch      │
│    [_]  revert        ▸ Revert a commit        │
│    [O]  reset         ▸ Reset this branch to   │
│                          a commit              │
│    [B]  bisect        ▸ Find the commit that   │
│                          introduced a bug      │
│    [T]  notes         ▸ Edit, remove, merge or │
│                          prune commit notes    │
│    [Y]  cherries        Which commits are not  │
│                          upstream yet          │
│                                                │
│  ▸ Branches                                    │
│    [b]  branch        ▸ Checkout, create,      │
│                          rename, delete, list  │
│                                                │
│  ▸ Stashing                                    │
│    [z]  stash         ▸ Stash operations       │
│                                                │
│  ▸ Remotes                                     │
│    [f]  fetch         ▸ Fetch from the remote   │
│                          without merging       │
│    [F]  pull          ▸ Fetch + integrate from │
│                          the remote            │
│    [P]  push          ▸ Push to the remote     │
│    [o]  submodule       Manage submodules      │
│    [M]  remote          Manage remotes — add,  │
│                          rename, remove, set   │
│                          URL, prune            │
│    [y]  refs            Show every branch,     │
│                          remote-tracking       │
│                          branch and tag        │
│    [C]  clone           Clone a repository     │
│                                                │
│  ▸ Misc                                        │
│    ["]  subtree       ▸ Add, merge, pull, push │
│                          or split a subtree    │
│    [w]  patches       ▸ Apply or create email  │
│                          patches               │
│    [r]  rebase        ▸ Rebase onto a target,   │
│                          or edit history       │
│    [t]  tag           ▸ Create, release,       │
│                          delete or prune tags  │
│    [i]  gitignore       Add a pattern to       │
│                          .gitignore            │
│    [m]  merge         ▸ Merge, preview, squash, │
│                          absorb or dissolve    │
│    [I]  init            Initialize a git       │
│                          repository            │
│                                                │
│  q dismiss  BS back                            │
└────────────────────────────────────────────────┘
```

`s`/`b`/`M` open the corresponding buffer directly — the same thing
`:magit-status`/`:magit-branch`/`:magit-remote` do. Once you're in that
buffer, use its own direct chords (`s`/`u`/`x`/`<CR>`/… — see
[`magit`](help:magit)) for the actual operations.

`d` and `l` open **argument menus** rather than the view itself: toggle
`-w`, `--stat`, `-n`, `--author` and so on, then press `d` / `l` again
to open the view with them applied. The same toggles are reachable with
`D` once a view is already open, which re-runs it in place.

`A` / `_` / `O` need a commit and this menu has no cursor on one, so
they **ask** — a picker of recent commits, then the operation runs on
what you pick. They fire the same actions the `A` / `_` / `O…` chords
do, so in a magit buffer with a commit under the cursor those chords
still act on it directly; only the menu path asks. `O` opens a submenu
for `s` soft / `m` mixed / `h` hard, matching the `Os` / `Om` / `Oh`
chords, and `h` confirms before discarding anything.

### What changes inside a magit buffer

The menu is built for the place you opened it, so two things differ
when you press `C-c g` from inside a magit buffer. Emacs magit does the
same, with `:if-derived` and `:if-mode` predicates on its own rows.

**Applying changes gains three rows** — in any magit buffer:

```
│  ▸ Applying changes                            │
│    [a]  apply           Apply the hunk at      │
│                          cursor to the tree    │
│    [-]  reverse         Reverse the hunk at    │
│                          cursor out of it      │
│    [x]  discard         Discard the hunk or    │
│                          file at cursor        │
│    [S]  stage all       …                      │
│    [U]  unstage all     …                      │
```

They act on the hunk under the cursor, so outside a magit buffer there
is no diff text to find one in and they are absent rather than present
and complaining. `S` and `U` are always there — `git add --update` and
`git reset` need no target.

Magit's `s` / `u` rows are deliberately not among them. They would
collide with the `s` row above, and unlike `a`/`-`/`x` their chords are
the first thing anyone reaches for in a magit buffer — a menu path to
them earns nothing and costs the status key.

**The `s` row becomes a section jump** — in magit-status only:

```
│  ▸ Working tree                                │
│    [s]  jump          ▸ Jump to a section      │
```

"Open the status buffer" is a no-op on the buffer you are already in,
so `s` opens a submenu instead:

| Key | Jumps to |
|---|---|
| `s` | Staged changes |
| `u` | Unstaged changes |
| `n` | Untracked files |
| `z` | Stashes |
| `c` | Recent commits |

Magit's own keys where the sections coincide; `c` is ours, since its
status buffer shows unpushed/unpulled where ours shows recent commits.
A section with no entries isn't rendered at all, so jumping to it says
so rather than leaving the cursor put.

In any *other* magit buffer — a diff, a log, a revision — `s` still
opens the status buffer, which is the useful thing to want there.

Two submenus drill down:

| Chord | Action |
|---|---|
| `c c` | Open the commit buffer |
| `c a` | Amend the previous commit |
| `z z` | Stash the working tree (`git stash push`) |
| `z l` | Open the stash list |
| `z a` / `z p` / `z k` / `z v` | Apply / pop / drop / show a stash |

`c c`/`c a` are the same two keystrokes as magit-status's own `cc`/`ca`
chords, so committing and amending feel identical whether you're in
the status buffer or reaching for the dispatch menu from an ordinary
file.

`z a`/`z p`/`z k`/`z v` resolve their stash the way `A`/`_`/`O` resolve
their commit: **the stash under the cursor when there is one, a picker
when there is not.** So they work on the row you are looking at in the
stash list *and* in magit-status's Stashes section, and from an
ordinary file — where there is no stash to point at — they ask.

These rows previously did nothing at all, silently, outside the
stash-list buffer: they resolved through the stash buffer's own state,
found none, and returned. That is why an earlier revision of this page
said they were not in the `z` submenu.

`S` and `U` act on the whole index. `S` runs `git add --update`:
every **tracked** modification, staged. Untracked files are
deliberately left out — "stage everything" quietly adding a file git
was never told about is how build artefacts and secrets get committed;
stage those explicitly with `s` on the Untracked entry in
[magit-status](help:magit-status-mode). `U` runs a bare `git reset`:
the index goes back to HEAD and your working tree is untouched, so
nothing is lost and everything is still there to re-stage. That is why
`U` doesn't ask, even though it undoes more than one file's worth of
staging.

`t` and `i` **ask for their one value** — a menu opened from anywhere has
no cursor to read a tag name off. `t` prompts for a name and tags HEAD;
`i` prompts for a pattern and appends it to `.gitignore`, skipping it if
that pattern is already there. Submitting an empty prompt cancels.

`m` and `I` ask the same way. `m` merges a branch you name into the
current one, passing `--no-edit` so git cannot stop to open an editor
for the merge message; when you would rather *pick* the branch from a
list, that is `b` `L` then `m` in the
[branch buffer](help:magit-branch-mode), which is where the list already
lives. (Note `b` `m` is a *different* thing — branch rename — so the two
`m`s do not mean the same operation one level apart.) `I` runs
`git init`, its prompt
pre-filled with your working directory — the usual answer, shown before
it happens rather than after.

All four are also ex-commands, which is how you script them or skip the
prompt: `:magit-tag v1.2.0`, `:magit-gitignore target/`,
`:magit-merge feature/x` and `:magit-init ~/src/thing` act immediately,
while the bare form asks exactly as the menu row does — same operation,
two ways in.

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

`C-c g` shows only what it can actually run — a row that does nothing
when pressed is worse than a row that isn't there. Two different
reasons keep a magit entry out:

**No operation behind it yet.** Magit's dispatch also offers patch
(`w`/`W`), worktree (`Z`), notes (`T`), clone (`C`) and more. These are
planned, and each appears the moment its operation exists.

**Here, and it changes with the repo.** `B` (bisect) opens a submenu
whose rows depend on whether a bisect is running. Idle, it offers only
`B` start — which asks for a known-bad revision (seeded `HEAD`, because
"the bug is here now" is why you are starting) and then a known-good
one. Running, it drops `start` and offers `g` good, `b` bad, `k` skip
and `r` reset, all acting on the revision git checked out for you.

The reason for the split is that git *errors* on the marks outside a
bisect and on `start` inside one — those rows would look actionable and
do nothing but log. While a bisect runs, magit-status's headerline
carries `BISECTING 3 left, ~2 steps`, which are git's own numbers from
its own plumbing, so they agree with what `git bisect` prints in a
terminal. Every mark refreshes every open magit buffer, because a
bisect moves HEAD and an open log or diff goes stale with it.

`git bisect run <script>` is not here yet, nor is marking a revision
other than the one checked out.

**Here, and both as buffers.** `o` (submodule management) opens
[`magit-submodule-mode`](help:magit-submodule-mode), listing every
submodule with git's own status marker — `a` add, `u` update, `s` sync,
`d` remove. Magit agrees on the shape here: `magit-list-submodules` is
a buffer there too. Its `p` populate and `r` register are folded into
`u`, which runs the command that subsumes both.

**Here, but as a buffer rather than a menu.** `M` (remote management)
is one deliberate divergence. Magit makes it a transient that renders
`remote.<name>.url` as *variable rows* inside the menu. Lattice has
variable rows now (that is what `C` configure uses), but a menu still
shows one value per row, and remote management is a list of remotes
each with a URL — so a straight port would hide the very thing you
opened it to look at. `M` therefore
opens [`magit-remote-mode`](help:magit-remote-mode), a buffer listing
every remote with its URL, where `a` / `r` / `d` / `u` / `p` act on the
row under the cursor. `M` — like `B` — is a transient key only: both stay unbound as chords
inside magit buffers so vim's middle-of-screen and back-WORD motions
survive, the same reasoning that keeps `V` free. Magit binds both in
its own buffers; it can, because it is not modal.

**`C` clone, with one thing it deliberately does not do.** It asks for
a URL, then where to put it — pre-filled with the directory
`git clone` would have picked, absolute, so you can see where it lands
before it starts. `:magit-clone <url> [<destination>]` is the
scriptable form, and with no destination it derives the same name.

What it does **not** do is switch you to the clone. Magit shows the new
repository's status buffer afterwards; here the magit buffers keep
showing the repository the editor was opened in, and the completion
notification says so. Open the editor in the new directory to work in
it. (This is the same process-wide-working-directory limit that keeps
`Z` worktree unimplemented — one repository per editor session, for
now.)

**`Y` cherries** opens [`magit-cherry-mode`](help:magit-cherry-mode) —
which of your commits are not upstream yet, and which already are under
a *different* SHA. The second half is why it beats
`git log upstream..HEAD`; see that page.

**`"` subtree, and the key is a deviation with a reason.** Magit puts
subtree on `O`. `O` here is the reset submenu — which is
evil-collection-magit's own remap of magit's `X` — so magit's `O` had to
move, and evil-collection already decided where: `"`. We follow it
rather than inventing a third answer. Every row prompts, because a
subtree operation needs a prefix directory and usually a repository and
a ref, none of which a menu can guess.

**`w` patches** holds both halves of the email-patch workflow: `w` apply
(`git am`) and `W` create (`git format-patch`). Magit splits these
across two top-level keys; one submenu holds five rows between them.

Applying stops on a patch that will not apply, exactly as a rebase stops
on `edit` — and then the menu shows only the three ways out (`c`
continue, `s` skip, `a` abort), gated the same way `B` bisect and `T`
notes are. `-3` on the apply line falls back to a three-way merge with
conflict markers instead of refusing; it is opt-in for the same reason
push uses `--force-with-lease` rather than `--force`.

`format-patch` writes into the **repository root**, not the editor's
current directory — a scatter of `.patch` files somewhere unexpected is
tedious to undo.

**And `y` show-refs, for the same reason.** It opens
[`magit-refs-mode`](help:magit-refs-mode): every local branch,
remote-tracking branch and tag, with how far each branch is ahead of or
behind its upstream. That last column is what the buffer is for, and it
is a column — a menu cannot show it. It is a different question from
the [branch list](help:magit-branch-mode), which shows local branches so
you can act on them and never mentions tags or remotes at all.

**Deliberately not coming.** Magit's `Q` runs an arbitrary git or shell
command and shows its output. Lattice has no row for it and will not
get one: [`:terminal`](help:terminal-mode) already gives you a real
shell in the repository, which does everything `Q` does — including the
interactive commands (`rebase -i`, anything that opens an editor or a
pager) that a captured-output version could not handle without extra
machinery. A menu row would be a second, worse way to do something the
editor already does well.

**Implemented, but the menu has no context to aim it at.** Revert,
reset and cherry-pick all work — as `_`, `Os`/`Om`/`Oh` and `A` in any
magit buffer that shows a commit (see
[`magit-core-mode`](help:magit-core-mode)). They act on *the commit
under the cursor*, and `C-c g` opens from anywhere, including buffers
with no commit anywhere in them. Until the menu can either ask you for
a commit or hide the rows outside magit buffers, they stay chords.
Branch *merge* is the same story one level down: it exists as `m`
inside the branch-list buffer because it needs a branch selected, so
`b` `L` then `m` gets you there — and `m` in this menu merges a branch
you type, for when you already know the name.

(Magit's own keys for those differ from ours — it uses `V` for revert
and `X` for reset. Ours follow **evil-collection-magit**, which remaps
them for a modal editor; `magit-core-mode` explains why.)

### The branch submenu (`b`)

```text
┌ Branch ────────────────────────────────────────┐
│  ▸ Checkout                                    │
│    [b]  branch/revision  Anything git can take:│
│                           branch, tag, remote  │
│                           ref or SHA           │
│    [l]  local branch     Pick from your local  │
│                           branches             │
│                                                │
│  ▸ Create                                      │
│    [c]  new branch and checkout                │
│    [n]  new branch       …without checking out │
│                                                │
│  ▸ Do                                          │
│    [s]  spin-off         Branch the unpushed   │
│                           commits, check it out│
│    [S]  spin-out         …staying where you are│
│    [x]  reset            Asks first            │
│    [m]  rename                                 │
│    [k]  delete           Asks first            │
│    [L]  list             Open the branch buffer│
│                                                │
│  ▸ Configure                                   │
│    [C]  rebase on pull   pull.rebase = true    │
└────────────────────────────────────────────────┘
```

**`b` and `l` are not the same operation**, and the difference is
what each one *lists* — both end in `git checkout`:

- **`l`** offers your local branches, and nothing else.
- **`b`** offers everything `git checkout` accepts: local branches,
  remote-tracking refs like `origin/main`, tags, and the recent
  commits. This is the [revision picker](help:picker#the-magit-sources),
  with refs listed before commits.

So `b` is what you reach for to check out `origin/main` or `v1.2.0`;
`l` is the shorter list when you know it is one of yours. Checking out
a commit detaches HEAD, exactly as it would on the command line.

Neither accepts free text. For a revision older than the last 200
commits — or one you want to script — `:magit-checkout <rev>` takes
any revision string git does.

**`c` and `n` differ only in where you end up.** Both pick a base and
then ask for a name; `c` checks the new branch out, `n` leaves you where
you are. `n` is what you want when you are mid-edit and only want to
mark a starting point.

**`x` asks before deleting**, because it is a force delete (`git branch
-D`) — see [`magit-branch-mode`](help:magit-branch-mode) for why that
bar exists. `m` does not ask: renaming discards nothing, and it *refuses*
rather than overwrites if the new name is already taken.

**`L` opens the branch buffer**, which is where per-branch chords live —
including `m` for merge, which needs a branch selected.

**`s` and `S` differ only in where you end up.** Both create a branch
from the commits you have not pushed yet and rewind the current branch
back to its upstream; `s` checks the new branch out, `S` leaves you
where you were. If you have uncommitted changes, `S` behaves like `s`
— staying on a branch that is about to be rewound would destroy them.
With no upstream, or nothing unpushed, the new branch is created and
the old one is left alone.

**`C` shows the current value.** Configure rows render the git-config
setting inline (`pull.rebase = true`), so you can see what it is before
changing it. `…` means the value has not been read yet; press it once
more. Clearing the prompt unsets the key rather than setting it empty.

The keys are magit's own, with **evil-collection-magit**'s remaps
applied where they concern buffer chords. Inside a transient the menu
owns every keystroke, so `x` is magit's reset and `k` is delete —
which is why they moved.

### What each submenu holds

Every key here is magit's own.

**Commit (`c`)** — `c` commit, `a` amend, `e` extend (add staged
changes, keep the message), `w` reword, `A` augment, `f` fixup,
`s` squash, `F` instant fixup, `S` instant squash.

**Reset (`O`)** — `s` soft, `m` mixed, `h` hard (asks), `k` keep,
`i` index, `w` worktree (asks), `f` a file.

**Stash (`z`)** — `z` stash, `i` index, `x` keeping index,
`Z`/`I`/`W` snapshots, `a` apply, `p` pop, `k` drop, `b` branch,
`l`/`v` list and show. Magit's `w` (stash the working tree only,
keeping the index) is **not** here: git has no flag for it, and every
way of approximating it stashes different content than the name
promises.

**Cherry-pick (`A`)** — `A` pick and `a` apply *copy* a commit;
`h` harvest, `d` donate, `n` spinout and `s` spinoff *move* it, removing
it from where it came from. While a cherry-pick is stopped the menu
shows only `A` continue, `s` skip, `a` abort.

**Revert (`_`)** — `V` revert commit, `v` revert changes (staged, not
committed). Gated the same way while a revert is stopped.

**Merge (`m`)** — `m` merge, `e` merge and edit the message, `n` merge
without committing, `s` squash, `a` absorb (merge another branch in and
delete it), `i` merge into (merge *this* branch into another and delete
this one), `p` preview.

**Rebase (`r`)** — `p`/`u`/`e` onto the push target, the upstream, or a
ref you name; `s` a subset; `m` edit a commit, `w` reword a commit,
`k` remove a commit, `f` autosquash, `i` interactively. While a rebase
is stopped: `r` continue, `s` skip, `a` abort.

**Tag (`t`)** — `t` tag, `r` release (annotated), `k` delete,
`p` prune, `C` configure.

**Push (`P`) / pull (`F`) / fetch (`f`)** — `p` the configured target,
`u` the upstream, `e` elsewhere, plus `o`/`r`/`T`/`t`/`a`/`m` where the
operation supports them, and `C` configure.

A menu whose operation is mid-flight (a stopped rebase, cherry-pick,
revert, bisect, `git am`, or notes merge) shows **only the ways out**.
That is deliberate: `--continue` / `--skip` / `--abort` error when
nothing is running, and starting a second operation is exactly what you
must not do while one is stopped.

### Which rows ask, and how

A row that needs you to name something either opens a **picker** or a
**prompt**, and which one is not arbitrary:

> Naming a thing that must already exist → **picker**.
> Naming a thing you are creating → **prompt**.

A prompt for a name that has to exist is a typo waiting to happen: git
reports it long after the keystroke, and the thing you wanted was on a
list the editor could have shown. A picker for a *new* name is worse
than useless — there is nothing to pick. So `Merge branch` lists and
`New branch name` asks, even though both are about branches. `Rename
<remote> to:` stays a prompt for the same reason: the remote was
already picked, and the input is the new name.

Every row that opens a picker, and what each one lists:

| Path | Row | Lists |
|---|---|---|
| `m m` | merge | local branches |
| `m e` | merge and edit message | local branches |
| `m n` | merge, don't commit | local branches |
| `m s` | squash | local branches |
| `m a` | absorb | local branches |
| `m i` | merge into | local branches |
| `r e` | rebase onto elsewhere | local branches |
| `r f` | autosquash | local branches |
| `b x` | reset branch | local branches |
| `b b` | branch/revision | **refs and commits** |
| `b l` | local branch | local branches, checks out directly |
| `b c` / `b n` | create (with / without checkout) | local branches, as the base — then prompts for the new name |
| `b m` | rename | local branches — then prompts for the new name |
| `b k` | delete | local branches — asks before deleting |
| `t k` | delete tag | tags |
| `t p` | prune tags | **remotes** — the row's label says tags, the operation takes a remote |
| `T T` / `T r` | edit / remove a note | recent commits, when the cursor is not on one |
| `T m` | merge notes ref | every ref |
| `A` / `_` / `O…` | cherry-pick / revert / reset | recent commits, when the cursor is not on one |
| `C-c f v` | view this file at a revision | **refs and commits** |
| `C-c f ,c` | checkout this file from a revision | **refs and commits** |
| `C-c f M` | merged | recent commits, when the cursor is not on one |

The rows that stay **prompts**, because each names something new: tag
name, remote name, any URL (remote, submodule, clone), clone
destination, `Path for <url>`, every rename target, the ignore
pattern, the new branch name, the stash message, the author, and the
context-line count.

See [`picker`](help:picker#the-magit-sources) for the sources
themselves, and for the `:picker <source> <ex-command>` form that
reaches any of them directly.

### How submenus work (mechanism)

Pressing a submenu's key pushes the current transient onto a stack and
opens the submenu.

**`Esc` and `BS` both unwind one level.** From `C-c g` `b` `L`, the
first `Esc` puts you back in the branch menu and the second back at the
dispatch; a third closes it. Only at the top, with nothing left to
unwind, does `Esc` dismiss.

That is deliberate: exiting all the way out on the first press punishes
the ordinary mistake — you opened `b` and meant `z`, and now you are
back in the buffer instead of the menu you were in.

A **half-typed multi-key row** is undone first. If you have typed `,`
of `,k` and press `Esc`, it forgets the `,` and leaves the menu open;
the next `Esc` goes back a level. Vim gives `<Esc>` the same precedence
over a partial chord.

`q` still dismisses outright from anywhere.

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
│  ▸ File                                        │
│    [,x] untrack         Stop tracking, keeping │
│                          the file on disk      │
│    [,r] rename          Rename this file       │
│    [,k] delete          Delete this file       │
│                          (asks first)          │
│    [,c] checkout        Replace this file with │
│                          its content at a      │
│                          revision (asks, then  │
│                          confirms)             │
│                                                │
│  ▸ Inspect                                     │
│    [d]  diff            Show diff for this file│
│    [l]  log             Show commit history    │
│                          for this file         │
│    [b]  blame           Blame this file        │
│    [M]  merged          Show the merge commit  │
│                          that brought a commit │
│                          into HEAD             │
│                                                │
│  ▸ More actions                                │
│    [e]  edit line       Start a rebase that    │
│                          stops on the commit   │
│                          that wrote this line  │
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
| `b` | Toggles [blame annotations](help:magit-blame-mode) on the file itself |
| `v` | Opens this file [as it was at a revision](help:magit-file-revision-mode) you type |
| `V` | From a file-at-revision, back to the **live** file at the same line |
| `f` | Reverse blame — only from a blob buffer, see below |
| `M` | Shows the merge commit that brought a commit into `HEAD` — see below |
| `e` | Starts a rebase that stops on the commit that wrote the line at the cursor — see below |
| `,x` | `git rm --cached` — stop tracking, **file stays on disk** |
| `,r` | `git mv` — asks for the new name, pre-filled with the current one |
| `,k` | `git rm` — **destructive**, asks `Delete <path>?` first |
| `,c` | `git checkout <rev> -- <file>` — opens the [revision picker](help:picker#the-magit-sources), then **confirms**: this overwrites the file |

`x`, `,k` and `,c` are the destructive items, and the ones that confirm
first — the same `y`/`n` dialog magit-status's own `x` uses. `s`/`u`
report optimistically ("magit: staged <path>") and log the real
outcome; they don't block on git.

#### `v` — this file at a revision

Opens a **picker of revisions** and shows the file as it was at the one
you choose, in a [blob buffer](help:magit-file-revision-mode). `gj` /
`gk` walk from there to the next and previous revisions that touched
it.

The list is branches, remote-tracking refs and tags **first**, then the
last 200 commits. That ordering is the point: reaching for another
branch is the common ask, and `origin/main` is something you recognise
where a sha is something you would have to read to identify. It also
makes *view this file as it is on another branch* reachable at all — a
file living on `origin/main` is not in your current branch's history,
so a list of commits could never surface it however long it was.

Commit rows show `abbrev subject` and pass the full sha. Anything older
than the last 200 commits is reachable by naming it directly:
`:magit-find-file <rev> <path>`.

Magit prompts for a revision *and* a file. Here only the revision is
asked, because `C-c f` already means "the file I am visiting" — asking
for something the menu already knows would be a question with one
answer. For a file you are **not** visiting, `:magit-find-file <rev>
<path>` takes both.

#### `V` — back to the live file

`gj` / `gk` walk a blob's history; `V` walks back out. From
`*magit:file:<rev>:<path>*` it opens the working-tree copy, landing on
the same line you were reading.

Same line is approximate on purpose: line numbers drift between
revisions, so this puts you roughly where you were rather than
promising the matching line. Landing at the top of a file you were
reading the middle of is the worse answer, and a diff-based line map is
a different feature.

If the file existed at that revision and no longer does, it says so
rather than opening an empty buffer named after a deleted path.

#### `f` — reverse blame

Ordinary blame answers "which commit added this line". `f` answers the
opposite: **for each line, the last commit in which it still existed**.
Lines that name something other than `HEAD` are the ones that have
since gone away, and the commit named is the last one they survived in.

It needs a revision to walk forward from, and it shows the file *as it
was at that revision* — so it only works from a buffer that is already
showing one: a `*magit:file:<rev>:<path>*` blob buffer, which `v` above
opens directly, or `<CR>` on a log entry / a commit's file list, walked
with `gj` / `gk`. Press `f` anywhere else and it says so rather than
guessing.
The index (`*magit:file:staged:…*`) is refused too — it is not a
commit, so there is no range.

`p` walks the starting revision back one commit and re-blames in
place.

The scriptable form is `:magit-blame-reverse <rev> <path>`, which takes
both halves explicitly and so works from anywhere. Both arguments are
required: there is no sensible default revision, since `HEAD..HEAD` is
empty and would report every line as still present.

#### `M` — the merge that brought a commit in

"This commit is on my branch. How did it get here?" `M` answers with
the **merge commit** that brought it in — the pull request that landed
it, in practice — and opens that merge in a
[revision buffer](help:magit-revision-mode).

The commit you name is the question, not the answer: the buffer shows a
different commit from the one you picked. From a magit buffer with a
commit under the cursor it uses that one; from an ordinary file buffer
there is no commit at the cursor, so it opens the commit picker first.

A commit made straight onto the branch you are on was never merged in.
That is the ordinary case for most of a repository's history, not a
failure, and the buffer says so in those words rather than coming up
empty.

The scriptable form is `:magit-log-merged <commit>`.

#### `e` — amend the commit that wrote this line

You find a line that's wrong, and the fix belongs in the commit that
introduced it rather than in a new "fix typo" commit on top. `e` blames
the line at the cursor, then opens a
[rebase todo](help:magit-rebase-mode) that rebases onto that commit's
parent with **that commit marked `edit`**:

```
pick a1b2c3d earlier commit
edit e4f5g6h the commit that wrote your line
pick i7j8k9l later commit
# e4f5g6h is marked `edit` — it is the commit that wrote that line.
# The rebase will stop there; amend, then `:magit-rebase-continue`.
```

`C-c C-c` runs it. The rebase stops on that commit with your working
tree at that point in history; fix the line, `git add`, `git commit
--amend`, then `:magit-rebase-continue` replays the rest.

The row it marks is found by **commit**, not by position: `--reverse`
orders the todo by date, so a merge in range can put a side branch's
older commits above the one you asked about. Marking the top row would
stop the rebase somewhere you never named, and the result would look
perfectly ordinary.

If the line isn't committed yet, there is nothing to amend and it says
so. If the commit is the repository's first, it rebases with `--root`
rather than failing on a parent that doesn't exist.

**This rewrites history.** Everything after the amended commit gets new
shas, so don't do it to commits you have already pushed and shared.

##### Getting out of a stopped rebase

A rebase that stops is a state you have to leave deliberately. Three
commands do it, from anywhere:

| Command | What it does |
|---|---|
| `:magit-rebase-continue` | Resume — after amending, or after resolving conflicts |
| `:magit-rebase-skip` | Drop the commit it stopped on and carry on |
| `:magit-rebase-abort` | Abandon the whole rebase, putting the branch back where it started |

`C-c C-k` in a rebase todo buffer also aborts, but that buffer is gone
once the rebase is actually running — which is why these exist.

Commit messages are accepted unchanged during a rebase: there is no
message-editing UI yet, so `reword` keeps the original text and
`--continue` doesn't stop to ask.

**The `,` prefix is deliberate**, and magit's own. Those four change
what the file *is* rather than what is staged of it, so they take an
extra keystroke. Two of them are gentler than they look: `,x` untrack
leaves the file on disk (only the index forgets it, and `s` puts it
back), which is why it doesn't ask; and `,k` delete runs a plain
`git rm`, so git itself refuses when the file has uncommitted changes —
the confirmation is the second line of defence, not the only one.

`,r` rename pre-fills the prompt with the current path, so renaming
within a directory is an edit rather than a retype. Submitting it
unchanged cancels.

`,c` checkout asks for a revision, pre-filled with `HEAD` — "put back
what I committed" is the common case. Then it confirms, naming both:
`Checkout src/main.rs from HEAD, discarding its uncommitted changes?`
Unlike `,k`, git will *not* refuse this one; it overwrites the file
whatever state it is in, and keeps no copy of what it replaced. That is
why the revision prompt and the confirmation are both there, and why
answering `n` runs nothing at all.

**No "which file?" prompt.** This is the one deliberate deviation from
Emacs magit, which asks you to confirm the file even though the default
is always the one you're visiting. Here the visited file is simply the
target. When you want a different one, that is what
`:magit-other-file-dispatch` is for.

### `:magit-other-file-dispatch` — a file you aren't visiting

A stand-alone command, tied to no buffer and **bound to no chord**.
Invoke it by name; bind it yourself if you'd rather have magit's
always-ask behaviour on a key.

```
┌─ File dispatch (other file) ───────────────────┐
│                                                │
│  ▸ Target                                      │
│    [=f] file            Repo-relative path to  │
│                          act on                │
│                                                │
│  ▸ Stage                                       │
│    [s]  stage           Stage the target file  │
│    [u]  unstage         Unstage the target file│
│    [x]  discard         Discard the target     │
│                          file's changes        │
│                          (asks first)          │
│                                                │
│  ▸ Inspect                                     │
│    [d]  diff            Show the target's diff │
│    [l]  log             Show the target's      │
│                          history               │
│    [b]  blame           Blame the target file  │
│                                                │
│  target: src/main.rs                           │
│  =f set target  q dismiss                      │
└────────────────────────────────────────────────┘
```

Press `=f`, type a repo-relative path, and the rows act on it. The
target is always shown on the preview line — including when it is
unset, where it says so rather than leaving you to guess. With no
target set the rows fall back to the visited file, so an unset menu
behaves exactly like `C-c f`.

`x` asks first, as it does everywhere, and the question names the target
you set — `Discard changes to src/main.rs?`. What you confirm is what
happens: the confirmation carries the file with it rather than
re-reading where your cursor happens to be when you answer.

(That was not always true. An earlier revision left `x` out of this menu
entirely, because a confirmation dialog is itself a transient and
replaced the target you had set — the follow-up step would fall back to
the visited file and discard something the prompt never named.)

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
