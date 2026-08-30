# Plugin-owned multibuffer views — slice plan

Design: [`architecture/plugin-multibuffer-views.md`](../../architecture/plugin-multibuffer-views.md).

## Why this phase exists

A plugin can feed rows to the one multibuffer the host built for it and can own
that view's chords. It cannot create a view. Org's second view — backlinks — has
nowhere to go, and so would any third-party plugin's first.

## Slices

### MV.0 — the seam stops claiming to be the agenda ✅

`agenda-source` → `scanned-excerpt-source`; `AgendaEntry` → `ScannedExcerpt`;
`AsyncAgendaSource` → `ScannedExcerptSource`; `PluginSeam::AgendaSource` →
`ScannedExcerptSource`; the manifest `provides` string with them.

Landed **first and separately** because it is a rename with no behaviour change,
and because the record it renames — `{ line, end-line, group, label, sort-key }`
— contains nothing about org, dates or TODOs. It is an excerpt plus an ordering
plus a group header. The agenda is its first consumer, not its definition, and a
project-TODO view or a tags view should not have to register as an "agenda
source". Done while org was the only consumer: a seam name is API, and this gets
expensive the moment a third-party plugin binds to it.

`providers/agenda.rs` keeps its name — it *is* the agenda, correctly named,
consuming a generic seam.

**Note:** a clean break, no alias for the old `provides` string. Pre-1.0, one
in-tree consumer, and a silently-accepted old name is how two spellings end up
in the wild.

### MV.1 — the seam and the generic provider 📝

**Deps:** MV.0.

`wit/multibuffer-view-source.wit` per design §2: `view-spec`, `view-excerpt`,
`view-input`, `spec()`, `build(view, args)`. The host side is
`crates/lattice-multibuffer/src/providers/plugin_view.rs`: one
`ProviderViewOpener` registered per declared view, resolving each excerpt's
`path` to a source buffer, creating (or reusing) the named view, activating the
guest's `view-mode`, and setting the headerline from `view-result.summary`.

Plus the actor bridge (`view_task.rs`, the `picker_task` shape), the loader
drain arm, and teardown that unregisters the provider names.

**The config registry must be stamped on this store.** Sixth-seam rule: five
seams shipped without it and each silently answered `none` to `get-option`.
Assume this one needs it and prove it with a test that reads an option from
`build`.

**Tests:** a guest-declared view opens by name through
`open-provider-view`; excerpts land against the right source buffers; an `err`
from `build` declines with the guest's message rather than opening an empty
view; an excerpt naming a missing path is dropped and the rest of the view
survives; two views from ONE component both register (the `picker-registry`
property); an id colliding with a native provider is refused with both names and
does not take the guest's other views down with it.

**Bench:** `build` round-trip for a 500-excerpt view — the crossing plus the
per-excerpt source resolution, which is the part that scales.

### MV.2 — org registers a view 📝

**Deps:** MV.1.

Org declares its first `pull` view. Which one it is depends on what OR.9 left:
backlinks shipped as a picker on navigation grounds, so the natural first
consumer is whichever view wants read-in-place — a tags view, or backlinks
gaining a multibuffer peer alongside the picker rather than replacing it.

**Do not migrate the agenda in this slice.** A first consumer that is also the
riskiest migration would confuse "the seam is wrong" with "the migration is
wrong".

**Tests:** the view opens from an ex-command and from a chord; its excerpts jump
to source; an edit in the view propagates to the source file; `gr` re-invokes
`build`; the view's own minor is active on it and its chords fire.

### MV.3 — the agenda migrates 📝

**Deps:** MV.2.

`providers/agenda.rs` becomes the **scan input strategy** the generic provider
uses rather than a provider of its own. `*agenda*`, the reuse policy, the
provider name and `org-agenda-mode` move into org's `view-spec`; the walk, the
batched reads, the read-and-parse-once handoff, the stable sort, the group-run
computation and the walk's progress headerline all stay host-side.

**The regression surface is the whole agenda**, so this slice is the one that
needs the existing agenda tests to pass unchanged rather than adapted. A test
that had to be edited to keep passing is the signal that behaviour moved.

**Tests:** every existing agenda test, unedited; plus rows still interleaving by
`sort_key` across files, the group header still landing on the first row of each
run after the sort, and the headerline still reporting files scanned during the
walk.

### MV.4 — docs 📝

**Deps:** MV.1–MV.3.

The design fragment lands amended where the build disagreed with it.
`plugin-host.md` gains the seam beside `picker-source` and
`scanned-excerpt-source` — a plugin author looking for "can I make a view" will
not think to open this fragment. `multibuffer-views.md` gains the plugin path
and the scan-vs-pull rule. `org-mode.md` §6.2 is amended where the agenda's
ownership changed. `site/data/dev-nav.toml` gains both new pages, and the sync
runs.

## The two rules this phase writes down

Both were load-bearing and neither was recorded, which is how the agenda ended
up looking like the only shape available.

1. **Multibuffer or picker: do you act on the rows in place, or go somewhere?**
   The agenda is where you change TODO states and reschedule — edit-propagates-
   to-source *is* the feature, so it is a multibuffer. Project search is a
   multibuffer for the same reason. Backlinks is "what points here, take me
   there" — navigation, so a picker. Not "is it a list of places".

2. **Scan or pull: does answering require reading many files' contents?** If
   yes, the host walks, reads and parses once and the guest classifies — the
   guest needs no capability and gets the tree free. If the guest already knows
   the answer, it returns excerpts and the host renders them. Choosing scan for
   a pull-shaped question walks the project to re-derive what an index holds;
   choosing pull for a scan-shaped one costs the guest a capability and 200× the
   boundary traffic.
