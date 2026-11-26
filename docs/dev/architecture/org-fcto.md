# org-fcto — client engagements on org

**Status: design, decided 2026-11-27.** Sequencing and status:
[`../operations/slice-plans/org-fcto.md`](../operations/slice-plans/org-fcto.md).
The host mechanism it needs, plugins extending plugins, is its own fragment:
[`plugin-extension.md`](plugin-extension.md).

## The ask

A port of the emacs package `org-fractional-cto` as a separate plugin,
`lattice-org-fcto-plugin`, that loads after org and extends its agenda and
capture. It is also the first plugin built on another plugin, so it
establishes how that is done.

The brief was rethought rather than translated. Dhruva's words set the bar:

> simplicity is key for a good user experience here, the paramount goal here
> is to reduce cognitive load, enable easy and quick context switching,
> quickly referencing information pertinent to a project / client

Every feature below is judged by that. The emacs package carries 25 capture
sections, 30 templates, a global active client and a config entry per
client. Those are the load this design removes.

## 1. Constraints from the brief

- **Capture works from anywhere.** You note a decision while in a client's
  code repo, or start meeting notes from whatever buffer is open. The buffer
  cannot be relied on to say which client a note is for.
- **No global state.** There is no "active client" to set and forget to
  unset.
- **Clients are added at runtime.** An onboarding command adds one; nothing
  in configuration names a client.
- **The ordinary agenda still plans everything.** Important work for every
  client shows in your normal agenda, beside your own. The per-client
  dashboard is an additional view, not where client work goes to hide.
- **Your capture menu stays yours.** Client capture is one sub-menu inside
  it, not a replacement.

## 2. The model: one file per client

```
~/org/                     org.directory
  clients/                 org-fcto.directory (default: <org.directory>/clients)
    acme.org               one client, one file
    globex.org
    archive/               ended engagements; never scanned, never offered
      initech.org
```

A client is a file. Its name is the file stem, which org already uses as the
agenda CATEGORY when the file sets none. So every agenda row of a client
shows the client in its category column, with nothing configured.

**Why a file and not a roam node or a heading in a shared file.** A file is
the unit every org feature already scopes by: agenda `file:` filters, the
category column, capture targets, the clock's location, `gf`, oil. A client
as a heading would need tag or property filters everywhere a file filter
works for free. A roam node adds an index the agenda does not read. The
file is the primitive with the most of org already pointed at it.

**Archive is a subfolder because org's directory scan is not recursive.**
An `agenda-files` directory entry means the `.org` files directly in it.
So `clients/archive/` drops out of the agenda and out of every chooser with
no exclusion rule. Archiving is a file move, and reversing it is a move back.

Stages beyond "active" and "archived" (prospect, paused) were cut. A stage is
a concept to hold, and a tag in the file (`#+FILETAGS: :prospect:`) expresses
it the day someone needs it, through the filters that already exist.

### The client file

Onboarding writes this skeleton and opens it:

```org
#+TITLE: Acme Corp
#+FILETAGS: :client:

* Context
* Meetings
* Tasks
* Decisions
* Notes
```

- **One file holds everything about the client.** Context (who, contract,
  stack, links), the meeting log, tasks, decisions and notes are headings,
  so referencing anything about a client is opening one file and folding.
- **`:client:` is a file tag, and org seeds tag inheritance from
  `#+FILETAGS:`.** Every heading in the file matches `+client`, which is what
  §4 uses to keep undated client tasks out of the planning agenda.
- **No `#+TODO:` line.** The file uses the user's own `org.todo-keywords`.
  Org honours `#+TODO:` only in export today, and a per-file vocabulary is a
  second one to learn anyway. "Waiting on someone" is a tag, `:waiting:`,
  which needs no keyword to exist.
- **No `#+CATEGORY:`.** The stem is the category; a rename of the file is the
  rename of the client.

## 3. Choosing the client: a guess you can see

Each capture or dashboard needs a client, and §1 rules out both reading it
silently from the buffer and holding one globally. The answer is a **guess,
always shown, changed with one key**:

1. **The buffer**, if it visits a file in the clients folder.
2. **The clock**, if it is running in a heading in a file in the clients
   folder. You clock in to a meeting or task; the next capture, made from
   your code repo, lands with that client.
