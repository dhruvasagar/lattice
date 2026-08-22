# Contributable registries — help topics and dashboard sections

**Status:** design. Slice plan:
[`../operations/slice-plans/archive/contributable-registries.md`](../operations/slice-plans/archive/contributable-registries.md).
Supersedes the runtime-doc-directory half of HD.6
([`help-docs`](../operations/slice-plans/archive/help-docs.md)) and specifies
DB.8 ([`dashboard`](dashboard.md) §10).

## 1. The gap

Two registries in the editor are built once, at boot, and are then
immutable for the process lifetime:

| Registry | Built | Held as |
|---|---|---|
| `HelpTopicRegistry` | `builtin_topics()`, boot | `Editor::help_topics: Arc<HelpTopicRegistry>` |
| `DashboardRegistry` | `builtin_registry()`, `lattice_dashboard::install` | a plain-value boot service |

Both are *documented* extensibility seams — `HelpTopicBody::Dynamic`
exists and its docstring names "any plugin-supplied source";
`DashboardRegistry::register` already implements replace-by-id and its
docstring calls that "the DB.8 plugin replace-by-id semantics". Neither
is reachable. A plugin cannot ship a `:help` page and cannot ship a
dashboard section, because there is no handle to write through after
boot has finished.

That is a paramount-goal-#2 hole in two of the surfaces a plugin most
obviously wants: its own documentation, and its own entry on the launch
page.

## 2. One mechanism

The fix is the same for both, and it is an idiom the tree already
carries four times over (`CommandRegistryHandle`,
`PickerRegistryHandle`, `KeymapHandle`,
`CompilationParserFactoriesHandle`):

```
type XRegistryHandle = Arc<ArcSwap<XRegistry>>;
```

Copy-on-write RCU. Reads are wait-free `.load()` snapshots; writes
clone-mutate-store and happen only on plugin load and unload. The
registry type becomes `Clone` — for `HelpTopicRegistry` that means its
topics move behind `Arc<HelpTopic>` so a clone is a refcount bump and
the `OnceLock` decompression cache is *shared*, not duplicated.

Three properties come with the idiom and are the reason it is the right
one here:

- **Read cost is unchanged.** `:help` and dashboard compose both
  snapshot once per invocation, not per line or per frame.
- **A load mid-read is coherent.** A plugin registering while a
  dashboard composes affects the next compose, not half of this one.
- **Teardown is by provenance, not by token.** Each contribution
  carries the host-issued `plugin_id` that produced it, and unload is
  `retain(|c| c.plugin_id() != Some(id))`. There is no list to record
  and therefore none to forget — the CM.6b argument, verbatim.

**Registration order.** Both handles must exist before
`lattice_plugin_loader::install` runs. `lattice_dashboard::install`
already sits at boot line 667 against the loader's 1808; the help handle
moves from its ad-hoc construction into a Phase-A `register_service`
alongside `ConfigRegistry`. Neither ordering is incidental — a handle
registered after the loader is a silent no-contribution, the
`NotWired` failure mode the loader's explicit `PluginLoaderError::NotWired`
variant exists to make loud.

## 3. Two seams, deliberately different shapes

The mechanism is shared; the WIT seams are not, and the difference is
principled rather than incidental.

**A help topic is data.** Its body is markdown that does not change
between the moment the plugin loads and the moment the user reads it.
So the guest hands the host a string at registration and the host keeps
it. Nothing about the plugin needs to be alive afterwards.

**A dashboard section is a function.** `DashboardSection::render`
takes a `DashboardCtx` — pane width, `ui.nerd_fonts`, editor version —
that the guest cannot know at load, and the whole point of DB.6's
recompose triggers is that those facts change. So the guest stays
instantiated and the host calls it.

This is the substrate-vs-mode-helper distinction applied one level out:
ask what the consumer actually reads, and let that pick the shape.
Making both data would freeze dashboard sections into text blind to the
icon palette; making both live would keep a `wasmtime::Store` alive per
documentation plugin for no gain.

