---
summary: "picker-magit: the pickers magit opens when a command needs you to name a branch, commit, tag, ref, remote, revision or stash."
related: [magit]
---

# The magit pickers

When a [magit](help:magit) command needs you to *name* something that
already exists — the branch to merge, the commit to revert, the tag to
delete — it opens one of these pickers instead of a prompt. Twelve
sources, one set of keys; this page covers them all.

You rarely open them yourself. A menu row does (`<C-c>g` → `m` → `m`
picks the branch to merge), or a command run without the argument it
needs (`:magit-revert` with no commit under the cursor). Every one is
also reachable as `:picker <source>`.

(`<C-h>` inside the transient **menus** that lead here — the `<C-c>g`
dispatch and its submenus — is not this page; menus do not have their
own help pages yet.)

---

## Keys in these pickers

| Key | Here |
|---|---|
| *(type)* | Filter, fuzzily — a branch by name, a commit by sha **or subject** |
| `<CR>` | Run the command on the pick (see *What `<CR>` runs* below) |
| `<C-n>` / `<C-p>`, `<Down>` / `<Up>`, `<Tab>` / `<S-Tab>` | Move the selection |
| `<BS>` / `<C-w>` | Delete a character / the previous word of the query |
| `<C-r>` | Append something from your yank history to the query — paste a sha you copied |
| `<Esc>` / `<C-c>` | Back out; nothing runs |
| `<C-h>` | This page |

**`<C-d>` deliberately does nothing**, even on a branch or tag list.
Deleting one is a git operation with its own confirmation and its own
failure modes — `k` in the branch or tag menu — not a list-tidying
keystroke, and a second, unconfirmed path to `git branch -D` would be a
trap. `<C-s>` / `<C-v>` / `<C-t>`, `<C-q>` and `<C-l>` do nothing here
either: a pick names a git object, not a place to open.

**Commands that throw work away still ask.** A pick that resets, drops a
stash, checks a file out over your changes or deletes a branch runs a
command that confirms first ("Delete branch topic?"); the picker is how
you name the target, not a way around the question.

---

## The sources

All of them list **the repository of the buffer you opened them from**
— the prompt names it — so two checkouts open side by side each get
their own branches.

**Seven take the command to run as an argument** —
`:picker magit-branch magit-merge` means "pick a branch, then
`:magit-merge <it>`":

| Source | Lists |
|---|---|
| `magit-branch` | Local branches, in git's order |
| `magit-commit` | The last **200** commits, newest first, as `<short sha> <subject>` |
| `magit-revision` | Branches, remote-tracking branches and tags **first**, then the same 200 commits |
| `magit-ref` | Every ref: `refs/heads`, then `refs/remotes`, then `refs/tags` |
| `magit-tag` | Tags |
| `magit-remote` | Configured remotes — `origin`, not `origin/main` |
| `magit-stash-pick` | Stashes, as `stash@{N}` and the stash's message |

**Five are complete branch actions** and take nothing:

| Source | Reached by | Then |
|---|---|---|
| `magit-branch-checkout-pick` | branch menu `l` | checks the branch out |
| `magit-branch-pick-base` | branch menu `c` | asks the **new** branch's name, from that base |
| `magit-branch-create-no-checkout-pick` | branch menu `n` | the same, without checking it out |
| `magit-branch-rename-pick` | branch menu `m` | asks the new name, pre-filled with the old one |
| `magit-branch-delete-pick` | branch menu `k` | asks "Delete branch …?", then deletes |

Why a picker for the base but a prompt for the name: *naming a thing
that must already exist* is a pick; *naming a thing you are creating*
is a prompt — there is nothing to pick yet.

---

## What `<CR>` runs

For the seven argument-taking sources, the pick is **appended** to the
command you gave — or substituted for a `{}` placeholder if there is
one, which is how `magit-find-file {} src/main.rs` puts the revision
before the path.

**A commit row shows an abbreviated sha but passes the full one.** An
abbreviation can become ambiguous as history grows, and git resolves
ambiguity by refusing.

**`magit-revision` is not `magit-commit`.** A commit list is *this
branch's* history, which cannot answer "show me this file as it is on
`origin/main`". Commands that want any revision (find-file, file
checkout, checkout) use `magit-revision`; commands that genuinely want a
commit (cherry-pick, revert, reset, fixup) use `magit-commit`, because
offering a branch there would be offering the wrong noun.

---

## Preview

Only one case previews: `magit-revision` opened for
`magit-find-file` (`<C-c>f v`). Moving the selection shows the file as
it is at that revision, after the selection has rested for 150 ms.
`:set magit.revision-preview=false` turns it off.

Nothing else previews — checking a branch out just to show it would
touch your working tree.

---

## See also

- [`magit`](help:magit) — the whole magit surface.
- [`magit-transient`](help:magit-transient) — the menus these pickers
  are opened from.
- [`picker`](help:picker) — keys and options shared by every picker.