3. **Otherwise no guess**, and a picker of the clients folder opens when the
   choice is needed.

The guess is computed fresh every time the menu opens. It is not state: it
is a reading of the buffer and the clock, both of which you can see.

This is a generic org primitive, a **chooser**: "pick one `.org` file
directly in this folder, guessing from the buffer and the clock". It is
declared with `choose = "<dir>"`, and the same primitive serves capture
(§5) and the agenda (§6). org-fcto only declares where its folder is.

## 4. The ordinary agenda includes every client

org-fcto **contributes** its folder to `org.agenda-files`
([`plugin-extension.md`](plugin-extension.md) §3). Your own value is not
replaced. If you never set `agenda-files`, the project root is still scanned
and the clients folder is scanned beside it. Onboarding a client makes its
file part of the next scan, because the folder, not the file, is what was
contributed.

Once scanned, the default agenda sections already pick out what matters for
planning:

| Section | Client rows it shows |
|---|---|
| Overdue | every overdue client task |
| Agenda (the span) | meetings, deadlines, scheduled work |
| Priority A | every `[#A]` client task, dated or not |
| Unscheduled | every undated client task |

So the rule is: **a client task reaches your planning agenda when it has a
date or is priority A.** The rest belongs to the client's dashboard.

The Unscheduled row is the one that can flood. org-fcto does **not** edit
`org.agenda-sections` to fix it: that option is your planning view, and a
plugin reshaping it silently is cognitive load of exactly the kind this
design removes. Its documentation gives the one line that does it,
`match = "-client"` on the Unscheduled section, which works because
`:client:` is inherited from `#+FILETAGS:` (§2).

## 5. Capture: one row in your menu, a sub-menu beneath it

`<leader>oc` keeps every template you configured. org-fcto contributes one
group row:

```
Capture                         Engagement · Client: Acme (clocked in) · c change
  t  Todo                         t  Task
  j  Journal                      w  Waiting on…
  e  Engagement…        ──▶       m  Meeting notes   (clocks in)
                                  d  Decision
                                  n  Note
```

| Key | Files under | Body |
|---|---|---|
| `t` Task | Tasks | `* TODO %?` |
| `w` Waiting on… | Tasks | `* TODO %^{What} :waiting:` with a `WHO` property from `%^{Who}` |
| `m` Meeting notes | Meetings | `* %^{Meeting}` with an active timestamp, clocked in |
| `d` Decision | Decisions | `* %^{Decision}` with context / options / outcome stubs |
| `n` Note | Notes | `* %^{Title}` |

Five templates instead of thirty, because the file already says which client
it is, and the heading says which kind of note. An ADR is a Decision; an idea
is a Note; a call is a Meeting.

**What org gains to make this work:**

- **Group rows, emacs' convention.** A template with a `key` and a
  `description` and no `target` is a group. Templates whose keys extend its
  key (`et`, `ew`) leave the top menu and appear in the group's sub-menu,
  under their remaining key. This is how `org-capture-templates` has always
  written prefixes (`("e" "Engagement")`), so an emacs config with prefix
  rows ports unchanged. The sub-menu is a drill-down: the group row opens the
  capture menu again with the group as its argument, which the transient seam
  already supports; no host change.
- **`choose` on a group.** `{ key = "e", description = "Engagement", choose =
  "~/org/clients" }`. A member template whose target omits `file` takes the
  group's chosen file. The sub-menu's title shows the guess and where it came
  from; `c` opens the chooser picker and reopens the sub-menu with its
  answer. With no guess, firing a template opens the picker first.
- **Contributions to `org.capture-templates`**, appended after your own.
  Duplicate keys already resolve first-wins, so yours always win. The group
  key is `org-fcto.key` (default `e`) so a collision is fixed in one option.

## 6. The dashboard is an agenda command

org-fcto contributes one entry to `org.agenda-custom-commands`:

```toml
[[command]]
key = "e"
description = "Engagement"
choose = "~/org/clients"

  [[command.section]]
  title = "Overdue"
  when = "overdue"
  todo-only = true

  [[command.section]]
  title = "This week"
  when = "days"
  days = 7

  [[command.section]]
  title = "Waiting on others"
  when = "any"
  match = "+waiting"

  [[command.section]]
  title = "Next"
  when = "any"
  min-priority = "A"

  [[command.section]]
  title = "Everything else"
  when = "undated"
  todo-only = true
```