### 3.1 `help` — data at registration

```wit
interface help {
    /// Register one free-form `:help` topic.
    ///
    /// Auto-namespaced by plugin id, like `theme.register-element`.
    register-topic: func(
        name: string,
        summary: string,
        body: string,
        related-commands: list<string>,
    ) -> result<_, string>;
}

world help-plugin {
    import help;
    import logging;
    import project;
    export register-help-topics: func();
}
```

The host calls the export once at load; the guest calls back into the
import once per page. Exactly the `theme-plugin` /
`register-theme-elements` precedent, which is exactly the
`config-plugin` / `register-options` precedent before it.

**Bodies ship inside the component.** A plugin's markdown is
`include_str!`'d at build time and baked into its `.wasm`, the same way
lattice's own docs are baked into the lattice binary. The docs travel
with the artefact that owns them, and a plugin's pages never enter the
lattice binary's own embedded-doc budget.

**Namespacing.** The host prefixes every registered name with the
plugin's id, so a plugin with id `fugitive` registering `status`
contributes `fugitive.status`. Collisions with builtins and between
plugins become structurally impossible rather than a policy the loader
has to enforce and the user has to debug.

One refinement on top of plain prefixing, because the common case is a
plugin with a single page: a topic registered under the plugin's own id
(or under the empty string) lands at the bare id. `:help fugitive` for
the main page, `:help fugitive.status` for the rest. Without it the
one-page case reads `:help fugitive.fugitive`, which no editor's `:help`
has ever looked like.

**Discovery.** Namespaced topics enumerate through the existing
`gen:help-topics` candidate generator, so `:help <Tab>` lists them with
no new surface. The generator reads the handle rather than a snapshot
taken at boot — otherwise a plugin's pages exist but cannot be found by
completion, which is the same gap one level down.

### 3.2 `dashboard` — live budgeted render

```wit
interface dashboard {
    /// Read-only facts a section renders against. Mirrors `DashboardCtx`.
    record ctx { pane-width: u32, nerd-fonts: bool, version: string }

    enum role { logo, cursor, title, tagline, section-heading, body, key, hint, link }
    enum align { left, center }

    variant link-target { command(string), topic(string), url(string) }

    record span { text: string, role: role, link: option<link-target> }
    record row { spans: list<span>, align: align }
    record fragment { rows: list<row> }

    /// Declare a section. `id` is NOT namespaced — replace-by-id is a
    /// stated DB.8 capability (§3.2 below).
    register-section: func(id: string, order: s32, default-enabled: bool)
        -> result<_, string>;
}

world dashboard-plugin {
    import dashboard;
    import logging;
    import project;
    export register-dashboard-sections: func();
    export render-section: func(id: string, ctx: ctx) -> fragment;
}
```

`register-dashboard-sections` runs once at load and declares ids;
`render-section` runs at every compose. The host wraps each declared id
in a `WasmDashboardSection` that implements the native
`DashboardSection` trait, so the registry's ordering, its
`dashboard.sections` selection, and the compositor treat a plugin
section and a builtin identically — which is what DB.1 wrote the trait
for.

**Where it runs, and the cost.** `render-section` is a **sync** call on
the host's sync linker (the one grammar and `error-parser` share),
carrying the Reflex-class `PluginBudget::grammar()` rather than the
generous lifecycle default. It executes on the actor thread inside
`Editor::compose_dashboard_sections`.

That is a real cost and worth naming plainly: a guest call lands on the
actor during `:dashboard`, at startup, and on a DB.6 recompose. It is
acceptable because dashboard composition is a `LatencyClass::Display`
action — an explicit user request, never per-keystroke and never
per-frame — and because the fuel budget bounds a pathological guest to a
bounded stall rather than a hang. A trap poisons the section: it
contributes nothing further this session and the rest of the page
composes, the `WasmErrorParser` contract verbatim.

