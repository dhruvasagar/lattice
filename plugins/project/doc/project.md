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
| `<leader>pd` | `<C-x>pd` | Browse **this** project's tree |

The distinction matters: `pf` acts on the project you are already in and shows
no project picker, while `pp` asks which project first. Both are wanted — the
first is the everyday verb and the second is what the plugin exists for.

> `<C-x>p` stays bound even with `:set noemacs-keys`. A plugin registers its
> keymap once at load and there is no unregister, so the gate the design wanted
> is not buildable today.

## Commands

| Command | Does |
|---|---|
| `:project-switch` | The project picker |
| `:project-find-file [dir]` | Find a file in a project |
| `:project-dired [dir]` | Browse a project's tree |
| `:project-grep [dir]` | Live grep in a project |
| `:project-shell [dir]` | A shell in a project |
| `:project-remember [dir]` | Add a project to the picker |
| `:project-forget [dir]` | Remove one |

With no argument each acts on the current buffer's project.

## Where the list comes from

Projects are **remembered as you visit them** — open a file in one and it joins
the list, most-recently-visited first. That is `project.el`'s own model.

It has one hole, and `:project-remember` fills it: a project you have *never*
opened a file in cannot be remembered by visiting it, which is precisely the
case "open a file in project B" describes. `:project-remember ~/src/thing` seeds
it without opening anything.

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
  { key = "d", label = "Browse tree", command = "project-dired" },
  { key = "g", label = "Find regexp", command = "project-grep" },
  { key = "s", label = "Shell",       command = "project-shell" },
  { key = "v", label = "Magit",       command = "magit-status" },
]
```

**Adding your own is one line.** The whole contract is: *a project command is an
ex-command whose first argument is a project root.* Any command that takes one
can be a row — which is why the Magit row names `magit-status` directly rather
than going through a wrapper.
