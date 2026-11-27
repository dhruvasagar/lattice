# org-clients and plugins extending plugins — slice plan (FT)

> **Slice plan.** Sequencing, slice IDs, dependencies, status icons.
> Design contracts: [`../../architecture/org-clients.md`](../../architecture/org-clients.md)
> (the plugin and the org primitives it uses) and
> [`../../architecture/plugin-extension.md`](../../architecture/plugin-extension.md)
> (the host mechanism).

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred (not yet) · ❌ dropped (not at all).
Every non-trivial slice ships the four artefacts (doc + bench where perf-relevant
+ tests including failure modes + graceful error handling).

**Status: 🚧 FT.0–FT.2 ✅ and the host side of FT.4 on lattice `org-clients`; FT.4–FT.8 ✅ on org `org-clients` (org's release waits on FT.3); FT.3 (the API publish) is Dhruva's; FT.9–FT.10 ✅; FT.11 ✅ but its release, which waits on FT.3 with org's.**

Three repos, in dependency order: lattice (host), lattice-org-plugin (org),
lattice-org-clients-plugin (new). **One plugin-API publish** for the whole
feature (FT.3): every WIT change lands in FT.2, so audit FT.4–FT.11 against
it before publishing.

## Host — lattice

| Slice | What | Crates | Status |
|---|---|---|---|
| FT.0 | Design fragments `org-clients.md` and `plugin-extension.md`; this plan; ledger entry. No code. | docs | ✅ |
| FT.1 | Manifest `requires = [ids]`: parse + validate (no self, no empty id); the loader parks a plugin whose requirements are not loaded, retries the parked set after each load, records the remainder as failed with the missing id or the cycle; unloading a required plugin unloads its dependents first. No WIT change (manifest only). | `lattice-plugin-host` (manifest), `lattice-plugin-loader` | ✅ 2 manifest unit + 8 loader integration tests (`tests/requires.rs`); a parked plugin is recorded in `failed` at once rather than at a settle point |
| FT.2 | Option contributions: `accept-contributions` / `contribute-option` / `option-contributions` on `config.wit`; a per-option side table in the config registry keyed by contributor id; validation against the option's type; refusal for grant-followed and non-accepting options; teardown by provenance on contributor unload; `:describe-option` lists contributions. Bench: `option-contributions` read at 0 / 1 / 8 contributors. | `lattice-wit`, `lattice-config`, `lattice-plugin-host`, `lattice-host` | ✅ values are typed `config-value`s, not text; 4 registry unit tests, teardown withdrawal in `teardown.rs`'s test, end to end through the `config-guest` fixture (owner, contributor, owner reload); bench 34 / 73 / 240 ns. `:describe-option` is built in `lattice-host`, not `lattice-help`. |
| FT.3 | Plugin-API publish: bump `lattice-wit` and `lattice-plugin-api` (minor), CHANGELOG, refresh the user world. **Dhruva publishes.** | `lattice-wit`, `lattice-plugin-api` | 📝 |

## Org — lattice-org-plugin

| Slice | What | Status |
|---|---|---|
| FT.4 | Re-pin to the FT.3 API. `agenda-files`, `capture-templates`, `agenda-custom-commands` accept contributions; each reader merges: agenda files = user entries (or the project root when unset) + contributed entries; templates and custom commands = user's then contributed, first key wins; a malformed contribution is skipped and named with its plugin. | ✅ org `b2d00bf`, host `16acaa75`. The project-root default is an empty `roots()` entry, which the host now reads as the project root (the guest cannot compute it). With no user templates the default `t Task` row stays beside contributed ones; a failed user set still refuses the menu. Built against the local `lattice-wit` through a temporary `[patch.crates-io]`, dropped with the re-pin after FT.3 |
| FT.5 | Capture group rows: a template with key + description and no target is a group; members (keys extending it) leave the top menu; the group row drills down by reopening the capture transient with the group as its argument. Tests cover nested prefixes, a group with no members, a member whose group is missing (stays top-level). | ✅ org `53b5995`. A group with no members is skipped and named; roam refuses group rows |
| FT.6 | The chooser: `choose = "<dir>"` on a group; members without `file` take the chosen file. Guess from the buffer's path, then the running clock's file; the sub-menu title names the guess and its source; `c` opens a picker of the folder's `.org` files (direct children) and reopens the sub-menu with the answer; no guess ⇒ firing a member opens the picker first. | ✅ org `b46832c`. The clock's file goes through the plugin store (`clock/running-file`) because the events and grammar seams are separate stores; the buffer guess is computed in the actions, which have the Document, and passed through transient args. The change row takes the first free of `c` / `C` / `,` |
| FT.7 | `choose` on an agenda custom command: the dispatcher row opens the chooser picker with the guess first; `org-agenda-command` accepts filter terms after the key and passes them as scan args (`["e", "file:acme.org"]`). | ✅ org `96d4c06` |
| FT.8 | `exported_commands`: the agenda command and the capture-group opener; doc updates (`doc/org.md` capture groups, `choose`, contributions); org release on the new API. | 🚧 exports ✅ org `c21c0e5` (`org-agenda-command`, `org-capture-menu`; `org-choose-file` stays private because it routes to a command named in its args); docs ✅; the org release waits on FT.3 |

## org-clients — lattice-org-clients-plugin (new repo)

| Slice | What | Status |
|---|---|---|
| FT.9 | Scaffold: manifest (`requires = ["org"]`, fs grant following `org-clients.directory`), `org-clients-mode` (global), options `org-clients.directory` / `org-clients.key`, the three contributions made at activation (agenda folder, the `e` capture group with its five templates, the `e` dashboard command). Integration test: with org loaded, org's capture menu shows the group and an agenda scan includes a client file. | ✅ `lattice-org-clients-plugin` `0800e00`. Contributions are made at load AND again on `option-changed` for `org-clients.directory` / `org-clients.key` / `org.directory`, so `:set` needs no reload. No fs grant yet: org reads the folder under its own; the write grant arrives with FT.10's onboard. 5 integration tests load both real components, incl. org absent ⇒ org-clients waits and says so |
| FT.10 | `:org-clients-onboard` (name → slug → skeleton → open; refuses an existing slug), `:org-clients-find` (picker of the folder), `:org-clients-archive` (move the confirmed client file into `archive/`). | ✅ `lattice-org-clients-plugin` `d5fb8ed`. Onboard writes through `write-to-file` with `create_parents` (the first client makes the folder) then opens the file; an existing slug is refused by name. Archive asks, refuses unsaved edits (a rewrite of the file's own bytes is refused while its buffer is modified), writes the copy, and a follow-up command removes the original only when the copy on disk matches; an archived file of the same name is never overwritten. Away-from-command warnings use `ui::notify`: the component is instantiated on the grammar seam, whose linker has no `logging`. 10 integration tests |
| FT.11 | `<leader>e` menu (find, dashboard, capture, onboard, archive) with the guess in its title; `:help org-clients`; README with the Unscheduled `match = "-client"` line; first release. | 🚧 `lattice-org-clients-plugin` `8d1c29a`: menu, help and README ✅; dashboard and capture rows hand over to org's exported commands, so the guess is org's. The first release waits on FT.3. 15 integration tests, both plugins installed under their real embedded manifests |

## Deferred

| Slice | What | Status |
|---|---|---|
| FT.12 | A cross-file heading picker over the clients folder (`<leader>e /`). | ⛔ until the find-then-navigate path proves slow in use; the refile target collector is the base. |
| FT.13 | A repo → client guess source. | ⛔ until the clock guess proves insufficient; needs configuration the design otherwise avoids. |

## Sequencing

FT.1 and FT.2 are independent and can land in either order; both precede the
FT.3 publish. FT.4 needs FT.3. FT.5 → FT.6 (the chooser hangs off a group);
FT.7 needs FT.6's picker. FT.8 closes org. FT.9 needs FT.1 and an org release
with FT.4–FT.8; FT.10 and FT.11 follow it.

The clients folder must sit inside org's fs grant; see the design's §2.

FT.5 and FT.6 need no host change and can be built against the current API
while FT.1–FT.3 are in review; only FT.4 waits on the publish.