The alternative — rendering off-actor and recomposing when the fragment
lands — is purer on paramount #1 and was rejected on UX. It makes the
launch page visibly reflow a frame or two after it appears, at startup,
which is precisely the "pixel change to content the user did not edit"
the UX contract vetoes. A bounded sub-millisecond call on an explicit
Display-class action is the better trade.

**Replace-by-id and unload.** DB.8 specifies add / replace / whole-author,
so section ids are deliberately *not* namespaced: a plugin replacing
`getting-started` is a supported thing to want. That makes unload
non-trivial in a way the help seam avoids — removing the plugin's
section must restore the builtin it displaced, not leave a hole.

The registry therefore *appends* rather than overwrites, and resolves an
id to its **last** registration. Shadowing is then a stack, and
`unregister_plugin` is a plain `retain` that resurfaces whatever was
underneath — no explicit save-and-restore bookkeeping, which is the kind
that gets forgotten on one of the three unload paths. A re-registration
by the same owner replaces in place, so a reload cannot grow the stack.

## 4. What this is not

- **Not a runtime doc directory.** The 2026-07-29 plan for HD.6 was
  `$LATTICE_RUNTIME/doc` with a five-step resolution chain and
  `<plugin>/doc/*.md` overlaid at load. It is retired as the plugin
  mechanism: it requires plugins to copy files into a shared directory
  at install time, and it separates a plugin's documentation from the
  artefact that owns it, so an unloaded plugin leaves its pages behind
  and a partially-installed one has pages for code that never loaded.
  Embedding in the component keeps docs and code in one artefact with
  one lifetime.

  The resolution chain remains available as a *builtin*-doc size lever
  if the embedded budget ever fires again, but it no longer has a
  second justification, and the budget is not currently near firing.
  See [`../operations/embedded-docs-budget.md`](../operations/embedded-docs-budget.md).

- **Not a `RwLock`.** A lock would serialise every `:help` lookup and
  every compose behind a mutex whose contention comes entirely from an
  event that happens a handful of times per session. RCU is strictly
  better shaped for read-mostly-write-rarely, and it is what every peer
  registry here already uses.

- **Not a new crate.** Neither seam carves out a dependency surface.
  Help topics belong to `lattice-help`, dashboard sections to
  `lattice-dashboard`, the WASM shims to `lattice-plugin-host`, and the
  drains to `lattice-plugin-loader` — every one of those crates already
  owns its domain. (Heuristic #6.)

## 5. Paramount-goal alignment

- **#1 Performance.** Help lookups and dashboard composes read a
  wait-free `ArcSwap` snapshot; neither is on the keystroke or frame
  path. The one new actor-thread guest call is bounded by the
  Reflex-class fuel budget and fires only on Display-class actions.
  Registration happens on the loader's off-boot-thread task.
- **#2 Extensibility.** The point of the work: a plugin can ship its
  own manual and its own launch-page section, through the same
  registries the builtins use, indistinguishable once registered.
- **#3 Modal editing.** Untouched.
- **#4 Asynchronicity.** Contributions land on the loader's spawned
  task and reach the screen through the ordinary registry read on the
  next `:help` / `:dashboard` — no tick-callback, no keypress
  dependency, because neither surface is a pushed async result.

**UX (higher court).** No new flicker surface: help pages are opened on
demand and the dashboard composes synchronously before it paints. The
namespaced `:help fugitive.status` is slightly more verbose than vim's
flat tag space; the bare-id refinement (§3.1) keeps the common case
identical to what a vim user expects.

## 6. Cross-references

- [`dashboard.md`](dashboard.md) §3.1 (the `DashboardSection` trait),
  §10 (DB.8 as a rejected-for-v1 alternative).
- [`plugin-host.md`](plugin-host.md) §5 (seam → registry drain), §12
  (the WIT is unstable until three real plugins exercise it).
- [`../operations/embedded-docs-budget.md`](../operations/embedded-docs-budget.md)
  — the retired runtime-directory plan.
- `wit/theme.wit` — the register-at-load precedent both seams follow.
- `crates/lattice-compilation/src/parser_factory.rs` — the RCU handle +
  teardown-by-provenance precedent.
