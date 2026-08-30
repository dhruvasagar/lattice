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

### MV.1a — the seam and its bridge ✅

**Deps:** MV.0.

**Carved from MV.1 mid-build.** The seam and the provider are separately
reviewable and separately committable, and the boundary can be proven through a
real guest before any of the view machinery exists — which is what keeps a
provider bug and a boundary bug from arriving together.

`wit/multibuffer-view-source.wit`: the `multibuffer-view-registry` import (the
guest declares N views, the `picker-registry` shape) plus the
`multibuffer-view-source` export (`build`). `types.wit` gains
`multibuffer-view-{spec,excerpt,input,result}`. Host side:
`multibuffer_view_host.rs` (the second `bindgen!` + the contributions
collector), `multibuffer_view_task.rs` (the actor bridge, the `picker_task`
shape), `PluginSeam::MultibufferViewSource`, and the registry wired on **both**
linkers.

**Both linkers, and that is not optional.** Org will provide `grammar` AND
`multibuffer-view-source`; a component's import set must resolve on every linker
it is instantiated against, and an import absent from one fails the WHOLE
component rather than the one seam. The OC.2 scar: a single `logging::log` call
once took org down entirely.

**The config registry is stamped here.** Seventh seam to need that line, and six
of the previous six shipped without it — each answering `none` to `get-option`
while looking perfectly wired. A view's contents very often depend on an option
(which directory, which filter), so it went in before the bug report rather than
after.

**Tests (5, through a real fixture guest):** one component declares several
views; the identity fields that make a view *ownable* (buffer name, view mode,
reuse, input) cross intact; `build` receives which view and which args; a
malformed spec costs only itself and the plugin keeps its other views; and a
guest DECLINE is a typed `err` that leaves the actor usable — distinct from a
trap, which is the difference between "nothing to show, here is why" and "this
plugin is broken".

### MV.1b — the generic provider, its drain, and two security fixes ✅

**Deps:** MV.1a.

`crates/lattice-multibuffer/src/providers/plugin_view.rs`: one
`ProviderViewOpener` registered per declared view, resolving each excerpt's
`path` to a source buffer, creating (or reusing) the named view, activating the
guest's `view-mode`, and setting the headerline from `view-result.summary`.
Plus the loader drain arm and teardown that unregisters the provider names.

**Landed with the drain, not after it**, and that was forced rather than
chosen. The loader's seam match carries the comment *"a new seam variant must
add its drain here — the compiler enforces it rather than a silent skip"*, so
MV.1a could not compile the workspace without one. An arm that registered
nothing would have been exactly the "wired end-to-end and answers nothing" scar
this codebase keeps re-finding, so the provider and the drain shipped together.

**The opener is sync; `build` is not.** `ProviderViewOpener` runs on the
dispatch path, and `build` may read files — so `open_plugin_view` seats an empty
view with an in-progress headerline and returns, and `fill_plugin_view` applies
the guest's result from a spawned task. `providers/agenda.rs`'s shape
(`open_agenda` + `spawn_agenda_scan`) for its reason. The fill publishes
`MultibufferExcerptsReady`, without which the rows would sit until the next
keypress and read as a rendering bug.

**Two host assumptions this broke, both fixed here:**

- `ProviderViewRegistry`'s doc said openers *"live for the process, so there is
  no RAII unregistration token"*. True of native providers; false of plugins,
  which unload and reload. Without `unregister`, a reload's `register` returns
  `false` against the plugin's OWN stale opener and its views come back dead.
  Added, with the lifetime note amended rather than left to mislead.
- `TeardownRegistries` had nowhere to reverse a view registration. It gains
  `provider_views`, `Option` because a headless boot may never publish the seam
  and a teardown must not require a service the load never used.

**`scan` views register their identity and open empty**, logged at info. The
composition that feeds them from the host's walk is MV.3. Said out loud rather
than silently no-op'd, because a view that opens empty for a structural reason
is indistinguishable from a broken one otherwise.

**The drain's own tests landed after**, and they paid for themselves
immediately: `unloading_frees_the_view_names_for_a_reload` failed on the first
run. `TeardownRegistries` is built behind an all-or-nothing guard — a missing
command registry skips the ENTIRE unload — and a command registry is not a
precondition for reversing a VIEW registration. Reversal moved above the guard,
beside `help` and `transient`, which are there for the same reason.

**Two security findings, both real, both fixed here.**

1. **A capability bypass.** An excerpt names a PATH the guest chose and the host
   reads it to build the source document. A plugin holding *no* fs capability
   could name `/etc/passwd` and have the host read it into a buffer on its
   behalf — the guest never touches the file, so WASI's sandbox never sees the
   read. `EffectAuthorizer` already states the rule: a guest-named path is
   checked at the **boundary**, where provenance is still known, because by the
   time an effect reaches the editor the host no longer knows which plugin asked.
   The conversion now filters through `grant_permits_read`, which canonicalises
   the file FIRST so a symlink inside a granted tree pointing out of it is
   refused rather than followed. Denied rows are counted into the headerline
   summary rather than silently dropped: a view quietly missing rows because of
   a manifest is indistinguishable from one whose data is wrong.
