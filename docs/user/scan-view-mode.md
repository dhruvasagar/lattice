---
summary: "Scan views: a plugin's own command walks your files, asks a scan source which rows they carry, and collects the answers into one ordered multibuffer; <CR> jumps to source, gr rescans."
related: [multibuffer-mode, project-search-mode, refreshable-view-mode]
---

# scan-view-mode

A **scan view** walks a set of paths, asks every installed **scan source**
which of its files carry rows, and collects the answers into a single
[multibuffer](help:multibuffer-mode) view — ordered globally across files,
with one header per group.

The rows are real excerpts of the source files, not rendered text. That is the
point: `<CR>` jumps to the entry, and an edit made *in the view* propagates to
the file it came from. A view you can only read is a lesser feature wearing the
name.

**Lattice ships no scan sources of its own.** The editor contributes the walk,
the ordering, the grouping and the view; a plugin contributes everything about
what a row *is* — and names the view in its own users' vocabulary. The org
plugin's agenda is the reference one, where a row is an open headline carrying
a `SCHEDULED:` or `DEADLINE:` date, or an active timestamp. It calls its view
the agenda and opens it with `:org-agenda`.

> **Status:** the view, the walk, the cross-file ordering, grouping,
> `<CR>` jump-to-source and `gr` refresh are shipped. Acting on a row —
> changing a TODO state, say — comes from the **source plugin's** own mode, so
> what you can do to a row depends on which plugin produced it.

---

## Quick reference

| Keystroke / command | Meaning |
|---|---|
| the source's own command | Build the view. Each source names it — the org plugin registers `:org-agenda` |
| `<that command> <path>` | Build it over `<path>` instead (`~` is expanded), ignoring configured paths for this one call |
| `<CR>` | Jump to the source file + line of the row under the cursor |
| `gr` | Re-scan — over the root this view already shows |
| `]e` / `[e` | Move between rows (excerpt motions — see [`multibuffer-mode`](help:multibuffer-mode)) |
| `:multibuffer-expand [n]` | Show `n` lines of context around the row under the cursor |

---

## Who produces the rows

A plugin providing the `scanned-excerpt-source` seam declares which file
extensions it wants offered, and lattice offers it only those — so opening a
view in a Rust checkout with only an org plugin installed costs a directory
walk and nothing else. Files nothing claims are never read.

With no scan source installed there is no command to type — the trigger belongs
to the source — and the view declines and says so rather than opening an empty
view you have to guess about.

---

## Reading the view

Rows are ordered **globally**, not file by file. A row from the last file
scanned can appear at the top, because the ordering is the source's own — which
is what lets one header cover entries drawn from several different files.

Each group renders one header. The rows under it continue that group until the
next header appears.

The headerline above the view reports the scan while it runs
(`Building agenda (120/400 files)` — the label is the view's own) and states
the result when it finishes:

```
[agenda] 17 row(s) in 43 file(s)
```

If a source stopped answering partway through, the view keeps every row it did
collect and the headerline says so:

```
[agenda] 9 row(s) in 43 file(s) — partial: 1 source(s) stopped responding
```

A partial view that admits it is better than an empty one that does not — and
much better than a complete-looking one that is quietly missing a plugin's
entries. Individual unreadable or malformed files are counted separately
(`(3 file(s) skipped)`) and never fail the scan: one bad file must not cost you
the view.

---

## Refreshing

`gr` re-runs the scan over **the root this view already shows**, not over the
current buffer's project. Opening a view over `~/notes` and refreshing it keeps
it pointed at `~/notes`; a refresh that silently re-targets itself is not a
refresh.

It also re-opens **this** view rather than some other one: the view remembers
which provider built it, so a scan view a plugin declared refreshes itself and
not whichever source happens to be first.

`gr` comes from [`refreshable-view-mode`](help:refreshable-view-mode), the same
chord that refreshes search results, compilation output and every magit buffer.

---

## One view per source

A view is reused: a second open re-scans into the same buffer rather than
accumulating views. The rows already there are kept until the new ones arrive,
so a refresh never shows you an empty view it is about to fill.

---

## Where the work happens

The walk, the file reads and every call into a plugin run off the UI thread.
The view opens immediately and empty; rows land when the scan finishes, without
needing a keypress to appear.

Unlike `:search`, a scan view does not stream results in as it goes — its order
is global, so a row found last may belong first, and rewriting every row on each
batch would mean the whole view reflowing repeatedly while you read it. Progress
goes to the headerline instead.
