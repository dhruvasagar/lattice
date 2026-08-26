# Org capture overhaul — slice plan

> Design:
> [`../../architecture/org-capture.md`](../../architecture/org-capture.md)
> (the capture system) and
> [`../../architecture/plugin-transients.md`](../../architecture/plugin-transients.md)
> (the seam the menu needs).
>
> Sequences both, because the menu is what makes many templates usable and
> the seam is what makes the menu possible.

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred.

**Status:** 🚧 in progress (2026-08-26). TR.1 ✅, TR.2a ✅, TR.2b ✅ — the
seam is live. OC.1a ✅, OC.1 ✅, OC.2 ✅, OC.3a ✅, OC.3 ✅ — a multi-template
set is usable end to end: `<leader>oc` opens a menu, the key picks the
template, the prompt files the note. **OC.4 is next** (`%^{Prompt}`).

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
  └── OC.1a `default_modes` — a plugin may have more than one on by default
  org plugin
  ├── OC.1  org-global-mode + the <C-x>o prefix
  ├── OC.2  parse `org.capture-templates`
  ├── OC.3a a plugin's prompt submit actually fires (host)
  ├── OC.3  the capture transient
  ├── OC.4  the %^{Prompt} chain
  ├── OC.5  targets + the placeholder set
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
| OC.4 | `%^{Prompt}` chain via `OpenPrompt` + `buffer-name` state | 📝 |
| OC.5 | `file` / `file+headline` targets, `%a` / `%U` / `%T` / `%t` / `%%` | 📝 |
| OC.6 | Docs, ledger, site nav | 📝 |

Every slice ships four artefacts (CLAUDE.md heuristic #5): doc, bench where a
hot path is touched, tests covering the failure mode as well as the happy
path, graceful error handling. One slice, one commit, committed as it goes
green, `scripts/precommit.sh <crate>` before each.

---

## TR.1 — the registry is the editor's 📝

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

## OC.4 — the `%^{Prompt}` chain 📝

One `OpenPrompt` per placeholder, in template order, each submitting to org's
continue-action with the answers so far in `buffer-name` (design §5). The
final submit expands and writes.

State rides the payload, not guest memory: `<Esc>` dispatches nothing, so an
accumulator would never be cleared and the next capture would inherit it.

Tests: a three-placeholder template collects three answers in order and
substitutes each at its own position; abandoning midway writes nothing and
leaves no state behind; a template with no `%^{}` still writes in one hop.

## OC.5 — targets and the placeholder set 📝

`file` (append) and `file+headline` (after that headline's subtree, via the
insertion line read through WASI inside the existing `fs:` grant — refile's
mechanism). A missing headline appends and echoes.

Placeholders: `%?`, `%U`, `%T`, `%t`, `%a`, `%%`, unknown-verbatim.

Tests: a headline target lands after that subtree rather than in front of its
children; a missing headline appends and echoes; `%a` names the buffer and
line the capture fired from; an unknown `%x` survives.

## OC.6 — docs, ledger, site 📝

`doc/org.md` (capture section rewritten, the `<C-x>o` shadowing note), the org
README's option list, `implementation.md`'s ledger entry, both design
fragments' statuses, Zola sync.

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
