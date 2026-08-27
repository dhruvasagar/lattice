# Org capture overhaul — slice plan

> Design:
> [`../../../architecture/org-capture.md`](../../../architecture/org-capture.md)
> (the capture system) and
> [`../../../architecture/plugin-transients.md`](../../../architecture/plugin-transients.md)
> (the seam the menu needs).
>
> Sequences both, because the menu is what makes many templates usable and
> the seam is what makes the menu possible.

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred.

**Status:** ✅ complete (2026-08-27). Every slice landed; nothing deferred.

Two host slices fell out of this plan that were not in it: `default_modes`
(plural), and `host-services.read-file` — the second because a grammar action
turns out to be unable to read a file through WASI at all.

**TR.2 was carved in two while executing it.** The registry's builders were
synchronous, so a guest-backed one had nowhere to live before the seam existed
— and that substrate change (plus the per-row `args` the first consumer needs)
is a self-contained, native-only slice with its own tests. Landing it separately
keeps the seam commit about the seam.

## Why now

OM.11's capture shipped one template, one target, one prompt. Measured
against a real nine-template org config it misses the feature: keyed
templates, `%^{Prompt}` input, per-template targets, `%a`. And capture sits
in org-mode's MAJOR keymap, so it only fires inside an org buffer — backwards
for the one verb meant to work from anywhere.

## Sequencing

```
  host (generic, no org in it)
  ├── TR.1  the transient registry is the editor's, not magit's
  ├── TR.2a a build that can answer later + per-row action args
  └── TR.2b the `transient-source` seam
  host (generic, no org in it)
  ├── OC.1a `default_modes` — a plugin may have more than one on by default
  ├── OC.3a a plugin's prompt submit actually fires
  ├── TR.3a an open carries its arguments
  └── TR.3b `Argument` rows cross the seam
  org plugin
  ├── OC.1  org-global-mode + the <leader>o prefix
  ├── OC.2  parse `org.capture-templates`
  ├── OC.3  the capture transient
  ├── OC.4  the %^{Prompt} fields
  ├── OC.5a targets (+ the host `read-file` it needed)
  ├── OC.5b %t and %a
  └── OC.6  docs, ledger, site
```

| Slice | Description | Status |
|---|---|---|
| TR.1 | `TransientSourceRegistry` service registered by `editor_boot` | ✅ |
| TR.2a | `TransientBuild` (Ready/Future) + per-row `Action` args | ✅ |
| TR.2b | `transient-source` WIT seam + loader drain + teardown | ✅ |
| OC.1a | manifest `default_modes` (plural) + one gate for all of them | ✅ |
| OC.1 | `org-global-mode` (Universal) owning `<C-x>o` / `oa` / `oc` | ✅ |
| OC.2 | Parse `org.capture-templates` (TOML-in-an-option) | ✅ |
| OC.3a | `Effect::OpenPrompt` reaches a plugin's action, with its smuggled state | ✅ |
| OC.3 | The capture transient, one key per template | ✅ |
| TR.3a | `Effect::OpenTransient` carries args → `TransientContext::args` | ✅ |
| TR.3b | `Argument` rows cross the seam (the park/resume mechanism) | ✅ |
| OC.4 | `%^{Prompt}` as `Argument` rows on a per-template fields menu | ✅ |
| OC.5a | `file+headline` targets (+ host `read-file`) | ✅ |
| OC.5b | `%t` and `%a` — the last two placeholders | ✅ |
| OC.6 | Docs, ledger, site nav | ✅ |

