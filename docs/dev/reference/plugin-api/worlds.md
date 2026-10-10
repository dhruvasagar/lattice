<!-- @generated from wit/ by crates/lattice-plugin-api (render.rs).
     Do not edit: run `UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api`. -->

# Worlds

A plugin component targets exactly one world. The world names the seams the plugin *imports* (host functions it may call) and *exports* (interfaces the host calls on it), plus freestanding functions — above all the `register-*` entry points the host calls once at load, where the plugin declares what it contributes. A plugin that needs seams from two worlds declares its own world that `include`s both.

## world `auto-pair-plugin`

The `auto-pair` bundled plugin's world (AP.1). ONE component providing three
seams — the multi-seam shape proven by the AP.1.0 spike:
  - **grammar** — the pairing actions (`auto-pair-open-*` / `auto-pair-close-*`
    / `auto-pair-backspace`), fired on insert-mode chords; `apply-action` reads
    the buffer around the cursor via the AP.0.1 `borrow<document>` handle,
  - **modes** — the `auto-pair-mode` minor mode owning the insert-mode keymap
    (the mode-ownership rule: bindings live at `MinorMode(auto-pair-mode)`,
    never the builtin layer),
  - **config** — the `auto-pair.style` / `auto-pair.close-key` options,
  - **help** — its own `:help auto-pair` page (CR.3). The markdown lives in
    this plugin's `doc/` and is `include_str!`'d into the component, so the
    plugin's manual travels with the plugin instead of inflating lattice's
    own embedded-doc budget.