2. **Cross-view tampering.** `reuse` resolved the view by its guest-chosen
   buffer name, so a plugin could declare `buffer-name: "*agenda*"` and take
   over the agenda's buffer — `replace_excerpts` on a view it does not own.
   Buffer names are a flat, unnamespaced space shared with every native
   provider, so a guest-chosen one cannot be an authority to reuse. The opener
   now remembers the id it created and re-enters only that.

**Tests:** 5 at the seam (through a real guest) + 5 at the drain — a declared
view openable by name, one component's several views, a malformed spec costing
only itself, an id a native provider owns refused per-view, the unload/reload
cycle, and the grant gate including the symlink escape.

**Still owed:** the bench for a 500-excerpt `build` round-trip, and a test that
drives a view's excerpts all the way into a buffer (the drain tests stop at
registration + the gate). Carried to MV.2, where org's real view makes that
assertion natural rather than synthetic.

### MV.2 — org registers a view ⛔ dropped

**Dropped 2026-08-30, by Dhruva's decision.** The slice existed to prove the
seam with a lower-risk consumer before the agenda migrated. By the time it came
up there was no such consumer without inventing one: backlinks had shipped as a
picker (OR.9, on the navigation rule), and the alternatives were a read-in-place
backlinks peer or a tags view — the latter being a feature nobody asked for.

Dhruva chose to fold it into MV.3 and migrate the agenda directly.

**The risk that buys, stated plainly** so it is not rediscovered later: the
agenda becomes the first thing ever to run through the new seam, and it is org's
most-used surface. A failure will present as "the agenda is broken" and the
cause could be in the seam, the provider, or the migration.

**The mitigation is MV.3's existing test rule, and it now carries more weight
than it did:** every existing agenda test must pass **unedited**. A test that
had to be adapted is the signal that behaviour moved rather than ownership, and
with no prior consumer it is the only signal there is.

### MV.3 — the agenda migrates ✅

**Deps:** MV.1b. (MV.2 dropped — see above.)

Org declares the agenda through `multibuffer-view-source`; the native
`register_agenda_provider` call is gone from `install.rs`. `*agenda*`, the
provider name and the reuse policy now come from org's `view-spec`.

**A rename of who-decides, not a rewrite of what-happens.** `open_agenda`
became a thin call into `open_scan_view(activator, identity, args)` — the same
body, with the four names it hard-coded lifted into a `ScanViewIdentity`
parameter. The walk, the batched reads, the read-and-parse-once handoff, the
stable sort across files, the group-run computation and the progress headerline
all stayed exactly where they were, because none of it is org-specific and all
of it is measured.

**The "existing tests pass unedited" rule earned its place.** Parameterising the
messages reworded the agenda's decline from *"no plugin provides agenda rows"*
to *"no plugin provides rows for it"*, and
`agenda_declines_when_no_plugin_provides_rows` failed on it. A user-visible
message IS behaviour, so the fix was to restore the wording — the message became
a field on the identity, with the agenda's exact sentence — rather than to edit
the assertion. **No agenda assertion was changed.**

**What the migration did cost, stated plainly rather than buried:**

- **Test SETUP changed in six org harnesses.** Declaring a seam the host has not
  wired is fatal (`NotWired` propagates and the whole load fails — the
  `picker-source` rule), so every harness that loads org now wires
  `provider_view_registry` and `multibuffer_registry` from the editor's own
  service registry. The org-agenda manifest also declares the new seam. These
  are setup, not assertions, and the distinction is the point: setup changing
  means ownership moved, an assertion changing would have meant behaviour did.
- **`view_input::scan` lost its payload.** It carried the file extensions, which
  a scan source already declares through `extensions()` — two places to say one
  thing. Removed before anything bound to it.
- **No agenda without org.** The provider is org's now, so a host without the
  plugin has no `agenda` provider at all. Nothing is lost: the agenda's ROWS
  always came from org's scan source, so such a host had a view that could only
  say "no plugin provides agenda rows", and the trigger (`:org-agenda`) was
  org's too. The provider and its command now arrive together.

**`view_mode: None` in org's spec, deliberately.** `org-agenda-mode` reaches the
view through the SOURCE's `view-mode` export, which is where it has always come
from. Naming it on the view as well would activate it twice.

### MV.4 — docs ✅

**Deps:** MV.1a–MV.3.

`plugin-host.md` gains the seam in its interface table and a section on who owns
a view plus the two input models — a plugin author asking "can I make a view"
looks there, not in this fragment. `multibuffer-views.md` gains §3.7b, the
plugin path, sitting directly after §3.7a's native provider-view seam and
naming the three things that bite: the sync-opener/async-build split, why
`reuse` cannot honour a guest-chosen buffer name, and why excerpt paths are
capability-gated at the boundary. `org-mode.md` §6 gains a note that the agenda
is org's view now, since that document described it as host machinery.

The design fragment needed no amendment on the substance — the build agreed with
it — but two things it specified changed during the work and are recorded where
they happened rather than silently: `view-input::scan` lost its extensions
payload (MV.1a introduced a second source of truth; the scan sources already
declare them), and MV.2 was dropped so the agenda migrated directly.

`site/data/dev-nav.toml` carries `architecture/plugin-multibuffer-views`, and
the sync has run — a docs change is not finished until the site has it.

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
