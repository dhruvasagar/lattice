# Plugin-owned multibuffer views

**Status:** design. Slice plan: [`slice-plans/plugin-multibuffer-views.md`](../operations/slice-plans/plugin-multibuffer-views.md).

A plugin can put rows into the one multibuffer the host built for it. It cannot
build one. This fragment closes that gap and, in doing so, stops the agenda from
being a special case.

## 1. What a plugin owns today, and what it does not

Two things are already the guest's, and they are the reason this is a gap rather
than a rewrite:

- **The view's interactions.** `scanned-excerpt-source` exports
  `view-mode: func() -> option<string>`; the host activates that minor on the
  view it creates. `org-agenda-mode`'s chords *and their handler bodies* live in
  org, which is what satisfies the mode-ownership rule today.
- **Opening a view.** `app-effect::open-provider-view(provider, args)` lets a
  guest open any registered provider by name, ungated — deliberately, on the
  `open-picker` / `open-transient` precedent.

Everything else is host-locked:

| capability | today | why it is locked |
|---|---|---|
| register a view | native only | `ProviderViewOpener` is `Arc<dyn Fn(&mut dyn ModeActivator, &Args)>` — a Rust closure. There is no WIT path to the registry. |
| name the view / reuse policy | `*agenda*`, host constant | baked into `providers/agenda.rs` |
| supply excerpts | file-scan only | the sole seam is "here is one file's text; what rows are in it" |
| order and group | host | host stable-sorts on the guest's `sort-key` and computes group runs |
| headerline | host | the walk writes its own progress |
| refresh | host | `gr` re-runs the host's scan |

