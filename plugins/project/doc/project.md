# project — choose the project, then the verb

Every project-aware surface in lattice roots itself at the buffer you are
standing in: `:files`, `:terminal`, `:search`, magit. That is right until it is
exactly wrong — a file open in project A, wanting to open one in project B —
where the only route was `:e` with a hand-typed path.

This plugin adds the missing verb: **choose the project first**.

## Keys

Both prefixes are bound, so `project.el` muscle memory works either way.

| `<leader>p` | `<C-x>p` | Does |
|---|---|---|
| `<leader>pp` | `<C-x>pp` | Pick a project, then pick what to do in it |
| `<leader>pf` | `<C-x>pf` | Find a file in **this** project |
| `<leader>pb` | `<C-x>pb` | Switch to an open buffer in **this** project |
| `<leader>pd` | `<C-x>pd` | Browse **this** project's tree |
| `<leader>pg` | `<C-x>pg` | Live grep in **this** project |
| `<leader>ps` | `<C-x>ps` | A shell in **this** project |
| `<leader>pm` | `<C-x>pm` | Magit status for **this** project |

The distinction matters: `pf` acts on the project you are already in and shows
no project picker, while `pp` asks which project first. Both are wanted — the
first is the everyday verb and the second is what the plugin exists for.

`pb` is the same distinction applied to buffers. `:b` lists every buffer you
have open across every checkout, which is right for `:b` and wrong when you are
inside one project and want the handful of files that belong to it. It lists
buffers whose file lives under the project root; a buffer with no file at all
(`*messages*`, a magit status, an oil listing) belongs to no project and is not
listed.

> `<C-x>p` stays bound even with `:set noemacs-keys`. A plugin registers its
> keymap once at load and there is no unregister, so the gate the design wanted
> is not buildable today.

## Commands

| Command | Does |
|---|---|
| `:project-switch` | The project picker |
| `:project-find-file [dir]` | Find a file in a project |
| `:project-buffers [dir]` | Switch to an open buffer in a project |
| `:project-dired [dir]` | Browse a project's tree |
| `:project-grep [dir]` | Live grep in a project |
| `:project-shell [dir]` | A shell in a project |
| `:project-remember [dir]` | Add a project to the picker |
| `:project-forget [dir]` | Remove one |
| `:project-choose-dir` | Browse the filesystem for a project |

With no argument each acts on the current buffer's project.

`:project-remember <Tab>` completes directory names, and `<C-x><C-o>` on the
argument opens the same directory browser `:project-choose-dir` does.

## Removing a project

`<C-d>` in the project picker forgets the highlighted entry — the list
re-draws and the picker stays open, so you can clear out several in a row.
`:project-forget [dir]` does the same from the `:` line.

**Nothing is deleted from disk.** Forgetting is just "stop listing this one",
and it is trivially undone: open a file in that project again and it comes
back. Deleting an actual directory is oil's job (`:project-dired`) or the file
tree's.

## Where the list comes from

Projects are **remembered as you visit them** — open a file in one and it joins
the list, most-recently-visited first. That is `project.el`'s own model.

It has one hole: a project you have *never* opened a file in cannot be
remembered by visiting it — which is precisely the case "open a file in project
B" describes. Two things fill it.

**`… (choose a dir)`**, the last row of the project picker. It is always there,
including on a fresh install where it is the only row, and it opens a directory
browser:

```
:project-switch
  lattice
  lattice-org-plugin
▸ … (choose a dir)                     <CR>
      ↓
  Choose a directory:  ~/src/dh▊
▸ ../                                 <CR>  go up
  ~/src/dhruvasagar/                   <Tab> go in
  ~/src/dharma/                        <C-h> go back up
                                       <CR>  choose this one
```

The prompt shows the directory you are in. Typing filters as you go, `<Tab>`
descends into the highlighted directory — keep pressing it to go as deep as you
like — and `<C-h>` climbs back out. `<C-l>` is `<Tab>`'s peer if you have
ranger / lf muscle memory; `<C-n>` / `<C-p>` and the arrows move the selection.
`<CR>` takes the one you are on: it is remembered, and its switch menu opens straight
away — the same place you would have landed had it been in the list all along.

`../` is the exception, and it reads the way it looks: `<CR>` on it **goes up**
rather than choosing the parent, the same as `<C-h>`.

**Any folder can be a project.** It does not have to be a git repo or carry any
other marker — a directory of notes or a scratch tree is a project if you want
to work in it, and every verb here works rooted at a plain directory.

Choosing anywhere *inside* a project picks the project, so you can stop at
`~/src/thing/src` and still get `~/src/thing`.

**`:project-remember ~/src/thing`** does the same seeding from the command
line, without opening anything.

Nothing is pruned automatically. The plugin holds no filesystem capability, so
it cannot tell a deleted checkout from one on an unmounted volume — a project
whose directory has gone shows up as an empty file picker, and
`:project-forget` removes it.

## The switch menu

`<leader>pp` picks a project, then shows a menu of what to do in it. The rows
come from `project.switch-commands`:

```toml
[project]
switch-commands = [
  { key = "f", label = "Find file",   command = "project-find-file" },
  { key = "b", label = "Buffers",     command = "project-buffers" },
  { key = "d", label = "Browse tree", command = "project-dired" },
  { key = "g", label = "Find regexp", command = "project-grep" },
  { key = "s", label = "Shell",       command = "project-shell" },
  { key = "m", label = "Magit",       command = "magit-status" },
]
```

**Adding your own is one line.** The whole contract is: *a project command is an
ex-command whose first argument is a project root.* Any command that takes one
can be a row — which is why the Magit row names `magit-status` directly rather
than going through a wrapper.
