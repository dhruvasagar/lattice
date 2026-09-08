# A guest publishing events — built-in and custom

**Status:** design agreed, not built. No slice IDs yet.

## What a guest can publish today, and what it cannot

`host-services.emit-event(name, payload)` produces exactly one thing:
`Event::Plugin { name, payload }`. A plugin can raise its **own** events and
nothing else. It cannot raise a built-in typed event at all.

That limit is invisible until something needs it, and then it looks like a
missing feature rather than a missing seam. `Event::BackgroundTaskFinished`
exists precisely so *any* producer can announce that async work completed
without knowing who reports it — its own doc names "a plugin's task" as a
producer — and no plugin can reach it. The first design response to that gap
was to propose a `notify` host import, which would have been a second seam
duplicating an existing one. Widening the publish path is the correct answer.

## The rule

**A plugin may publish any built-in event that is marked publishable, and any
custom event registered by any plugin.**

Custom events carry no host meaning, so any guest may raise any of them —
including one another's. That is not a hole: a plugin event is opaque bytes the
host routes and never interprets, and a subscriber already cannot assume who
sent it.

## The gate is intrinsic to the event, not a capability

Built-in events divide into two kinds, and the division is about what a
subscriber *does* with them:

- **Announcements a producer legitimately makes.** `BackgroundTaskFinished` is
  the model — it is a report about the producer's own work, and a plugin raising
  it is the intended use.
- **Observations of fact the host owns.** `DocumentSaved`, `FilesChanged`,
  `BeforeQuit`, `PluginCrashed`. A guest raising one of these does not *report* a
  fact, it *fabricates* one, and every subscriber acts on it. A faked
  `DocumentSaved` drives formatters, LSP `didSave` and autocommands off an event
  where nothing was saved.

**Publishability is a property of the event, declared where the event is
declared** — not a capability grant, and not a list held by the host.

The reason is what happens to a *new* event under each design. With the property
on the declaration, adding a variant to `Event` forces an answer at the point of
declaration: the author must say whether a guest may raise this. With a
capability, or with a host-side allowlist, a new event defaults into whichever
answer the list's absence implies — and the dangerous default (a guest may raise
anything not yet listed) is exactly the one nobody notices until a plugin
fabricates a `DocumentSaved`.

A capability would also be answering a different question. `events:raise` says
*this plugin is trusted*; the actual question is *this event is safe for anyone
to raise*, which does not vary by plugin. Trust does not make a fabricated
`DocumentSaved` less of a lie.

## Where it lands

- `crates/lattice-protocol/src/event.rs` — classify every `Event` variant. The
  classification is the design work; the rest follows from it.
- `wit/host-services.wit` — widen the publish seam past `Event::Plugin`.
- `crates/lattice-plugin-host/src/event_task.rs` — the emit path, where a
  refused publish is logged and dropped rather than trapping (a plugin author's
  mistake is not a reason to kill a running plugin).

## First consumer

Org's roam index, replacing its modeline segment with three published events:
sync started, halfway, completed. The modeline was never the right surface for
it — `roam_scan.rs` justifies the choice only against an *echo* ("a scan
outlives the message line"), which argues against echoes and not at all for
permanent modeline residency.

See also: the standing rule that async **buffer** progress belongs in the
buffer's headerline. The roam index has no buffer, so it falls outside that
rule rather than contradicting it — but the rule's neighbourhood is close
enough that the distinction is worth stating when this is built.
