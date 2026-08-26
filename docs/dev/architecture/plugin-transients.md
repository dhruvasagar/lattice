# Plugin-contributed transient menus

**Status:** TR.1, TR.2a, TR.2b landed — the seam is live. Extends
[`plugin-host.md`](plugin-host.md)
(the seam vocabulary). Slice plan:
[`../operations/slice-plans/org-capture.md`](../operations/slice-plans/org-capture.md),
which sequences this together with the org capture overhaul that motivated
it.

## 1. What is missing

A transient is a keyed menu: one keystroke per row, fires and closes. The
mechanism is **`lattice-picker`'s** — `TransientSpec`, `TransientGroup`,
`TransientItem`, and a `TransientSourceRegistry` of named builders. Magit is
its only *user*, not its owner.

A plugin cannot contribute one. `Effect::OpenTransient(name)` crosses the WIT
boundary and opens a menu registered under `name`, but there is no seam that
*registers* one — so a plugin can open magit's menus and none of its own.

Two things follow, and the second is the sharper:

- Org's capture menu (`<C-x>oc`, one key per template) is inexpressible.
- **The registry only exists if magit is installed.** `lattice-magit::install`
  constructs it and calls `register_service::<TransientSourceRegistryHandle>`.
  A plugin transient would therefore work or not depending on whether an
  unrelated feature crate happened to load — a dependency nothing declares and
  nothing would explain at the point of failure.

## 2. The thesis

> A plugin registers a named menu through the same registry magit uses, and
> the registry exists because the editor has a picker, not because it has
> magit.

## 3. TR.1 — the registry is the editor's

The service registration moves to `editor_boot`, beside its sibling
`PickerRegistryHandle`. Magit keeps registering its own *sources* into it and
loses only the `new()` + `register_service` pair.

This is not tidying. It is the difference between "org's menu depends on the
picker" (true, and declared) and "org's menu depends on magit" (accidental,
and invisible until it fails).

## 4. TR.2a — a build that can answer later

The registry's builders were `Fn(&TransientContext) -> TransientSpec`:
synchronous, because every native menu is a pure function of the open
context. A guest's `build` cannot be — it is an async call on the plugin's
own actor task, and blocking the editor actor on it is paramount-#4
territory. So a builder now answers a value:

```rust
pub enum TransientBuild {
    Ready(TransientSpec),
    Future(TransientBuildFuture),
}
```

Native builders answer `Ready` through the unchanged `register`, and seat in
the frame the chord fired — nothing about magit changed. A guest-backed one
registers through `register_async` and answers `Future`; the host spawns it on
the plugin runtime, parks it in `Editor::pending_transient_build`, and seats it
from `drain_pending_transient_build` on the **async-landed wake** — never on the
next keystroke, which is the failure mode `SubsystemBoot::inbound` exists to
design out. A second open supersedes the first (its token is cancelled), for the
same reason `pending_picker_init` is single-slot: the user pressed another chord.

Making that difference a *value* rather than a second registry is what keeps
`Effect::OpenTransient { source }` one code path — the effect still carries only
a name, and neither the chord nor the ex-command that emits it knows which kind
of builder answers. The whole open body moved onto `Editor::open_named_transient`
in the same slice, so the two renderer peers are one call rather than two copies
that would each have needed the async path written into them.

### Per-row arguments

`TransientItemKind::Action` gained an `args: Args` slot, and this is a
correction the seam forced rather than a convenience. A row's arguments used to
come from exactly one place: the menu's `TransientState`, projected through the
fired command's `args_schema` (MG.17a — the flags the user toggled before
pressing the key). That is per-MENU, so it cannot express *"this row means
template `t`, that one means `n`"* — which is the shape every plugin menu has,
starting with the one that motivated this fragment. Without the slot, org's
capture menu would need one registered command per template, and templates are
a config option read at capture time, not at plugin load.

Native rows build through `TransientItemKind::action(cmd)` and leave it
`Args::None`, keeping today's behaviour exactly. A row that fills it wins over
the state projection, because the row's args were chosen when the row was built
and the state's were not. `Variable` keeps no slot: its action prompts for its
own value.

## 5. TR.2b — the seam

Mirrors `picker-source` exactly, because the shapes are the same: a named
thing the host asks a guest to build, given a context the host owns.

```wit
interface transient-source {
    use types.{transient-spec, transient-context};

    /// The menu's name, as `Effect::OpenTransient` will name it.
    id: func() -> string;

    /// Build the menu for the place it was opened from. The host calls this
    /// per open, not once at registration: a builder's rows depend on where
    /// the user is, which is exactly why `TransientContext` exists.
    build: func(ctx: transient-context) -> result<transient-spec, string>;
}
```

The host wraps those two exports as a `register_async` builder (§4): `id()` is
called once, at load, to name the registry entry; `build` per open, its future
parked and seated on the async-landed wake.

`transient-context` is the owned projection of `TransientContext`
(`major-mode`, `minor-modes`, `buffer-id`) — the `picker-context` precedent.

### What crosses, and what does not

`TransientItemKind` has six variants. v1 mirrors **two**:

| Variant | v1 | Why |
|---|---|---|
| `Action` | ✅ | The whole point. Crosses as a **command name plus its `args`**, the name resolved to a `CommandId` host-side — a plugin cannot forge an id (§7, the `register_*` rule). The args are the per-row slot TR.2a added; without them a menu whose rows differ only in a parameter is inexpressible. |
| `Dismiss` | ✅ | Free, and a menu without `q` is a trap. |
| `Submenu` | 📝 | `Arc<TransientSpec>` is recursive; the WIT mirror needs the same care `Range`'s recursion needed. No consumer yet. |
| `Flag` / `Argument` | 📝 | Both round-trip `TransientState` through park/resume. A real second seam, not a field. |
| `Variable` | 📝 | Prefetched external value + an action that prompts. Wants the config seam more than the transient one. |

`TransientSpec::preview` is a `Box<dyn Fn(&TransientState) -> String>` and
does **not** cross at all — a closure has no WIT form. A guest spec gets
`preview: None`. Saying so here rather than discovering it at bindgen.

## 6. Failure behaviour

A guest `err` from `build` is logged and the menu does not open, with an echo
naming the plugin — the `picker-source::init` rule, and for the same reason: a
menu that opens empty is worse than one that says why it did not.

An `Action` naming a command that does not resolve is dropped from the menu
with a `debug!`, not an error. A plugin whose sixth row references a command
it failed to register should still get the other five, and the alternative —
refusing the whole menu — makes one bad row cost the feature.

## 7. Paramount-goal alignment

**#2 Extensibility.** This is the goal the seam serves: a keyed menu is a
first-class UI primitive and was reachable only by native crates.

**#1 Performance.** `build` is called on menu open — an explicit user action,
never per keystroke or per frame. It is an async guest call on the plugin's
own task, like `picker-source::init`.

**#4 Asynchronicity.** Same shape as the picker seam: the host parks, the
guest builds on its own store, the menu seats when the result lands.
