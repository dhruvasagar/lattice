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