The consequence is concrete: **org cannot have a second multibuffer view.**
Backlinks, a tags view, "notes touched this week" — each would need a new native
provider in `lattice-multibuffer`. That fails the acid test
`multibuffer-views.md` already sets for provider crates ("a new provider should
require zero host additions"), and fails it worse for plugins, which cannot add
host code at all.

## 2. The seam

A guest declares N views at load, exactly as `picker-registry` declares N picker
sources, and the host registers one provider-view opener per declaration.

```wit
interface multibuffer-view-source {
    /// One row of the view. `path` names a FILE; the host opens (or reuses)
    /// its Document and adds it as the excerpt's source, because `Excerpt`
    /// carries a `BufferId` and only the host can mint one.
    record view-excerpt {
        path: string,
        start-line: u32,
        end-line: u32,
        /// Rendered above this excerpt. **Empty renders no header row** —
        /// which is the whole grouping mechanism: a group is "title on the
        /// first excerpt of a run, empty on the rest".
        header: string,
        /// The `· N matches` badge. `none` ⇒ no badge.
        match-count: option<u32>,
    }

    record view-spec {
        /// The provider name. `open-provider-view` and `gr` both use it.
        id: string,
        /// The buffer name — the GUEST's to choose (`*agenda*`,
        /// `*org-roam-backlinks*`).
        buffer-name: string,
        /// The minor mode to activate on the view. This is where the view's
        /// chords and their handler bodies live, and it moves here from
        /// `scanned-excerpt-source` because it is a property of the VIEW, not
        /// of how its rows were found.
        view-mode: option<string>,
        /// Reuse one buffer across triggers (the agenda: a second `:agenda`
        /// re-scans into the same buffer) or open a fresh view each time.
        reuse: bool,
        /// Where the rows come from. See §3.
        input: view-input,
    }

    /// How a view's excerpts are produced.
    variant view-input {
        /// The guest already knows the answer — an index lookup, a computed
        /// set. The host calls `build` and renders what it returns.
        pull,
        /// The host walks and reads; the guest classifies each file. Carries
        /// the extensions the walk should offer. See §3.
        scan(list<string>),
    }

    /// Called once at load.
    spec: func() -> list<view-spec>;

    /// Produce a `pull` view's excerpts, in FINAL order — the guest sorts,
    /// because only the guest knows what its ordering means. `args` come from
    /// the trigger verbatim.
    ///
    /// An `err` declines the view with the guest's own message rather than
    /// opening an empty one, which is `ProviderViewOutcome::Declined`.
    build: func(view: string, args: list<string>) -> result<view-result, string>;

    record view-result {
        excerpts: list<view-excerpt>,
        /// The headerline's terminal summary ("42 backlinks"). Returned with
        /// the rows rather than fetched separately: one crossing, and the
        /// count is a fact the guest already has.
        summary: string,
    }
}
```

`scan` views keep answering `roots` / `begin` / `scan` on the existing seam. The
host drives the walk, sorts on `sort-key`, computes group runs, and drives
progress — none of which changes.

## 3. Scan is an *input*, not a provider

The temptation is one seam: "the guest returns excerpts, always". It is the
wrong unification and the numbers say so.

`scanned-excerpt-source` exists because the host **must read each file anyway**
to build the source `Document`. So it reads once, parses once, and hands over
text *and* tree. That buys the guest a 1–2 ms parse for a **217 ns** copy
(`benches/agenda_scan_input.rs`) and — the part that matters — means the guest
needs **no filesystem capability at all**. A pull-only world forces the guest to
discover and read files itself through WASI, losing the free tree and taking on
a capability, and a guest reading node text through the tree seam instead would
cross the boundary per headline: about 50 µs per file, 200× the copy it was
avoiding.

So the two are different **cost models**, not duplication:

| | discovery | who reads | guest capability | ordering |
|---|---|---|---|---|
| **scan** | host walks by extension | host, once, with the tree | none | host sorts on `sort-key` |
| **pull** | guest already knows | nobody, or the guest | `fs:read` only if it wants text | guest returns final order |

Choosing between them is the view author's, declared in `view-input`. Backlinks
is `pull` — one `get` on `b/<id>` answers it, and a scan would walk the project
to re-derive what an index already holds. The agenda is `scan` — its answer
genuinely requires reading every candidate file's contents.

## 4. How the agenda re-expresses

`providers/agenda.rs` stops being a provider and becomes the **scan input
strategy** the generic provider uses. What moves and what stays:

**Moves to org's `view-spec`:** the provider name, `*agenda*`, the reuse policy,
`org-agenda-mode`. All four are today host constants describing org's feature.

**Stays host-side, unchanged:** the bounded `fs:read`-gated walk, the batched
`spawn_blocking` reads, the read-and-parse-once handoff, the stable sort on
`sort-key`, the group-run computation, and the progress headerline during the
walk. None of it is org-specific and all of it is measured.

**Stays exactly as-is:** the whole-scan-before-append rule. The agenda's order is
global — a row from the last file scanned may belong at the top — so appending
per file and re-sorting per batch would rewrite every row on each batch, a
whole-viewport restyle the UX rules veto outright.

The net effect is that the agenda becomes an ordinary consumer. Nothing about it
is special-cased in the host except the scan strategy it shares with any other
`scan` view.

## 5. Rejected alternatives

**One seam for both cost models.** §3. It makes the agenda strictly worse to
make backlinks possible.

**Let the guest hold the multibuffer handle and mutate it.** A view would be a
resource the guest appends to. Rejected: `create_multibuffer_view` needs
`&mut ModeActivator`, which exists only on the host's dispatch path, and
exporting it as a guest-held resource would mean a guest could mutate the view
tree at arbitrary times — including during a frame. The declare-and-return shape
keeps every mutation on the host's own path, which is the same reason
`Effect::OpenSyntheticBuffer` exists rather than a guest-side buffer factory.

**Give the guest `BufferId`s directly instead of paths.** A guest cannot mint
one and should not learn the host's identity space; a path is the stable name
both sides already share (`WriteToFile` resolves paths to buffers for the same
reason).

**Keep the bespoke agenda provider and add a second bespoke one for backlinks.**
The N+1th view costs a host crate change forever, which is the restriction being
removed.

## 6. Paramount-goal alignment

- **#2 Extensibility.** The point of the fragment. A plugin gains the last piece
  of a view it could otherwise only half-own: it already owned the interactions
  and the trigger; now it owns the view's identity, contents, order and status.
- **#1 Performance.** No new work on the keystroke path — `build` and `scan` are
  both off-thread producers, as today. The scan path's measured advantages are
  preserved deliberately rather than traded away for uniformity (§3).
- **#4 Asynchronicity.** `build` is a guest call on the plugin's actor;
  completion reaches the screen through `MultibufferExcerptsReady`, which has a
  wake wired (`boot.wake_on_event`). Named because the alternative — a bare
  `TickCallback` — is the bug class re-introduced repeatedly, whose symptom
  reads as a rendering fault rather than a missing wake.
- **#3 Everything is a buffer.** A plugin view is a multibuffer like any other:
  `:ls` lists it, `:bd` closes it, splits place it.

## 7. Failure behaviour

- `build` returning `err` **declines** with the guest's message
  (`ProviderViewOutcome::Declined`) rather than opening an empty view. Declining
  is first-class: an empty view leaves the user guessing why.
- An excerpt naming an unreadable or missing path is **dropped with a log**, not
  a failed view — `error-parser`'s rule, because it is the same failure class: a
  stale index must not cost you the whole view.
- A `view-spec` whose `id` collides with a registered provider is refused at
  load with a warning naming both, and the rest of that guest's views still
  register.
- A guest that traps during `build` is quarantined by the existing actor
  machinery; the view shows the decline.

## 8. `refresh-view` — acting on a view from outside a trigger (OA.15a)

`app-effect::open-provider-view` already says "open my view", and every trigger
that RETURNS an effect keeps using it: an action handler, an ex-command, a
transient row. §2's seam gave a plugin the view; that effect gave it the
trigger.

What neither covers is a producer that returns nothing at all. `on-event` is
`func(handler: u32, ev: event)` **by construction** — the event seam is
observation-shaped (§5.10) — and `on-wake` is the same. So a guest handler
could reindex a store, emit an event or push a modeline segment, but could not
touch a view it owns.

### What made the gap visible: a guest mode cannot be a switch

A guest minor mode. The host already delivers `minor-activated` /
`minor-deactivated` to plugins, so a guest can *see* its mode go on and off.
What it cannot do is answer.

The asymmetry is with native modes, and it is worth naming precisely because
it looks like a parity bug and is not. `scan-view-clockreport-mode` (`cr`, the
clock report) registers its virtual-row provider in `on_activate` and drops it
via the guard's `Drop` — that body is what makes the *mode* the single switch
rather than a label sitting beside one, which is the property OA.16 chose the
shape for. A plugin mode has no such body: `mode-declaration` is DATA, and the
host builds it into a `PluginMode` whose `on_activate` is a no-op
(`plugin_host/mode_host.rs`).

So `org-agenda-log-mode` could be toggled and change nothing. Declaring it
anyway would have put a live `:org-agenda-log-mode` / `ToggleMode` path in the
tree that flips a mode changing nothing — the "registered a chord that silently
does nothing" class this codebase keeps paying for.

`refresh-view` is the body the host cannot supply: the guest's
`minor-activated` handler re-opens its own view with the mode's argument set,
and the mode becomes the switch it claimed to be.

### Contract

```wit
refresh-view: func(view: string, args: list<string>);
```

A **request, not an apply** — `enable-mode`'s shape (`modes.wit`), and for the
same reason: the opener needs the `&mut ModeActivator` and a plugin store
cannot reach it. The host publishes `ProviderViewRefreshRequested`
(`lattice-mode`), and `Editor::drain_provider_view_refresh` applies it on the
next tick through **the same opener the effect arm calls** — deliberately one
opening path, since a second that differed is exactly the divergence
`ProviderViewRegistry` exists to prevent.

Three decisions worth having written down:

- **Typed event, so the wake is structural.** `ModeEnablementRequested` is a
  plain `Event` variant with no wake, which is tolerable there because it fires
  during boot, when ticks are running anyway. A mode toggle does not: a bare
  channel would refresh the view only when the user next pressed a key, which
  is the "works, but only after I hit something" class. The typed event gets a
  `wake_on_event`-shaped forwarder firing `async_landed`, and the test asserts
  the wake directly rather than inferring it from the drain — verified to fail
  when the notify is removed, so it is not vacuous.
- **The drain does NOT activate the view.** The effect arm does, because the
  user just asked for the view and expects to land in it. This path runs
  because a mode changed, so stealing focus would be a jump nobody asked for —
  and a `reuse: true` view, which is the shape this is for, is already on
  screen and re-scans in place.
- **Requests de-duplicate by (provider, args) within a tick**, since a mode
  toggled twice before a tick lands would otherwise re-scan identically twice
  and the scan is the expensive thing at the end of this path (org-agenda
  OA.0's measurements). Distinct args are *not* collapsed: two arguments are
  two questions.

### Failure behaviour

- An **unknown view name** is a `warn` and a skip. A stale name after a plugin
  reload is an author mistake, not a reason to kill a running plugin — and a
  test pins that one bad name does not poison the drain for a good one.
- **No bus wired** (a test harness, a plugin not spawned onto one) is a `warn`
  and a drop, matching `emit-event` and `enable-mode`.
- An **empty view name** is refused at the boundary, where it is the one thing
  wrong regardless of what is registered. Whether the name resolves is checked
  in the drain, where the registry is visible — the division
  `register-multibuffer-view` already makes with `providers::plugin_view`.
- A **decline is echoed**, not swallowed. Nothing the user typed is behind this
  path, so silence would leave a view that did not refresh with nothing to
  explain it.

### Why no benchmark

The four-artefact rule asks for one; this seam has no hot path to measure. The
drain is a `try_recv` loop that is empty on virtually every tick, and the work
it can trigger — a provider re-scan — is already covered by the agenda's own
scan measurements (`org-agenda.md` phase 0), which is where a regression would
show. A bench of the empty drain would measure the tick loop, not this.

### The import's world, and the OC.2 scar

`events-plugin` gains `import multibuffer-view-registry`. That is safe only
because the seam is already wired into **both** linkers — the async one and the
sync grammar one (`PluginHost::new`). An import absent from a linker a
component is instantiated against fails the WHOLE component, silently, not just
the one seam; one `logging::log` call once took org down entirely. An import is
added to a world only once its seam is known to resolve everywhere.
