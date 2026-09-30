<!-- @generated from wit/ by crates/lattice-plugin-api (render.rs).
     Do not edit: run `UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api`. -->

# `picker-registry`

**Direction:** guest calls into the host through it · **Capability:** none (pure data / dispatch) · **Worlds:** `picker-source-plugin` (imports), `project-plugin` (imports)

Mirrors `PickerSourceGenerator` (lattice-picker/src/source.rs:294). A WASM
picker source *exports* this interface; the host wraps its exports as an
`Arc<dyn PickerSourceGenerator>` (PH7.4c.2) and registers it through the
`SubsystemBoot` install seam → `PickerRegistry::register_generator`, so a
plugin source is indistinguishable from a first-party one at the registry.
The ⭐ Phase-7-exit interface; validated by `plugins/fuzzy-finder` (PH7.4d).
OR.5b — the host import a picker plugin registers its sources through.

**Why this is an import and not an export.** Before OR.5b the seam was
shaped "the component IS one picker source": it exported `spec()`, and the
host registered exactly one source per component. That made picker-source
the only contribution seam in the system shaped that way — `language`,
`grammar`, `config`, `modes`, `theme`, `help` and `keymap` are all "the
guest calls a host import to register N things" — and the exception was not
free. Org needs three pickers (refile, roam find-node, roam insert-node) and
could register one.

So this matches the rest: the host calls `register-picker-sources` once, the
guest calls `register-picker-source` for each, and `init` / `accept` take the
source id so one actor serves them all.

## Uses

- [`picker-source-spec`](types.md#record-picker-source-spec) from [`types`](types.md)

## Functions (1)

### `register-picker-source`

```wit
register-picker-source: func(spec: picker-source-spec)
```

Declare one picker source. Called from the guest's
`register-picker-sources` export; the host registers each into the same
`PickerRegistry` a first-party source lives in.

A second registration under an id this plugin already used replaces it —
a plugin reload, not a collision. Two DIFFERENT plugins claiming one id
is resolved the way the registry has always resolved it: last write
wins, and the teardown token unregisters by id.

**Example — Register two picker sources from one component, each with a full spec** · [`crates/lattice-plugin-host/tests/fixtures/picker-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/picker-guest/src/lib.rs)

```rust
fn register_picker_sources() {
    register_picker_source(&PickerSourceSpec {
        id: FIXTURE.to_string(),
        doc: "PH7.4c.1b fixture picker source".to_string(),
        args_schema: Vec::new(),
        args_hint: "[fail]".to_string(),
        live: false,
        // OR.5: the source declares that it can create what the query
        // names. `%s` is replaced by the query when the row renders.
        create_label: Some("Create fixture: %s".to_string()),
        // PP.2: `true` on purpose. The field's failure mode is a boundary
        // arm that writes the default, which a fixture declaring `false`
        // cannot tell apart from one that carries the value — the hole
        // PC.11's `fill-action` shipped through.
        rooted: true,
        // PD.1, same reasoning as `rooted` above and the same hole it
        // guards: a boundary arm writing `None` is indistinguishable from
        // one that carried a `None`, so this source names a command and
        // its sibling names none.
        delete_command: Some("fixture-forget".to_string()),
    });
    register_picker_source(&PickerSourceSpec {
        id: SECOND.to_string(),
        doc: "OR.5b: a SECOND source from the same component".to_string(),
        args_schema: Vec::new(),
        args_hint: String::new(),
        live: false,
        create_label: None,
        // …and `false` here, so the pair proves the value TRAVELS rather
        // than that the host defaults everything to the same answer.
        rooted: false,
        delete_command: None,
    });
}
```

