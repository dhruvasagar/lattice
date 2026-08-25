---
summary: "The agenda: :agenda collects every dated row plugin agenda-sources find under a project root into one multibuffer, date-grouped and globally ordered; <CR> jumps to source, gr rescans."
related: [ex:agenda, multibuffer-mode, project-search-mode]
---

# agenda-view-mode

`:agenda` walks a project, asks every installed **agenda source** which
of its files carry dated rows, and collects the answers into a single
[multibuffer](help:multibuffer-mode) view — ordered by date across
files, with one header per day.

The rows are real excerpts of the source files, not rendered text. That
is the point: `<CR>` jumps to the entry, and an edit made *in the
agenda* propagates to the file it came from. An agenda you can only read
is a lesser feature wearing the name.

> **Status:** the view, the walk, the cross-file ordering, date
> grouping, `<CR>` jump-to-source and `gr` refresh are shipped. Acting
> on an entry — changing a TODO state from the agenda — comes from the
> **source plugin's** own mode, so what you can do to a row depends on
> which plugin produced it (see [Who produces the rows](#who-produces-the-rows)).

---

## Quick reference

| Keystroke / command | Meaning |
|---|---|
| `:agenda` | Build the agenda over the active buffer's project |
| `:agenda <path>` | Build it over `<path>` instead (`~` is expanded) |
| `<CR>` | (in the agenda) Jump to the source file + line of the row under the cursor |
| `gr` | (in the agenda) Re-scan — over the root this view already shows |
| `]e` / `[e` | Move between rows (excerpt motions — see [`multibuffer-mode`](help:multibuffer-mode)) |
| `:multibuffer-expand [n]` | Show `n` lines of context around the row under the cursor |

---

## Who produces the rows

**Lattice ships no agenda sources of its own.** The editor contributes
the walk, the ordering and the view; a plugin contributes everything
about what a dated row *is*.

A plugin providing the `agenda-source` seam declares which file
extensions it wants offered, and lattice offers it only those — so
`:agenda` in a Rust checkout with only an org plugin installed costs a
directory walk and nothing else. Files nothing claims are never read.

With no agenda source installed, `:agenda` declines and says so rather
than opening an empty view you have to guess about.

The reference source is the org plugin, where a row is an open headline
carrying a `SCHEDULED:` or `DEADLINE:` date, or an active timestamp.
Its own documentation ships inside the component: `:help org`, once it
is installed.

---

## Reading the view

Rows are ordered **globally**, not file by file. A row from the last
file scanned can appear at the top, because the ordering is by date —
which is what lets one day's header cover entries drawn from several
different files.

Each day renders one header. The rows under it continue that day until
the next header appears.

The headerline above the view reports the scan while it runs
(`Building agenda (120/400 files)`) and states the result when it
finishes:

```
[agenda] 17 row(s) in 43 file(s)
```

If a source stopped answering partway through, the view keeps every row
it did collect and the headerline says so:

```
[agenda] 9 row(s) in 43 file(s) — partial: 1 source(s) stopped responding
```

A partial agenda that admits it is better than an empty one that does
not — and much better than a complete-looking one that is quietly
missing a plugin's entries. Individual unreadable or malformed files are
counted separately (`(3 file(s) skipped)`) and never fail the scan: one
bad file must not cost you the agenda.

---

## Refreshing

`gr` re-runs the scan over **the root this view already shows**, not
over the current buffer's project. Refreshing an agenda you opened with
`:agenda ~/notes` keeps it pointed at `~/notes`; a refresh that silently
re-targets itself is not a refresh.

`gr` in the agenda comes from
[`refreshable-view-mode`](help:refreshable-view-mode), the same chord
that refreshes search results, compilation output and every magit
buffer.

---

## One agenda at a time

The view is named `*agenda*` and is reused: a second `:agenda`
re-scans into the same buffer rather than accumulating views. The
existing rows are cleared before the new scan starts, so you never see
two scans' results at once.

---

## Where the work happens

The walk, the file reads and every call into a plugin run off the UI
thread. The view opens immediately and empty; rows land when the scan
finishes, without needing a keypress to appear.

Unlike `:search`, the agenda does not stream results in as it goes —
its order is global, so a row found last may belong first, and
rewriting every row on each batch would mean the whole view reflowing
repeatedly while you read it. Progress goes to the headerline instead.