Every slice ships four artefacts (CLAUDE.md heuristic #5): doc, bench where a
hot path is touched, tests covering the failure mode as well as the happy
path, graceful error handling. One slice, one commit, committed as it goes
green, `scripts/precommit.sh <crate>` before each.

---

## TR.1 — the registry is the editor's ✅

`lattice-magit::install` currently does `TransientSourceRegistry::new()` +
`register_service`. Move the pair to `editor_boot`, beside
`PickerRegistryHandle`; magit keeps registering its own sources into the
service it now looks up.

**The bug this fixes is not cosmetic.** As it stands the registry exists only
if magit loaded, so a plugin transient would work or not depending on an
unrelated feature crate — with nothing at the failure point to explain it.

Landed with the registration placed **before** `lattice_magit::install`
rather than beside its picker sibling: magit installs early and now looks the
service up, so the ordering is load-bearing.

**Nearly shipped with the `ServiceRegistry` TypeId pitfall.** The handle is
registered as `TransientSourceRegistryHandle` (= `Arc<Registry>`), and a
lookup of `Registry` keys on a different `TypeId` and silently answers
`None` — all three tests failed that way on the first run. Register and look
up with the SAME `T`.

Tests: the service resolves on a booted editor; magit's `magit-dispatch`
still builds through it; an unregistered name is `None` rather than a panic
(the property TR.2's guest-supplied names lean on).

## TR.2a — a build that can answer later ✅

Design §4. The registry stored `Fn(&TransientContext) -> TransientSpec`;
a guest's `build` is an async call on its own actor task and cannot answer that
way. Builders now return `TransientBuild::{Ready, Future}` — natives through the
unchanged `register`, guests through `register_async` — and the host parks a
future in `Editor::pending_transient_build`, seating it from
`drain_pending_transient_build` on the **async-landed wake**.

The open body moved onto `Editor::open_named_transient` in the same slice: both
renderer peers held identical copies, and the async path would otherwise have
been written into each.

**`TransientItemKind::Action` gained an `args` slot** (design §4, "Per-row
arguments"). Not a convenience — the state projection is per-MENU, so a menu
whose rows differ only in a parameter (org's capture menu, one row per template)
was inexpressible. Native rows use `TransientItemKind::action(cmd)` and are
unaffected.

Tests: an async builder's menu opens with NO second keystroke and the wake fires
(`async_transient_seats_off_keystroke.rs`, 5 tests — plus the failed-build echo
naming its source, the supersede path, and the sync builder still seating in the
same call); each row fires with its own args while an args-less row still reads
the menu state, both driven through `press`
(`transient_row_carries_its_own_args.rs`).

## TR.2b — the `transient-source` seam ✅

Mirrors `picker-source`: guest exports `id()` + `build(ctx) -> result<spec>`;
the host wraps the exports as a `register_async` builder and registers it under
the guest's id. `PluginSeam::TransientSource`, `spawn_transient_source` +
`TransientActor`/`TransientClient`, the boundary conversion, the loader's
`drain_transient`, and the loader-side teardown reversal (the registry is
`Arc`-shared, so it sits beside `help_topics`, not in `TeardownRegistries`).

v1 mirrors `Action` (crossing as a command **name plus its args**, the name
resolved host-side — a plugin cannot forge a `CommandId`) and `Dismiss`.
`Submenu` / `Flag` / `Argument` / `Variable` are 📝 with reasons in the design
fragment; `TransientSpec::preview` is a closure and cannot cross at all.

Also completed the `plugin_seam_as_str_round_trips_from_str_for_every_variant`
list, which had drifted to eleven of eighteen while claiming all of them.

Tests: `transient-guest` fixture + `plugin-host/tests/transient_source.rs`
(5 — the guest names its menu; the context projection crosses IN and rebuilds
per open; two rows fire ONE command with different args; an unregistered
command drops only its row; a guest `err` is typed and does not quarantine) and
`plugin-loader/tests/transient_drain.rs` (3 — a discovered plugin registers
under the name its own `id()` chose and builds through the registry; unload
withdraws the name so the chord says "unknown source" rather than reaching a
dead actor; an unrelated name stays unknown).

## OC.1a — a plugin may have more than one on-by-default mode ✅

Carved while executing OC.1, because OC.1 does not work without it.

A plugin minor is inert until enabled (`auto_activatable_minors` filters on
enablement, CI.3) and the only enablement trigger is the manifest's
`default_mode` — a single string. Org now needs two modes on out of the box:
`org-todo-mode` inside org files, `org-global-mode` everywhere. Naming one left
the other registered, correct, and permanently inert. **The symptom is a chord
that silently does nothing**, with the enablement filter the only place that
would have explained it, which is why this is a host fix rather than a
work-around in the plugin.

`default_modes` (plural) in `PluginManifest`; the singular key still parses and
folds in (blanks dropped, duplicates collapsed — a manifest that says the same
mode both ways must not request enablement twice). Still ONE `<id>.enabled`
gate: the user is turning org on or off, not curating its internals, and a
half-disabled plugin whose remaining chords keep firing is worse than one that
stayed on.

Tests: the plural key parses, merges with the singular (singular first — it
names the primary mode), and dedupes (`manifest.rs`); a two-mode plugin gets
BOTH enabled on load from one gate, registers only that one option, and
toggling it off reaches both (`mode_gate.rs`).

## OC.1 — `org-global-mode` and the prefix ✅

A minor with `ActivationPolicy::Universal` (the `magit-global-mode`
precedent) owning `<C-x>o`: `oa` → agenda, `oc` → capture.

`<C-x>o` **shadows `action:next-pane`** — decided deliberately (design §6).
The slice includes the note in `doc/org.md`, because a silently-shadowed
emacs chord is exactly the kind of thing that reads as a bug later.

Moves capture off org-mode's major keymap, which is what makes it reachable
from a non-org buffer at all.

`oa` binds to the HOST's `:agenda` ex-command by name: the agenda view belongs
to the multibuffer provider and org only supplies its rows through the
`agenda-source` seam, so the chord names what already exists rather than adding
an org-side wrapper for it.

## OC.2 — parse the templates ✅

`org.capture-templates`, a string holding TOML. Replaces
`org.capture-file` / `org.capture-template`.

Parsed on read rather than cached: `:set` must take effect on the next
capture, and caching would need an `OptionChanged` subscription to stay
honest (the `todo-keywords` precedent, OM.7).

A malformed template set **echoes the parse error and captures nothing**
rather than silently offering an empty menu. A single bad template is skipped
with its key named, and the rest survive — one typo should not cost the
feature.

Tests: the nine-template set from a real config round-trips; a body's newlines
survive the option; a malformed entry is skipped by key with the others
intact; an absent option gives a menu that says so rather than an empty one.

## OC.3a — a plugin's prompt submit actually fires ✅

Carved on the way into OC.3, because OC.4 cannot be built on a mechanism that
does not work — and because it is a live bug, not a missing feature.

`do_prompt_line_submit` looked its handler up in the `ActionHandlerRegistry`
ONLY. Native modes register there; the plugin seams do not — a plugin's grammar
action lives in the `CommandRegistry` with an `apply` closure. So
`Effect::OpenPrompt` was **unusable by any plugin**: the prompt opened, the user
typed, and the submit died with `prompt: no handler registered`. Org's capture
(OM.11, OC.2) and its `<leader>o:` tag prompt both took that path.

**Nothing caught it because every org test dispatches the submit action
directly rather than through a prompt** — the seam was wired end to end and the
one path a user actually takes was the one that did not work. Same shape as
`plugin-gates-hand-guests-throwaway-contexts`.

A missing native handler now falls through to the ordinary plugin-action
dispatch. The typed text arrives as the first argument; the name the caller
smuggled through `buffer-name` as the second, because a plugin gets a
`buffer-id` over WIT and cannot resolve a name from it the way a native handler
reads it off the buffer it is handed.

Tests (`prompt_submit_reaches_a_plugin_action.rs`, 4): a plugin grammar action
fires with the typed text; the smuggled name comes back beside it; a native
handler still wins and the fallback does NOT also run; an unknown action still
echoes.

## OC.3 — the capture transient ✅

Built from the parsed set: one `Action` row per template, keyed by its `key`,
labelled by its `description`, plus `q` → `Dismiss`. Each row's action is
`org-capture` carrying the template key in its own args (the TR.2a slot), so
one action serves every row and the key you press is what decides the template.
Built per open, never cached — a `:set` takes effect on the next `<leader>oc`.

Templates the set could not use are named in the menu's FOOTER: the one place a
missing row is noticeable is while the user is looking at the menu, and it is
once per open rather than once per capture.

**The prefix moved from `<C-x>o` to `<leader>o`.** The design fragment
specified `<C-x>o` and it cannot work — org's major binds a TERMINAL `<C-x>`
(timestamp decrement), and a prefix in one layer beside a terminal binding in
another needs an ambiguous-chord timeout this editor does not have. Found by
driving the real chord, not by reading the keymap. `<leader>o` breaks no muscle
memory (capture already lived at `<leader>oc`) and drops the `action:next-pane`
shadowing the fragment had accepted as a price.

**Two host holes surfaced, both the same shape as OC.3a's:**

1. **A plugin's transient row could never fire.** `TransientItemKind::Action`
   dispatched through the `ActionHandlerRegistry` only, so a plugin could build
   a menu (TR.2b) whose rows resolved and did nothing. Fixed the same way — a
   missing native handler falls through to the ordinary plugin-action dispatch,
   carrying the row's own args. It survived because the seam's tests convert a
   spec and never fire a row through the editor.
2. **The transient seam's store had no config registry**, so the guest's
   `get-option` answered `None` and the menu built from a template set it could
   not read — reporting "no capture templates" while `:set …?` showed a value.
   `spawn_transient_source` now attaches it, as the `context` seam does.

Tests (org, 4): the menu opens on the chord with a row per template in
declaration order plus a way out; the key you press decides which template
captures (asserted on the SECOND row, so a first-wins bug cannot pass); the
whole chain chord → menu → key → prompt → filed note through the real prompt;
a broken set leaves the menu closed and names the option. Host (1): a row
naming a plugin action fires it with the row's args.

Duplicate keys are a user error the menu cannot resolve: the first wins and
the later one is skipped with a warning naming both descriptions (OC.2).

## TR.3a — an open carries its arguments ✅

Design §6. `Effect::OpenTransient { source, args }`, reaching the builder as
`TransientContext::args`. Without it a menu cannot drill down: org's capture
menu has a row per template, and the fields menu that row opens must know which
template it is collecting for. The only alternative is guest memory, which
`<Esc>` never clears — so the next open would inherit the last one's subject.

**The same gap exists on `Effect::OpenSyntheticBuffer`**, and magit pays for it
with two `Mutex<HashMap<buffer-name, payload>>` side tables
(`ViewArgsRequests`, `BlameRequests`) whose doc says exactly why: the toggles
are answered before the buffer exists. Giving that effect an `args` field would
delete both. Deliberately deferred so org validates the shape on one path
first. `parse_buffer_name` is NOT part of this — it carries buffer identity,
which has to outlive any single open.

Tests: the args reach a native builder and cross the WIT boundary into a guest
(the fixture echoes them into its menu title, so the assertion is on data only
the guest could have produced); a plain open carries `Args::None` rather than a
stale subject.

## TR.3b — `Argument` rows cross the seam ✅

Mirror `TransientItemKind::Argument` over WIT so a plugin menu can collect named
values through the mechanism lattice already has: `PendingTransientArgument`
parks the whole menu, the value lands in `TransientState`, `resume_parked_
transient` puts the menu back, and `<Esc>` cancels the value with the menu
untouched. Magit's argument rows use it today.

**Chosen over sequential prompts with the answers encoded in `buffer-name`** —
that would have been a second spelling of a mechanism the editor already has.
(The `buffer-name` channel is real and documented, and magit's blame/diff/
revision modes use it for buffer identity; it is the *multi-step input* part
that was redundant.)

**The schema problem, answered.** `project_transient_state` maps
`TransientState` into an action's args through the command's **static**
`args_schema`, and `%^{}` questions come from an option read at capture time.
So when the fired command declares no schema, the host projects the menu's own
`Argument` rows in declaration order — the row order IS the schema, and org's
substitution is positional. An unanswered field projects its default rather
than being skipped, or the third answer would slide into the second slot.

**And a row can now say two things.** TR.2a had a row's args replace the state
projection; a menu that drills down is both a parameterised row and a set of
fields, so for a schema-less command the row's args come first and the fields
follow. The schema'd path is byte-identical, which is what the tests pin
hardest — every native menu goes through it.

Tests: a schema-less command reads the menu's rows in declaration order (with
an unanswered middle field holding its position); a row's args and the menu's
fields both arrive, in that order; a menu with no fields passes the row's args
through alone; an `Argument` row crosses the WIT boundary with its name, prompt
and default, `source` deferred.

## OC.4 — the `%^{Prompt}` fields ✅

A template with `%^{}` questions gets a per-template fields menu: one
`Argument` row per question, plus a row that captures. `<leader>oc` → the
template menu → a key → the fields menu → fill → fire.

Diverges from emacs org-capture, which asks sequentially. Chosen deliberately
(see TR.3b): it is lattice's existing mechanism, it keeps one surface rather
than flashing between prompts, and a form is re-editable in a way a
questionnaire is not.

**One registered source serves both menus.** The seam gives a guest one
`id()`, so which shape gets built is decided by what the open was FOR (TR.3a):
opened for nothing it is the template chooser, opened for a key it is that
template's fields. Two names would have needed two `id()`s.

Field names are positional (`q0`, `q1`, …) rather than the question text — a
template may ask the same question twice, and two rows sharing a state key
would overwrite each other. Expansion consumes them positionally too, so a
question that was never asked (an empty `%^{}`) consumes no answer; shifting
would substitute every later answer one slot early and look plausible.

Two submit actions rather than one that guesses: the prompt hop hands its
action `[text, buffer-name]` and the fields hop hands its action
`[key, answer…]`.

Tests (org): a three-question template shows a row per question in TEMPLATE
order and substitutes each answer at its own position; a template with no
questions still captures in one hop through the direct prompt; abandoning the
menu writes nothing and the next capture does not inherit the abandoned answer.
Unit (expansion): answers substitute in order, a question and `%?` coexist, a
missing answer leaves its slot without shifting the rest, an empty question
consumes nothing and stays visible, an unclosed one survives verbatim.

## OC.5a — targets ✅

**Carved out of OC.5 while executing it**, because the two halves turned out to
be independent and the first one uncovered a host gap the second does not have.

`file` appends. `file+headline` resolves the insertion line — find the headline,
anchor at `subtree_end + 1` — sharing `headline::subtree_end` with refile so the
two cannot drift. A missing headline appends **and echoes**: the note has been
typed already, and losing it is the one outcome capture must never produce.

Matching strips a leading TODO keyword and trailing `:tags:`, ignores case and
collapses inner spacing. A user names the target once in a config file; adding
`:drill:` to that headline months later must not silently redirect every future
capture to the bottom of the file. Deliberately the opposite of
`refile::title_of`, which keeps both because its consumer is a picker.

**It needed a host slice first, and that was not foreseen.** The design said the
guest would read the target "through WASI inside the existing `fs:` grant —
refile's mechanism". Neither half held. Refile never reads a file: it computes
its insertion line from buffer text handed to it by a picker, and pickers run on
the **async** linker. Capture is a *grammar action*, called synchronously on the
dispatch thread from a **sync** linker whose WASI filesystem shim blocks on a
runtime internally — so `std::fs::read_to_string` there panics instead of
reading. A comment in the host asserted the opposite, which is what made it look
safe to try.

So `host-services.read-file` landed first (a host-side read, gated on the same
`fs:` grant capture already needs to write) and org calls that. See
`plugin-manager.md`'s neighbour note and the corrected comment at the grammar
linker.

Tests: subtree-end insertion, nested headline stopping at its sibling,
last-headline-past-the-end, missing headline appending, tags and TODO keywords
not breaking a match, case/spacing insensitivity, body text never matching,
duplicates resolving to the first — all pure, plus three end-to-end through a
real component covering the headline target, the missing-headline echo, and a
plain `file` target still appending.

## OC.5b — `%t` and `%a` ✅

`%t` is the active date-only stamp, a sibling of the `%U` / `%T` already built.

`%a` is the interesting one: an org link back to where the capture fired. **The
origin is gone by submit time** — `open_prompt_line` focuses a synthetic prompt
buffer, so the `doc` reaching `capture_submit` is the prompt, not the source.
It has to be computed in `capture_open` (from `doc.path()` + `ctx.cursor.line`)
and smuggled forward through both hops: the prompt hop's `buffer_name` (already
carrying the template key) and the fields hop's `OpenTransientPayload.args`
(already carrying it too). Both are single-string slots, so this needs an
encoding — `refile::encode`'s tab-token is the precedent.

**Solved guest-side, not across the seam.** Both smuggle slots were wrong for
it: the prompt hop's `buffer-name` is shown to the user and would grow a file
path, and the fields hop's `args` already carries the template key — both would
need an encoding for something neither the host nor the menu uses. The origin is
state whose lifetime is exactly one capture flow, so it lives with the flow, in
a thread-local beside the `SCAN` one that does the same job for the agenda.

Consumed on use, and `capture_open` always writes (including the `None`), so an
abandoned capture cannot leave an annotation for the next one to inherit — a
link that would look right and point at a buffer the user was never in.

`%t` renders identically to `%T` today, and that is a gap in `%T`: org gives it
a time of day, and the only clock here reads whole days. `%t` is still correct
now, so templates using it keep meaning what they mean when `%T` is fixed.

Tests: `%t` is date-only and angle-bracketed (active, or the agenda never sees
it); `%a` substitutes where asked and does not consume a `%^{}` answer; an empty
annotation leaves no broken link; and end to end, the origin names the buffer
the capture fired FROM across two dispatches, and is not reused by the next
capture.

## OC.6 — docs, ledger, site ✅

`doc/org.md`'s placeholder table had listed `%?` / `%U` / `%T` / `%%` and
stopped — wrong since OC.4 shipped `%^{Question}`, and wronger with `%t` and
`%a`. A table that silently omits half a vocabulary is worse than no table,
because it reads as complete.

`%a` and headline targets each got a paragraph rather than a row: the row cannot
carry the part that matters (why `%a` is what makes capture-while-reading-code
worth having; that a headline match survives tags and TODO keywords, which a
user would otherwise discover by losing notes). The org README's option list
still named only the pre-OC.2 pair, and its capability note now says the write
grant already covers the read a headline target needs.

`implementation.md` records the two host slices this plan produced without
planning them.

---

## What this does NOT do

- **Clocking.** `:clock-in` / `:clock-resume` need persistent "currently
  clocked" state and a modeline contribution; deferred in `org-mode.md` §9 and
  still deferred here.
- **Capture contexts.** `%:from` / `%:subject` read a mail or org-protocol
  context lattice does not have. The placeholder would always be empty, which
  is worse than its absence.
- **Computed placeholders.** `%(org-id-new)` / `%(format-time-string …)` need
  a named vocabulary of computed values, which is its own design question.