`logging` is intentionally NOT imported: keeping it out of the combined world
keeps `log` off the sync grammar linker (the "no logging on the grammar hot
path" invariant). The host instantiates this same `.wasm` once per seam —
grammar sync, modes+config async — against the superset linkers (AP.1.0).

**Imports:** [`buffer`](buffer.md), [`config`](config.md), [`grammar`](grammar.md), [`help`](help.md), [`modes`](modes.md), [`tree-sitter`](tree-sitter.md), [`types`](types.md)  
**Exports:** [`grammar-callbacks`](grammar-callbacks.md)

**Entry points it exports**

### `register-grammar`

```wit
register-grammar: func()
```

### `register-modes`

```wit
register-modes: func()
```

### `register-options`

```wit
register-options: func()
```

### `register-help-topics`

```wit
register-help-topics: func()
```


## world `comment-plugin`

The `comment` bundled plugin's world (CM.3). One component, four seams:
  - **grammar** — the `comment-toggle` OPERATOR. The first operator any
    plugin has contributed, which is why CM.1 gave `apply-operator` the
    `borrow<document>` its four sibling callbacks already had: deciding
    comment-vs-uncomment, finding the indent column and stripping an
    existing leader are all reads of the range it was handed.
  - **modes** — `comment-mode`, the minor mode that owns everything here.
    `activation-policy = global` — every *document* buffer, not `universal`:
    `gc` over user-edited text is the point, and `gc` in `*messages*`, the
    file tree or a help popup is noise. Its keymap layer is where CM.2 binds
    the operator's chord, so `:set comment.enabled=false` takes the keys
    with it.
  - **config** — `comment.enabled` is auto-registered by the loader from
    `default_modes`; this seam carries the plugin's own options.
  - **help** — `:help comment`, `include_str!`'d from this plugin's `doc/`.

`tree-sitter` is imported because `grammar-callbacks`' signatures take a
`tree-snapshot`; the operator never queries it. The leader comes from the
plugin's own table keyed on the file extension (CM.3), because the host
does not expose comment syntax across the boundary and shipping the table
here is what Comment.nvim and every other editor's comment plugin does.

`logging` is intentionally NOT imported, for auto-pair's reason: it would
put `log` on the sync grammar linker, and the operator is on the keystroke
path.

**Imports:** [`buffer`](buffer.md), [`config`](config.md), [`grammar`](grammar.md), [`help`](help.md), [`modes`](modes.md), [`tree-sitter`](tree-sitter.md), [`types`](types.md)  
**Exports:** [`grammar-callbacks`](grammar-callbacks.md)

**Entry points it exports**

### `register-grammar`

```wit
register-grammar: func()
```

### `register-modes`

```wit
register-modes: func()
```

### `register-options`

```wit
register-options: func()
```

### `register-help-topics`

```wit
register-help-topics: func()
```


## world `completion-source-plugin`

The world a completion-source plugin implements: it exports `completion-source`
and imports the capability-gated `host-services` (`walk`, PH7.4b) a source may
use (e.g. a path-completion source). A second `bindgen!` reuses the `plugin`
world's generated `types` + `host-services` via `with:` so the crossed values
are the SAME Rust types the boundary round-trips.

**Imports:** [`host-services`](host-services.md), [`logging`](logging.md), [`project`](project.md), [`types`](types.md)  
**Exports:** [`completion-source`](completion-source.md)


## world `config-plugin`

The world a config/options plugin implements. Imports the `config` register +
read API; exports `register-options` (the host calls it once to drive
declaration — the guest invokes the imported `register-option` inside it, the
`register-events` precedent). Synchronous: registration only records into the
`ConfigRegistry` (no async, off any hot path).

**Imports:** [`config`](config.md), [`logging`](logging.md), [`project`](project.md)  
**Exports:** —

**Entry points it exports**

### `register-options`

```wit
register-options: func()
```


## world `context-plugin`

The world a context-provider plugin implements. Imports `tree-sitter` so the
host-owned `tree-snapshot` / `node` / `query` resources are in scope for the
`borrow<>` parameter above (the `grammar-plugin` precedent), and
`host-services` for the walk seam a provider may want. **Async** — the
producer is off the render path, so a 7th `bindgen!` reuses the `plugin`
world's generated `types` + `host-services` via `with:` so crossed values are
the SAME Rust types the boundary round-trips (`boundary_context.rs`).

**Imports:** [`host-services`](host-services.md), [`logging`](logging.md), [`project`](project.md), [`tree-sitter`](tree-sitter.md), [`types`](types.md)  
**Exports:** [`context`](context.md)


## world `dashboard-plugin`

The world a dashboard-contributing plugin implements.

`register-dashboard-sections` runs ONCE at load and declares ids;
`render-section` runs on every compose, for each id the guest declared.
Both are synchronous — see the interface docs for why.

**Imports:** [`dashboard`](dashboard.md), [`logging`](logging.md), [`project`](project.md)  
**Exports:** —

**Entry points it exports**

### `register-dashboard-sections`

```wit
register-dashboard-sections: func()
```

### `render-section`

```wit
render-section: func(id: string, ctx: ctx) -> fragment
```

Render one declared section. `id` is one the guest declared during
`register-dashboard-sections`; a guest that does not recognise it
should return an empty fragment rather than trap.


## world `decorations-plugin`

The world a decoration-provider plugin implements: it exports `decorations`
and imports the capability-gated `host-services` (`walk`, PH7.4b) a provider
may use (e.g. a git-gutter source reading the repo). **Async** (event delivery
is off the render path, like picker/completion): a 6th `bindgen!` reuses the
`plugin` world's generated `types` + `host-services` via `with:` so crossed
values are the SAME Rust types the boundary round-trips (`boundary_decoration.rs`).

**Imports:** [`host-services`](host-services.md), [`logging`](logging.md), [`project`](project.md), [`types`](types.md)  
**Exports:** [`decorations`](decorations.md)


## world `error-parser-plugin`

The world an error-parser plugin implements.

`feed` is called once per captured output line, in arrival order, for the
life of a compilation. `reset` is called before the first line of each
run.

**Imports:** [`error-parser`](error-parser.md), [`logging`](logging.md)  
**Exports:** —

**Entry points it exports**

### `reset`

```wit
reset: func()
```

Drop any pending multi-line state. Called at the start of a run.

### `feed`

```wit
feed: func(line: string) -> list<entry>
```

Feed one line; return the entries it completed (usually none).


## world `events-plugin`

The world an event-observing plugin implements (PH7.8). **Async** (unlike the
sync grammar seam): event delivery is OFF the keystroke path — the host owns
an mpsc and pushes each serialized `event` to `on-event` on the plugin's own
task, so a slow handler never delays a keystroke or another subscriber
(§5.10.4, paramount #4). Mirrors the picker/completion dedicated-world shape
(a 5th `bindgen!` reusing the `plugin` world's `types` via `with:` so crossed
values are the SAME Rust types `WitBoundary` round-trips, `boundary_event.rs`).

`register-events` is the host-called registration entry (the guest invokes the
imported `subscribe` inside it — the grammar `register-grammar` precedent);
`on-event` is the host→guest delivery. A plugin that observes nothing exports
an empty `register-events` (and an `on-event` the host never calls).

**Imports:** [`events`](events.md), [`host-services`](host-services.md), [`logging`](logging.md), [`multibuffer-view-registry`](multibuffer-view-registry.md), [`project`](project.md), [`types`](types.md)  
**Exports:** —

**Entry points it exports**

### `register-events`

```wit
register-events: func()
```

Called once by the host to drive subscription registration; the guest
calls the imported `events.subscribe(filter, handler)` inside it.

### `on-event`

```wit
on-event: func(handler: u32, ev: event)
```

Deliver one matching event to `handler` (host→guest, async). An error /
trap is the graceful-degradation guard: the host logs + skips this
delivery, the plugin stays subscribed, other subscribers are untouched
(§8; PH7.8c bounds it with the event budget, PH7.8d).

### `on-wake`

```wit
on-wake: func(id: wake-id)
```

OC.2: an armed `wake-every` came due. Same task, same budget, same
graceful-degradation contract as `on-event` — a trap here quarantines the
plugin exactly as one there does, and the actor keeps running for every
other plugin. A plugin that arms no wakes exports an empty body the host
never calls.


## world `grammar-plugin`

The world a grammar-extension plugin implements. **Fully synchronous** (the
PH7.7 fork) — no `exports: { default: async }` in the host `bindgen!`, so the
`register-grammar` + `grammar-callbacks` exports are sync-callable from the
dispatch thread. Imports the `grammar` register API; exports `register-grammar`
(the host calls it once to drive registration — the guest invokes the imported
`register-*` inside it) + the `grammar-callbacks` behaviors.

A 4th `bindgen!` reuses the `plugin` world's generated `types` via `with:` so
crossed values are the SAME Rust types `WitBoundary` round-trips
(`boundary_grammar.rs`). `import buffer` brings the host-owned `document`
resource so `apply-action` can take a `borrow<document>` (AP.0.1) — the host
implements `HostDocument` (backed by `DocumentResource`) and adds it to the
SYNC grammar linker. Text-reading *motions* (structural / word motions) can
reuse the same handle when a motion signature needs it; AP.0.1 wires the
action path only.

**Imports:** [`buffer`](buffer.md), [`grammar`](grammar.md), [`tree-sitter`](tree-sitter.md), [`types`](types.md)  
**Exports:** [`grammar-callbacks`](grammar-callbacks.md)

**Entry points it exports**

### `register-grammar`

```wit
register-grammar: func()
```


## world `help-plugin`

The world a help-contributing plugin implements. Imports the `help`
registration API; exports `register-help-topics`, which the host calls
once to drive declaration (the `theme-plugin` / `register-theme-elements`
precedent, and the `config-plugin` / `register-options` precedent before
it).

**Imports:** [`help`](help.md), [`logging`](logging.md), [`project`](project.md)  
**Exports:** —

**Entry points it exports**

### `register-help-topics`

```wit
register-help-topics: func()
```


## world `keymap-plugin`

The world a keymap-registration plugin implements. Imports the `keymap`
register API; exports `register-keymap` (the host calls it once to drive
registration — the guest invokes the imported `register-binding` inside it,
the `register-options` / `register-events` precedent). Async (registration is
off any hot path); the `keymap` host func itself is a synchronous, non-trapping
`bool` return.

**Imports:** [`keymap`](keymap.md), [`logging`](logging.md), [`project`](project.md)  
**Exports:** —

**Entry points it exports**

### `register-keymap`

```wit
register-keymap: func()
```


## world `language-plugin`

The world a language-contributing plugin implements. Imports the
`language` registration API; exports `register-languages`, which the host
calls once to drive declaration — the `help-plugin` / `register-help-topics`
precedent, shape for shape.

**Imports:** [`language`](language.md), [`logging`](logging.md), [`project`](project.md)  
**Exports:** —

**Entry points it exports**

### `register-languages`

```wit
register-languages: func()
```


## world `lighthouse-plugin`

The `lighthouse` bundled plugin's world (LH.1) — the language-server
manager. Design: `docs/dev/architecture/lighthouse.md`.

The seams, and why each is here:
  - **grammar** — the `:lsp-install` family of ex-commands.
    `apply-ex-command`'s signature takes a `borrow<document>` and an
    optional `borrow<tree-snapshot>`, so `buffer` and `tree-sitter` are
    imported for the SIGNATURE; this plugin reads neither.
  - **host-services** — everything it does: `host-platform` and `data-dir`
    to decide what to fetch and where, `http-download` /
    `extract-archive` / `set-executable` to fetch it, `register-server` to
    hand it to the editor, `output-*` to show the work, `store-*` to
    remember it.
  - **events** — a job's outcome arrives as `job-finished`, and that is
    the only place it arrives. The install is a state machine stepped from
    `on-event`.

The component is instantiated once per seam, and the two instances share
nothing but the store and the data directory: an ex-command starts a job on
the grammar instance, and the events instance hears how it went. Whatever
one must tell the other goes through `store-*`.

`logging` is intentionally NOT imported, the `auto-pair` rule: keeping it
out of the combined world keeps `log` off the sync grammar linker.

**Imports:** [`buffer`](buffer.md), [`events`](events.md), [`grammar`](grammar.md), [`host-services`](host-services.md), [`tree-sitter`](tree-sitter.md), [`types`](types.md)  
**Exports:** [`grammar-callbacks`](grammar-callbacks.md)

**Entry points it exports**

### `register-grammar`

```wit
register-grammar: func()
```

### `register-events`

```wit
register-events: func()
```

### `on-event`

```wit
on-event: func(handler: u32, ev: event)
```

### `on-wake`

```wit
on-wake: func(id: wake-id)
```


## world `media-plugin`

The world an inline-media provider implements.

Mirrors `decorations-plugin`: exports the producer, imports the
capability-gated host services a scan might need. Async, because
production is off the render path.

**Imports:** [`host-services`](host-services.md), [`logging`](logging.md), [`project`](project.md), [`types`](types.md)  
**Exports:** [`media`](media.md)


## world `modes-plugin`

The world a mode-declaring plugin implements. Imports the `modes` register
API; exports `register-modes` (the host calls it once to drive declaration —
the guest invokes the imported `register-mode` inside it, the
`register-grammar` / `register-events` precedent). Synchronous.

**Imports:** [`logging`](logging.md), [`modes`](modes.md), [`project`](project.md)  
**Exports:** —

**Entry points it exports**

### `register-modes`

```wit
register-modes: func()
```


## world `multibuffer-view-plugin`

The world a multibuffer-view plugin implements.

`host-services` is imported because a pull view's answer usually comes from
somewhere — a plugin store, a file it reads under its own grant. Nothing
here requires it: a view computing its excerpts from data it already holds
calls nothing.

**Imports:** [`host-services`](host-services.md), [`logging`](logging.md), [`multibuffer-view-registry`](multibuffer-view-registry.md), [`project`](project.md), [`types`](types.md)  
**Exports:** [`multibuffer-view-source`](multibuffer-view-source.md)

**Entry points it exports**

### `register-multibuffer-views`

```wit
register-multibuffer-views: func()
```

Called once at load. The guest registers each view it owns.


## world `picker-source-plugin`

The world a picker-source plugin implements: it exports `picker-source` and
imports the host seams it needs — the `buffer` `document` resource (the host
implements `HostDocument`; this `init(doc)` signature is what finally wires
the resource, deferred since PH7.3c) and the capability-gated `host-services`
(`walk`, PH7.4b). A second `bindgen!` reuses the `plugin` world's generated
`types` + `host-services` via `with:` so the crossed values are the SAME Rust
types the boundary round-trips.

**Imports:** [`host-services`](host-services.md), [`logging`](logging.md), [`picker-registry`](picker-registry.md), [`project`](project.md), [`types`](types.md)  
**Exports:** [`picker-source`](picker-source.md)

**Entry points it exports**

### `register-picker-sources`

```wit
register-picker-sources: func()
```

OR.5b: the host calls this once at load; the guest calls the imported
`register-picker-source` for each source it provides. The
`register-grammar` / `register-modes` / `register-options` shape.


## world `plugin`

The lifecycle surface every guest component implements.

**First consumer is the user's `init.rs`** — compiled to WASM and loaded
by the host with a boot-capability set (CLAUDE.md tech stack; design.md
§5.12.2). Its `activate` runs the user's configuration: registering
keymaps, autocmds, hooks, and custom commands through the (stubbed here)
host-services / grammar / command / config / events interfaces. A plugin
is just another component implementing this same world with a narrower
capability grant — the bundled `plugins/` are exactly that — but none of
them is the first consumer.

The degenerate case — a component whose `activate` registers nothing — is
the empty `init.rs`, and is exactly what the PH7.0 scaffold instantiates
to prove the host round-trip end to end.

The async ABI (design.md §5.5) and the `on-event` lifecycle export land
with the runtime core (PH7.1) and the event seam (PH7.8) respectively;
PH7.0 wires the two synchronous exports needed to prove instantiation.

**Imports:** [`buffer`](buffer.md), [`host-services`](host-services.md), [`logging`](logging.md), [`project`](project.md), [`types`](types.md), [`ui`](ui.md)  
**Exports:** —

**Entry points it exports**

### `activate`

```wit
activate: func()
```

Called once when the component is first instantiated. Runs config /
contribution registration for `init.rs`; no-op for the scaffold.

### `deactivate`

```wit
deactivate: func()
```

Called on teardown / reload (the Guard-`Drop` teardown seam, §8).


## world `plugin-manager-plugin`

The world a config plugin (`init.rs`) implements when it declares plugins.

Deliberately a *separate* world from the fixture worlds that came before it
rather than an extra import on one of them: a plugin that only contributes
a grammar has no business holding the `require` capability, and worlds are
how that stays true by construction.

**Imports:** [`logging`](logging.md), [`plugin-manager`](plugin-manager.md), [`project`](project.md)  
**Exports:** —

**Entry points it exports**

### `register-plugins`

```wit
register-plugins: func()
```


## world `project-plugin`

The `project` bundled plugin's world (PC.4).

A `project.el`-style command layer: choose the project FIRST, then the verb.
Design: `docs/dev/architecture/project-commands.md`.

The seams, and why each is here:
  - **grammar** — the `:project-*` ex-commands. `apply-ex-command`'s
    signature takes a `borrow<document>` and an optional
    `borrow<tree-snapshot>`, so `buffer` and `tree-sitter` are imported for
    the SIGNATURE even though this plugin reads neither: it works on paths
    and roots, never on buffer text.
  - **project** — `root-for-buffer` / `root-for-path`. The plugin READS the
    root and never supplies one, which is the boundary `project.wit`'s own
    header draws: resolution is core and can never depend on a plugin being
    alive.
  - **host-services** — the persisted project list, via `store-*`. Gated on
    `state:write`, which is the plugin's ONLY capability: it holds no `fs:`
    grant because the host resolves roots and the native pickers do every
    walk.
  - **events** — `document-opened`, which is how a project comes to be
    remembered at all (project.el's `project-remember-project`).
  - **picker-source** (PC.5) — the `projects` picker. Registered through the
    `picker-registry` IMPORT rather than by the component being one source,
    which is the shape OR.5b moved every picker plugin to.
  - **modes / config / transient-source** (PC.6) — `project-mode` (a
    `universal` minor owning both prefixes' chords), the
    `project.switch-commands` option, and the menu those chords open.
    `transient-source` is a ONE-per-component export, so the single source
    dispatches on `transient-context.args` — org's shape.

`logging` is intentionally NOT imported, the `auto-pair` rule: keeping it out
of the combined world keeps `log` off the sync grammar linker. A guest that
calls it makes the component IMPORT it, and an unwired linker then fails the
WHOLE component rather than that one call.

**Imports:** [`buffer`](buffer.md), [`config`](config.md), [`events`](events.md), [`grammar`](grammar.md), [`help`](help.md), [`host-services`](host-services.md), [`modes`](modes.md), [`picker-registry`](picker-registry.md), [`project`](project.md), [`tree-sitter`](tree-sitter.md), [`types`](types.md)  
**Exports:** [`grammar-callbacks`](grammar-callbacks.md), [`picker-source`](picker-source.md), [`transient-source`](transient-source.md)

**Entry points it exports**

### `register-grammar`

```wit
register-grammar: func()
```

### `register-picker-sources`

```wit
register-picker-sources: func()
```

### `register-modes`

```wit
register-modes: func()
```

### `register-options`

```wit
register-options: func()
```

### `register-help-topics`

```wit
register-help-topics: func()
```

### `register-events`

```wit
register-events: func()
```

### `on-event`

```wit
on-event: func(handler: u32, ev: event)
```

### `on-wake`

```wit
on-wake: func(id: wake-id)
```


## world `scanned-excerpt-source-plugin`

The world an scanned-excerpt-source plugin implements.

`extensions` is called ONCE at load and cached; `begin` then `scan` are
called per scan, `scan` once per matching file in walk order.

**Imports:** [`config`](config.md), [`logging`](logging.md), [`project`](project.md), [`scanned-excerpt-source`](scanned-excerpt-source.md), [`tree-sitter`](tree-sitter.md), [`types`](types.md)  
**Exports:** —

**Entry points it exports**

### `extensions`

```wit
extensions: func() -> list<string>
```

File extensions this source wants offered, WITHOUT the leading dot
(`["org"]`). Matched case-insensitively. Called once at load.

This export is why the host does not know what an org file is. Two
alternatives were rejected: offering every project file to every
source (one boundary crossing carrying the full text of every file
in the tree — the producer-critical-path cost §7 warns about), and
resolving the extensions from the plugin's `language` seam, which
would make an agenda source *require* a language when the two are
independent contributions.

A source returning an empty list scans nothing and is logged at
load — a silently-inert producer is the `NotWired` failure the host
spends effort avoiding elsewhere.

### `view-mode`

```wit
view-mode: func() -> option<string>
```

A MINOR mode this source wants activated on the agenda view, by id.
Called once at load, beside `extensions`.

This is how a source gets to act on its own rows. The view's generic
behaviour — jump-to-source, `gr` refresh — belongs to the host, which
built the view and is the only thing that can re-walk it. But
*changing a TODO state from the agenda* is the source's semantics, and
it needs chords in a buffer whose major is `multibuffer-mode`.

An activation policy cannot express "the buffer this provider just
built": `majors(["multibuffer-mode"])` would fire the source's chords
in search results and diffs too. So the provider activates it, and
this is the source naming what to activate.

`none` for a source that only produces rows. A `major` named here is
refused with a warning — the view already has one, and replacing it
would take the multibuffer's own motions away.

### `roots`

```wit
roots: func() -> list<string>
```

AF.1: the paths this source wants scanned. Called per scan.

Each entry is a **file** or a **directory**: the host walks a directory
(applying `extensions` as it does today) and takes a file as given
without asking whether anyone claims its extension — naming a file IS
the claim. `~` is expanded host-side. Relative paths resolve against the
editor's working directory.

**Empty means "no opinion", not "scan nothing".** The host then uses the
root it would have used anyway, so a source that does not implement this
behaves exactly as it did before, and a user who has configured nothing
keeps the project-root scan.

**Per scan, unlike `extensions` and `view-mode`.** Those are facts about
the source and are resolved once at load. This one comes from user
configuration and has to follow a `:set` without a reload — caching it
would make the setting appear not to work until the editor restarted,
which is the worst shape of "configured and behaving as if it were not".

The grant is unchanged: every path still passes the host's `fs` check,
so naming a directory does not acquire the right to read it. A path
outside the grant, or one that does not exist, is skipped with a log and
the rest of the scan continues — `error-parser`'s rule, because one bad
entry in a config list is the same failure class as one bad file.

##### Why the source answers this and not the host

The host owns the walk, so the host owning the *list* is the obvious
design. It is not the one here, because the list is the user's
configuration and every source's users already have a name for it in
whatever ecosystem the source came from. A host-side setting would have
to pick one of those names, or invent a neutral one nobody's fingers
know.

So each source configures its own file set under its own option, and the
host never learns any option's name — it asks and merges. Two sources
with completely different configuration vocabularies coexist without a
line of host code knowing either.

### `begin`

```wit
begin: func(args: list<string>) -> u64
```

Drop per-scan state, and declare what would invalidate this scan's
results. Called before the first file of a scan.

Every scan is a fresh one — `gr` re-runs from `begin`, so a guest
accumulating across a scan (a "seen headlines" set, a today anchor
captured once) clears here rather than leaking into the next.

##### The return value is a GENERATION KEY (OT.3b)

An opaque `u64` the guest computes from everything scan-wide that
changes what its rows would say — for org, the day the scan is anchored
to and the configured TODO keywords. The host caches results under it
and discards the whole cache when it changes.

**Opaque on purpose.** The host must be able to persist a scan's rows
across restarts without re-running the guest, and it cannot key that on
state it can only guess at: `today` and the keyword set live inside the
guest. Handing back an integer lets the guest declare its own
invalidation without the host learning what a date group or a TODO
keyword is — the property this whole seam is built to protect.

A guest with nothing to declare returns a constant, and its results are
then cached until the files themselves change.

##### `args` — what this particular scan was asked for (OA.11a)

The `scan-args` the view was opened with, verbatim. **The host never
interprets these** — they are the guest's own vocabulary, and the host
carries them the way it carries a generation key: as something it can
route but not read.

This is what lets one source serve more than one scan. Org's agenda
dispatcher opens "Waiting and Postponed" by name; without a channel
here, the guest would have to publish its selection through an option
and read it back, which makes view state look like user configuration
and leaves the host unable to tell that the view is parameterised at
all.

Distinct from the provider view's `argument`, which is the **root
override** and is host-interpreted precisely because the host does the
walk. The two are separate slots because they have separate owners:
folding them together means a command key gets taken for a directory
path, and the scan silently covers nothing.

`begin` runs before `roots`, so args stashed here are in hand for
`roots`, every `scan` call and the generation key — which is where they
belong, since a scan parameterised differently must not read a cache
filled by the previous one.

Empty is the ordinary case and means "the default scan".

### `describe`

```wit
describe: func(args: list<string>) -> string
```

OA.22: what this view IS, in the guest's own words, for its headerline.

The host knows how many rows it composed and how many files it walked,
and that is all it can say — it deliberately does not read `args` (see
`begin`). So an agenda narrowed to one tag looks exactly like an
unfiltered one, and "you have no tasks" is the worst thing this view can
say incorrectly. A forgotten filter is the likeliest way to make it say
so.

Return a short phrase naming the command, the span and any active
filters — `"Waiting · next 7 days · +work"`. The host prefixes its own
counts, so do NOT repeat them; empty means "nothing worth saying" and
the header keeps the plain form.

**Also the only place a bad view argument can be reported.** A guest
`logging::log` call makes the component import `logging`, which org's
multi-seam linker does not wire on every seam — the whole component then
fails to instantiate, a trap this plugin has paid for more than once. So
an unrecognised argument has nowhere else to surface, and silently
dropping it means a typo'd command shows the default agenda while
looking like the one that was asked for.

Called ONCE per scan, after `begin`, so it sees the args `begin` stashed.
Off the per-file path by construction: one crossing per scan, not one
per file.

### `scan`

```wit
scan: func(path: string, text: string, tree: option<borrow<tree-snapshot>>) -> result<scan-result, string>
```

Scan one file; return its agenda rows AND the time clocked in it.

`path` is absolute. Rows may be returned in any order; the host
stable-sorts every file's rows together on `sort-key`.

An `err` skips this file with a `debug` log and the scan continues.
One bad file must not fail the agenda.

##### Why a record rather than a bare row list (OA.14b)

The clock report is not a view of the agenda's rows. Emacs's clocktable
totals every clocked headline in the agenda files; agenda rows are a
FILTERED subset, so a headline clocked yesterday with no TODO and no
date is not a row at all. Hanging clock data off `entry` would report
only the time that happened to land on a row and silently drop the
rest — and a clock report that under-reports is worse than none, since
nothing distinguishes a quiet week from a lossy one.

It rides this call rather than an export of its own so the walk still
makes ONE guest call per file. The scan is a producer's critical path
(§7); a second crossing per file would double it to carry data most
files have none of.


## world `sign-plugin`

The world a sign-contributing plugin implements. Imports the `signs`
registration API; exports `register-signs`, which the host calls once to
drive declaration (the `theme-plugin` / `config-plugin` precedent).
Synchronous work behind an async export: registration only records into the
registry and is off every hot path.

**Imports:** [`logging`](logging.md), [`project`](project.md), [`signs`](signs.md)  
**Exports:** —

**Entry points it exports**

### `register-signs`

```wit
register-signs: func()
```


## world `theme-plugin`

The world a theme-contributing plugin implements. Imports the `theme`
registration API; exports `register-theme-elements`, which the host calls
once to drive declaration (the `config-plugin` / `register-options`
precedent). Synchronous work behind an async export: registration only
records into the registry and is off every hot path.

**Imports:** [`logging`](logging.md), [`project`](project.md), [`theme`](theme.md)  
**Exports:** —

**Entry points it exports**

### `register-theme-elements`

```wit
register-theme-elements: func()
```


## world `transient-source-plugin`

The world a transient-source plugin implements.

It exports `transient-source` and imports the host seams a builder plausibly
needs to decide its rows — `config` is absent deliberately: a plugin that
reads its own options declares the `config` seam separately and both drain
into the same component.

**Imports:** [`logging`](logging.md), [`project`](project.md), [`types`](types.md)  
**Exports:** [`transient-source`](transient-source.md)


## world `treesitter-context-plugin`

The `treesitter-context` bundled plugin's world (TC.5). ONE component
providing the multi-seam shape `auto-pair` proved:
  - **context** — the scope producer (this file's `context` interface),
  - **config**  — the `context.*` options,
  - **grammar** + **modes** — the mode, its `[u` chord and `:context-toggle`.

It does NOT provide **theme**. It used to, registering four elements no
renderer read: the strip is host chrome, so its appearance belongs to the
host's `sticky.context.*` set. An element that resolves in
`:describe-element` but never paints is worse than an absent one.

The host instantiates this same `.wasm` once per seam against the superset
async linker, exactly as it does for `auto-pair`. A component exporting more
than a given world requires is fine; each `spawn_*` matches only what its
own world declares.

**Imports:** [`buffer`](buffer.md), [`config`](config.md), [`grammar`](grammar.md), [`help`](help.md), [`modes`](modes.md), [`tree-sitter`](tree-sitter.md), [`types`](types.md)  
**Exports:** [`context`](context.md), [`grammar-callbacks`](grammar-callbacks.md)

**Entry points it exports**

### `register-options`

```wit
register-options: func()
```

### `register-grammar`

```wit
register-grammar: func()
```

### `register-modes`

```wit
register-modes: func()
```

### `register-help-topics`

```wit
register-help-topics: func()
```