`choose` on a command makes the dispatcher row open the chooser picker with
the guess first. Accepting it opens the command's sections filtered to that
file (`file:acme.org`, a filter the agenda already has). Inside the view,
`sf` re-filters to another file as it does in any agenda, so switching
clients needs no new key.

**What org gains:** `choose` on a custom command, and `org-agenda-command`
accepting filter terms after the key (`["e", "file:acme.org"]`), which the
scan-args grammar already parses; today the action passes only the key.

## 7. What org-fcto itself is

A small plugin. Its mode, `org-fcto-mode`, is global and owns:

- **`<leader>e`**, one menu, so there is one key to remember:

  ```
  Engagement · Client: Acme (from buffer)
    f  find client          a  dashboard
    c  capture…             o  onboard a client
    A  archive this client
  ```

  `a` and `c` hand over to org's agenda command and capture group (§5–6), so
  the guess and the chooser behave identically wherever you start. Org
  exports the commands they name (`exported_commands`,
  [`plugin-extension.md`](plugin-extension.md) §4).
- **`:org-fcto-onboard`.** Asks for the client's name, writes
  `clients/<slug>.org` from the skeleton, opens it. Refuses an existing slug
  and names the file.
- **`:org-fcto-find`.** A picker of the clients folder; opens the file.
- **`:org-fcto-archive`.** Moves the client file (the guessed one, confirmed)
  into `clients/archive/`. Its tasks leave every agenda at the next scan.
- **The contributions** of §4–6, made at activation and withdrawn by the host
  when the plugin unloads.
- **Options:** `org-fcto.directory` (where clients live), `org-fcto.key` (the
  group key in org's menus).

It holds no client list, no active client and no per-client configuration.

## 8. Rejected

- **A global active client** (the emacs design). It is state the user must
  remember to change, and a wrong one files notes silently into the wrong
  client. The guess (§3) costs no keystroke when right and is visible when
  wrong.
- **Inferring the client from the buffer alone.** Wrong from a code repo or
  a scratch buffer, which is where much capture happens.
- **A project-to-client mapping** (repo path → client). It would make the
  buffer guess work from code repos, at the cost of configuration per client,
  which §1 rules out. Deferred: the clock covers the case, and a mapping can
  be added as a third guess source later without changing anything here.
- **org-fcto setting `org.agenda-files` or `org.agenda-sections` with
  `set-option`.** That replaces the user's value, and is a plugin silently
  editing the user's configuration. Contributions (§4) add beside it, show in
  `:describe-option`, and vanish when the plugin does.
- **Recursive `agenda-files` entries** (`~/org/**`). One line, but it sweeps
  in `archive/` and everything else under `~/org`; a precise rule traded for
  a broad one.
- **One file per meeting / a roam ledger.** More files to navigate when the
  question is "what's going on with Acme". Meetings are headings in the
  client file; roam stays available for notes that span clients.
- **A cross-file heading picker** (`f/`, jump to any heading in any client).
  Deferred: `<leader>e f` then org's in-file navigation covers the common
  case, and the refile target collector is the natural base when it is
  wanted.
- **Timesheets, invoicing, an AI profile per client.** Cut for now. The clock
  already records time per heading, so a per-client clock report is a later
  filter over data that exists, not a new store.

## 9. Paramount goals

- **UX (higher court):** one key (`<leader>e`), one capture row, one
  dashboard row; nothing on screen changes unless asked. The guess is shown
  before it is used.
- **#1 Performance:** no new work on the keystroke path. The chooser lists
  one directory when a menu opens; the agenda scans what it scanned plus the
  clients folder.
- **#2 Extensibility:** the deliverable that outlasts this plugin is
  [`plugin-extension.md`](plugin-extension.md): any plugin can extend
  another's options and call its exported commands, with provenance and
  teardown.
- **#3 Vim modal editing:** untouched.
- **#4 Asynchronicity:** untouched. Scans stay off the UI thread as today.
- **Heuristic #6:** no crate. org-fcto is a plugin repo; org's features land
  in org; the host mechanism lands in `lattice-config` and
  `lattice-plugin-host`, which own options and plugins.
- **A feature set is a mode:** `org-fcto-mode` owns every key, command and
  contribution of the feature; the org primitives it uses are org's.
