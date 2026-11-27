# Plugins that extend plugins

**Status: design, decided 2026-11-27.** First consumer:
[`org-clients.md`](org-clients.md). Sequencing and status:
[`../operations/slice-plans/org-clients.md`](../operations/slice-plans/org-clients.md).

## 1. The gap

A plugin can already call another plugin's commands, if the owner lists them
in `exported_commands` (`manifest.rs`). That is half of extension. The other
half, adding to what another plugin is configured with, has no seam:

- **Options have one writer.** `set-option` on another plugin's option
  replaces the whole value. Two plugins contributing agenda files would
  overwrite each other, and either would overwrite the user. There is no
  merge, and the host could not write a correct one: how a contributed
  capture template ranks against the user's, or whether an unset
  `agenda-files` still means the project root, is org's rule, not the
  host's.
- **Load order is unstated.** Plugins load in declaration order on the
  `require` path and in directory order on the scan path. A plugin that needs
  org's options to exist when it activates has no way to say so, and gets a
  silent `false` from every call when it loads first.

## 2. `requires`: load after, or not at all

```toml
id = "org-clients"
requires = ["org"]
```

- **A plugin whose requirements are not loaded is parked, not failed.** Each
  successful load re-checks the parked set, and a plugin whose requirements
  are now met loads then. This makes order independent of both paths: a
  `lattice.toml` that declares org-clients before org still works.
- **A parked plugin is visible at once.** It is recorded under *Failed to
  load* with the reason a user can act on, "waiting for `org`, which is not
  loaded", and the record clears when it loads. There is no "loading has
  settled" moment to wait for: a plugin still waiting after boot shows the
  same line it showed during it.
- **A cycle is a failure of every plugin in it**, named as a cycle. It is a
  packaging bug; loading half of it would be a guess.
- **Unloading a required plugin unloads its dependents first**, and
  reloading it reloads them after. A dependent left loaded would hold
  contributions to options that no longer exist and rows naming commands
  that no longer resolve.
- **Not a version constraint.** `requires` names ids. A plugin that needs a
  minimum org version says so in its own docs; the plugin-API version already
  guards the ABI. A semver field can be added when a real break needs it.

## 3. Option contributions

The owner of an option decides whether others may add to it, and how the
additions combine. The host stores contributions and their provenance; it
never merges.

```wit
/// Owner: let other plugins contribute values to this option of yours.
/// `false` for an option that is not yours, or that a capability grant
/// follows (grants are the user's alone to widen).
accept-contributions: func(name: string) -> bool;

/// Contributor: add `value` to another plugin's option, validated against
/// the option's schema as `set-option-value` validates. Replaces this
/// plugin's previous contribution to that option; `none` withdraws it.
/// `false` when the option does not accept contributions or the value does
/// not fit.
contribute-option: func(name: string, value: option<config-value>) -> bool;

/// Owner: every live contribution to its option, in the order they were
/// first made.
record option-contribution { plugin: string, value: config-value }
option-contributions: func(name: string) -> list<option-contribution>;
```

- **The user's value is never touched.** `get-option` returns what the user
  set, as today. The owner reads contributions beside it and decides what
  they mean. Org's rules: `agenda-files` scans the user's entries (or the
  project root, when unset) **and** each contributed entry;
  `capture-templates` and `agenda-custom-commands` append contributed entries
  after the user's, so a user key always wins a collision.
- **The owner merges because the merge has meaning only the owner knows.**
  All three of org's contributable options are lists, and a host append would
  produce a list. But "unset scans the project root" has to survive a
  contribution, a user's template key has to beat a contributed one, and a
  contributed entry that does not parse has to be skipped and named with the
  plugin that sent it rather than failing the user's own. Each is the owner's
  rule for its own option, and a host merge would either flatten them or need
  a rule language to express them. One rule: the owner reads, the owner
  combines.
- **Provenance is the teardown.** Each contribution is stored under the
  contributor's manifest id. Unloading the contributor drops its
  contributions with the rest of its teardown, by id, as the
  contributable registries already do
  ([`contributable-registries.md`](contributable-registries.md) §2). An owner
  reloading keeps them: they are keyed by option name and come back into
  effect when it accepts again.
- **Contributions cannot widen a grant.** `accept-contributions` refuses an
  option a capability grant follows. And a contributed agenda directory is
  still read with org's own grant: a contribution pointing outside it fails
  the read exactly as the same path typed by the user would.
- **Visible.** `:describe-option org.agenda-files` lists each contribution
  under its plugin's name, after the value. A user asking "why is this folder
  in my agenda" gets the answer where they would look.
- **Live.** Options are read live, so the owner sees a contribution on its
  next read: the next agenda scan, the next capture menu. No event is needed
  for the cases here; one can be added when an owner caches.

## 4. Exported commands, the other half

Unchanged, and recorded here so the whole extension surface is on one page.
A plugin's commands are its own unless its manifest lists them in
`exported_commands`; another plugin may then name them from an action, a
picker row, a prompt or a transient row, and from `invoke-command`. Org-clients
names org's agenda-command and capture-group commands from its menu, so org
exports those two.

## 5. Rejected

- **A host-side merge per option type** (append for lists). It cannot keep
  an owner's rules: an unset default that must still apply, a precedence
  between the user's entries and contributed ones, and per-contributor error
  reporting, which needs provenance the merged value has lost.
- **Contributions as `set-option` with an append flag.** The user's value and
  the plugin's additions would become one value, so unloading the plugin
  could not take back what it added, and `:describe-option` could not say who
  added what.
- **Events (`org/register-template`) instead of options.** The owner would
  keep contributions in guest memory, which a reload of the owner loses and
  nothing restores; teardown on the contributor's unload would need a second
  event the contributor might never send.
- **Ordering by a numeric load priority.** A number says nothing about why,
  and two plugins picking priorities cannot know each other's.

## 6. Paramount goals

- **UX:** nothing changes on screen. A user sees contributions in
  `:describe-option` and a requires failure in `:plugins`.
- **#2 Extensibility:** protected; this is the seam. It stays WIT-complete: a
  plugin in any language uses it with no SDK help.
- **#1, #3, #4:** untouched. Contributions are read where options are read;
  parking happens on the loader's existing task.
- **Heuristic #6:** no crate. Contributions are an option-registry side table
  in `lattice-config`; `requires` is the loader's.
