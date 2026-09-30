<!-- @generated from wit/ by crates/lattice-plugin-api (render.rs).
     Do not edit: run `UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api`. -->

# `transient-source`

**Direction:** guest implements this interface · **Capability:** none (pure data / dispatch) · **Worlds:** `project-plugin` (exports), `transient-source-plugin` (exports)

TR.2b: plugin-contributed transient menus.

A transient is a keyed menu — one keystroke per row, fires and closes. The
mechanism belongs to `lattice-picker` (`TransientSpec`,
`TransientSourceRegistry`); magit is its first *user*, not its owner. Until
this seam a plugin could `Effect::OpenTransient` one of magit's menus and
none of its own, which made org's capture menu — one row per template —
inexpressible.

### Mirrors `picker-source`, because it is the same shape

A named thing the host asks a guest to build, given a context the host
owns: `id()` names the registry entry once at load, `build(ctx)` produces
the menu per open.

### Per open, not once at registration

A builder's rows depend on where the user is — which is why
`transient-context` exists at all, and why the host calls `build` on every
open rather than caching a spec. Emacs magit answers the same question with
`:if-mode` / `:if-derived` predicates on its prefixes; the two mode axes
are separate fields here for the same reason.

### Where it runs

On the plugin's own actor task, off the editor actor. `build` is reached by
an explicit user action (a chord, an ex-command) — never per keystroke and
never per frame — and the host parks on it, seating the menu when it lands.
A slow guest delays its own menu and nothing else.

## Uses

- [`transient-spec`](types.md#record-transient-spec) from [`types`](types.md)
- [`transient-context`](types.md#record-transient-context) from [`types`](types.md)

## Functions (2)

### `build`

```wit
build: func(ctx: transient-context) -> result<transient-spec, string>
```

Build the menu for the place it was opened from.

An `err` is echoed with the plugin named and the menu does NOT open —
the `picker-source::init` rule, and for the same reason: a menu that
opens empty is worse than one that says why it did not.

**Example — Build a transient menu from config, with its subject passed in `ctx.args`** · [`plugins/project/src/lib.rs`](../../../../plugins/project/src/lib.rs)

```rust
/// One row per configured command, each carrying the chosen root.
///
/// The root rides `ctx.args` (TR.3a) rather than guest memory, and that is
/// the whole reason TR.3a exists: guest state is never cleared by `<Esc>`,
/// so a remembered subject would leak into the next open — the menu would
/// act on the project you looked at last rather than the one in front of
/// you.
fn build(ctx: TransientContext) -> Result<TransientSpec, String> {
    let Args::String(root) = &ctx.args else {
        return Err("project: the switch menu was opened without a project".to_string());
    };
    let root = root.trim();
    if root.is_empty() {
        return Err("project: the switch menu was opened without a project".to_string());
    }
    let mut items: Vec<TransientItem> = switch_commands()
        .into_iter()
        .map(|row| TransientItem {
            key: vec![row.key],
            label: row.label,
            description: String::new(),
            kind: TransientItemKind::Action(TransientAction {
                command: row.command,
                args: Args::String(root.to_string()),
            }),
        })
        .collect();
    // A menu with no way out is a trap.
    items.push(TransientItem {
        key: vec!["q".to_string()],
        label: "quit".to_string(),
        description: String::new(),
        kind: TransientItemKind::Dismiss,
    });
    Ok(TransientSpec {
        // The project is NAMED in the title. The whole point of this menu
        // is that you are acting on somewhere you are not standing, so a
        // title that did not say which project would be the one piece of
        // information the user most needs.
        title: format!("Project: {}", projects::basename(root)),
        groups: vec![TransientGroup {
            label: String::new(),
            items,
        }],
        footer: Some(root.to_string()),
    })
}
```

### `id`

```wit
id: func() -> string
```

The menu's name, as `Effect::OpenTransient` names it. Called once, at
load, to key the registry entry.

Guest-controlled, so it is a *name* and nothing more: it grants no
authority, and a plugin that picks a name another source already holds
simply overwrites it (`register`'s last-writer-wins, as for pickers).

**Example — Name the transient menu the host registers and `open-transient` addresses** · [`plugins/project/src/lib.rs`](../../../../plugins/project/src/lib.rs)

```rust
fn id() -> String {
    SWITCH_TRANSIENT.to_string()
}
```

