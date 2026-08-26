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

**Status:** 📝 planned (2026-08-26).

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
  └── TR.2  the `transient-source` seam
  org plugin
  ├── OC.1  org-global-mode + the <C-x>o prefix
  ├── OC.2  parse `org.capture-templates`
  ├── OC.3  the capture transient
  ├── OC.4  the %^{Prompt} chain
  ├── OC.5  targets + the placeholder set
  └── OC.6  docs, ledger, site
```

| Slice | Description | Status |
|---|---|---|
| TR.1 | `TransientSourceRegistry` service registered by `editor_boot` | 📝 |
| TR.2 | `transient-source` WIT seam + loader drain + teardown | 📝 |
| OC.1 | `org-global-mode` (Universal) owning `<C-x>o` / `oa` / `oc` | 📝 |
| OC.2 | Parse `org.capture-templates` (TOML-in-an-option) | 📝 |
| OC.3 | The capture transient, one key per template | 📝 |
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

Test: a booted editor with magit absent still resolves
`TransientSourceRegistryHandle`, and magit's own menus still build with it
present.

## TR.2 — the `transient-source` seam 📝

Mirrors `picker-source`: guest exports `id()` + `build(ctx) -> result<spec>`;
the host wraps the export as a registry builder and registers it under the
guest's id. Loader gains a `PluginSeam::TransientSource` drain and its
teardown counterpart.

v1 mirrors `Action` (crossing as a **command name**, resolved host-side — a
plugin cannot forge a `CommandId`) and `Dismiss`. `Submenu` / `Flag` /
`Argument` / `Variable` are 📝 with reasons in the design fragment;
`TransientSpec::preview` is a closure and cannot cross at all.

Tests: a fixture guest contributes a two-row menu that opens and fires; a
guest `err` from `build` leaves the menu closed with an echo naming the
plugin; an `Action` naming an unresolvable command drops that row and keeps
the rest.

## OC.1 — `org-global-mode` and the prefix 📝

A minor with `ActivationPolicy::Universal` (the `magit-global-mode`
precedent) owning `<C-x>o`: `oa` → agenda, `oc` → capture.

`<C-x>o` **shadows `action:next-pane`** — decided deliberately (design §6).
The slice includes the note in `doc/org.md`, because a silently-shadowed
emacs chord is exactly the kind of thing that reads as a bug later.

Moves capture off org-mode's major keymap, which is what makes it reachable
from a non-org buffer at all.

Tests: `<C-x>oc` resolves in a plain text buffer (the whole point); `<C-x>o`
is a prefix, not a terminal binding; the agenda chord still reaches `:agenda`.

## OC.2 — parse the templates 📝

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

## OC.3 — the capture transient 📝

Built from the parsed set: one `Action` row per template, keyed by its `key`,
labelled by its `description`, plus `q` → `Dismiss`. Each row's action is a
grammar action carrying the template key in its args.

Duplicate keys are a user error the menu cannot resolve: the first wins and
the later one is skipped with a warning naming both descriptions.

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
