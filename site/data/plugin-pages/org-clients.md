One org file per client, built on [org](@/plugins/org.md) and developed in its
own repository:
[`dhruvasagar/lattice-org-clients-plugin`](https://github.com/dhruvasagar/lattice-org-clients-plugin).
For anyone who keeps several clients' work in org: consultants, freelancers,
agencies, a fractional CTO.

A client is a `.org` file in one folder:

```
~/org/clients/        org-clients.directory
  acme.org            one client, one file
  globex.org
  archive/            finished clients; never scanned, never offered
```

There is no client list to keep and no "current client" to remember to
change: the buffer you are in, or the clock you are running, says which
client you mean.

## What you get

| | |
|---|---|
| **Agenda** | org's ordinary agenda includes every client's tasks, beside your own |
| **Capture** | org's capture menu gains a `Client` group (`e`): Task, Waiting on…, Meeting notes, Decision and Note, filed into the client you pick |
| **Dashboard** | org's agenda dispatcher gains a `Client` view (`e`): one client's overdue, this week, waiting, next and everything else |
| **The menu** | `<leader>e`, from any buffer: find a client, its dashboard, capture, onboard a new one, archive a finished one |

Everything is added to org's options rather than written into them. Your own
templates and agendas come first, a key you already use keeps meaning yours,
and `:describe-option org.capture-templates` shows what this plugin added.

## Getting it

It needs lattice 0.10.6 or later and the org plugin. In
`~/.config/lattice/lattice.toml`:

```toml
[plugins.org]
release = "dhruvasagar/lattice-org-plugin"

[plugins.org-clients]
release = "dhruvasagar/lattice-org-clients-plugin"
```

then `:reload-config`, or restart lattice. The order does not matter: the
plugin declares that it requires org, so until org is loaded it waits, and
`:plugins` says what it is waiting for. The
[README](https://github.com/dhruvasagar/lattice-org-clients-plugin#installing)
covers `init.rs` and building from source.

## A plugin built on a plugin

org-clients has no agenda, capture or dashboard code of its own. It
contributes to org's options (agenda files, capture templates, named agendas),
calls the two commands org exports (`org-agenda-command` and
`org-capture-menu`), and declares `requires = ["org"]` so it loads after org
does. Nothing in lattice or in org names it. If you are writing a plugin that
extends another one, it is the small example to read, where org is the large
one.

## Documentation

- `:help org-clients` inside the editor: the menu, the folder, capture, the
  dashboard and every option
- the [README](https://github.com/dhruvasagar/lattice-org-clients-plugin#readme)
  in the plugin repository
- [Plugins](@/docs/config/plugins.md): installing from `lattice.toml` and how
  plugins load
